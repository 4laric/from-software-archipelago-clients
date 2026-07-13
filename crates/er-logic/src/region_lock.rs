//! Pure region-lock decisions, extracted from `features.rs`.
//!
//! These are the decision halves only — the latch/flag side effects stay in the Windows code and
//! get covered later via the `GameHook` seam (PR-C). Here we lock the pure rules: when a region
//! counts as locked (→ kick), and when a natural-key clause set fires.

use std::collections::HashSet;

/// Decide whether the player should be KICKED this tick: the current region is in a locked range
/// AND the random-start guard allows it (non-random seed, or the random-start warp already done).
///
///  - `pr` — raw `play_region_id`. Overworld sub-areas report a 7-digit id (`subregion * 100`); the
///    major area reports the 5-digit subregion. We reduce a 7-digit id to its 5-digit subregion
///    (matches `features.rs`: `if pr >= 1_000_000 { pr / 100 }`).
///  - `area_lock_flags` — `[lo, hi, open_flag]` inclusive 5-digit subregion ranges; a range is
///    locked when its open flag is off.
///  - `random_start_done_flag` — `0` means non-random (no guard); else the kick waits until set.
pub fn kick_decision(
    pr: i32,
    area_lock_flags: &[[i32; 3]],
    random_start_done_flag: u32,
    get_flag: &dyn Fn(u32) -> bool,
) -> bool {
    let sub = if pr >= 1_000_000 { pr / 100 } else { pr };
    let locked = area_lock_flags
        .iter()
        .any(|e| sub >= e[0] && sub <= e[1] && !get_flag(e[2] as u32));
    if !locked {
        return false;
    }
    random_start_done_flag == 0 || get_flag(random_start_done_flag)
}

/// One natural-key clause: ALL items received AND ALL flags set => the clause is satisfied.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NkClause {
    pub items: Vec<String>,
    pub flags: Vec<u32>,
}

/// A region's natural-key trigger fires when ANY clause is satisfied (anyOf disjunction).
pub fn natural_key_fired(
    clauses: &[NkClause],
    received: &HashSet<String>,
    get_flag: &dyn Fn(u32) -> bool,
) -> bool {
    clauses.iter().any(|cl| {
        cl.items.iter().all(|n| received.contains(n)) && cl.flags.iter().all(|&f| get_flag(f))
    })
}

/// Rising-edge enforcement latch for region-lock (and reused for incoming DeathLink). `fire(active)`
/// returns true exactly once each time `active` rises false->true, and re-arms only when `active` goes
/// false. For region-lock, `active` = [`kick_decision`] (locked AND guard-open). This both throttles
/// to one action per lock-entry AND is the death-loop guard under KILL enforcement: after a kill the
/// player respawns; if they land STILL locked, `active` stays true so the latch won't re-fire — it
/// only re-arms once they leave the locked region. Pure + host-tested; the Windows code holds one of
/// these (per enforcement site) and calls `fire` each tick with the live decision.
#[derive(Debug, Default, Clone)]
pub struct EnforcementLatch {
    armed: bool,
}

impl EnforcementLatch {
    pub const fn new() -> Self {
        Self { armed: false }
    }

    /// True on the rising edge of `active` (the one tick it goes false->true); re-arms when `active`
    /// is false. Idempotent while `active` stays true (returns false after the first).
    pub fn fire(&mut self, active: bool) -> bool {
        if active {
            !std::mem::replace(&mut self.armed, true)
        } else {
            self.armed = false;
            false
        }
    }
}

/// A region is bloom-SETTLED only when its open flag AND every warp-unlock grace / reveal flag
/// read back set. Replaces the Windows bloom-pass `get_event_flag(open_flag)` skip-latch
/// (region.rs), which conflated "front door open" with "all graces applied" and stranded
/// interior graces after a save-load (gf-region-grace-loss-frontdoor-latch). Host-tested by
/// `region_lock_replay`.
pub fn region_bloom_settled(open_flag: u32, flags: &[u32], get_flag: &dyn Fn(u32) -> bool) -> bool {
    get_flag(open_flag) && flags.iter().all(|&f| get_flag(f))
}

