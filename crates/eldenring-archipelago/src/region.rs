//! Region locks (Milestone B Stage 3). The pure decision is `er_logic::region_lock::kick_decision`
//! (host-tested); this module is the game-side glue: parse the region config out of slot_data, set
//! the baked KICK flag (76970) when the player is in a locked region, and open regions on receipt of
//! their unlock item.
//!
//! PURE-RUNTIME (2026-07-01/02, baker retired): both halves that used to be baked reactors are now
//! client-side. Kick enforcement = warp-out to Roundtable via `warp::warp_to_grace` (tick_kick;
//! kill only as fallback; flag 76970 still set for bake-compat). Random-start = `tick_random_start_warp`: `randomStartAreaId` (18000) is the
//! TRIGGER area (tutorial / Chapel of Anticipation -- REGION_ID_MAP.md), NOT the destination.
//! A fresh character in the trigger area gets the retired reactor's job done client-side
//! (`warp::warp_to_grace` out to the hub/rolled grace); an established character just has the
//! trigger consumed in place. Both paths latch `randomStartDoneFlag` (76968, persistent) +
//! `randomStartWarpFlag` (76969, bake-compat), and the done flag unblocks KICK's start-window
//! guard (see `kick_decision`) -- until it sets, region enforcement is silently OFF (seen live
//! 2026-07-02: area_locks=42 configured, zero kicks all day).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use serde_json::Value;

use er_logic::region_lock::EnforcementLatch;

use crate::flags;

/// Flag the baked `common.emevd` reactor (event 6970) watches: set while the player is in a locked
/// region -> the reactor warps them to Roundtable Hold and clears it.
const KICK_FLAG: u32 = 76970;

/// Once-per-lock-entry latch for setting KICK_FLAG (rising edge of `kick_decision`); the reactor's
/// warp ejects the player and the latch re-arms once they're back in an open region. Pure er-logic type.
static KICK_LATCH: Mutex<EnforcementLatch> = Mutex::new(EnforcementLatch::new());

/// Latch so the random-start warp trigger fires once per session (the persistent `randomStartDoneFlag`
/// is the cross-session guard; this is the in-session dedup, mirroring the standalone `START_LATCHED`).
static START_LATCHED: AtomicBool = AtomicBool::new(false);

/// kick-watch diagnostic: last play_region_id seen by tick_kick (i32::MIN = none yet).
static KICK_WATCH_LAST_PR: AtomicI32 = AtomicI32::new(i32::MIN);

/// When in_world first went true for the CURRENT world session (None while at menu/loading).
/// play_region_id can serve a STALE region for a moment after a load, so the random-start
/// trigger waits out a settle window after every world entry before trusting it.
static WARP_WORLD_SETTLE: Mutex<Option<std::time::Instant>> = Mutex::new(None);
const WARP_SETTLE_SECS: u64 = 5; // let play_region_id settle after world entry before trusting it

/// Warp destination fallback for seeds whose apworld predates the `randomStartGraceId`
/// slot_data key: grace entity 11102950 = Table of Lost Grace, Roundtable Hold (the hub the
/// shipping random-start mode warps out to). Same id the CE table's warp uses.
const ROUNDTABLE_GRACE_ID: u32 = 11102950;

/// One disjunctive clause of a natural-key trigger: satisfied when ALL `items` were received AND ALL
/// `flags` are set. Ported from the standalone `features.rs::NkClause`.
#[derive(Default)]
pub struct NkClause {
    pub items: Vec<String>,
    pub flags: Vec<u32>,
}

/// Region-lock config, parsed from slot_data (shapes mirror the standalone `net.rs`).
#[derive(Default)]
pub struct RegionConfig {
    /// `[lo, hi, open_flag]` inclusive 5-digit subregion ranges; locked when the open flag is off.
    pub area_lock_flags: Vec<[i32; 3]>,
    /// `0` = non-random seed (no start guard); else KICK waits until this flag is set.
    pub random_start_done_flag: u32,
    /// `0` = no random start; else the baked warp trigger flag to set once you reach the start area.
    pub random_start_warp_flag: u32,
    /// `0` = no random start; else the play-region id of the rolled start area (where to fire the warp).
    pub random_start_area_id: i32,
    /// Grace ENTITY id to physically warp to on a random-start seed (pure-runtime warp primitive).
    /// `0` = not emitted by the apworld yet; `tick_random_start_warp` then falls back to the
    /// Roundtable grace when the start area is the Roundtable hub (area 18000), else logs the gap.
    pub random_start_grace_id: u32,
    /// lock item name -> the region's physical open flag.
    pub region_open_flags: HashMap<String, u32>,
    /// lock item name -> map-reveal / enforcement-open flags.
    pub lock_reveal_flags: HashMap<String, Vec<u32>>,
    /// lock item name -> grace warp-unlock flags.
    pub region_graces: HashMap<String, Vec<u32>>,
    /// grace_rando: "Grace: ..." item name -> that grace's warp-unlock flag (slot_data graceItems).
    pub grace_items: HashMap<String, u32>,
    /// region (lock name) -> disjunction of natural-key clauses. When ANY clause holds, the region's
    /// apparatus blooms WITHOUT an AP lock item being received (vanilla keys / world flags). The
    /// region's open flag doubles as the once-latch. (Ported from the standalone naturalKeyTriggers.)
    pub natural_key_triggers: HashMap<String, Vec<NkClause>>,
    /// lock item name -> packed FullIDs to physically grant in-game on that lock's FIRST open
    /// (slot_data `lockGrantItems`). Currently the unpooled medallions riding their locks
    /// (Rold -> Mountaintops Lock; both Secret Medallion halves -> Snowfield Lock), so the Grand
    /// Lift stays usable and medallion-triggered quest content (Ensha, Latenna) fires naturally.
    /// SPEC-region-spine-surgery.md SS3.5 (grant-on-receipt rider).
    pub lock_grant_items: HashMap<String, Vec<i32>>,
    /// BAKED-TABLE FALLBACK (bedrock interop): prepared at connect -- from the generated
    /// `er_logic::region_locks` table -- for seeds that ship NEITHER `areaLockFlags` NOR
    /// `regionOpenFlags`. Holds the scoped-but-COLD derived config until `tick_baked_fallback`
    /// merges it into the live fields on first receipt of a scoped "<Region> Lock".
    pub baked_fallback: Option<er_logic::region_lock::DerivedLocks>,
}

