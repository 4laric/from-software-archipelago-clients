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
            c.region_targets.len(),
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
    if TICK.fetch_add(1, Ordering::Relaxed) % THROTTLE != 0 {
        return;
    }
    let Ok(wcm) = (unsafe { WorldChrMan::instance() }) else {
        return;
    };
    let Some(player) = wcm.main_player.as_ref() else {
        return;
    };
    let region = player.play_region_id as i32;
    let player_handle = player.field_ins_handle; // skip the player itself in the sweep

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
