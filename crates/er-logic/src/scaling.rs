//! Pure sphere/completion enemy-scaling decisions (see `SPEC-runtime-enemy-scaling.md`).
//!
//! Maps a region's sphere target to a scaling TIER, and a tier to the vanilla `SpEffectParam` row the
//! client applies to an enemy (`ChrIns::apply_speffect`). Host-tested, no game.
//!
//! The ladder is the game's OWN progressive enemy-scaling SpEffects (`7010..7200`, from the offline
//! `SpEffectParam.csv` dump): all visually silent (`vfxId/stateInfo/iconId = -1/0/-1`) and leave rune
//! reward unchanged (`haveSoulRate = 1`). We use the lower-to-mid subset `7010..7100` (1.14x..3.70x HP)
//! — the full ladder tops out at ~7.4x HP (NG+-cycle steepness), too much for sphere depth; extend
//! toward `7200` here for a harsher curve. (`7400..7680` is a DESCENDING co-op/rune set — not usable.)

use std::collections::HashMap;

/// Which basis the apworld chose (`completionScalingBasis`). The mapping is basis-agnostic (it consumes
/// a per-region target); the client keeps the basis for logging / option gating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalingBasis {
    Geographic,
    Sphere,
}

/// One scaling tier → the vanilla `SpEffectParam` id applied to an enemy at that tier, plus its rate
/// multipliers (for logging / reference). `1.0` == vanilla.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScalingTier {
    pub speffect_id: i32,
    pub hp: f32,
    pub attack: f32,
    pub defense: f32,
}

/// The tier ladder — vanilla `7010..7100`, ascending. Index 0 = shallowest sphere, last = deepest.
pub const SCALING_TIERS: &[ScalingTier] = &[
    ScalingTier { speffect_id: 7010, hp: 1.141, attack: 1.097, defense: 1.013 },
    ScalingTier { speffect_id: 7020, hp: 1.281, attack: 1.202, defense: 1.026 },
    ScalingTier { speffect_id: 7030, hp: 1.656, attack: 1.495, defense: 1.039 },
    ScalingTier { speffect_id: 7040, hp: 1.813, attack: 1.495, defense: 1.053 },
    ScalingTier { speffect_id: 7050, hp: 1.953, attack: 1.690, defense: 1.066 },
    ScalingTier { speffect_id: 7060, hp: 2.266, attack: 1.758, defense: 1.079 },
    ScalingTier { speffect_id: 7070, hp: 2.406, attack: 1.831, defense: 1.093 },
    ScalingTier { speffect_id: 7080, hp: 2.688, attack: 2.000, defense: 1.106 },
    ScalingTier { speffect_id: 7090, hp: 3.250, attack: 2.279, defense: 1.119 },
    ScalingTier { speffect_id: 7100, hp: 3.703, attack: 2.473, defense: 1.133 },
];

/// Number of tiers in the ladder.
pub const NUM_TIERS: usize = SCALING_TIERS.len();

/// Vanilla enemy-scaling SpEffects live in this id range. Used to CLEAR an enemy's baked scaling
/// (remove any active `param_id` in this range) before applying our sphere tier — vanilla `70xx` are
/// `spCategory = 0` so they'd otherwise stack (double-scale).
pub const SCALING_ID_RANGE: std::ops::Range<i32> = 7000..8000;

/// Whether `param_id` is a (vanilla or ours) enemy-scaling SpEffect — the ones to clear before applying.
pub fn is_scaling_speffect(param_id: i32) -> bool {
    SCALING_ID_RANGE.contains(&param_id)
}

/// Connect-time config, parsed from slot_data by the client (`regionSphereTargets` etc.).
#[derive(Debug, Clone)]
pub struct ScalingConfig {
    pub basis: ScalingBasis,
    /// Minimum tier — from `completion_scaling_floor`; nothing scales below this.
    pub floor_tier: usize,
    /// `regionSphereTargets`: region id → raw target (sphere depth / power).
    pub region_targets: HashMap<i32, i32>,
    /// Deepest target present (normalization denominator). `0` disables scaling (→ floor everywhere).
    pub max_target: i32,
}

/// Map a raw target to a tier index in `[floor_tier, NUM_TIERS)`. `max_target <= 0` → the floor tier.
/// Monotonic in `target`.
pub fn tier_for_target(target: i32, max_target: i32, floor_tier: usize) -> usize {
    let floor = floor_tier.min(NUM_TIERS - 1);
    if max_target <= 0 {
        return floor;
    }
    let frac = (target.max(0) as f32 / max_target as f32).clamp(0.0, 1.0);
    let tier = (frac * (NUM_TIERS - 1) as f32).round() as usize;
    tier.clamp(floor, NUM_TIERS - 1)
}

/// Region → tier. A region absent from `region_targets` falls back to the floor tier (unknown = don't
/// scale up).
pub fn tier_for_region(cfg: &ScalingConfig, region: i32) -> usize {
    match cfg.region_targets.get(&region) {
        Some(&target) => tier_for_target(target, cfg.max_target, cfg.floor_tier),
        None => cfg.floor_tier.min(NUM_TIERS - 1),
    }
}

/// The `SpEffectParam` id to apply for a tier (clamped to the ladder).
pub fn speffect_id_for_tier(tier: usize) -> i32 {
    SCALING_TIERS[tier.min(NUM_TIERS - 1)].speffect_id
}