// --- areaLockFlags: SINGLE SOURCE OF TRUTH is the apworld's data ----------------------------------
// The region -> play_region geometry lives in exactly ONE editable place: the generator's
// features/area_locks.py, which ships the fully-resolved kick-watch ranges as slot_data
// `areaLockFlags`, and slot_data always WINS. The client's only copy of that geometry is the
// GENERATED `er_logic::region_locks` table (tools/gen_region_locks.py, CI drift-gated) -- it feeds
// the FOREIGN-apworld fallback below and is never consulted while slot_data speaks. A HAND-typed
// mirror in this file repeatedly drifted from the generator (e.g. the Consecrated Snowfield ->
// Mountaintops fold); a gen-side test (test_gf_data.py) still forbids one here, and the generated
// table cannot drift because CI regenerates and diffs it.

pub fn parse(sd: &Value) -> RegionConfig {
    // Re-arm the random-start warp latch on each fresh parse (mirrors the standalone `configure`
    // per-connect reset) so a second seed loaded in the same game process can warp again. The
    // persistent `randomStartDoneFlag` still prevents a re-warp within one save.
    START_LATCHED.store(false, Ordering::Relaxed);
    *WARP_WORLD_SETTLE.lock().unwrap() = None;
    let region_open_flags = str_to_u32(sd.get("regionOpenFlags"));
    // Kick-watch ranges come straight from the generator's `areaLockFlags` (single source of truth;
    // see the note above). parse() itself never derives ranges: an apworld that EMITS region keys
    // (either of them) said what it wanted, and a seed that ships an empty/absent table keeps an
    // empty kick-watch. The one exception lives OUTSIDE parse: for a seed that ships NEITHER
    // region key (a region-lock-ignorant foreign apworld), core.rs may prepare the baked-table
    // fallback -- see `prepare_baked_fallback` / `tick_baked_fallback` below.
    let area_lock_flags = parse_triples(sd.get("areaLockFlags"));
    RegionConfig {
        area_lock_flags,
        random_start_done_flag: sd
            .get("randomStartDoneFlag")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        random_start_warp_flag: sd
            .get("randomStartWarpFlag")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        random_start_area_id: sd
            .get("randomStartAreaId")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        random_start_grace_id: sd
            .get("randomStartGraceId")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        region_open_flags,
        lock_reveal_flags: str_to_u32vec(sd.get("lockRevealFlags")),
        region_graces: str_to_u32vec(sd.get("regionGraces")),
        grace_items: str_to_u32(sd.get("graceItems")),
        natural_key_triggers: parse_natural_keys(sd.get("naturalKeyTriggers")),
        lock_grant_items: str_to_i32vec(sd.get("lockGrantItems")),
        baked_fallback: None,
    }
}

/// Per-tick (settled / in-world): bloom regions whose natural-key trigger disjunction is now
/// satisfied. A clause fires when ALL its items are in `received` AND ALL its flags are set; ANY
/// clause fires the region. The region's open flag doubles as the once-latch, so this is idempotent
/// and cheap after the first bloom. Sets graces + open flag + reveal flags directly (the converged
/// client sets flags directly, unlike the standalone's queue). Mirrors `EvaluateNaturalKeyTriggers`.
pub fn tick_natural_key_triggers(cfg: &RegionConfig, received: &HashSet<String>) {
    if cfg.natural_key_triggers.is_empty() {
        return;
    }
    for (name, clauses) in &cfg.natural_key_triggers {
        let open_flag = match cfg.region_open_flags.get(name) {
            Some(&f) => f,
            None => continue, // no apparatus to bloom
        };
        // Reconcile-safe latch (gf-region-grace-loss-frontdoor-latch): skip only when the region
        // is FULLY bloomed -- open flag AND every grace AND every reveal flag observed set.
        // Latching on the open flag alone stranded interior graces after a save-load when the
        // front-door grace doubles as the open flag (Limgrave 73100). Pure gate host-tested by
        // region_lock_replay.
        let mut bloom_flags: Vec<u32> = Vec::new();
        if let Some(fs) = cfg.region_graces.get(name) {
            bloom_flags.extend_from_slice(fs);
        }
        if let Some(fs) = cfg.lock_reveal_flags.get(name) {
            bloom_flags.extend_from_slice(fs);
        }
        if er_logic::region_lock::region_bloom_settled(open_flag, &bloom_flags, &|f| {
            flags::get_event_flag(f)
        }) {
            continue; // fully bloomed -- reconcile-safe
        }
        let fired = clauses.iter().any(|cl| {
            cl.items.iter().all(|nm| received.contains(nm))
                && cl.flags.iter().all(|&fl| flags::get_event_flag(fl))
        });
        if !fired {
            continue;
        }
        let mut n = 0u32;
        if let Some(fs) = cfg.region_graces.get(name) {
            for &f in fs {
                flags::set_event_flag(f, true);
                n += 1;
            }
        }
        flags::set_event_flag(open_flag, true);
        n += 1;
        if let Some(fs) = cfg.lock_reveal_flags.get(name) {
            for &f in fs {
                flags::set_event_flag(f, true);
                n += 1;
            }
        }
        log::info!("Natural-key '{name}' satisfied -> bloomed region ({n} flag(s) set)");
    }
}

