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

use eldenring::cs::{ChrIns, ChrInsExt, ChrType, WorldChrMan};
use er_logic::scaling::{
    NUM_TIERS, ScalingConfig, blessing_floor_for_region, is_scaling_speffect,
    raw_target_for_region, speffect_id_for_tier, tier_for_region, tier_rates,
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
///
/// TIGHTENED 2026-07-19 (Alaric): 4s -> 2500ms. The effective un-scaled window after a warp is this
/// PLUS the time `play_region_id` takes to stabilize to the new region (each transient region value
/// resets the timer), which stacked to ~8s in play -- long enough to get killed by vanilla-statted
/// enemies before scaling arms. Shaving 1.5s here narrows that danger window while keeping a margin
/// over the native-crash guard this delay exists for (iterating a still-spawning ChrIns set natively
/// crashed Siofra / the Eternal Cities, 2026-07-09). If that CTD returns, raise this back toward 4s.
const REGION_SETTLE: Duration = Duration::from_millis(2500);

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
            dlc_region: blessing_floor_for_region(&cfg.dlc_blessing_floors, region).is_some(),
            hp: rates.hp,
            attack: rates.attack,
        };
        (speffect_id_for_tier(tier), dbg)
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
    for p in wcm.player_chr_set.characters() {
        let c = &p.chr_ins;
        census.push(("player", c.npc_id, c.chr_type as i32, c.team_type as i32));
    }
    for c in wcm.ghost_chr_set.characters() {
        census.push(("ghost", c.npc_id, c.chr_type as i32, c.team_type as i32));
    }
    for c in wcm.summon_buddy_chr_set.characters() {
        census.push(("summon", c.npc_id, c.chr_type as i32, c.team_type as i32));
    }
    {
        // (set, npc_id, chr_type, team) per phantom/summon -- aliased to keep the static's type simple.
        type PhantomCensus = Vec<(&'static str, i32, i32, i32)>;
        static LAST: Mutex<Option<PhantomCensus>> = Mutex::new(None);
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
    for p in wcm.player_chr_set.characters() {
        scaled += scale_hostile_phantom(&mut p.chr_ins, target, &player_handle);
    }
    for c in wcm.summon_buddy_chr_set.characters() {
        scaled += scale_hostile_phantom(c, target, &player_handle);
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
