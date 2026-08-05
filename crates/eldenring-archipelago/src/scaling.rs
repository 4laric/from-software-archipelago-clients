//! scaling.rs — runtime sphere/completion enemy scaling (SPEC-runtime-enemy-scaling.md).
//!
//! Replaces the retired baker's enemy scaling. On connect we read `regionSphereTargets` /
//! `completionScalingBasis` into a pure `er_logic::scaling::ScalingConfig`; each tick (throttled) we
//! sweep the loaded enemy `ChrIns` and, for each, clear its baked vanilla `70xx` scaling SpEffect and
//! apply the sphere tier's `70xx` (the vanilla ladder). All via typed crate calls — no raw offsets.
//!
//! Basis: MVP scales every loaded enemy to the PLAYER's current `play_region` tier (enemies loaded
//! around you are effectively in your region). Per-enemy region is one field away (`chr.play_region_id`)
//! if we want the Full basis later. Stateless: we re-check each enemy's active SpEffects, so it's
//! idempotent and re-scales correctly when the player changes region or an enemy reloads.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use eldenring::cs::{ChrIns, ChrInsExt, ChrLoadStatus, ChrSet, ChrType, WorldChrMan};
use er_logic::scaling::{
    NUM_TIERS, RegionToastLedger, ScaleAction, ScalingConfig, ScalingKind, band_native_tier,
    is_dlc_bucket, is_scaling_speffect, native_tier, raw_target_for_region, region_name_for_bucket,
    scale_action, scaling_kind, speffect_id_for_tier, tier_for_region, tier_rates,
};
use er_logic::scaling_settle::{SettlePolicy, SweepGate, sweep_blocked_by_death};
use fromsoftware_shared::{FromStatic, Subclass};
use serde_json::Value;

static CONFIG: Mutex<Option<ScalingConfig>> = Mutex::new(None);
static TICK: AtomicU32 = AtomicU32::new(0);

/// Apply the region's tier SpEffect only a few times a second (enemy stats don't need per-frame).
///
/// ⚠️ This is a REPEAT-sweep throttle only. It used to be checked BEFORE the settle guard, which
/// meant the first sweep after a transition also had to wait for the next multiple of 30 -- ~500 ms
/// at 60fps and a full second at 30, and post-load framerate is exactly when it is worst. It is now
/// measured as "ticks since the last sweep", so the first allowed sweep happens immediately.
const THROTTLE: u32 = 30;

/// Transition/region bookkeeping for the time-based backstop (`er_logic::scaling_settle`, host-tested;
/// see `SETTLE` for the policy). This is the PRIMARY crash guard again -- see sweepable_characters.
static GATE: Mutex<SweepGate> = Mutex::new(SweepGate::new());
/// Process-relative clock for the gate (which is pure and takes `now_ms`).
static EPOCH: Mutex<Option<Instant>> = Mutex::new(None);
/// Ticks at the last sweep -- the repeat throttle. `None` = never swept, so the first allowed tick
/// sweeps immediately instead of waiting for a modulo phase.
static LAST_SWEEP_TICK: Mutex<Option<u32>> = Mutex::new(None);
/// Per-sweep tally of the `chr_load_status` of every entry we walked, indexed by `status_slot`.
/// Diagnostic ONLY -- nothing filters on it. It exists because the last thing that DID filter on it
/// rejected every enemy in the game, and no log at the time could say what the real states were.
static STATUS_HIST: [AtomicU32; 6] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];
/// One-shot release log per transition.
static RELEASE_LOGGED: Mutex<bool> = Mutex::new(true);
/// Once-per-announcement ledger for the region-entry scaling toast (pure + host-tested in
/// `er_logic::scaling::RegionToastLedger`; the dedup policy and its tests live there). Reset in
/// `configure` so a new seed's tiers announce afresh.
static TOAST_LEDGER: Mutex<RegionToastLedger> = Mutex::new(RegionToastLedger::new());

fn now_ms() -> u64 {
    let mut epoch = match EPOCH.lock() {
        Ok(e) => e,
        Err(_) => return 0,
    };
    epoch.get_or_insert_with(Instant::now).elapsed().as_millis() as u64
}