/// The full tier row (id + rates) for a tier (clamped).
pub fn tier_rates(tier: usize) -> ScalingTier {
    SCALING_TIERS[tier.min(NUM_TIERS - 1)]
}

/// Lowest tier whose HP rate is ≥ `floor_mult` — converts a `completion_scaling_floor` multiplier
/// into a floor tier index. Below the ladder → tier 0; above it → the top tier.
pub fn floor_tier_from_multiplier(floor_mult: f32) -> usize {
    SCALING_TIERS
        .iter()
        .position(|t| t.hp >= floor_mult)
        .unwrap_or(NUM_TIERS - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(pairs: &[(i32, i32)], floor: usize) -> ScalingConfig {
        let region_targets: HashMap<i32, i32> = pairs.iter().copied().collect();
        let max_target = region_targets.values().copied().max().unwrap_or(0);
        ScalingConfig { basis: ScalingBasis::Sphere, floor_tier: floor, region_targets, max_target }
    }

    // --- ladder integrity (the vanilla 7010..7100 rows) ---

    #[test]
    fn ladder_is_monotonic_nondecreasing() {
        for w in SCALING_TIERS.windows(2) {
            assert!(w[1].hp >= w[0].hp, "hp not monotonic");
            assert!(w[1].attack >= w[0].attack, "attack not monotonic");
            assert!(w[1].defense >= w[0].defense, "defense not monotonic");
            assert!(w[1].speffect_id > w[0].speffect_id, "ids not ascending");
        }
    }

    #[test]
    fn ids_are_the_vanilla_ladder() {
        assert_eq!(SCALING_TIERS[0].speffect_id, 7010);
        assert_eq!(SCALING_TIERS[NUM_TIERS - 1].speffect_id, 7100);
    }

    #[test]
    fn scaling_range_membership() {
        assert!(is_scaling_speffect(7010));
        assert!(is_scaling_speffect(7500)); // vanilla baked scaling we must clear
        assert!(is_scaling_speffect(7999));
        assert!(!is_scaling_speffect(6999));
        assert!(!is_scaling_speffect(8000));
    }

    // --- tier_for_target ---

    #[test]
    fn target_zero_is_floor_tier() {
        assert_eq!(tier_for_target(0, 100, 0), 0);
        assert_eq!(tier_for_target(0, 100, 2), 2); // floor clamps up
    }

    #[test]
    fn target_max_is_top_tier() {
        assert_eq!(tier_for_target(100, 100, 0), NUM_TIERS - 1);
    }

    #[test]
    fn target_midpoint_rounds_to_middle_tier() {
        // frac 0.5 * (10-1) = 4.5 -> round 5
        assert_eq!(tier_for_target(50, 100, 0), 5);
    }

    #[test]
    fn tier_is_monotonic_in_target() {
        let mut last = 0;
        for t in (0..=100).step_by(5) {
            let tier = tier_for_target(t, 100, 0);
            assert!(tier >= last, "tier decreased at target {t}");
            last = tier;
        }
    }

    #[test]
    fn floor_clamps_low_targets_but_not_high() {
        assert_eq!(tier_for_target(0, 100, 3), 3);
        assert_eq!(tier_for_target(100, 100, 3), NUM_TIERS - 1);
    }

    #[test]
    fn no_scaling_info_returns_floor() {
        assert_eq!(tier_for_target(999, 0, 0), 0);
        assert_eq!(tier_for_target(999, -5, 1), 1);
    }

    #[test]
    fn out_of_range_target_clamps() {
        assert_eq!(tier_for_target(1000, 100, 0), NUM_TIERS - 1);
        assert_eq!(tier_for_target(-50, 100, 0), 0);
    }

    // --- tier_for_region ---

    #[test]
    fn known_region_maps_to_its_tier() {
        let c = cfg(&[(60000, 0), (63000, 50), (76000, 100)], 0);
        assert_eq!(tier_for_region(&c, 60000), 0);
        assert_eq!(tier_for_region(&c, 63000), 5);
        assert_eq!(tier_for_region(&c, 76000), NUM_TIERS - 1);
    }

    #[test]
    fn unknown_region_falls_back_to_floor() {
        let c = cfg(&[(60000, 100)], 2);
        assert_eq!(tier_for_region(&c, 99999), 2);
    }

    // --- ids + floor conversion ---

    #[test]
    fn speffect_id_lookup_and_clamp() {
        assert_eq!(speffect_id_for_tier(0), 7010);
        assert_eq!(speffect_id_for_tier(NUM_TIERS - 1), 7100);
        assert_eq!(speffect_id_for_tier(999), 7100); // clamp
    }

    #[test]
    fn floor_tier_from_multiplier_picks_lowest_qualifying() {
        assert_eq!(floor_tier_from_multiplier(0.0), 0); // 7010 hp 1.141 >= 0
        assert_eq!(floor_tier_from_multiplier(1.5), 2); // first hp >= 1.5 is 7030 (1.656)
        assert_eq!(floor_tier_from_multiplier(2.0), 5); // first hp >= 2.0 is 7060 (2.266)
        assert_eq!(floor_tier_from_multiplier(99.0), NUM_TIERS - 1); // above ladder -> top
    }
}