/// `{ "LockName": { "anyOf": [ {"items":[..],"flags":[..]}, ... ] } }` -> region -> clause disjunction.
/// Ported from the standalone `net.rs::parse_natural_keys`.
fn parse_natural_keys(v: Option<&Value>) -> HashMap<String, Vec<NkClause>> {
    let mut m = HashMap::new();
    if let Some(Value::Object(o)) = v {
        for (region, body) in o {
            let mut clauses = Vec::new();
            if let Some(any_of) = body.get("anyOf").and_then(|x| x.as_array()) {
                for c in any_of {
                    let items = c
                        .get("items")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    let flags = c
                        .get("flags")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_u64().map(|n| n as u32))
                                .collect()
                        })
                        .unwrap_or_default();
                    clauses.push(NkClause { items, flags });
                }
            }
            m.insert(region.clone(), clauses);
        }
    }
    m
}

/// Per-tick: when the player enters a locked region, warp them out to Roundtable Hold (the
/// retired baked reactor's behavior, now done client-side via `warp::warp_to_grace`; kill only
/// as fallback). Evaluated EVERY tick; the rising-edge latch throttles the action to once per
/// sealed-region entry and re-arms once the warp lands the player back in an open region.
/// KICK_FLAG still set for bake-compat. Returns a player-facing overlay message when the kick
/// fires (the caller logs it -- players otherwise get relocated with no explanation).
pub fn tick_kick(cfg: &RegionConfig) -> Option<String> {
    let pr = flags::play_region_id()?;
    let kick = er_logic::region_lock::kick_decision(
        pr,
        &cfg.area_lock_flags,
        cfg.random_start_done_flag,
        &|f| flags::get_event_flag(f),
    );
    // KICK-WATCH (2026-07-02 diagnostic, keep -- cheap and this path has burned us twice): log
    // every play-region CHANGE with the full lock evaluation, so a silent no-kick session tells
    // us exactly what the client saw (id-space mismatch vs stale pr vs open-flag state).
    {
        let last = KICK_WATCH_LAST_PR.swap(pr, Ordering::Relaxed);
        if last != pr {
            let sub = if pr >= 1_000_000 { pr / 100 } else { pr };
            let hit = cfg
                .area_lock_flags
                .iter()
                .find(|e| sub >= e[0] && sub <= e[1]);
            let gate_open = cfg.random_start_done_flag == 0
                || flags::get_event_flag(cfg.random_start_done_flag);
            match hit {
                Some(e) => log::info!(
                    "kick-watch: play_region {last} -> {pr} (sub {sub}); range [{},{}] flag {} = {} | start-gate open = {gate_open} | kick = {kick}",
                    e[0],
                    e[1],
                    e[2],
                    flags::get_event_flag(e[2] as u32)
                ),
                None => log::info!(
                    "kick-watch: play_region {last} -> {pr} (sub {sub}); NO lock range covers it ({} ranges) | start-gate open = {gate_open}",
                    cfg.area_lock_flags.len()
                ),
            }
        }
    }
    if KICK_LATCH.lock().unwrap().fire(kick) {
        // PURE-RUNTIME KICK = WARP-OUT (2026-07-02, replaces the 2026-07-01 kill stopgap): the
        // baked reactor's actual behavior was "warp to Roundtable Hold", and the latch's
        // rising-edge model DEPENDS on the player leaving the sealed region -- a kill respawns
        // them at the last grace, which can be INSIDE the region, so kick stayed true, the
        // latch never re-armed, and enforcement was one death then free roam. Warping out also
        // retires the kick rune-loss wart (P1 kick-keep-runes). Kill remains only as the
        // fallback when the warp primitive is unavailable (stale RVA on a new game build).
        flags::set_event_flag(KICK_FLAG, true);
        match crate::warp::warp_to_grace(ROUNDTABLE_GRACE_ID) {
            Ok(()) => {
                log::info!(
                    "RegionLock: area {pr} LOCKED -> kick warp to Roundtable (flag {KICK_FLAG} set)"
                );
                return Some(format!(
                    "SEALED REGION (area {pr}) -- lock not received yet. Returning to Roundtable Hold."
                ));
            }
            Err(e) => {
                let killed = crate::deathlink::kill_local_player();
                log::warn!(
                    "RegionLock: area {pr} LOCKED -> kick warp FAILED ({e}); fallback kill (direct={killed})"
                );
                return Some(format!(
                    "SEALED REGION (area {pr}) -- lock not received yet. Kicked."
                ));
            }
        }
    }
    None
}

