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
    NUM_TIERS, ScalingConfig, is_dlc_bucket, is_scaling_speffect, raw_target_for_region,
    speffect_id_for_tier, tier_for_region, tier_rates,
};
use er_logic::scaling_settle::{SettlePolicy, SweepGate};
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
/// see `active_characters` below for the PRIMARY crash defence and `SETTLE` for the policy).
static GATE: Mutex<SweepGate> = Mutex::new(SweepGate::new());
/// Process-relative clock for the gate (which is pure and takes `now_ms`).
static EPOCH: Mutex<Option<Instant>> = Mutex::new(None);
/// Ticks at the last sweep -- the repeat throttle. `None` = never swept, so the first allowed tick
/// sweeps immediately instead of waiting for a modulo phase.
static LAST_SWEEP_TICK: Mutex<Option<u32>> = Mutex::new(None);
/// Entries skipped this sweep because `chr_load_status != Active` (see `active_characters`).
/// Reported in the release log: a non-zero count AFTER the settle window is the proof that the old
/// timer alone was never sufficient.
static SKIPPED_NOT_ACTIVE: AtomicU32 = AtomicU32::new(0);
/// One-shot release log per transition.
static RELEASE_LOGGED: Mutex<bool> = Mutex::new(true);

fn now_ms() -> u64 {
    let mut epoch = match EPOCH.lock() {
        Ok(e) => e,
        Err(_) => return 0,
    };
    epoch.get_or_insert_with(Instant::now).elapsed().as_millis() as u64
}

/// Iterate a chr set's LOADED, FULLY-CONSTRUCTED characters.
///
/// 🛑 THIS IS THE CTD FIX. Upstream `ChrSet::characters()` (fromsoftware-rs
/// `cs/world_chr_man.rs`) walks the entries array and yields every slot where `chr_ins.is_some()` --
/// it never looks at `chr_load_status`. So during a map load it hands out `ChrIns` pointers in state
/// `Initializing` (still being constructed) and `Unloading` (being torn down), and
/// `scale_one`'s `apply_speffect` dereferences them. That is the Siofra / Eternal Cities native
/// crash of 2026-07-09, and the 2.5 s `REGION_SETTLE` window was built to avoid it by WAITING --
/// a timer standing in for a state byte the game had been publishing all along.
///
/// `chr_load_status` lives on the ENTRY, in the flat `capacity`-sized array -- reading it
/// dereferences no `ChrIns`, so it is safe to consult mid-stream, which is exactly what a timer
/// could never be. Filtering on `Active` also covers the warp-OUT teardown edge that
/// `notify_transition` was bolted onto in 2026-07-24.
///
/// The time-based gate is deliberately RETAINED as a backstop for the one hazard this cannot
/// describe: a torn read of the entries ARRAY itself while the game rebuilds it. Two independent
/// guards, because the crash class is still open elsewhere in this client.
fn active_characters<T>(set: &ChrSet<T>) -> impl Iterator<Item = &mut T>
where
    T: Subclass<ChrIns> + 'static,
{
    let mut current = set.entries;
    let end = unsafe { current.add(set.capacity as usize) };
    std::iter::from_fn(move || {
        while current != end {
            // Read the ENTRY (flat array slot) -- never the ChrIns behind it -- before deciding.
            let entry = unsafe { current.as_ref() };
            let (chr_ins, status) = (entry.chr_ins, entry.chr_load_status);
            current = unsafe { current.add(1) };
            let Some(mut chr_ins) = chr_ins else {
                continue;
            };
            if status != ChrLoadStatus::Active {
                // COUNT the skip; a filter with no tally is a lie (CONTRIBUTING rule 4).
                SKIPPED_NOT_ACTIVE.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            return Some(unsafe { chr_ins.as_mut() });
        }
        None
    })
}
/// How long to let a freshly-loaded map settle before sweeping, and how long the region must have
/// been stable. Pure policy; the gate that consumes it is `er_logic::scaling_settle` (host-tested).
///
/// `settle_ms` is UNCHANGED at 2500 (4s -> 2500 was Alaric's 2026-07-19 tightening). It is not
/// lowered here on purpose: it moves the native-crash boundary, `active_characters` above has just
/// replaced its *mechanism* and is not yet confirmed in-game, and the crash class is still open (the
/// "Beside the Rampart Gaol" warp). Lower it from the release-log data, not from a third guess.
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
}

/// What drove the tier for a region this sweep -- captured while `CONFIG` is locked so the emit can
/// explain the applied speffect (raw sphere target, normalization ceiling, resolved tier + its HP/atk
/// rates, and whether this is a DLC region -- a bucket with a blessing floor). Diagnostic only.
struct RegionScaleDbg {
    tier: usize,
    raw_target: Option<i32>,
    max_target: i32,
    dlc_region: bool,
    hp: f32,
    attack: f32,
}

