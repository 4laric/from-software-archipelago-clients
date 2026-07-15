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

use serde_json::Value;

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
    ScalingTier {
        speffect_id: 7010,
        hp: 1.141,
        attack: 1.097,
        defense: 1.013,
    },
    ScalingTier {
        speffect_id: 7020,
        hp: 1.281,
        attack: 1.202,
        defense: 1.026,
    },
    ScalingTier {
        speffect_id: 7030,
        hp: 1.656,
        attack: 1.495,
        defense: 1.039,
    },
    ScalingTier {
        speffect_id: 7040,
        hp: 1.813,
        attack: 1.495,
        defense: 1.053,
    },
    ScalingTier {
        speffect_id: 7050,
        hp: 1.953,
        attack: 1.690,
        defense: 1.066,
    },
    ScalingTier {
        speffect_id: 7060,
        hp: 2.266,
        attack: 1.758,
        defense: 1.079,
    },
    ScalingTier {
        speffect_id: 7070,
        hp: 2.406,
        attack: 1.831,
        defense: 1.093,
    },
    ScalingTier {
        speffect_id: 7080,
        hp: 2.688,
        attack: 2.000,
        defense: 1.106,
    },
    ScalingTier {
        speffect_id: 7090,
        hp: 3.250,
        attack: 2.279,
        defense: 1.119,
    },
    ScalingTier {
        speffect_id: 7100,
        hp: 3.703,
        attack: 2.473,
        defense: 1.133,
    },
];

/// Number of tiers in the ladder.
pub const NUM_TIERS: usize = SCALING_TIERS.len();

// REMOVED 2026-07-15 (fable consult): `DLC_ENEMY_TIER_CAP`. It capped DLC-bucket enemy tiers at
// index 3 (7040) on the theory that the deep-sphere tier double-counted the blessing floor. The REAL
// double-count was the clear-range bug (see `DLC_SCALING_ID_RANGE`): DLC enemies kept their un-cleared
// 7-14× vanilla scaling, and the cap only ever limited the *added* tier, never that baked multiplier
// -- which is why capping "didn't help". With the clear fixed, DLC enemies are normalized to sphere
// depth exactly like base game, so a cap would only make DLC anomalously EASIER than base at deep
// spheres, breaking the "difficulty follows logic, not geography" invariant. DLC now scales by sphere
// with no special-case.

/// Vanilla enemy-scaling SpEffects live in this id range. Used to CLEAR an enemy's baked scaling
/// (remove any active `param_id` in this range) before applying our sphere tier — vanilla `70xx` are
/// `spCategory = 0` so they'd otherwise stack (double-scale).
pub const SCALING_ID_RANGE: std::ops::Range<i32> = 7000..8000;

/// The SAME scaling ladder, re-emitted in the DLC's `+20,000,000` param block. DLC enemies carry these
/// as their innate, always-on region scaling — and they are FAR steeper than the base ladder
/// (param-verified: `20007010` = 7.84x HP / 3.76x atk, `20007060` = 11.5x, `20007110` = 14.1x; the
/// whole ladder `20007000..20007310` is `spCategory 0`, `effectEndurance -1`, `haveSoulRate 1`).
///
/// THE BUG (2026-07-15, fable consult): the clear used to be `SCALING_ID_RANGE` only, so it stripped
/// base-game enemies' `70xx` (normalizing them to ~1.14x) but NEVER touched DLC enemies' `20007xxx`
/// (outside 7000..8000). DLC enemies therefore kept full vanilla SotE scaling (7-14x HP) AND had the
/// mod's sphere tier stacked on top, while every base-game enemy around them was normalized down --
/// the entire "DLC scaling is still crazy even with everything we're doing" report. Clearing this
/// range too puts DLC enemies on the same sphere curve as base game. Verified sufficient + no
/// collateral: the only `20007xxx` rows DLC enemies carry are scaling rows; their non-scaling innate
/// speffects live in the `5xxx`/`90xxx` blocks.
pub const DLC_SCALING_ID_RANGE: std::ops::Range<i32> = 20007000..20008000;

/// Whether `param_id` is a (base-game OR DLC, vanilla or ours) enemy-scaling SpEffect — the ones to
/// clear off an enemy before applying our sphere tier, so vanilla region scaling never stacks with it.
pub fn is_scaling_speffect(param_id: i32) -> bool {
    SCALING_ID_RANGE.contains(&param_id) || DLC_SCALING_ID_RANGE.contains(&param_id)
}