/// Per-tick: on a random-start seed, set the baked warp trigger ONCE when the player reaches the
/// rolled start area. Sets `randomStartDoneFlag` (persistent guard, also unblocks KICK) +
/// `randomStartWarpFlag` (the bake's `WarpPlayer` reactor keys on this). No-op on non-random seeds
/// (all three values are 0) or after the warp has fired. Mirrors the standalone `features.rs` latch.
/// Returns a player-facing overlay message on warp request / trigger consumption (the caller
/// logs it).
///
/// SEMANTICS (corrected 2026-07-02): `randomStartAreaId` is the TRIGGER area, not the
/// destination -- REGION_ID_MAP.md: 18000 = Stranded Graveyard / Chapel of Anticipation
/// (tutorial), annotated "= randomStartAreaId"; Roundtable Hold is 11100. Baked-era flow: a
/// FRESH character spawns in the tutorial (18000), the client sets the trigger flags there, and
/// the bake's WarpPlayer reactor warped them OUT to the rolled start. The first port of this
/// function misread the id as the destination and warped the player TO the hub whenever they
/// were anywhere else -- i.e. always (seen live: 3x re-warp to Roundtable mid-run, cap, kick
/// gated forever). Pure-runtime flow now:
///   - pr == trigger area (fresh character in the tutorial): set done+warp flags, then
///     physically warp to the hub/rolled grace (the reactor's job, ours now).
///   - pr != trigger area with done unset (established character, e.g. a save from before this
///     fix): the start already happened -- consume the trigger WITHOUT warping, which arms KICK.
pub fn tick_random_start_warp(cfg: &RegionConfig) -> Option<String> {
    if cfg.random_start_warp_flag == 0
        || cfg.random_start_area_id == 0
        || cfg.random_start_done_flag == 0
    {
        return None; // not a random-start seed
    }
    if flags::get_event_flag(cfg.random_start_done_flag) {
        return None; // trigger already consumed (persisted across sessions)
    }
    if START_LATCHED.load(Ordering::Relaxed) {
        return None; // consumed this session; the persistent flag lands with the next save-sync
    }
    let pr = flags::play_region_id()?;
    // Interior play regions are 7-digit (bucket*100 + sub) -- normalize to the 5-digit bucket
    // slot_data speaks, the SAME rule kick_decision applies.
    let pr = if pr >= 1_000_000 { pr / 100 } else { pr };

    // Settle window: don't trust the play region until in_world has been continuously true for
    // WARP_SETTLE_SECS (stale pr right after a load). Resets on every menu/load.
    {
        let mut settle = WARP_WORLD_SETTLE.lock().unwrap();
        if !crate::flags::in_world() {
            *settle = None;
            return None;
        }
        let entered = settle.get_or_insert_with(std::time::Instant::now);
        if entered.elapsed() < std::time::Duration::from_secs(WARP_SETTLE_SECS) {
            return None;
        }
    }

    // R4 (SWEEP): only latch once the flag writes verifiably stuck (a discarded write would
    // otherwise keep KICK's start-window guard closed all session). Both branches consume the
    // trigger the same way; they differ only in whether a physical warp follows.
    let _ = flags::try_set_event_flag(cfg.random_start_done_flag, true);
    let _ = flags::try_set_event_flag(cfg.random_start_warp_flag, true);
    if !flags::get_event_flag(cfg.random_start_done_flag)
        || !flags::get_event_flag(cfg.random_start_warp_flag)
    {
        return None; // flag holder not ready -- retry next tick
    }
    START_LATCHED.store(true, Ordering::Relaxed);
    log::info!(
        "RandomStart: trigger consumed in area {pr} (done {} / warp {})",
        cfg.random_start_done_flag,
        cfg.random_start_warp_flag
    );

    if pr != cfg.random_start_area_id {
        // Established character already out in the world: no warp, just arm enforcement.
        return Some("Region-lock enforcement armed.".to_string());
    }

    // Fresh character in the tutorial: do the retired reactor's job and warp them out.
    let target = if cfg.random_start_grace_id != 0 {
        cfg.random_start_grace_id
    } else {
        // apworld doesn't emit randomStartGraceId yet; the Roundtable-hub mode is the only
        // shipping random-start flavor, so its grace is the fallback destination.
        ROUNDTABLE_GRACE_ID
    };
    match crate::warp::warp_to_grace(target) {
        Ok(()) => {
            log::info!("RandomStart: fresh start -> warp to grace {target} requested");
            Some("Warping to your start region...".to_string())
        }
        Err(e) => {
            log::warn!(
                "RandomStart: start warp to grace {target} FAILED ({e}) -- travel out manually (trigger already consumed, enforcement armed)"
            );
            Some("Auto-warp failed -- travel to your start region manually.".to_string())
        }
    }
}

/// Per-tick (settled / in-world): reconcile received lock items whose region never actually
/// opened. `open_on_received_name` fires ONCE per receive and its flag writes are silently
/// discarded when the game isn't ready (menu/load) -- the dispatch watermark advances anyway, so
/// the unlock (open flag + graces + reveals) was LOST for the session (seen live 2026-07-01:
/// lock received, no graces). The region open flag doubles as the latch, so this is idempotent
/// and cheap once applied. Same pattern as `tick_natural_key_triggers`.
/// KNOWN EDGE: a PARTIAL application (open flag landed, graces lost mid-batch) latches and won't
/// re-heal -- rare, since a not-ready game discards the whole batch together.
pub fn tick_reconcile_received_locks(cfg: &RegionConfig, received: &HashSet<String>) {
    // Menu/load gate (2026-07-01 playtest: retry-SPAMMED at menu -- the caller's can_grant
    // (inventory) resolves before flag writes stick, so every re-apply was discarded and
    // re-logged per tick). in_world() is the same signal the other flag writers gate on.
    if !crate::flags::in_world() {
        return;
    }
    for (name, &open_flag) in &cfg.region_open_flags {
        if !received.contains(name) || flags::get_event_flag(open_flag) {
            continue;
        }
        log::info!("RegionLock '{name}': received but never applied -- reconciling");
        open_on_received_name(cfg, name);
    }
    // BUNDLE_LOCK_GRACE_RECONCILE: grace-only bundle locks (Spelunker torches) have NO region_open_flags
    // entry, so the loop above never reconciles them -- a grant lost to a not-ready receive
    // stays lost (2026-07-04 softlock: Ghostflame Torch was the sole sphere-0 key). Re-apply
    // each received lock's graces directly, using every grace flag as its own try_set latch
    // (idempotent; only the unset flags re-try). Also heals the PARTIAL-application edge for
    // open-flag locks (open flag landed, some graces lost mid-batch) that the loop above skips.
    for (name, fs) in &cfg.region_graces {
        if !received.contains(name) {
            continue;
        }
        for &f in fs {
            if !flags::get_event_flag(f) {
                let _ = flags::try_set_event_flag(f, true);
            }
        }
    }
}