/// Per-tick sweep (call from `update_live`, in-world). Throttled; no-op unless configured.
pub fn tick() {
    {
        let guard = CONFIG.lock().unwrap();
        if guard.is_none() {
            return;
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
        return;
    };
    let Some(player) = wcm.main_player.as_ref() else {
        return;
    };
    // SCALING_WIRE: resolve in play_region/100 sub-id space -- the same bucket the
    // region-lock kick uses and the space regionSphereTargetRanges is emitted in.
    let region = (player.play_region_id / 100) as i32;
    let player_handle = player.field_ins_handle; // skip the player itself in the sweep
    let player_team = player.chr_ins.team_type; // hostiles (invader/NPC phantoms) carry a different team

    // Time-based backstop (er_logic::scaling_settle). Not the primary CTD defence any more -- that is
    // `active_characters`'s chr_load_status filter -- but retained for the hazard a per-entry status
    // byte cannot describe: a torn read of the entries ARRAY while the game rebuilds it.
    let now = now_ms();
    let (allowed, diag) = {
        let Ok(mut gate) = GATE.lock() else {
            return;
        };
        gate.on_region(region, now, &SETTLE); // EVERY tick, before any throttle
        (gate.sweep_allowed(now, &SETTLE), gate.release_diag(now))
    };
    if !allowed {
        return;
    }
    // Repeat-sweep throttle, measured from the LAST SWEEP rather than a modulo phase, so the first
    // allowed sweep after a transition runs on this tick instead of waiting up to 30 more frames.
    {
        let Ok(mut last) = LAST_SWEEP_TICK.lock() else {
            return;
        };
        match *last {
            Some(t) if tick_no.wrapping_sub(t) < THROTTLE => return,
            _ => *last = Some(tick_no),
        }
    }
    SKIPPED_NOT_ACTIVE.store(0, Ordering::Relaxed);

    // Resolve the tier once, and capture the inputs that drove it so the emit below can EXPLAIN the
    // number instead of just printing it. (Before: the log showed only `-> speffect NNNN`, which
    // couldn't distinguish "sphere resolved this tier" from "DLC cap clamped it" from "unmapped ->
    // floor" -- the exact ambiguity the fable consult flagged, 2026-07-15.)
    let (target, dbg) = {
        let guard = CONFIG.lock().unwrap();
        let Some(cfg) = guard.as_ref() else {
            return;
        };
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
        (speffect_id_for_tier(tier), dbg)
    };

    let mut scaled = 0u32;
    // Overworld enemies.
    for chr in active_characters(&wcm.open_field_chr_set.base) {
        scaled += scale_one(chr, target, &player_handle);
    }
    // Legacy-dungeon / block chr sets.
    for slot in wcm.chr_sets.iter().flatten() {
        for chr in active_characters(slot) {
            scaled += scale_one(chr, target, &player_handle);
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
    for p in active_characters(&wcm.player_chr_set) {
        let c = &p.chr_ins;
        census.push(("player", c.npc_id, c.chr_type as i32, c.team_type as i32));
    }
    for c in active_characters(&wcm.ghost_chr_set) {
        census.push(("ghost", c.npc_id, c.chr_type as i32, c.team_type as i32));
    }
    for c in active_characters(&wcm.summon_buddy_chr_set) {
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
    for p in active_characters(&wcm.player_chr_set) {
        scaled += scale_hostile_phantom(&mut p.chr_ins, target, &player_handle);
    }
    for c in active_characters(&wcm.summon_buddy_chr_set) {
        scaled += scale_hostile_phantom(c, target, &player_handle);
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
        let inactive = SKIPPED_NOT_ACTIVE.load(Ordering::Relaxed);
        log::info!(
            "enemy-scaling: settle release after {}ms (region stable {}ms, flaps {}); \
             swept {scaled}, skipped {inactive} not-Active entr{}",
            diag.since_transition_ms,
            diag.since_region_change_ms,
            diag.flaps,
            if inactive == 1 { "y" } else { "ies" },
        );
    }

    if scaled > 0 {
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
             (tier {tier}/{}, sphere target {tgt}/{max_target}, {hp:.2}x HP / {attack:.2}x atk{}); \
             (re)scaled {scaled} enemy(ies)",
            NUM_TIERS - 1,
            if dlc_region { ", DLC region" } else { "" },
        );
    }
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
/// no-op. Returns 1 if it (re)applied the tier. Logs once per hostile that gets scaled (scale_one
/// no-ops once the entry already carries the tier), so a co-op session's log names exactly what landed.
fn scale_hostile_phantom(
    chr: &mut ChrIns,
    target: i32,
    player_handle: &eldenring::cs::FieldInsHandle,
) -> u32 {
    if !is_hostile_phantom(chr.chr_type) {
        return 0;
    }
    let (ty, team, npc_id) = (chr.chr_type, chr.team_type, chr.npc_id);
    let applied = scale_one(chr, target, player_handle);
    if applied > 0 {
        log::info!(
            "enemy-scaling: scaled hostile phantom (chr_type={ty:?} team={team} npc_id={npc_id})"
        );
    }
    applied
}

/// Ensure one enemy carries exactly `target` as its scaling SpEffect: skip if it already has it, else
/// clear any baked/stale scaling SpEffect (`70xx`) and apply `target`. Returns 1 if it (re)applied.
fn scale_one(chr: &mut ChrIns, target: i32, player_handle: &eldenring::cs::FieldInsHandle) -> u32 {
    if &chr.field_ins_handle == player_handle {
        return 0; // never scale the player
    }
    if chr.special_effect.entries().any(|e| e.param_id == target) {
        return 0; // already on the right tier
    }
    // Collect first (entries() borrows immutably) then remove (borrows mutably).
    let stale: Vec<i32> = chr
        .special_effect
        .entries()
        .map(|e| e.param_id)
        .filter(|&id| is_scaling_speffect(id))
        .collect();
    for id in stale {
        chr.remove_speffect(id);
    }
    chr.apply_speffect(target, false);
    1
}
