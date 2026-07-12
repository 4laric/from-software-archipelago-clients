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
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use eldenring::cs::{ChrIns, ChrInsExt, WorldChrMan};
use er_logic::scaling::{
    ScalingConfig, is_scaling_speffect, speffect_id_for_tier, tier_for_region,
};
use fromsoftware_shared::FromStatic;
use serde_json::Value;

static CONFIG: Mutex<Option<ScalingConfig>> = Mutex::new(None);
static TICK: AtomicU32 = AtomicU32::new(0);

/// Apply the region's tier SpEffect only a few times a second (enemy stats don't need per-frame).
const THROTTLE: u32 = 30;

/// Native-crash guard (Siofra / Eternal Cities CTD, 2026-07-09): the last play_region the sweep ran
/// in, and when we first observed the CURRENT one. We do NOT walk the enemy `ChrIns` lists in the
/// first `REGION_SETTLE` after a region change -- the new map's enemies are still streaming in, and
/// iterating / mutating a chr set mid-spawn can dereference a half-constructed `ChrIns` and crash the
/// game natively (no Rust panic; it's the game's own memory). Enemies are merely left un-scaled for a
/// couple of seconds after a load; the very next sweep after the window scales them.
static LAST_REGION: AtomicI32 = AtomicI32::new(i32::MIN);
static REGION_ENTERED: Mutex<Option<Instant>> = Mutex::new(None);
/// How long to let a freshly-entered region populate before sweeping its enemies.
const REGION_SETTLE: Duration = Duration::from_secs(4);

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

/// Per-tick sweep (call from `update_live`, in-world). Throttled; no-op unless configured.
pub fn tick() {
    {
        let guard = CONFIG.lock().unwrap();
        if guard.is_none() {
            return;
        }
    }
    if !TICK
        .fetch_add(1, Ordering::Relaxed)
        .is_multiple_of(THROTTLE)
    {
        return;
    }
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

    // Native-crash guard: on a region change, note the entry time and SKIP this sweep; keep skipping
    // until the region has settled (`REGION_SETTLE`). This keeps the ChrIns walk out of the mid-load
    // window where enemies are still being constructed. (See LAST_REGION / REGION_ENTERED above.)
    let prev_region = LAST_REGION.swap(region, Ordering::Relaxed);
    if prev_region != region {
        *REGION_ENTERED.lock().unwrap() = Some(Instant::now());
        return;
    }
    match *REGION_ENTERED.lock().unwrap() {
        Some(entered) if entered.elapsed() < REGION_SETTLE => return,
        _ => {}
    }

    let target = {
        let guard = CONFIG.lock().unwrap();
        let Some(cfg) = guard.as_ref() else {
            return;
        };
        speffect_id_for_tier(tier_for_region(cfg, region))
    };

    let mut scaled = 0u32;
    // Overworld enemies.
    for chr in wcm.open_field_chr_set.base.characters() {
        scaled += scale_one(chr, target, &player_handle);
    }
    // Legacy-dungeon / block chr sets.
    for slot in wcm.chr_sets.iter().flatten() {
        for chr in slot.characters() {
            scaled += scale_one(chr, target, &player_handle);
        }
    }
    // Hostile NPC invaders / phantoms (e.g. Fia's Champions, the Deathbed Companions) are `PlayerIns`
    // entities living in `player_chr_set`, NOT in the enemy sets above -- so the enemy sweep never
    // reached them and they stayed at vanilla scaling (the reported bug). Scale the HOSTILE ones only:
    // an entry whose `team_type` differs from the local player's is not on our side (an invader / a
    // hostile NPC phantom); the local player and any friendly coop allies / summoned NPCs share the
    // player's team and are skipped. (Spirit ashes / Torrent live in `summon_buddy_chr_set` and are
    // never touched here.) The 70xx ladder is a plain stat multiplier, so it scales a `PlayerIns`
    // phantom exactly as it does an enemy `ChrIns`.
    // OBSERVABILITY FIRST. The hostile-phantom sweep below has never once been seen to fire (Alaric,
    // 2026-07-12: "we still don't have scaling on npc invaders" -- and his DLL DOES contain this code:
    // b4ad4e7 is an ancestor of the build he ran). But the only log line here fires when something is
    // SCALED, so "no invader was present" and "an invader was present and this sweep cannot see it"
    // produce byte-identical logs. That is why the bug has survived: it is unfalsifiable from a log.
    //
    // So census the set. This prints only when the population CHANGES (not per tick), and it answers
    // the question outright: if an invader is on screen and player_chr_set is empty, they live in some
    // other WorldChrMan set and this sweep is looking in an empty room. If they are here but skipped,
    // the team_type test is wrong. One session with an invader now decides it.
    let mut census: Vec<(i32, i32)> = Vec::new();
    for p in wcm.player_chr_set.characters() {
        census.push((p.chr_ins.npc_id, p.chr_ins.team_type as i32));
    }
    {
        use std::sync::Mutex;
        static LAST: Mutex<Option<Vec<(i32, i32)>>> = Mutex::new(None);
        let mut last = LAST.lock().unwrap();
        if last.as_ref() != Some(&census) {
            log::info!(
                "enemy-scaling: player_chr_set census = {} entr(ies) (player_team={player_team}): {:?} \
                 -- an NPC invader on screen with an EMPTY census means invaders are NOT in this set",
                census.len(),
                census
            );
            *last = Some(census);
        }
    }

    for p in wcm.player_chr_set.characters() {
        let team = p.chr_ins.team_type;
        if team == player_team {
            continue; // local player or a same-team ally
        }
        let ty = p.chr_ins.chr_type;
        let applied = scale_one(&mut p.chr_ins, target, &player_handle);
        if applied > 0 {
            scaled += applied;
            // Diagnostic: fires once per phantom (scale_one no-ops once it carries the tier), so a
            // test session's log names exactly what got scaled -- confirm Fia's Champions land here.
            log::info!(
                "enemy-scaling: scaled hostile player_chr_set entry (chr_type={ty:?} team={team} npc_id={})",
                p.chr_ins.npc_id
            );
        }
    }
    if scaled > 0 {
        log::info!(
            "enemy-scaling: region {region} -> speffect {target}; (re)scaled {scaled} enemy(ies)"
        );
    }
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