/// Per-tick (settled / in-world): light received grace_rando "Grace: ..." items. PORT-GAP wired
/// 2026-07-01: `graceItems` was emitted but consumed by NOTHING (its client half was retired with
/// the C++ client), so grace items granted from the pool did nothing in-game. Reconciled with the
/// grace flag itself as the latch, and try_set (only latch on a successful write) so a receive at
/// menu/load self-heals next settled tick. Returns names lit this tick for the overlay console.
pub fn tick_grace_items(cfg: &RegionConfig, received: &HashSet<String>) -> Vec<String> {
    let mut lit = Vec::new();
    for (name, &flag) in &cfg.grace_items {
        if received.contains(name)
            && !flags::get_event_flag(flag)
            && flags::try_set_event_flag(flag, true)
        {
            log::info!("GraceItem '{name}' -> grace flag {flag} lit");
            lit.push(name.clone());
        }
    }
    lit
}

/// lockGrantItems rider check: the packed FullIDs to grant for `name`, but ONLY when this is the
/// lock's FIRST open (its open flag is still OFF -- the same once-latch the natural-key bloom
/// uses). Call BEFORE `open_on_received_name` (which sets the flag). Reconnect replays re-run
/// `on_item_received` for every item; the latch keeps the physical grant once-per-save.
pub fn first_open_grants(cfg: &RegionConfig, name: &str) -> Vec<i32> {
    match (
        cfg.lock_grant_items.get(name),
        cfg.region_open_flags.get(name),
    ) {
        (Some(ids), Some(&f)) if !flags::get_event_flag(f) => ids.clone(),
        _ => Vec::new(),
    }
}

/// On receiving an unlock item (by name): open its region + reveal/grace flags. Idempotent. Returns
/// true if `name` is a region-lock item (so the caller can surface a console notification).
pub fn open_on_received_name(cfg: &RegionConfig, name: &str) -> bool {
    let mut opened = false;
    if let Some(&f) = cfg.region_open_flags.get(name) {
        flags::set_event_flag(f, true);
        log::info!("RegionLock '{name}' received -> open flag {f}");
        opened = true;
    }
    if let Some(fs) = cfg.lock_reveal_flags.get(name) {
        for &f in fs {
            flags::set_event_flag(f, true);
        }
    }
    if let Some(fs) = cfg.region_graces.get(name) {
        // INSTRUMENT (patch_log_grace_readback): set each grace flag, then read it back so the
        // log pins whether these writes actually land in EventFlagMan.
        let mut set = 0usize;
        let mut failed: Vec<u32> = Vec::new();
        for &f in fs {
            flags::set_event_flag(f, true);
            if flags::get_event_flag(f) {
                set += 1;
            } else {
                failed.push(f);
            }
        }
        log::info!(
            "RegionLock '{name}' graces: {} requested, {} set, {} failed{}",
            fs.len(),
            set,
            failed.len(),
            if failed.is_empty() {
                String::new()
            } else {
                format!(" = {failed:?}")
            }
        );
    } else if opened {
        // Only a genuine region-lock (its open flag matched) that is missing its grace bundle is a
        // real drift worth flagging. A normal non-lock item (opened == false) is silently ignored --
        // this function is called for EVERY received item to test lock-ness, so without this guard
        // every filler/gear pickup spammed a false "NO region_graces entry" warning (264 in one run).
        log::warn!(
            "RegionLock '{name}': NO region_graces entry (cfg.region_graces empty or key mismatch)"
        );
    }
    opened
}

// --- baked-table region-lock fallback (bedrock interop) ------------------------------------------
// Decision logic + the WHY of the arming rule live in er_logic::region_lock (host-tested); these
// three are the game-side glue. Flow: core.rs gates on `foreign_seed_without_region_keys`, calls
// `prepare_baked_fallback` at connect with the datapackage-resolved names of the seed's
// apIdsToItemIds entries, and `tick_baked_fallback` each tick with the cumulative received-name
// set. Nothing is enforced until a scoped "<Region> Lock" is actually RECEIVED (measured on the
// real foreign apworld: its item table carries lock NAMES even on no-lock seeds, so table
// presence alone must never arm -- the foreign_apworld_degrade contract).

/// True when slot_data speaks NEITHER region-lock key -- the apworld is region-lock-ignorant
/// (Bedrock-shaped), so the baked fallback MAY apply. Key PRESENCE is the test, not emptiness:
/// an apworld that emits `regionOpenFlags` without `areaLockFlags` made a choice (see the note
/// at `parse`) and we do not second-guess it with baked geometry.
pub fn foreign_seed_without_region_keys(sd: &Value) -> bool {
    sd.get("areaLockFlags").is_none() && sd.get("regionOpenFlags").is_none()
}

/// Derive + stash (do NOT arm) the baked fallback from the seed's item names. Unknown
/// "<X> Lock" granularities and geometry-only (flagless) regions are logged and dropped --
/// never a panic, never a guessed region.
pub fn prepare_baked_fallback<'a>(
    cfg: &mut RegionConfig,
    seed_item_names: impl IntoIterator<Item = &'a str>,
) {
    let d = er_logic::region_lock::derive_region_locks(seed_item_names);
    if !d.unknown.is_empty() {
        log::warn!(
            "baked region-lock fallback: unknown lock name(s) {:?} -- a foreign region granularity this client cannot map; ignored (rename to the baked '<Region> Lock' names to enforce them)",
            d.unknown
        );
    }
    if !d.ungateable.is_empty() {
        log::warn!(
            "baked region-lock fallback: {:?} name baked region(s) with no resolved open flag --              cannot gate; ignored",
            d.ungateable
        );
    }
    if d.is_empty() {
        return;
    }
    log::info!(
        "baked region-lock fallback PREPARED: {} region(s), {} kick range(s) -- COLD until a scoped '<Region> Lock' is received",
        d.open_flags.len(),
        d.ranges.len()
    );
    cfg.baked_fallback = Some(d);
}