// ---------------------------------------------------------------------------------------------
// Countdown kick — region-gate polish (additive over the hard `kick_decision` above).
//
// This is PURELY ADDITIVE polish on top of the existing hard region kick: it does NOT change
// whether a player is sealed (that stays `kick_decision`), only WHEN/HOW the kick is announced.
// Today an out-of-sphere player is teleported out with no explanation; the jarring part is the
// *unexplained* kick, not the kick itself (SPEC-gf-boss-lock-tracker.md "Region-gate polish — the
// countdown kick"). `KickCountdown` keeps the hard gate but first surfaces a named warning banner
// ("The seal of <Region> repels you... Ns", naming the missing "<Region> Lock") for a short grace
// window, THEN kicks.
//
// TIME IS INJECTED for testability: `update` takes `now_ms` as an explicit input (no real clock,
// no `std::time`), so the whole state machine replays deterministically in `region_lock_replay`.
//
// INTENDED GAME-FACING CALL SITE (wired separately on Windows — do NOT edit those files here):
//   * The hard kick / teleport lives in `eldenring-ap` `region.rs::tick_kick`, at the
//     `crate::warp::warp_to_grace(ROUNDTABLE_GRACE_ID)` call (the sealed-region warp-out). The
//     Windows wiring holds one `KickCountdown` beside the existing `KICK_LATCH`, feeds it the live
//     `kick_decision` result as `currently_in_sealed` plus the region/lock names, and only performs
//     the warp when `update` returns `KickAction::Kick`.
//   * The warning banner is shown through the same player-facing overlay channel `tick_kick`
//     already uses: its returned `Option<String>` is pushed to `region_msgs` and logged via
//     `self.log(ap::Print::message(..))` in `core.rs` (the persistent overlay console). This is the
//     region-lock message path — distinct from `notif_ticker.rs`, which governs the native
//     right-side item-gain ticker (`showDialogCondType`) for AP item pickups; the countdown banner
//     rides the overlay-console path, not the item ticker. `KickAction::banner()` renders the text.

/// Default grace window before the countdown kick fires, in milliseconds (~10s; SPEC banner shows
/// "...10s"). Override per-instance with [`KickCountdown::with_grace_ms`].
pub const DEFAULT_KICK_GRACE_MS: u64 = 10_000;

/// What the caller should do THIS tick for the region gate. Returned by [`KickCountdown::update`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KickAction {
    /// Not sealed (or the countdown was disarmed): do nothing.
    None,
    /// Inside the grace window: show the warning banner, do NOT teleport yet. `secs_left` counts
    /// down (ceil of remaining ms, so it reads full at arm and hits 1 on the last second before the
    /// kick). `region` / `lock_name` name the sealed region and the missing "<Region> Lock" item.
    Warn {
        region: String,
        secs_left: u32,
        lock_name: String,
    },
    /// Grace elapsed: teleport the player to the nearest open grace (the hard kick).
    Kick { region: String },
}

impl KickAction {
    /// The player-facing warning banner for a [`KickAction::Warn`] (SPEC wording); `None` otherwise.
    /// Pure/ASCII so it round-trips through the overlay-console path unchanged.
    pub fn banner(&self) -> Option<String> {
        match self {
            KickAction::Warn {
                region, secs_left, ..
            } => Some(format!("The seal of {region} repels you... {secs_left}s")),
            _ => None,
        }
    }
}

/// Grace-window state machine for the countdown kick. Deterministic + clock-free: [`update`] takes
/// `now_ms` as an input, so it replays exactly in tests.
///
/// Lifecycle per sealed-region visit:
///  - ENTER a sealed region: the countdown arms at that tick's `now_ms` and starts warning.
///  - each subsequent sealed tick: warns with a decreasing `secs_left` until the grace elapses.
///  - grace elapses: emits exactly one [`KickAction::Kick`], then goes quiet (no per-tick re-kick
///    while the player is still reported sealed — mirrors [`EnforcementLatch`]'s death-loop guard).
///  - LEAVE the region (`currently_in_sealed == false`): disarms and clears the kicked latch, so a
///    later RE-ENTRY re-arms and re-warns from full (kicks are never permanently suppressed).
///
/// [`update`]: KickCountdown::update
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KickCountdown {
    /// Grace window length in ms before the kick fires.
    grace_ms: u64,
    /// `now_ms` at which the current sealed visit armed; `None` when not in a sealed region.
    armed_at: Option<u64>,
    /// Set once the kick has fired for the current sealed visit; cleared on leaving. Prevents the
    /// update from re-emitting `Kick` every tick while the player is still reported sealed.
    kicked: bool,
}

