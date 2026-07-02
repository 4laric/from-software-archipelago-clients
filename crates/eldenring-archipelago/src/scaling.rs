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
    ScalingBasis, ScalingConfig, floor_tier_from_multiplier, is_scaling_speffect,
    speffect_id_for_tier, tier_for_region,
};
use fromsoftware_shared::FromStatic;
use serde_json::Value;

static CONFIG: Mutex<Option<ScalingConfig>> = Mutex::new(None);
static TICK: AtomicU32 = AtomicU32::new(0);

/// Apply the region's tier SpEffect only a few times a second (enemy stats don't need per-frame).
const THROTTLE: u32 = 30;

/// Parse slot_data at connect. No-op (and disables) unless `options.completion_scaling` is on.
pub fn configure(sd: &Value) {
    // int-or-bool tolerant, unified onto er_logic::options (apworld ships options as ints).
    let enabled = er_logic::options::parse_bool_option(sd, "completion_scaling");
    if !enabled {
        *CONFIG.lock().unwrap() = None;
        return;
    }
    let region_targets = i32_i32_map(sd.get("regionSphereTargets"));
    if region_targets.is_empty() {
        // R6 (SWEEP H4): with an empty/missing map, every region resolves to floor_tier and the
        // sweep strips baked vanilla scaling from EVERY loaded enemy (the whole game flattens).
        // Refuse to arm instead: the feature goes INERT and enemies keep their baked scaling.
        log::error!(
            "completion_scaling requested but regionSphereTargets is empty -- enemy scaling left VANILLA"
        );
        *CONFIG.lock().unwrap() = None;
        return;
    }
    let max_target = region_targets.values().copied().max().unwrap_or(0);
    let basis = match sd.get("completionScalingBasis").and_then(|v| v.as_str()) {
        Some("sphere") => ScalingBasis::Sphere,
        _ => ScalingBasis::Geographic,
    };
    let floor_mult = sd
        .pointer("/options/completion_scaling_floor")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as f32;
    let floor_tier = floor_tier_from_multiplier(floor_mult);
    let n = region_targets.len();
    *CONFIG.lock().unwrap() = Some(ScalingConfig { basis, floor_tier, region_targets, max_target });
    log::info!(
        "enemy-scaling: enabled ({basis:?}), {n} region targets, max {max_target}, floor tier {floor_tier}"
    );
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

/// `{ "<i32>": <i32> }` slot_data object -> `i32 -> i32` map (regionSphereTargets). Tolerant.
fn i32_i32_map(v: Option<&Value>) -> std::collections::HashMap<i32, i32> {
    let mut m = std::collections::HashMap::new();
    if let Some(obj) = v.and_then(|v| v.as_object()) {
        for (k, val) in obj {
            if let (Ok(key), Some(value)) = (k.parse::<i32>(), val.as_i64()) {
                m.insert(key, value as i32);
            }
        }
    }
    m
}