/// Arm the prepared fallback once ANY scoped lock has been RECEIVED, merging the derived config
/// into the live fields every existing path already consumes (`open_on_received_name` opens on
/// the merged name->flag map, `tick_kick` watches the merged ranges, the reconcile ticks
/// self-heal lost writes). One-shot: the stash is consumed on arming. A scoped region whose lock
/// never arrives afterwards stays sealed -- that is a sealed region, not a special case.
pub fn tick_baked_fallback(cfg: &mut RegionConfig, received: &HashSet<String>) -> bool {
    let armed = cfg
        .baked_fallback
        .as_ref()
        .is_some_and(|d| er_logic::region_lock::fallback_armed(d, received));
    if !armed {
        return false;
    }
    let d = cfg.baked_fallback.take().expect("armed implies prepared");
    for (name, flag) in &d.open_flags {
        cfg.region_open_flags.entry(name.clone()).or_insert(*flag);
    }
    cfg.area_lock_flags.extend(d.ranges.iter().copied());
    log::info!(
        "baked region-lock fallback ARMED: {} region(s), {} kick range(s) (first scoped Lock          received)",
        d.open_flags.len(),
        d.ranges.len()
    );
    true
}

// --- slot_data parse helpers (shapes from the standalone net.rs) ---------------------------------

fn parse_triples(v: Option<&Value>) -> Vec<[i32; 3]> {
    v.and_then(|v| v.as_array())
        .map(|outer| {
            outer
                .iter()
                .filter_map(|row| row.as_array())
                .filter(|r| r.len() >= 3)
                .map(|r| {
                    [
                        r[0].as_i64().unwrap_or(0) as i32,
                        r[1].as_i64().unwrap_or(0) as i32,
                        r[2].as_i64().unwrap_or(0) as i32,
                    ]
                })
                .collect()
        })
        .unwrap_or_default()
}

fn str_to_u32(v: Option<&Value>) -> HashMap<String, u32> {
    let mut m = HashMap::new();
    if let Some(Value::Object(o)) = v {
        for (k, val) in o {
            if let Some(n) = val.as_u64() {
                m.insert(k.clone(), n as u32);
            }
        }
    }
    m
}

fn str_to_i32vec(v: Option<&Value>) -> HashMap<String, Vec<i32>> {
    // lockGrantItems values are GOODS-packed FullIDs (er_code | 0x40000000), all < i32::MAX.
    let mut m = HashMap::new();
    if let Some(Value::Object(o)) = v {
        for (k, val) in o {
            if let Some(arr) = val.as_array() {
                m.insert(
                    k.clone(),
                    arr.iter()
                        .filter_map(|x| x.as_i64().map(|n| n as i32))
                        .collect(),
                );
            }
        }
    }
    m
}

fn str_to_u32vec(v: Option<&Value>) -> HashMap<String, Vec<u32>> {
    let mut m = HashMap::new();
    if let Some(Value::Object(o)) = v {
        for (k, val) in o {
            if let Some(arr) = val.as_array() {
                m.insert(
                    k.clone(),
                    arr.iter()
                        .filter_map(|x| x.as_u64().map(|n| n as u32))
                        .collect(),
                );
            }
        }
    }
    m
}

// --- capital-version reconciler (SPEC-capital-reconciler.md; apworld features/capital.py) -------
// Leyndell is TWO mutually exclusive map versions on one save-persisted flag (9116: OFF = Royal
// m11_00, ON = Ashen m11_05 + Elden Throne m19), and vanilla only ever SETS it (Maliketh's
// death), so the Erdtree burn permanently strands the ~152 Royal checks. The DECISIONS are pure
// er-logic (`er_logic::capital`, host-tested by `capital_replay`); this is the game-side glue:
// parse the five `capital*` slot_data keys at connect, hold the per-tick latch in `tick_capital`,
// and write 9116 from the warp TARGET in `capital_warp_intercept` (called by
// `warp::warp_to_grace`, so kick / random-start / `!warp` all get the intercept). Everything is
// armed on the burn-done latch (118, monotonic): the first burn stays 100% the game's own
// sequence. Reconcile-don't-dispatch: write on readback mismatch only, re-apply per tick until
// it sticks, never advance past an unverified write. The shop release re-key rides
// `shop_flags::run_capital_release` (configured here so the five keys stay one parse).

/// Parsed capital-reconciler config (None = INERT: option off / old apworld / malformed keys).
static CAPITAL: Mutex<Option<er_logic::capital::CapitalConfig>> = Mutex::new(None);
/// One-time telemetry latch: log "capital reconciler armed" the first time the burn-done flag
/// reads set in a session (re-armed on each configure).
static CAPITAL_ARMED_LOGGED: AtomicBool = AtomicBool::new(false);

/// Called by core.rs once slot_data is parsed (beside `region::parse`). The five `capital*`
/// keys travel together; absent keys are the off-wire (`capital_reconciler: false`, or an
/// apworld that predates the feature) -- the client logs INERT and never touches 9116.
pub fn configure_capital(sd: &Value) {
    CAPITAL_ARMED_LOGGED.store(false, Ordering::Relaxed);
    let cfg = er_logic::capital::parse(sd);
    match &cfg {
        Some(c) => log::info!(
            "capital reconciler configured: burn flag {}, done latch {}, ashen {:?}, royal {:?}, {} release row(s)",
            c.burn_flag,
            c.burn_done_flag,
            c.sets.ashen,
            c.sets.royal,
            c.release_rows.len()
        ),
        None => log::info!(
            "capital reconciler INERT: capital* slot_data keys absent (option off or old apworld)"
        ),
    }
    crate::shop_flags::configure_capital_release(
        cfg.as_ref()
            .map(|c| c.release_rows.clone())
            .unwrap_or_default(),
    );
    *CAPITAL.lock().unwrap() = cfg;
}