/// Iterate a chr set's characters, recording the `chr_load_status` distribution as it goes.
///
/// 🛑 THIS FUNCTION FILTERED ON `chr_load_status == Active` FROM 2026-07-27 (98a2362) UNTIL THE SAME
/// DAY, AND IT BROKE ENEMY SCALING COMPLETELY. Alaric's repro log, a rolled Mohgwyn start:
///
///     enemy-scaling: settle release after 2512ms (...); swept 1, skipped 242 not-Active entries
///     enemy-scaling: settle release after 2508ms (...); swept 2, skipped 1219 not-Active entries
///     enemy-scaling: region 12050 -> speffect 7010 (tier 0/19, sphere target 0/10000, 1.14x HP)
///
/// The apworld was perfect -- Mohgwyn as the start region resolved to tier 0, exactly as designed.
/// But ~99.5% of entries were rejected, and the phantom census shows the one entity actually being
/// swept was `npc_id 8000` -- TORRENT. Not a single real enemy was ever scaled, so every one of them
/// kept its VANILLA rung: a player starting in Mohgwyn met endgame-strength enemies and was
/// oneshot. Reported by ShadowTL on Nexus for a Snowfield start, same mechanism.
///
/// WHAT WAS TRUE AND WHAT WAS NOT. True: `chr_load_status` exists on the ENTRY, so reading it
/// dereferences no `ChrIns`, and upstream's `characters()` genuinely ignores it. NOT true, and never
/// checked against a running game: that a loaded, fightable enemy is in state `Active`. It is not.
/// I inferred the meaning of an enum variant from its NAME and shipped it flagged
/// "UNVERIFIED IN-GAME", which is not the same as safe.
///
/// So this is back to upstream's behaviour -- every non-null entry -- which is what shipped for
/// months before 98a2362 and never had this problem. The 2500 ms `SETTLE` window is untouched and is
/// once again the primary guard, so the CTD exposure is exactly what it was, not worse.
///
/// The histogram is the part that stops this recurring: the next build's logs will say what states
/// enemies are ACTUALLY in, and only then can this be narrowed on evidence instead of on a name.
pub(crate) fn sweepable_characters<T>(set: &ChrSet<T>) -> impl Iterator<Item = &mut T>
where
    T: Subclass<ChrIns> + 'static,
{
    let mut current = set.entries;
    let end = unsafe { current.add(set.capacity as usize) };
    std::iter::from_fn(move || {
        while current != end {
            // Read the ENTRY (flat array slot) -- never the ChrIns behind it.
            let entry = unsafe { current.as_ref() };
            let (chr_ins, status) = (entry.chr_ins, entry.chr_load_status);
            current = unsafe { current.add(1) };
            let Some(mut chr_ins) = chr_ins else {
                continue;
            };
            // COUNT, do not filter. A tally with no filter is evidence; a filter with no tally was
            // the bug (CONTRIBUTING rule 4 cuts both ways).
            STATUS_HIST[status_slot(status)].fetch_add(1, Ordering::Relaxed);
            return Some(unsafe { chr_ins.as_mut() });
        }
        None
    })
}

/// Stable index for a `ChrLoadStatus` in `STATUS_HIST`. Exhaustive on purpose: if upstream adds a
/// variant this stops compiling rather than silently bucketing it as something else.
fn status_slot(s: ChrLoadStatus) -> usize {
    match s {
        ChrLoadStatus::Unloaded => 0,
        ChrLoadStatus::Initializing => 1,
        ChrLoadStatus::Active => 2,
        ChrLoadStatus::NetworkInitializing => 3,
        ChrLoadStatus::ReadyForActivation => 4,
        ChrLoadStatus::Unloading => 5,
    }
}
/// How long to let a freshly-loaded map settle before sweeping, and how long the region must have
/// been stable. Pure policy; the gate that consumes it is `er_logic::scaling_settle` (host-tested).
///
/// `settle_ms` is UNCHANGED at 2500, and after the 98a2362 revert it is once again the PRIMARY
/// crash guard rather than a backstop -- `sweepable_characters` no longer filters. Do not lower it
/// without live evidence: that is the mistake this file has already made once today.
///
/// `stable_ms` is the NEW half. The old guard restarted the full 2500 on every observed region
/// change, so the transient `play_region_id` values a warp produces compounded the real window to
/// ~8s in play. Worse, the region was only SAMPLED every 30 ticks, so a flap shorter than ~500ms was
/// never seen at all -- too slow and too blind, from the same line ordering. Observation is now
/// per-tick (strictly MORE protective: those flaps now hold the gate) and a flap costs `stable_ms`
/// rather than a fresh `settle_ms`. Starts conservative at the shipping default; see scaling_settle.
const SETTLE: SettlePolicy = SettlePolicy::SHIPPING;