impl Default for KickCountdown {
    fn default() -> Self {
        Self::new()
    }
}

impl KickCountdown {
    /// A countdown with the [`DEFAULT_KICK_GRACE_MS`] grace window.
    pub const fn new() -> Self {
        Self::with_grace_ms(DEFAULT_KICK_GRACE_MS)
    }

    /// A countdown with an explicit grace window (ms).
    pub const fn with_grace_ms(grace_ms: u64) -> Self {
        Self {
            grace_ms,
            armed_at: None,
            kicked: false,
        }
    }

    /// The configured grace window (ms).
    pub const fn grace_ms(&self) -> u64 {
        self.grace_ms
    }

    /// True while a sealed visit is being counted down (armed and not yet kicked).
    pub const fn is_armed(&self) -> bool {
        self.armed_at.is_some() && !self.kicked
    }

    /// Advance the state machine one tick and decide the region-gate action.
    ///
    ///  - `now_ms` — a monotonic-ish tick clock in milliseconds (injected; never read from `std`).
    ///  - `currently_in_sealed` — the hard gate's verdict for THIS tick (typically
    ///    [`kick_decision`]): is the player in a region they should be kicked from?
    ///  - `region_name` / `lock_name` — the sealed region and its missing "<Region> Lock" item, for
    ///    the banner and the returned action.
    ///
    /// Returns [`KickAction::None`] when not sealed, [`KickAction::Warn`] during the grace window,
    /// and exactly one [`KickAction::Kick`] once the grace elapses (then quiet until the player
    /// leaves). A backwards `now_ms` (e.g. a clock reset on load) only re-lengthens the current
    /// window; it never fires a spurious kick.
    pub fn update(
        &mut self,
        now_ms: u64,
        currently_in_sealed: bool,
        region_name: &str,
        lock_name: &str,
    ) -> KickAction {
        if !currently_in_sealed {
            // Left the sealed region (or never sealed): disarm and re-arm the kicked latch.
            self.armed_at = None;
            self.kicked = false;
            return KickAction::None;
        }
        if self.kicked {
            // Already kicked this visit; stay quiet until they leave (guards against re-kick spam
            // if the player is somehow reported still-sealed after the warp).
            return KickAction::None;
        }
        // Sealed and not yet kicked: arm on the first sealed tick, then count down.
        let started = *self.armed_at.get_or_insert(now_ms);
        let elapsed = now_ms.saturating_sub(started);
        if elapsed >= self.grace_ms {
            self.kicked = true;
            KickAction::Kick {
                region: region_name.to_string(),
            }
        } else {
            let remaining = self.grace_ms - elapsed;
            // ceil to whole seconds: reads full (grace/1000) at arm, hits 1 on the final second.
            let secs_left = remaining.div_ceil(1000) as u32;
            KickAction::Warn {
                region: region_name.to_string(),
                secs_left,
                lock_name: lock_name.to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // [lo, hi, open_flag] — a 5-digit subregion range gated on open flag 76980.
    const CAELID_LOCK: [i32; 3] = [60000, 60999, 76980];

    fn names(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn locked_region_with_open_flag_off_kicks() {
        // 5-digit subregion 60010, open flag off -> locked.
        assert!(kick_decision(60010, &[CAELID_LOCK], 0, &|_| false));
    }

    #[test]
    fn normalizes_7digit_overworld_id() {
        // Overworld reports subregion*100 = 60010 * 100 = 6_001_000 (>= 1_000_000);
        // /100 -> 60010, still inside [60000, 60999].
        assert!(kick_decision(6_001_000, &[CAELID_LOCK], 0, &|_| false));
    }

    #[test]
    fn open_flag_set_means_not_locked_no_kick() {
        assert!(!kick_decision(60010, &[CAELID_LOCK], 0, &|f| f == 76980));
    }

    #[test]
    fn region_outside_all_ranges_no_kick() {
        // 5-digit subregion 10000, not in [60000, 60999].
        assert!(!kick_decision(10000, &[CAELID_LOCK], 0, &|_| false));
    }

    #[test]
    fn random_start_guard_suppresses_kick_until_warp_done() {
        let done = 76950u32;
        // Locked region but the random-start warp hasn't fired -> guard suppresses the kick.
        assert!(!kick_decision(60010, &[CAELID_LOCK], done, &|_| false));
        // Once the done flag is set, the guard passes -> kick.
        assert!(kick_decision(60010, &[CAELID_LOCK], done, &|f| f == done));
    }

    #[test]
    fn nk_fully_satisfied_clause_fires() {
        let clauses = vec![NkClause {
            items: vec!["Rold Medallion".into()],
            flags: vec![11000800],
        }];
        let recv = names(&["Rold Medallion"]);
        assert!(natural_key_fired(&clauses, &recv, &|f| f == 11000800));
    }

    #[test]
    fn nk_item_present_but_flag_missing_does_not_fire() {
        let clauses = vec![NkClause {
            items: vec!["Rold Medallion".into()],
            flags: vec![11000800],
        }];
        let recv = names(&["Rold Medallion"]);
        assert!(!natural_key_fired(&clauses, &recv, &|_| false));
    }

    #[test]
    fn nk_flag_set_but_item_missing_does_not_fire() {
        let clauses = vec![NkClause {
            items: vec!["Rold Medallion".into()],
            flags: vec![11000800],
        }];
        assert!(!natural_key_fired(&clauses, &names(&[]), &|f| f == 11000800));
    }

    #[test]
    fn nk_second_clause_satisfied_fires_even_if_first_isnt() {
        let clauses = vec![
            NkClause {
                items: vec!["Missing".into()],
                flags: vec![],
            },
            NkClause {
                items: vec![],
                flags: vec![71000, 71001],
            },
        ];
        assert!(natural_key_fired(&clauses, &names(&[]), &|f| f == 71000 || f == 71001));
    }

    #[test]
    fn nk_empty_clause_is_vacuously_true() {
        let clauses = vec![NkClause::default()];
        assert!(natural_key_fired(&clauses, &names(&[]), &|_| false));
    }

    // --- EnforcementLatch (kick/kill rising-edge throttle + death-loop guard) ---

    #[test]
    fn latch_fires_once_on_entry() {
        let mut l = EnforcementLatch::new();
        assert!(l.fire(true)); // rising edge -> fire
        assert!(!l.fire(true)); // still locked -> no re-fire
        assert!(!l.fire(true));
    }

    #[test]
    fn latch_rearms_after_leaving_and_refires() {
        let mut l = EnforcementLatch::new();
        assert!(l.fire(true)); // enter locked -> fire
        assert!(!l.fire(false)); // leave (unlocked) -> re-arm, no fire
        assert!(l.fire(true)); // re-enter -> fire again
    }

    #[test]
    fn latch_death_loop_guard_no_refire_while_locked() {
        // Models a KILL whose respawn lands the player STILL in the locked region: `active` stays
        // true across the kill+respawn, so the latch must NOT re-fire (no death loop).
        let mut l = EnforcementLatch::new();
        assert!(l.fire(true)); // violation -> kill
        for _ in 0..100 {
            assert!(!l.fire(true)); // respawned still-locked -> never re-fires
        }
        assert!(!l.fire(false)); // finally leaves -> re-arm
        assert!(l.fire(true)); // a fresh violation later fires
    }

    #[test]
    fn latch_inactive_never_fires() {
        let mut l = EnforcementLatch::new();
        assert!(!l.fire(false));
        assert!(!l.fire(false));
    }
}

// ---------------------------------------------------------------------------------------------
// Baked-table fallback — region lock for FOREIGN apworlds (bedrock interop).
//
// Region geometry (play_region ids) and per-region open flags are STATIC GAME DATA, baked into
// this crate as the GENERATED `region_locks` module (tools/gen_region_locks.py in the apworld
// repo — never hand-edited, drift-gated in CI exactly like `tracker_regions`). The only
// seed-specific input is which regions are in the pool, and a foreign apworld communicates that
// simply by NAMING its lock items "<Region> Lock". slot_data always WINS: this path exists only
// for seeds that ship NEITHER `areaLockFlags` NOR `regionOpenFlags` (the Windows glue gates on
// key ABSENCE, region.rs).
//
// ARMING IS EVIDENCE-BASED, deliberately. The obvious signal — "a '<Region> Lock' name exists
// in the seed's item table" — is UNSAFE, measured on the real foreign apworld
// (fswap/Archipelago@er, 2026-07-12): its `apIdsToItemIds` carries the WHOLE item table
// ("all the items the game knows about", its fill_slot_data says so) on EVERY seed, including
// world_logic=open_world seeds whose pool contains no locks at all. Arming on table presence
// would seal name-matching regions forever on a no-lock seed — the exact "kick the player out
// of a region the seed never locked" failure the foreign_apworld_degrade tests forbid. So:
//   * the item TABLE only sets the SCOPE (which regions WOULD be gated),
//   * enforcement arms only when a scoped lock is actually RECEIVED — proof the pool placed
//     locks this seed. Until then the watch stays cold (under-enforce, never mis-kick).
// A region whose lock exists in scope but never arrives stays sealed once armed (= a sealed
// region, same semantics as the generator's dead-drop ranges). Unknown "<X> Lock" names and
// baked regions without an open flag are reported, never a panic.

/// Region-lock config derived from the baked table for a foreign seed: the exact shapes
/// region.rs::RegionConfig consumes (`open_flags` -> regionOpenFlags, `ranges` ->
/// areaLockFlags), plus the telemetry lists the caller logs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DerivedLocks {
    /// lock item name -> the region's open flag (sorted by name; deterministic).
    pub open_flags: Vec<(String, u32)>,
    /// `[pid, pid, open_flag]` kick-watch ranges, one per play_region id of each scoped region.
    pub ranges: Vec<[i32; 3]>,
    /// "<X> Lock"-shaped names with no baked region — a foreign granularity we cannot map
    /// (e.g. a lock naming only part of one of our regions). Ignored + logged, never fatal.
    pub unknown: Vec<String>,
    /// Names that matched a baked region which has NO resolved open flag (geometry only) —
    /// we cannot gate that region, so its lock is inert. Logged.
    pub ungateable: Vec<String>,
}

impl DerivedLocks {
    pub fn is_empty(&self) -> bool {
        self.open_flags.is_empty()
    }
}

/// Derive the fallback region-lock config from the seed's item NAMES (any iterable source —
/// the Windows glue feeds datapackage-resolved `apIdsToItemIds` ids). Only names ending in
/// " Lock" are considered; everything else is silently ignored (this gets called with whole
/// item tables). Output is sorted and deduplicated, so the same name set always yields the
/// same config regardless of source order.
pub fn derive_region_locks<'a>(seed_item_names: impl IntoIterator<Item = &'a str>) -> DerivedLocks {
    let mut d = DerivedLocks::default();
    let mut names: Vec<&str> = seed_item_names
        .into_iter()
        .filter(|n| n.ends_with(" Lock"))
        .collect();
    names.sort_unstable();
    names.dedup();
    for name in names {
        match crate::region_locks::by_lock_item(name) {
            Some(r) => match r.open_flag {
                Some(flag) => {
                    d.open_flags.push((name.to_string(), flag));
                    for &pid in r.play_regions {
                        d.ranges.push([pid, pid, flag as i32]);
                    }
                }
                None => d.ungateable.push(name.to_string()),
            },
            None => d.unknown.push(name.to_string()),
        }
    }
    d
}

/// The arming decision: true once ANY scoped lock item has actually been RECEIVED (see the
/// module note above for why receipt, not table presence, is the evidence bar).
pub fn fallback_armed(
    derived: &DerivedLocks,
    received: &std::collections::HashSet<String>,
) -> bool {
    derived.open_flags.iter().any(|(n, _)| received.contains(n))
}

#[cfg(test)]
mod derive_tests {
    use super::*;
    use std::collections::HashSet;

    /// A baked region WITH an open flag, straight from the generated table (no hand-pinned
    /// ids — the table is the ground truth this logic runs against).
    fn some_flagged() -> &'static crate::region_locks::BakedRegionLock {
        crate::region_locks::REGION_LOCKS
            .iter()
            .find(|r| r.open_flag.is_some())
            .expect("baked table has flagged regions")
    }

    #[test]
    fn empty_names_derive_nothing() {
        let d = derive_region_locks([]);
        assert!(d.is_empty() && d.ranges.is_empty() && d.unknown.is_empty());
    }

    #[test]
    fn a_baked_lock_name_derives_its_flag_and_one_range_per_play_region() {
        let r = some_flagged();
        let d = derive_region_locks([r.lock_item]);
        assert_eq!(
            d.open_flags,
            vec![(r.lock_item.to_string(), r.open_flag.unwrap())]
        );
        assert_eq!(d.ranges.len(), r.play_regions.len());
        for (range, &pid) in d.ranges.iter().zip(r.play_regions) {
            assert_eq!(*range, [pid, pid, r.open_flag.unwrap() as i32]);
        }
    }

    #[test]
    fn whole_item_tables_pass_through_only_locks() {
        // Feed a realistic mixed table: gear, key items, and one real lock. Non-" Lock" names
        // never show up anywhere — not even in `unknown`.
        let r = some_flagged();
        let d = derive_region_locks(["Rusty Key", "Uchigatana", r.lock_item, "Golden Seed"]);
        assert_eq!(d.open_flags.len(), 1);
        assert!(d.unknown.is_empty());
    }

    #[test]
    fn foreign_granularity_is_reported_not_invented() {
        // SYNTHETIC names of the observed foreign SHAPE (provenance: hand-written, not the
        // foreign world's data): a "<X> Lock" whose region is not in our baked table must land
        // in `unknown` with no flag and no range — we log it, we never guess a region for it.
        let d = derive_region_locks(["Zzz Nonexistent Region Lock"]);
        assert!(d.is_empty() && d.ranges.is_empty());
        assert_eq!(d.unknown, vec!["Zzz Nonexistent Region Lock".to_string()]);
    }

    #[test]
    fn geometry_only_regions_are_ungateable_not_sealed() {
        // Baked regions with open_flag None (geometry known, no resolved warp-grace flag) must
        // produce NO range: a range we could never unlock would seal the region forever.
        let Some(r) = crate::region_locks::REGION_LOCKS
            .iter()
            .find(|r| r.open_flag.is_none())
        else {
            return; // every baked region has a flag now — nothing to test
        };
        let d = derive_region_locks([r.lock_item]);
        assert!(d.is_empty() && d.ranges.is_empty());
        assert_eq!(d.ungateable, vec![r.lock_item.to_string()]);
    }

    #[test]
    fn duplicate_and_unordered_sources_yield_one_deterministic_config() {
        // The glue unions apIdsToItemIds names with the received stream — same lock can arrive
        // from both. One flag entry, one range set, source order irrelevant.
        let r = some_flagged();
        let a = derive_region_locks([r.lock_item, "Uchigatana", r.lock_item]);
        let b = derive_region_locks(["Uchigatana", r.lock_item]);
        assert_eq!(a, b);
        assert_eq!(a.open_flags.len(), 1);
        assert_eq!(a.ranges.len(), r.play_regions.len());
    }

    #[test]
    fn armed_only_by_receipt_of_a_scoped_lock() {
        // The degrade contract: table presence alone must never arm (measured hazard — the
        // foreign apworld ships its whole item table on no-lock seeds). Receipt of a scoped
        // lock arms; receipt of anything else does not.
        let r = some_flagged();
        let d = derive_region_locks([r.lock_item]);
        let mut received: HashSet<String> = HashSet::new();
        assert!(
            !fallback_armed(&d, &received),
            "cold until a lock is RECEIVED"
        );
        received.insert("Uchigatana".to_string());
        assert!(!fallback_armed(&d, &received));
        received.insert(r.lock_item.to_string());
        assert!(fallback_armed(&d, &received));
        // And an empty derivation can never arm, whatever arrives.
        assert!(!fallback_armed(&DerivedLocks::default(), &received));
    }

    #[test]
    fn derived_ranges_drive_kick_decision_like_slot_data_ranges() {
        // End-to-end with the pure kick: a scoped region is sealed (kick) while its flag is
        // off and open (no kick) once the lock's flag is set — same rule slot_data ranges obey.
        let r = some_flagged();
        let flag = r.open_flag.unwrap();
        let d = derive_region_locks([r.lock_item]);
        let pid = r.play_regions[0];
        assert!(
            kick_decision(pid, &d.ranges, 0, &|_| false),
            "sealed -> kick"
        );
        assert!(
            !kick_decision(pid, &d.ranges, 0, &|f| f == flag),
            "lock received (flag on) -> open"
        );
        // A play_region NOT in any baked scope is never kicked.
        assert!(
            !kick_decision(11100, &d.ranges, 0, &|_| false),
            "hub is unscoped"
        );
    }
}