/// The pending GAME-MENU fast-travel destination (bonfire ENTITY id, the space
/// `warp_to_grace` speaks) while a menu grace warp is resolving -- i.e. between the player
/// confirming a destination on the map and the load completing -- or None when no menu warp
/// is in flight.
///
/// // NEEDS CRATE API: the queued fast-travel destination the engine's own warp machinery was
/// // handed -- CSLuaEventManager's queued warp arg or the GameMan equivalent -- read between
/// // map-confirm and load, converted to the BONFIRE ENTITY id space (if the crate surfaces a
/// // BonfireWarpParam ROW id instead, read that row's `bonfireEntityId`; the two spaces
/// // differ: row 110500 vs entity 11051950). Return Some(entity_id) while a menu warp is
/// // resolving, None otherwise.
///
/// Until the seam is filled this returns None and the reconciler self-degrades gracefully:
/// the per-tick latch corrects a menu warp one tick AFTER the load instead of before it.
/// Client-initiated warps (kick, random start, `!warp`) already get the full before-load
/// intercept via `warp::warp_to_grace`.
///
/// SUPERSEDED-BY-HOOK (pending in-game confirm; see warp_hook.rs): the LuaWarp probe detour
/// now pushes EVERY warp target straight into `capital_warp_intercept` at the moment of warp,
/// menu-initiated included — if Alaric's log confirms menu fast-travel routes through LuaWarp,
/// this poll-style seam never needs filling and stays permanently `None` (kept as the
/// documented fallback surface should the hook be refused on a stale build).
pub fn capital_pending_warp_target() -> Option<u32> {
    None
}

/// Per-tick 9116 latch: standing in an Ashen bucket -> hold ON; in a Royal bucket -> hold OFF;
/// elsewhere -> leave the flag alone (the next warp's intercept restores the Royal default).
/// Defends against anything flipping 9116 mid-session -- the map version would swap under the
/// player on the next load. Self-configured (no RegionConfig borrow); call every update tick.
pub fn tick_capital() {
    let guard = CAPITAL.lock().unwrap();
    let Some(cfg) = guard.as_ref() else { return };
    if !crate::flags::in_world() {
        return; // menu/load: neither play_region_id nor the flag holder is trustworthy
    }
    let armed = flags::get_event_flag(cfg.burn_done_flag);
    if !armed {
        return; // pre-burn: 9116-OFF *is* vanilla, and mid-burn a write would fight $Event(900)
    }
    if !CAPITAL_ARMED_LOGGED.swap(true, Ordering::Relaxed) {
        log::info!(
            "capital reconciler armed: burn-done flag {} is set -- {} now reconciled to the player's capital",
            cfg.burn_done_flag,
            cfg.burn_flag
        );
    }
    // Menu fast-travel seam first (decide from the TARGET before the load when available;
    // today the seam returns None and the latch corrects one tick after the load instead).
    let desired = capital_pending_warp_target()
        .and_then(|t| er_logic::capital::capital_flag_state_for_warp_target(&cfg.sets, t))
        .or_else(|| {
            flags::play_region_id()
                .and_then(|pr| er_logic::capital::capital_flag_state(&cfg.sets, pr))
        });
    let current = flags::get_event_flag(cfg.burn_flag);
    if let Some(w) = er_logic::capital::reconcile_write(armed, desired, current) {
        let _ = flags::try_set_event_flag(cfg.burn_flag, w);
        let stuck = flags::get_event_flag(cfg.burn_flag) == w;
        log::info!(
            "capital reconcile: flag {} -> {} (play_region {:?}); readback {}",
            cfg.burn_flag,
            if w { "ON" } else { "OFF" },
            flags::play_region_id(),
            if stuck {
                "STUCK"
            } else {
                "PENDING -- re-applying next tick"
            }
        );
    }
}

/// Warp-target intercept: decide 9116 from the TARGET before the load resolves, so the player
/// always loads the correct capital version. Ashen/Throne grace -> ON; ANY other resolvable
/// target (Royal graces, Roundtable, every overworld grace) -> OFF -- every warp anywhere
/// except the 7 Ashen/Throne graces restores the Royal default. Called by
/// `warp::warp_to_grace` right after the warp request (the warp is asynchronous; the write
/// lands before the load screen resolves). No-op while INERT or pre-burn.
pub fn capital_warp_intercept(grace_entity_id: u32) {
    let guard = CAPITAL.lock().unwrap();
    let Some(cfg) = guard.as_ref() else { return };
    let armed = flags::get_event_flag(cfg.burn_done_flag);
    let desired = er_logic::capital::capital_flag_state_for_warp_target(&cfg.sets, grace_entity_id);
    let current = flags::get_event_flag(cfg.burn_flag);
    if let Some(w) = er_logic::capital::reconcile_write(armed, desired, current) {
        let _ = flags::try_set_event_flag(cfg.burn_flag, w);
        let stuck = flags::get_event_flag(cfg.burn_flag) == w;
        log::info!(
            "capital warp intercept: target {grace_entity_id} -> flag {} {}; readback {}",
            cfg.burn_flag,
            if w { "ON" } else { "OFF" },
            if stuck {
                "STUCK"
            } else {
                "PENDING -- the per-tick latch converges after the load"
            }
        );
    }
}