/// Re-arm the settle window from an EXTERNAL transition signal.
///
/// CTD guard (2026-07-24, "Beside the Rampart Gaol" warp postmortem). Two callers:
///   * `warp_hook`'s LuaWarp detour — the moment ANY warp (menu or client) is requested. The
///     region-change guard in `tick` cannot cover the warp-OUT side: `play_region` keeps its old
///     value through the first teardown frames (main_player still placed, so `in_world()` still
///     reads true), so the sweep kept walking the ORIGIN region's ChrIns sets while the engine
///     streamed them out — the same native-crash class `REGION_SETTLE` exists for (Siofra,
///     2026-07-09), just on the other edge of the transition.
///   * core's in-world false->true edge — covers a SAME-REGION reload (death respawn), where
///     `LAST_REGION` never changes so the region-change reset never fires, yet the ChrIns sets
///     were just torn down and rebuilt around the sweep.
///
/// Worst case is the documented degrade: enemies stay vanilla-statted for `REGION_SETTLE`
/// longer. No panic path (a poisoned lock is skipped; `Instant::now` cannot fail): the detour
/// caller sits inside the game's own warp call frame and must never unwind across it.
pub fn notify_transition() {
    let now = now_ms();
    if let Ok(mut gate) = GATE.lock() {
        gate.on_transition(now);
    }
    // Re-arm the one-shot release log so the NEXT release describes this transition.
    if let Ok(mut logged) = RELEASE_LOGGED.lock() {
        *logged = false;
    }
}

/// Parse slot_data at connect. The parse itself — including the SWEEP H4 / R6 refuse-to-arm on an
/// empty/missing `regionSphereTargets` — lives in `er_logic::scaling::parse_scaling_config`
/// (host-tested); this wrapper only owns the logging and the CONFIG swap.
pub fn configure(sd: &Value) {
    let requested = er_logic::options::parse_bool_option(sd, "completion_scaling");
    let cfg = er_logic::scaling::parse_scaling_config(sd);
    match (&cfg, requested) {
        (Some(c), _) => log::info!(
            "enemy-scaling: enabled ({:?}), {} region targets, max {}, floor tier {}",
            c.basis,
            c.region_targets.len() + c.region_ranges.len(),
            c.max_target,
            c.floor_tier
        ),
        (None, true) => {
            // R6 (SWEEP H4): with an empty/missing map, arming would resolve every region to
            // floor_tier and the sweep would strip baked vanilla scaling from EVERY loaded enemy
            // (the whole game flattens). The parse returned None: feature INERT, enemies vanilla.
            log::error!(
                "completion_scaling requested but regionSphereTargets is empty -- enemy scaling left VANILLA"
            );
        }
        (None, false) => {}
    }
    *CONFIG.lock().unwrap() = cfg;
    // New config = new tiers: the region-entry announcements start over. A poisoned lock only
    // ever costs announcements, never the sweep.
    if let Ok(mut ledger) = TOAST_LEDGER.lock() {
        ledger.reset();
    }
}