/// Connect-time config, parsed from slot_data by the client (`regionSphereTargets` etc.).
#[derive(Debug, Clone)]
pub struct ScalingConfig {
    pub basis: ScalingBasis,
    /// Minimum tier — from `completion_scaling_floor`; nothing scales below this.
    pub floor_tier: usize,
    /// `regionSphereTargets`: region id → raw target (sphere depth / power).
    pub region_targets: HashMap<i32, i32>,
    /// `regionSphereTargetRanges` (SCALING_WIRE): `(lo, hi, target)` in play_region/100 sub-id
    /// space -- the apworld's client-parseable form (the flat map only ever carried region
    /// NAMES). Consulted when the exact map misses.
    pub region_ranges: Vec<(i32, i32, i32)>,
    /// Deepest target present (normalization denominator). `0` disables scaling (→ floor everywhere).
    pub max_target: i32,
    /// `dlcScadutreeFloorRanges` (mode 2): `(lo, hi, floor)` in play_region/100 sub-id space -> the
    /// Scadutree-blessing FLOOR level for that DLC bucket. Consumed by upgrades.rs (the blessing
    /// floor); also used purely as a "is this a DLC region" flag for the enemy-scaling diagnostic log.
    /// (No longer gates enemy tiers -- the DLC tier cap was removed.) Empty = no DLC / mode != 2.
    pub dlc_blessing_floors: Vec<(i32, i32, i32)>,
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
/// scale up). DLC buckets are NOT special-cased: with the DLC baked-scaling clear fixed
/// (`DLC_SCALING_ID_RANGE`), DLC enemies scale by sphere depth exactly like base game.
pub fn tier_for_region(cfg: &ScalingConfig, region: i32) -> usize {
    if let Some(&target) = cfg.region_targets.get(&region) {
        tier_for_target(target, cfg.max_target, cfg.floor_tier)
    } else if let Some(&(_, _, target)) =
        // SCALING_WIRE: range fallback -- `region` is the play_region/100 sub id; the apworld
        // emits [lo, hi, target] buckets in the same space (a few dozen; linear scan is fine).
        cfg
            .region_ranges
            .iter()
            .find(|&&(lo, hi, _)| (lo..=hi).contains(&region))
    {
        tier_for_target(target, cfg.max_target, cfg.floor_tier)
    } else {
        cfg.floor_tier.min(NUM_TIERS - 1)
    }
}

/// The Scadutree-blessing FLOOR level for a play_region/100 `region` bucket, or `None` if the bucket
/// isn't in the DLC floor wire (unknown = no floor). Pure. Used by upgrades.rs (write
/// max(fragment level, floor)) and by the client's enemy-scaling log as a DLC-region flag.
pub fn blessing_floor_for_region(ranges: &[(i32, i32, i32)], region: i32) -> Option<i32> {
    ranges
        .iter()
        .find(|&&(lo, hi, _)| (lo..=hi).contains(&region))
        .map(|&(_, _, floor)| floor)
}

/// The raw sphere target the config resolves for `region` (exact map first, then range scan), or
/// `None` when the region is unmapped (client then falls back to the floor tier). Diagnostic-only --
/// mirrors the lookup order in `tier_for_region` so a log line can show the target that drove the
/// tier alongside the applied speffect. (observability: see scaling.rs emit, 2026-07-15.)
pub fn raw_target_for_region(cfg: &ScalingConfig, region: i32) -> Option<i32> {
    if let Some(&t) = cfg.region_targets.get(&region) {
        return Some(t);
    }
    cfg.region_ranges
        .iter()
        .find(|&&(lo, hi, _)| (lo..=hi).contains(&region))
        .map(|&(_, _, t)| t)
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

/// `{ "<i32>": <i32> }` slot_data object -> `i32 -> i32` map (`regionSphereTargets`). Tolerant:
/// non-numeric keys / non-int values are skipped, anything else yields an empty map.
pub fn i32_i32_map(v: Option<&Value>) -> HashMap<i32, i32> {
    let mut m = HashMap::new();
    if let Some(obj) = v.and_then(|v| v.as_object()) {
        for (k, val) in obj {
            if let (Ok(key), Some(value)) = (k.parse::<i32>(), val.as_i64()) {
                m.insert(key, value as i32);
            }
        }
    }
    m
}

/// Parse the connect-time scaling config out of slot_data. `None` = the feature stays INERT.
///
/// SWEEP H4 / R6: with `completion_scaling` on but an empty/missing `regionSphereTargets`, arming
/// would resolve EVERY region to `floor_tier` and the sweep would strip baked vanilla scaling from
/// every loaded enemy — the whole game flattens. So an empty target map REFUSES to arm (returns
/// `None`); the caller logs the "left VANILLA" error line and enemies keep their baked scaling.
pub fn parse_scaling_config(sd: &Value) -> Option<ScalingConfig> {
    if !crate::options::parse_bool_option(sd, "completion_scaling") {
        return None;
    }
    let region_targets = i32_i32_map(sd.get("regionSphereTargets"));
    // SCALING_WIRE (er-completion-scaling P1 wire fix): the apworld's client-parseable form is
    // range-keyed [[lo, hi, target], ...] in play_region/100 sub-id space; the flat map only
    // ever carried region NAMES (unparseable -> empty), so ranges are the live path now.
    let region_ranges = parse_triple_ranges(sd.get("regionSphereTargetRanges"));
    // DLC Scadutree-blessing floors (mode 2). Independent of completion_scaling, but folded into the
    // same config so the enemy-tier CAP has the DLC-bucket set. upgrades.rs reads these floors too.
    let dlc_blessing_floors = parse_triple_ranges(sd.get("dlcScadutreeFloorRanges"));
    if region_targets.is_empty() && region_ranges.is_empty() {
        return None; // H4: refuse to arm — see doc above.
    }
    let max_target = region_targets
        .values()
        .copied()
        .chain(region_ranges.iter().map(|&(_, _, t)| t))
        .max()
        .unwrap_or(0);
    // Tolerant like the rest of the options parses: the apworld ships the Choice VALUE (int 1 =
    // sphere, fill_slot_data "completionScalingBasis": ...basis.value); older/hand-rolled slot_data
    // may ship the string. The old string-only match silently read int 1 as Geographic (drift
    // caught by the slot_data fixture pipeline, 2026-07-02).
    let basis = match sd.get("completionScalingBasis") {
        Some(v) if v.as_str() == Some("sphere") || v.as_i64() == Some(1) => ScalingBasis::Sphere,
        _ => ScalingBasis::Geographic,
    };
    let floor_mult = sd
        .pointer("/options/completion_scaling_floor")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as f32;
    let floor_tier = floor_tier_from_multiplier(floor_mult);
    Some(ScalingConfig {
        basis,
        floor_tier,
        region_targets,
        region_ranges,
        max_target,
        dlc_blessing_floors,
    })
}

/// Parse a `[[lo, hi, v], ...]` slot_data triple-list into `(lo, hi, v)` i32 tuples. Tolerant: a row
/// that isn't a length-3 int array is skipped. Shared by `regionSphereTargetRanges` and
/// `dlcScadutreeFloorRanges` (both live in play_region/100 sub-id space).
pub fn parse_triple_ranges(v: Option<&Value>) -> Vec<(i32, i32, i32)> {
    let mut out: Vec<(i32, i32, i32)> = Vec::new();
    if let Some(arr) = v.and_then(|v| v.as_array()) {
        for row in arr {
            let Some(r) = row.as_array() else { continue };
            if r.len() != 3 {
                continue;
            }
            if let (Some(lo), Some(hi), Some(t)) = (r[0].as_i64(), r[1].as_i64(), r[2].as_i64()) {
                out.push((lo as i32, hi as i32, t as i32));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(pairs: &[(i32, i32)], floor: usize) -> ScalingConfig {
        let region_targets: HashMap<i32, i32> = pairs.iter().copied().collect();
        let max_target = region_targets.values().copied().max().unwrap_or(0);
        ScalingConfig {
            basis: ScalingBasis::Sphere,
            floor_tier: floor,
            region_targets,
            region_ranges: vec![],
            max_target,
            dlc_blessing_floors: vec![],
        }
    }

    #[test]
    fn blessing_floor_lookup_matches_bucket_ranges() {
        let floors = vec![(6800, 6800, 1), (20010, 20010, 15), (21000, 21010, 10)];
        assert_eq!(blessing_floor_for_region(&floors, 6800), Some(1));
        assert_eq!(blessing_floor_for_region(&floors, 20010), Some(15));
        assert_eq!(blessing_floor_for_region(&floors, 21005), Some(10)); // inside an lo..=hi span
        assert_eq!(blessing_floor_for_region(&floors, 61000), None); // base-game bucket -> no floor
        assert_eq!(blessing_floor_for_region(&[], 6800), None);
    }

    #[test]
    fn raw_target_lookup_prefers_exact_then_range() {
        let mut c = cfg(&[(60000, 42)], 0);
        c.region_ranges = vec![(62000, 62999, 7777)];
        c.max_target = 7777;
        assert_eq!(raw_target_for_region(&c, 60000), Some(42)); // exact map wins
        assert_eq!(raw_target_for_region(&c, 62500), Some(7777)); // range fallback
        assert_eq!(raw_target_for_region(&c, 99999), None); // unmapped -> client floors
    }

    #[test]
    fn dlc_buckets_scale_by_sphere_like_base_no_cap() {
        // Post-fix: DLC buckets are NOT capped. A deep DLC bucket resolves to the top tier, same as a
        // base-game bucket at the same depth -- the presence of a blessing-floor wire no longer clamps
        // the enemy tier (the DLC baked-scaling clear, DLC_SCALING_ID_RANGE, is what balances DLC now).
        let mut c = cfg(&[], 0);
        c.max_target = 100;
        c.region_ranges = vec![(6850, 6850, 100), (64000, 64000, 100)]; // Jagged Peak + a base bucket
        c.dlc_blessing_floors = vec![(6850, 6850, 12)]; // 6850 is a DLC bucket (has a blessing floor)
        assert_eq!(tier_for_region(&c, 6850), NUM_TIERS - 1); // DLC: full tier, uncapped
        assert_eq!(tier_for_region(&c, 64000), NUM_TIERS - 1); // base: identical treatment
                                                               // floor_tier still applies to DLC buckets like any other.
        c.floor_tier = 2;
        c.region_ranges = vec![(6850, 6850, 0)]; // shallow DLC bucket
        c.max_target = 100;
        assert_eq!(tier_for_region(&c, 6850), 2);
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

    // --- SCALING_WIRE: range-keyed targets (play_region/100 buckets) ---

    #[test]
    fn range_fallback_resolves_sub_id_buckets() {
        let mut c = cfg(&[], 0);
        c.region_ranges = vec![(10000, 10000, 2000), (62000, 62999, 10000)];
        c.max_target = 10000;
        // Stormveil sub 10000 -> low tier; Liurnia sub 62400 -> top tier; unmapped -> floor.
        assert!(tier_for_region(&c, 10000) < tier_for_region(&c, 62400));
        assert_eq!(tier_for_region(&c, 99999), 0);
        assert_eq!(tier_for_region(&c, 62400), NUM_TIERS - 1);
    }

    #[test]
    fn parse_arms_from_ranges_alone() {
        // The name-keyed flat map is unparseable by design (yields empty); ranges alone arm.
        let sd = serde_json::json!({
            "options": { "completion_scaling": 1, "completion_scaling_floor": 0.0 },
            "completionScalingBasis": 1,
            "regionSphereTargets": { "Limgrave": 0.1 },
            "regionSphereTargetRanges": [[61000, 61001, 100], [62000, 62999, 10000]],
        });
        let c = parse_scaling_config(&sd).expect("ranges must arm the feature");
        assert_eq!(c.region_ranges.len(), 2);
        assert_eq!(c.max_target, 10000);
    }

    #[test]
    fn scaling_range_membership() {
        assert!(is_scaling_speffect(7010));
        assert!(is_scaling_speffect(7500)); // vanilla baked scaling we must clear
        assert!(is_scaling_speffect(7999));
        assert!(!is_scaling_speffect(6999));
        assert!(!is_scaling_speffect(8000));
    }

    #[test]
    fn dlc_scaling_range_is_cleared_too() {
        // The DLC +20,000,000 scaling ladder DLC enemies carry innately -- must be cleared like base
        // 70xx, or DLC enemies keep 7-14x vanilla scaling under the mod's tier (the 2026-07-15 bug).
        assert!(is_scaling_speffect(20007010)); // 7.84x HP
        assert!(is_scaling_speffect(20007060)); // 11.5x HP
        assert!(is_scaling_speffect(20007110)); // 14.1x HP
        assert!(is_scaling_speffect(20007310)); // top of the observed DLC ladder
        assert!(!is_scaling_speffect(20006999)); // just below the DLC block
        assert!(!is_scaling_speffect(20008000)); // just above
                                                 // The non-scaling innate speffects DLC enemies also carry are NOT in either range.
        assert!(!is_scaling_speffect(5400));
        assert!(!is_scaling_speffect(90000));
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

    // --- parse_scaling_config (connect-time slot_data parse; SWEEP H4 refuse-to-arm) ---

    use serde_json::json;

    #[test]
    fn parse_disabled_option_is_none() {
        let sd = json!({ "options": { "completion_scaling": 0 },
                         "regionSphereTargets": { "60000": 3 } });
        assert!(parse_scaling_config(&sd).is_none());
    }

    #[test]
    fn parse_empty_targets_refuses_to_arm() {
        // SWEEP H4 / R6 regression: enabled + empty/missing regionSphereTargets used to arm with
        // every region at floor_tier, and the sweep then flattened ALL baked enemy scaling.
        let missing = json!({ "options": { "completion_scaling": 1 } });
        assert!(
            parse_scaling_config(&missing).is_none(),
            "missing map must stay INERT (H4)"
        );
        let empty = json!({ "options": { "completion_scaling": 1 },
                            "regionSphereTargets": {} });
        assert!(
            parse_scaling_config(&empty).is_none(),
            "empty map must stay INERT (H4)"
        );
        let garbage = json!({ "options": { "completion_scaling": 1 },
                              "regionSphereTargets": { "not-a-number": 3 } });
        assert!(
            parse_scaling_config(&garbage).is_none(),
            "all-garbage map must stay INERT (H4)"
        );
    }

    #[test]
    fn parse_full_config_round_trips() {
        let sd = json!({
            "options": { "completion_scaling": 1, "completion_scaling_floor": 1.5 },
            "completionScalingBasis": "sphere",
            "regionSphereTargets": { "60000": 0, "63000": 5, "76000": 9 },
        });
        let cfg = parse_scaling_config(&sd).expect("should arm");
        assert_eq!(cfg.basis, ScalingBasis::Sphere);
        assert_eq!(cfg.floor_tier, floor_tier_from_multiplier(1.5));
        assert_eq!(cfg.max_target, 9);
        assert_eq!(cfg.region_targets.get(&63000), Some(&5));
        assert_eq!(cfg.region_targets.len(), 3);
    }

    #[test]
    fn parse_basis_accepts_apworld_int_form() {
        // fill_slot_data ships completion_scaling_basis.value (int: 0 geographic / 1 sphere);
        // the string-only match used to silently demote sphere -> Geographic.
        let sphere = json!({ "options": { "completion_scaling": 1 },
                             "completionScalingBasis": 1,
                             "regionSphereTargets": { "60000": 1 } });
        assert_eq!(
            parse_scaling_config(&sphere).unwrap().basis,
            ScalingBasis::Sphere
        );
        let geo = json!({ "options": { "completion_scaling": 1 },
                          "completionScalingBasis": 0,
                          "regionSphereTargets": { "60000": 1 } });
        assert_eq!(
            parse_scaling_config(&geo).unwrap().basis,
            ScalingBasis::Geographic
        );
    }

    #[test]
    fn parse_defaults_basis_geographic_and_floor_zero() {
        let sd = json!({ "options": { "completion_scaling": true },
                         "regionSphereTargets": { "60000": 1 } });
        let cfg = parse_scaling_config(&sd).expect("should arm");
        assert_eq!(cfg.basis, ScalingBasis::Geographic);
        assert_eq!(cfg.floor_tier, 0);
        assert_eq!(cfg.max_target, 1);
    }

    #[test]
    fn i32_map_skips_bad_entries_keeps_good() {
        let v = json!({ "60000": 3, "bad": 1, "61000": "nope", "62000": 7 });
        let m = i32_i32_map(Some(&v));
        assert_eq!(m.len(), 2);
        assert_eq!(m.get(&60000), Some(&3));
        assert_eq!(m.get(&62000), Some(&7));
        assert!(i32_i32_map(None).is_empty());
        assert!(i32_i32_map(Some(&json!([1, 2]))).is_empty());
    }
}