#[cfg(test)]
mod foreign_apworld_degrade {
    //! A FOREIGN APWORLD MUST YIELD A PLAYABLE VANILLA SEED, NOT AN ERROR.
    //!
    //! Bedrock's apworld (fswap/archipelago@er) emits none of the region-lock keys -- it has no
    //! region lock at all; `region lock ideas.md` is a wishlist asking someone else to build one.
    //! Alaric promised him, in writing: "when these arguments aren't present, they fall back to
    //! vanilla behaviour". THESE TESTS ARE THAT PROMISE. If one fails, we have silently made our
    //! client refuse to drive anyone else's world.
    //!
    //! Vanilla = every region open, no kick-watch, no random start, no warp.
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_slot_data_yields_vanilla_region_config() {
        let c = parse(&json!({}));
        assert!(
            c.region_open_flags.is_empty(),
            "no regionOpenFlags => no locks; every region open"
        );
        assert!(
            c.area_lock_flags.is_empty(),
            "no areaLockFlags => NO kick-watch. A foreign seed must never be kicked out of a region \
             it does not know is locked."
        );
        assert!(c.lock_reveal_flags.is_empty());
        assert_eq!(
            c.random_start_done_flag, 0,
            "0 = non-random seed, no start guard"
        );
        assert_eq!(c.random_start_warp_flag, 0, "0 = no random start");
        assert_eq!(c.random_start_area_id, 0);
        assert_eq!(c.random_start_grace_id, 0);
    }

    #[test]
    fn a_bedrock_shaped_slot_data_parses_and_stays_vanilla() {
        // Exactly the keys Bedrock's fill_slot_data emits (his words, 2026-07-06): item map, the
        // matt key table, and the goal. No regionOpenFlags, no startRegion, no areaLockFlags.
        let sd = json!({
            "apIdsToItemIds":   { "7770001": 1073750026u64 },
            "locationIdsToKeys":{ "7770001": "301200,0:0000520110::" },
            "goalLocations":    [7770875, 7770876],
        });
        let c = parse(&sd);
        assert!(c.region_open_flags.is_empty());
        assert!(c.area_lock_flags.is_empty());
        assert_eq!(c.random_start_warp_flag, 0);
    }

    #[test]
    fn a_partial_region_table_does_not_invent_kick_ranges() {
        // regionOpenFlags present but areaLockFlags absent: we must NOT derive kick ranges
        // client-side (see the note at parse()). No table => no enforcement, by design.
        let sd = json!({ "regionOpenFlags": { "Caelid Lock": 73202u64 } });
        let c = parse(&sd);
        assert_eq!(c.region_open_flags.len(), 1);
        assert!(
            c.area_lock_flags.is_empty(),
            "kick ranges are the generator's job; deriving them here would enforce a lock the \
             apworld never asked for"
        );
        // ...and the baked fallback is out of bounds too: the seed SPOKE a region key.
        assert!(
            !foreign_seed_without_region_keys(&sd),
            "a seed that emits either region key never gets baked geometry"
        );
    }

    // --- the baked-table fallback keeps the same promise -----------------------------------
    // (pure derivation + arming rule are host-tested in er_logic::region_lock::derive_tests;
    // these cover the glue: gating, cold-until-receipt, and the merge.)

    /// A real baked lock name + flag, read from the generated table (never hand-pinned).
    fn baked_example() -> (&'static str, u32) {
        let r = er_logic::region_locks::REGION_LOCKS
            .iter()
            .find(|r| r.open_flag.is_some())
            .expect("baked table has flagged regions");
        (r.lock_item, r.open_flag.unwrap())
    }

    #[test]
    fn fallback_gate_is_key_presence_not_content() {
        assert!(foreign_seed_without_region_keys(&json!({})));
        assert!(foreign_seed_without_region_keys(&json!({
            "apIdsToItemIds": { "7770001": 1073750026u64 },
            "locationIdsToKeys": { "7770001": "301200,0:0000520110::" },
        })));
        // Emitted-but-empty still counts as SPOKEN.
        assert!(!foreign_seed_without_region_keys(
            &json!({ "areaLockFlags": [] })
        ));
        assert!(!foreign_seed_without_region_keys(
            &json!({ "regionOpenFlags": {} })
        ));
    }

    #[test]
    fn prepared_fallback_stays_cold_until_a_scoped_lock_is_received() {
        // The degrade promise, fallback edition: a foreign seed whose item TABLE names locks
        // (the real one always does -- it ships its whole item table) must not kick anyone
        // until a lock is RECEIVED. Names here: one real baked name + one synthetic foreign
        // granularity (hand-written shape, not the foreign world's data).
        let (lock, flag) = baked_example();
        let mut c = parse(&json!({}));
        prepare_baked_fallback(&mut c, [lock, "Zzz Nonexistent Region Lock", "Uchigatana"]);
        assert!(c.baked_fallback.is_some(), "scoped");
        assert!(
            c.area_lock_flags.is_empty(),
            "COLD: no kick-watch before receipt"
        );
        assert!(c.region_open_flags.is_empty());

        let mut received: HashSet<String> = ["Uchigatana".to_string()].into();
        assert!(
            !tick_baked_fallback(&mut c, &received),
            "non-lock receipts never arm"
        );
        assert!(c.area_lock_flags.is_empty());

        received.insert(lock.to_string());
        assert!(
            tick_baked_fallback(&mut c, &received),
            "first scoped lock arms"
        );
        assert_eq!(c.region_open_flags.get(lock), Some(&flag));
        assert!(!c.area_lock_flags.is_empty(), "kick-watch live");
        assert!(c.area_lock_flags.iter().all(|r| r[2] as u32 == flag));
        assert!(
            !tick_baked_fallback(&mut c, &received),
            "one-shot: stash consumed"
        );
    }

    #[test]
    fn a_seed_with_no_lock_names_never_prepares_never_arms() {
        let mut c = parse(&json!({}));
        prepare_baked_fallback(&mut c, ["Uchigatana", "Golden Seed"]);
        assert!(c.baked_fallback.is_none(), "nothing scoped");
        let received: HashSet<String> = ["Caelid Lock".to_string()].into();
        assert!(
            !tick_baked_fallback(&mut c, &received),
            "no prepared scope -> nothing to arm, whatever arrives"
        );
        assert!(c.area_lock_flags.is_empty() && c.region_open_flags.is_empty());
    }
}