/// What drove the tier for a region this sweep -- captured while `CONFIG` is locked so the emit can
/// explain the applied speffect (raw sphere target, normalization ceiling, resolved tier + its HP/atk
/// rates, and whether this is a DLC region -- a bucket with a blessing floor). Diagnostic only.
/// Per-sweep census of WHAT WE FOUND, not just what we changed (#346).
///
/// Three different failures look identical in play -- (a) the entity is never swept because it lives
/// in a `ChrSet` we don't walk, (b) it is swept but carried no vanilla ladder rung so the rung we
/// apply is a buff, (c) it is swept and the tier is simply wrong -- and until now no log line told
/// them apart. `scale_one` already computed the answer to (b) and threw it away.
///
/// 🛑 The classification is `ScalingKind`, NOT "was the cleared set empty". The clear range is the
/// whole of `7000..8000`, so a `7800`-block effect makes the cleared set non-empty while the enemy
/// still has no area-scaling tier. See `er_logic::scaling::ScalingKind`.
#[derive(Default)]
struct SweepTally {
    /// (Re)applied the tier -- the number the log line has always printed.
    scaled: u32,
    /// Swept, carried NO ladder rung: hand-tuned. The class #346 is about.
    unrunged: u32,
    /// Unrunged AND left completely untouched -- either we have no native tier for it, or the
    /// target is not stronger than the one we have.
    left_vanilla: u32,
    /// Unrunged, but we HAD a native tier and the target beat it, so it scaled up.
    scaled_by_native: u32,
    /// Carried something the clear catches that is not a rung (`7210..`, `7800..`).
    other_in_range: u32,
    /// Distinct `npc_param_id`s of unrunged entities, capped -- enough to NAME the offender in one
    /// log without turning a per-sweep line into a wall of ids. 🛑 `npc_param_id`, not `npc_id`:
    /// only the former joins to `NpcParam`, and the first version of this census logged the latter,
    /// which is why it named ids like `8000` that do not exist in the table at all.
    unrunged_ids: Vec<i32>,
    /// Carried a BAND row and NO ladder rung. The class the band-as-native-tier ruling is about.
    band_only: u32,
    /// Carried a BAND row AND a ladder rung -- these take the Replace path today, which strips the
    /// band. Where the band is the stronger of the two, that is a DOWN-scale nobody ordered.
    band_and_rung: u32,
    /// Samples of `(ladder rung id, band id)` for entities carrying BOTH -- which the census showed
    /// is essentially every scaled enemy (198 of 198, 153 of 153).
    ///
    /// 🛑 THIS IS A CALIBRATION MEASUREMENT, NOT A DEFECT HUNT. The band is area difficulty and
    /// stripping it is the feature working as designed; that is settled. What is NOT settled is
    /// whether our ladder's top end (7.422x) is strong enough, because vanilla's effective
    /// multiplier is the PRODUCT of the two (up to ~7.4 x ~3.4). This pair is how we find out.
    ///
    /// ⭐ Report BOTH ENDS of this distribution when the time comes. Quoting one end of a band is how
    /// `7400..7680` got misidentified twice.
    rung_band_pairs: Vec<(i32, i32)>,
    /// Samples of `(band-implied tier, getSoul-table tier)` for entities that have
    /// both. The band-as-native-tier ruling PREDICTS band >= table for most carriers. If this log
    /// shows the band routinely BELOW the table, the band is not a strength statement and the ruling
    /// does not survive -- so this pair is the thing to read first, not the counts.
    band_vs_table: Vec<(usize, usize)>,
    /// Entities that ALREADY carry our target rung and still carry some other scaling effect.
    ///
    /// Non-zero means the unidentified mechanism RE-APPLIES a band after we swept -- and because
    /// `scale_one` returns early once the target is present, we never clear it, so it stacks under
    /// our rung indefinitely. Counted before that early return, which is the only place it is
    /// visible. Behaviour is unchanged; this only measures.
    residue: u32,
    /// Distinct non-ladder ids found inside the clear range, capped. WITHOUT this the census says
    /// "199 of 240 enemies carried something we stripped" and cannot say WHAT -- and only 20 rows in
    /// all of `NpcParam` carry a non-ladder in-range effect innately, so nearly all of those 199 are
    /// applied at RUNTIME by something we have not identified. Naming them is how that stops being a
    /// mystery (candidates: the `7800..7902` spCategory-140 block, the `7400..7680` co-op ladder).
    other_ids: Vec<i32>,
}

/// Cap on `SweepTally::unrunged_ids`. A census, not a dump.
const UNRUNGED_ID_CAP: usize = 12;

impl SweepTally {
    fn note_unrunged(&mut self, npc_param_id: i32) {
        self.unrunged += 1;
        if self.unrunged_ids.len() < UNRUNGED_ID_CAP && !self.unrunged_ids.contains(&npc_param_id) {
            self.unrunged_ids.push(npc_param_id);
        }
    }

    fn note_pair(&mut self, band: usize, table: usize) {
        if self.band_vs_table.len() < UNRUNGED_ID_CAP {
            self.band_vs_table.push((band, table));
        }
    }

    fn note_rung_band(&mut self, rung: i32, band: i32) {
        if self.rung_band_pairs.len() < UNRUNGED_ID_CAP
            && !self.rung_band_pairs.contains(&(rung, band))
        {
            self.rung_band_pairs.push((rung, band));
        }
    }

    fn note_other(&mut self, param_id: i32) {
        if self.other_ids.len() < UNRUNGED_ID_CAP && !self.other_ids.contains(&param_id) {
            self.other_ids.push(param_id);
        }
    }
}

struct RegionScaleDbg {
    tier: usize,
    raw_target: Option<i32>,
    max_target: i32,
    dlc_region: bool,
    hp: f32,
    attack: f32,
}

/// Per-tick sweep (call from `update_live`, in-world). Throttled; no-op unless configured.
///
/// Returns the region-entry scaling announcement owed this tick, if any (the production caller of
/// `er_logic::scaling::region_scaling_toast`). Resolved AFTER the settle gate and the repeat
/// throttle, so it fires when the tier actually lands rather than during teardown flaps, and
/// deduped by `RegionToastLedger` -- once per distinct announcement per session. The caller
/// (core.rs) owns the toast deck; this function owns no I/O beyond the sweep itself.
pub fn tick() -> Option<String> {
    {
        let guard = CONFIG.lock().unwrap();
        if guard.is_none() {
            return None;
        }
    }
    // ORDERING IS THE FIX (2026-07-27). The throttle used to be checked HERE, before the region was
    // read -- so the region was sampled only every 30 ticks (missing any flap shorter than that),
    // the region-change branch RESTARTED the clock at the first throttled tick (discarding the
    // arrival-edge arm from core.rs), and the settle expiry was likewise only noticed on a throttled
    // tick. Three terms of pure latency on top of the 2500ms anyone actually chose. Now: observe
    // every tick, and throttle only REPEAT sweeps.
    let tick_no = TICK.fetch_add(1, Ordering::Relaxed);
    let Ok(wcm) = (unsafe { WorldChrMan::instance() }) else {
        return None;
    };
    let player = wcm.main_player.as_ref()?;
    // DEATH GUARD (2026-07-30, generalizing the guard no_fall_damage / no_equip_load have each
    // carried for the player's OWN SpEffect list -- NOT deathlink's, whose two `hp <= 0` tests are
    // "is the player dead" and "don't re-kill", different rules that must not be unified with this
    // one; see er_logic::death_guard): at the death-cam transition the
    // engine tears chr_ins + special_effect lists down, and mutating one mid-teardown is a native
    // CTD. A death in place raises no signal this sweep's gates can see -- no warp request, no
    // region change, and the in-world edge only fires AFTER teardown -- yet scale_one mutates the
    // same list type on every swept entity. hp is read exactly as DeathLink's read_local_hp reads
    // it; the predicate + timeline live in er_logic::scaling_settle (host-tested). Degrade:
    // enemies keep their current tier while the player is dead, invisible in play.
    if sweep_blocked_by_death(player.chr_ins.modules.data.hp) {
        return None;
    }
    // SCALING_WIRE: resolve in play_region/100 sub-id space -- the same bucket the
    // region-lock kick uses and the space regionSphereTargetRanges is emitted in.
    let region = (player.play_region_id / 100) as i32;
    let player_handle = player.field_ins_handle; // skip the player itself in the sweep
    let player_team = player.chr_ins.team_type; // hostiles (invader/NPC phantoms) carry a different team

    // Time-based backstop (er_logic::scaling_settle). Not the primary CTD defence any more -- that is
    // the PRIMARY crash guard once more: the chr_load_status filter that briefly replaced it
    // rejected every enemy in the game and was reverted the same day (see sweepable_characters).
    let now = now_ms();
    let (allowed, diag) = {
        let Ok(mut gate) = GATE.lock() else {
            return None;
        };
        gate.on_region(region, now, &SETTLE); // EVERY tick, before any throttle
        (gate.sweep_allowed(now, &SETTLE), gate.release_diag(now))
    };
    if !allowed {
        return None;
    }
    // Repeat-sweep throttle, measured from the LAST SWEEP rather than a modulo phase, so the first
    // allowed sweep after a transition runs on this tick instead of waiting up to 30 more frames.
    {
        let Ok(mut last) = LAST_SWEEP_TICK.lock() else {
            return None;
        };
        match *last {
            Some(t) if tick_no.wrapping_sub(t) < THROTTLE => return None,
            _ => *last = Some(tick_no),
        }
    }
    for h in &STATUS_HIST {
        h.store(0, Ordering::Relaxed);
    }

    // Resolve the tier once, and capture the inputs that drove it so the emit below can EXPLAIN the
    // number instead of just printing it. (Before: the log showed only `-> speffect NNNN`, which
    // couldn't distinguish "sphere resolved this tier" from "DLC cap clamped it" from "unmapped ->
    // floor" -- the exact ambiguity the fable consult flagged, 2026-07-15.)
    let (target, dbg, entry_toast) = {
        let guard = CONFIG.lock().unwrap();
        let cfg = guard.as_ref()?;
        let tier = tier_for_region(cfg, region);
        let rates = tier_rates(tier);
        let dbg = RegionScaleDbg {
            tier,
            raw_target: raw_target_for_region(cfg, region),
            max_target: cfg.max_target,
            dlc_region: is_dlc_bucket(cfg, region),
            hp: rates.hp,
            attack: rates.attack,
        };
        // Region-entry announcement: decided while the config is in hand, deduped per-session by
        // the ledger (message-keyed -- see er_logic::scaling::RegionToastLedger). Named buckets
        // only: the baked geometry (region_locks.rs) names exactly the buckets the wire can
        // target, so hub/tutorial buckets stay silent instead of guessing.
        let entry_toast = TOAST_LEDGER
            .lock()
            .ok()
            .and_then(|mut l| l.on_region(cfg, region, region_name_for_bucket(region)));
        (speffect_id_for_tier(tier), dbg, entry_toast)
    };
    let target_tier = dbg.tier;

    let mut tally = SweepTally::default();
    // Overworld enemies.
    for chr in sweepable_characters(&wcm.open_field_chr_set.base) {
        scale_one(chr, target, target_tier, &player_handle, &mut tally);
    }
    // Legacy-dungeon / block chr sets.
    for slot in wcm.chr_sets.iter().flatten() {
        for chr in sweepable_characters(slot) {
            scale_one(chr, target, target_tier, &player_handle, &mut tally);
        }
    }
    // HOSTILE-PHANTOM SWEEP -- revised 2026-07-19 (Alaric: "scaling works for mobs/bosses; NPC
    // invaders specifically don't scale"). Mobs + bosses are `ChrIns` in the open-field / block sets
    // swept above and DO scale, so an unscaled invader is a phantom entity, not a `ChrIns` in those
    // sets. The prior sweep touched only `player_chr_set` and skipped any entry whose `team_type`
    // matched the local player's -- but an NPC invader (BloodyFingerNpc / RecusantNpc) summoned into a
    // co-op session can share the host's `team_type`, so that `== player_team` skip silently excluded
    // exactly the entities it was meant to scale. Key off the unambiguous `chr_type` instead (see
    // `is_hostile_phantom`): scale the actual hostiles (player + NPC invaders, duelists), never the
    // local player, friendly white/blue phantoms, white-summon NPCs, or cosmetic ghosts. The 70xx
    // ladder is a plain stat multiplier, so it scales a `PlayerIns` phantom just like an enemy `ChrIns`.

    // Census (set, npc_id, chr_type, team) across ALL phantom-bearing sets, printed only when the
    // population CHANGES. If an invader ever appears in a set the scaling sweep below doesn't cover,
    // this names it outright (set + chr_type) so one co-op session settles where invaders live.
    let mut census: Vec<(&'static str, i32, i32, i32)> = Vec::new();
    for p in sweepable_characters(&wcm.player_chr_set) {
        let c = &p.chr_ins;
        census.push(("player", c.npc_id, c.chr_type as i32, c.team_type as i32));
    }
    for c in sweepable_characters(&wcm.ghost_chr_set) {
        census.push(("ghost", c.npc_id, c.chr_type as i32, c.team_type as i32));
    }
    for c in sweepable_characters(&wcm.summon_buddy_chr_set) {
        census.push(("summon", c.npc_id, c.chr_type as i32, c.team_type as i32));
    }
    {
        type CensusRow = (&'static str, i32, i32, i32);
        static LAST: Mutex<Option<Vec<CensusRow>>> = Mutex::new(None);
        let mut last = LAST.lock().unwrap();
        if last.as_ref() != Some(&census) {
            log::info!(
                "enemy-scaling: phantom census (set,npc_id,chr_type,team) player_team={player_team}: {:?}",
                census
            );
            *last = Some(census.clone());
        }
    }

    // Scale hostiles wherever a phantom can live (player_chr_set + summon_buddy_chr_set): keyed off
    // chr_type, so the set an invader lands in no longer matters and no friendly is ever touched.
    // (ghost_chr_set is cosmetic bloodstain/message/replay playback -- non-interactive, left alone;
    // the census still watches it in case that assumption is ever wrong.)
    for p in sweepable_characters(&wcm.player_chr_set) {
        scale_hostile_phantom(
            &mut p.chr_ins,
            target,
            target_tier,
            &player_handle,
            &mut tally,
        );
    }
    for c in sweepable_characters(&wcm.summon_buddy_chr_set) {
        scale_hostile_phantom(c, target, target_tier, &player_handle, &mut tally);
    }

    // ---- SETTLE RELEASE TELEMETRY (one line per transition) ------------------------------------
    // The old guard returned early on BOTH skip paths with no log, ever -- so a window that stacked
    // to ~8s in play was invisible to everything except a human noticing enemies felt wrong. This is
    // the "tolerance requires telemetry" line, and it is what turns `settle_ms` from a feel-based
    // constant into a measured one:
    //   +Xms       how long the player actually spent unscaled after the load
    //   flaps N    how many transient play_region values fired (each cost a full 2500ms before)
    //   inactive K entries skipped as chr_load_status != Active. K > 0 HERE, after the settle
    //              window expired, is direct evidence the timer alone never sufficed -- the exact
    //              claim the old design could not check.
    if let Ok(mut logged) = RELEASE_LOGGED.lock()
        && !*logged
    {
        *logged = true;
        let hist: Vec<u32> = STATUS_HIST
            .iter()
            .map(|h| h.load(Ordering::Relaxed))
            .collect();
        log::info!(
            "enemy-scaling: settle release after {}ms (region stable {}ms, flaps {}); swept \
             {}; chr_load_status seen unloaded={} init={} active={} netinit={} ready={} \
             unloading={}",
            diag.since_transition_ms,
            diag.since_region_change_ms,
            diag.flaps,
            tally.scaled,
            hist[0],
            hist[1],
            hist[2],
            hist[3],
            hist[4],
            hist[5],
        );
    }

    // Emit on CHANGE, not on `scaled > 0`.
    //
    // Two reasons, and the first one is a trap. (1) Unrunged entities that the bottom-rung skip
    // leaves alone never converge -- they are re-examined and re-counted every sweep and NEVER
    // scaled -- so a `scaled > 0` gate would hide the census in exactly the region the census was
    // written for. (2) Gating on the whole tuple instead is strictly QUIETER than what shipped:
    // once a region settles the numbers stop moving and the line stops repeating, which the old
    // condition did not guarantee. Same idiom as the phantom census above.
    {
        type ScaleLine = (i32, i32, u32, u32, u32, u32, u32, u32, u32, u32);
        static LAST: Mutex<Option<ScaleLine>> = Mutex::new(None);
        let line: ScaleLine = (
            region,
            target,
            tally.scaled,
            tally.unrunged,
            tally.left_vanilla,
            tally.other_in_range,
            tally.scaled_by_native,
            tally.band_only,
            tally.band_and_rung,
            tally.residue,
        );
        let changed = match LAST.lock() {
            Ok(mut last) => {
                let changed = *last != Some(line);
                if changed {
                    *last = Some(line);
                }
                changed
            }
            // A poisoned lock costs telemetry, never the sweep.
            Err(_) => false,
        };
        if changed && (tally.scaled > 0 || tally.unrunged > 0) {
            let RegionScaleDbg {
                tier,
                raw_target,
                max_target,
                dlc_region,
                hp,
                attack,
            } = dbg;
            let tgt = raw_target.map_or_else(|| "unmapped".to_string(), |t| t.to_string());
            log::info!(
                "enemy-scaling: region {region} -> speffect {target} \
                 (tier {tier}/{}, sphere target {tgt}/{max_target}, {hp:.2}x HP / {attack:.2}x \
                 atk{}); (re)scaled {} enemy(ies); unrunged {} (up-scaled by native tier {}, left \
                 vanilla {}, npc_param_ids {:?}), other-in-range {} {:?}; band-only {}, \
                 band+rung {} {:?}, band_vs_table {:?}, residue {}",
                NUM_TIERS - 1,
                if dlc_region { ", DLC region" } else { "" },
                tally.scaled,
                tally.unrunged,
                tally.scaled_by_native,
                tally.left_vanilla,
                tally.unrunged_ids,
                tally.other_in_range,
                tally.other_ids,
                tally.band_only,
                tally.band_and_rung,
                tally.rung_band_pairs,
                tally.band_vs_table,
                tally.residue,
            );
        }
    }

    entry_toast
}

/// True for chr_types that are ACTUAL hostiles worth scaling -- player invaders (BloodyFinger /
/// Recusant / FesteringBloodyFinger), NPC invaders (BloodyFingerNpc / RecusantNpc), and arena/world
/// duelists (Duelist / GrayPhantom). False for everything friendly or cosmetic: the local player,
/// white/blue phantoms, white-summon NPCs, and every ghost variant. Keyed off chr_type because the
/// old `team_type != player_team` test wrongly skipped NPC invaders that share the host's team in co-op.
fn is_hostile_phantom(t: ChrType) -> bool {
    matches!(
        t,
        ChrType::Duelist
            | ChrType::GrayPhantom
            | ChrType::BloodyFinger
            | ChrType::Recusant
            | ChrType::FesteringBloodyFinger
            | ChrType::BloodyFingerNpc
            | ChrType::RecusantNpc
    )
}

/// Scale one phantom-set entry ONLY if it is an actual hostile (see `is_hostile_phantom`); otherwise a
/// no-op. Logs once per hostile that gets scaled (scale_one no-ops once the entry already carries the
/// tier), so a co-op session's log names exactly what landed.
fn scale_hostile_phantom(
    chr: &mut ChrIns,
    target: i32,
    target_tier: usize,
    player_handle: &eldenring::cs::FieldInsHandle,
    tally: &mut SweepTally,
) {
    if !is_hostile_phantom(chr.chr_type) {
        return;
    }
    let (ty, team, npc_id) = (chr.chr_type, chr.team_type, chr.npc_id);
    let before = tally.scaled;
    scale_one(chr, target, target_tier, player_handle, tally);
    if tally.scaled > before {
        log::info!(
            "enemy-scaling: scaled hostile phantom (chr_type={ty:?} team={team} npc_id={npc_id})"
        );
    }
}

/// Ensure one enemy carries exactly `target` as its scaling SpEffect: skip if it already has it, else
/// clear any baked/stale scaling SpEffect and apply `target` -- and record what we found on the way
/// through (`SweepTally`).
///
/// 🛑 THE CLASSIFY-THEN-SKIP ORDER IS LOAD-BEARING. The bottom-rung skip returns BEFORE the remove
/// loop. Stripping an enemy's vanilla state and then declining to replace it would be worse than
/// either doing the whole thing or none of it -- and, because the sweep re-derives everything from
/// what the enemy currently carries, it would also be irreversible: the evidence of what the enemy
/// natively was is gone, so the next pass sees an unrunged enemy that genuinely has no rung.
fn scale_one(
    chr: &mut ChrIns,
    target: i32,
    target_tier: usize,
    player_handle: &eldenring::cs::FieldInsHandle,
    tally: &mut SweepTally,
) {
    if &chr.field_ins_handle == player_handle {
        return; // never scale the player
    }
    // Collect first (entries() borrows immutably) then remove (borrows mutably). Collected BEFORE
    // the already-on-target check so that path can be measured -- see `SweepTally::residue`.
    let carried: Vec<i32> = chr
        .special_effect
        .entries()
        .map(|e| e.param_id)
        .filter(|&id| is_scaling_speffect(id))
        .collect();
    if carried.contains(&target) {
        // Already on the right tier. Behaviour unchanged (we still return), but count anything ELSE
        // in the clear range that is riding along: that is an effect applied AFTER our sweep, and
        // this early return is the reason it never gets cleared.
        if carried.iter().any(|&id| id != target) {
            tally.residue += 1;
            for &id in &carried {
                if id != target {
                    tally.note_other(id);
                }
            }
        }
        return;
    }
    let stale = carried;

    // CLASSIFY. `!stale.is_empty()` is NOT "this enemy had a vanilla tier" -- the clear range is the
    // whole 7000..8000 block and only some of it is the ladder (er_logic::scaling::ScalingKind).
    let mut carried_ladder_rung = false;
    let mut carried_other = false;
    let mut band_tier: Option<usize> = None;
    let mut rung_id: Option<i32> = None;
    let mut band_id: Option<i32> = None;
    for &id in &stale {
        match scaling_kind(id) {
            Some(ScalingKind::Ladder) => {
                carried_ladder_rung = true;
                rung_id = Some(id);
            }
            Some(ScalingKind::OtherInRange) => {
                carried_other = true;
                tally.note_other(id);
                // Keep the STRONGEST band row if an entity somehow carries several.
                if let Some(t) = band_native_tier(id) {
                    if band_tier.is_none_or(|cur: usize| t > cur) {
                        band_id = Some(id);
                    }
                    band_tier = Some(band_tier.map_or(t, |cur: usize| cur.max(t)));
                }
            }
            None => {}
        }
    }
    if carried_other {
        tally.other_in_range += 1;
    }
    if let Some(band) = band_tier {
        if carried_ladder_rung {
            tally.band_and_rung += 1;
            if let (Some(r), Some(b)) = (rung_id, band_id) {
                tally.note_rung_band(r, b);
            }
        } else {
            tally.band_only += 1;
        }
        if let Some(table) = native_tier(chr.npc_param_id) {
            tally.note_pair(band, table);
        }
    }
    if !carried_ladder_rung {
        tally.note_unrunged(chr.npc_param_id);
    }

    // ...THEN decide, before anything is mutated.
    //
    // 🛑 `NoTouch` is the DEFAULT for anything we cannot place, and it is the whole point. What
    // shipped resolved an unplaceable enemy to the region's rung, which on a hand-tuned endgame NPC
    // in a shallow sphere is a multiplier stacked on stats that already assume you meet it at the
    // end of the game. Leaving it vanilla is under-scaled in deep spheres -- a blemish -- and that
    // trade is deliberate.
    match scale_action(carried_ladder_rung, chr.npc_param_id, target_tier) {
        ScaleAction::NoTouch => {
            tally.left_vanilla += 1;
            return;
        }
        ScaleAction::Apply => tally.scaled_by_native += 1,
        ScaleAction::Replace => {}
    }

    for id in stale {
        chr.remove_speffect(id);
    }
    chr.apply_speffect(target, false);
    tally.scaled += 1;
}
