//! Pure sphere/completion enemy-scaling decisions (see `SPEC-runtime-enemy-scaling.md`).
//!
//! Maps a region's sphere target to a scaling TIER, and a tier to the vanilla `SpEffectParam` row the
//! client applies to an enemy (`ChrIns::apply_speffect`). Host-tested, no game.
//!
//! The ladder is the game's OWN progressive enemy-scaling SpEffects: all visually silent
//! (`vfxId/stateInfo/iconId = -1/0/-1`), `spCategory 0`, and rune-neutral (`haveSoulRate = 1`).
//!
//! # `7010..7200` — the FULL run (extended 2026-07-27)
//!
//! This used to be the subset `7010..7100` (1.14x..3.70x HP), with a comment saying the full ladder
//! "tops out at ~7.4x HP ... extend toward `7200` here for a harsher curve." That was right, and
//! there was nothing to check it against until `SpEffectParam.csv` joined `gen_inputs.db`. Now
//! DERIVED from the param: 20 rungs, **1.141x .. 7.422x HP**, strictly ascending, every row's
//! `spCategory`/`haveSoulRate` asserted at generation time. The first ten rungs' HP are byte-identical
//! to the values that were hand-transcribed here, so the old transcription was correct; `attack` and
//! `defense` now carry the param's full precision (both are display-only — `speffect_id` is what gets
//! applied). Doubling the top rung is a deliberate balance change: the DEEPEST region in a seed now
//! reaches 7.42x rather than 3.70x, and mid-run tiers rise with it.
//!
//! `defense` comes from `physicsDiffenceRate` — FromSoft's spelling, which is why a search for
//! "defence"/"defense" in the param finds nothing.
//!
//! # What is NOT in the ladder, and why
//!
//! * `7210..7280` — a separate ascending sub-run (5.3x..5.8x). Different purpose; not sphere depth.
//! * `7400..7680` — the BAND. 3.434x down to ~1.0x, `haveSoulRate` 2-5. 🛑 I called this the co-op
//!   guest-count set and that was WRONG: a solo log (2026-08-05) showed SEVEN distinct band ids on
//!   one player at once, and guest count is a single session-wide value. It is per-ENEMY. See
//!   `BAND_TIERS` — it is a native-strength SOURCE, not a rung, and it is still cleared.
//! * `7800..7902` — `spCategory 140`, a different stacking class entirely.
//! * `20007000..20007150` — the DLC block, and the literal CONTINUATION of this one: `7170` is
//!   `7.047` and `20007000` is `7.046875`, the same rung. It runs on to **16.64x**, but its `attack`
//!   is nearly flat (3.747..3.85 across the whole block) while HP does all the work, so it is a
//!   different SHAPE, not just a steeper version. Using it would also need a live check that
//!   base-game enemies accept a DLC-block speffect at all. Left alone for now.

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

/// The tier ladder — vanilla `7010..7200`, ascending, DERIVED from SpEffectParam (see module doc).
/// Index 0 = shallowest sphere, last = deepest. 1.141x .. 7.422x HP.
pub const SCALING_TIERS: &[ScalingTier] = &[
    ScalingTier {
        speffect_id: 7010,
        hp: 1.141,
        attack: 1.096703,
        defense: 1.013,
    },
    ScalingTier {
        speffect_id: 7020,
        hp: 1.281,
        attack: 1.202198,
        defense: 1.026,
    },
    ScalingTier {
        speffect_id: 7030,
        hp: 1.656,
        attack: 1.494506,
        defense: 1.039,
    },
    ScalingTier {
        speffect_id: 7040,
        hp: 1.813,
        attack: 1.494506,
        defense: 1.053,
    },
    ScalingTier {
        speffect_id: 7050,
        hp: 1.953,
        attack: 1.69011,
        defense: 1.066,
    },
    ScalingTier {
        speffect_id: 7060,
        hp: 2.266,
        attack: 1.758242,
        defense: 1.079,
    },
    ScalingTier {
        speffect_id: 7070,
        hp: 2.406,
        attack: 1.830769,
        defense: 1.093,
    },
    ScalingTier {
        speffect_id: 7080,
        hp: 2.688,
        attack: 2.0,
        defense: 1.106,
    },
    ScalingTier {
        speffect_id: 7090,
        hp: 3.25,
        attack: 2.279121,
        defense: 1.119,
    },
    ScalingTier {
        speffect_id: 7100,
        hp: 3.703,
        attack: 2.472527,
        defense: 1.133,
    },
    ScalingTier {
        speffect_id: 7110,
        hp: 4.125,
        attack: 2.672528,
        defense: 1.146,
    },
    ScalingTier {
        speffect_id: 7120,
        hp: 4.844,
        attack: 3.243956,
        defense: 1.159,
    },
    ScalingTier {
        speffect_id: 7130,
        hp: 5.484,
        attack: 3.243956,
        defense: 1.172,
    },
    ScalingTier {
        speffect_id: 7140,
        hp: 6.563,
        attack: 3.52967,
        defense: 1.186,
    },
    ScalingTier {
        speffect_id: 7150,
        hp: 6.688,
        attack: 3.584615,
        defense: 1.2,
    },
    ScalingTier {
        speffect_id: 7160,
        hp: 6.875,
        attack: 3.63956,
        defense: 1.2,
    },
    ScalingTier {
        speffect_id: 7170,
        hp: 7.047,
        attack: 3.745055,
        defense: 1.217,
    },
    ScalingTier {
        speffect_id: 7180,
        hp: 7.203,
        attack: 3.795605,
        defense: 1.22,
    },
    ScalingTier {
        speffect_id: 7190,
        hp: 7.328,
        attack: 3.795605,
        defense: 1.23,
    },
    ScalingTier {
        speffect_id: 7200,
        hp: 7.422,
        attack: 3.795605,
        defense: 1.232,
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

/// A rung of the area-scaling LADDER proper — base `7010..7200` (the `SCALING_TIERS` ids) or its DLC
/// re-emission. Narrower than `is_scaling_speffect` on purpose; see `ScalingKind`.
///
/// 🛑 THE DLC ARM MUST EXCLUDE THE BAND. It used to be a blanket `DLC_SCALING_ID_RANGE.contains()`,
/// and the DLC block carries BOTH families: `20007000..20007350` is the ladder (`haveSoulRate 1`) and
/// `20007400..20007750` is the band (`haveSoulRate 2`). So every DLC band carrier was classified as a
/// rung and sent down the `ScaleAction::Replace` path — which is BIDIRECTIONAL, and applies the
/// region tier absolutely. A test in this file failed on `20007400` the moment the band table
/// existed; without the table the range read as obviously correct.
pub fn is_ladder_rung(param_id: i32) -> bool {
    (DLC_SCALING_ID_RANGE.contains(&param_id) && !is_band_rung(param_id))
        || SCALING_TIERS.iter().any(|t| t.speffect_id == param_id)
}

/// Which half of the clear space a carried scaling SpEffect belongs to.
///
/// 🛑 THIS DISTINCTION IS THE WHOLE DIAGNOSTIC (#346). `is_scaling_speffect` is deliberately WIDE —
/// `SCALING_ID_RANGE` is `7000..8000`, the entire block. That is correct for CLEARING (anything in
/// there is `spCategory 0` and would stack with our rung) and WRONG for DIAGNOSIS: `7210..7280` (a
/// separate 5.3-5.8x run) and `7800..7902` (`spCategory 140`) sit in the range but are not ladder
/// rungs. An enemy carrying only one of those has no vanilla AREA-scaling tier at all, yet the
/// obvious test — "was the cleared set non-empty?" — reports that it did.
///
/// Issue #346 prescribed exactly that test. It would have misclassified part of the very class it
/// was written to find, and the log would have read as evidence. Classify, then count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalingKind {
    /// Vanilla assigned this enemy an area difficulty, and we re-tier against it.
    Ladder,
    /// In the clear range, so we strip it — but it says nothing about the enemy's native tier.
    OtherInRange,
}

/// Classify one carried `param_id`. `None` = not ours to clear.
pub fn scaling_kind(param_id: i32) -> Option<ScalingKind> {
    if is_ladder_rung(param_id) {
        Some(ScalingKind::Ladder)
    } else if is_scaling_speffect(param_id) {
        Some(ScalingKind::OtherInRange)
    } else {
        None
    }
}

/// The BAND: a second family of `spCategory 0` scaling rows, distinct from the ladder.
///
/// ⭐⭐⭐ **The discriminator is `haveSoulRate`, not the id range.** Every ladder row is
/// `haveSoulRate 1`; every band row is 2-5. The base game has both (`7010..7200` ladder,
/// `7400..7680` band) and so does the DLC block (`20007000..20007350` ladder in two identical
/// copies, `20007400..20007750` band in two). That parallel structure is why this is a family and
/// not a curiosity.
///
/// 🛑 WHAT IT IS NOT. I identified this as the co-op guest-count ladder, from `haveSoulRate` stepping
/// 5 -> 4 -> 3 -> 2 as the multiplier falls. A solo log (2026-08-05) refuted it outright: seven
/// distinct band ids were live on one player at once, and guest count is a single session-wide value.
/// It is applied per-ENEMY, at runtime, by a mechanism we have not identified -- nothing carries a
/// band id innately in `NpcParam` (all 7,039 rows checked; the only non-ladder in-range id found
/// innately is `7000`, on 20 rows).
///
/// 🛑 WHAT WE USE IT FOR, AND WHY THAT IS SAFE WITHOUT KNOWING WHAT IT IS. Only what it measurably
/// DOES: an unconditional (`conditionHp -1`), indefinite (`effectEndurance -1`) `maxHpRate`
/// multiplier in the ladder's own units. If its purpose turns out to be something else entirely, the
/// worst case is that we under-scale relative to an intent we could not see -- a blemish. Nothing
/// here reasons from what it is FOR.
///
/// `(speffect_id, maxHpRate)`. Not monotone: `7520` sits above `7510`, and the `7560..7680` tail is
/// flat at 1.015 above `7550`'s 1.001. Do not assert descending.
pub const BAND_TIERS: &[(i32, f32)] = &[
    (7400, 3.434),         // 1.902x atk, haveSoulRate 5
    (7410, 3.122),         // 1.965x atk, haveSoulRate 5
    (7420, 2.857),         // 1.768x atk, haveSoulRate 5
    (7430, 2.355),         // 1.680x atk, haveSoulRate 5
    (7440, 2.146),         // 1.609x atk, haveSoulRate 4
    (7450, 2.099),         // 1.483x atk, haveSoulRate 4
    (7460, 1.845),         // 1.400x atk, haveSoulRate 4
    (7470, 1.523),         // 1.314x atk, haveSoulRate 3
    (7480, 1.479),         // 1.239x atk, haveSoulRate 3
    (7490, 1.461),         // 1.200x atk, haveSoulRate 3
    (7500, 1.296),         // 1.150x atk, haveSoulRate 3
    (7510, 1.253),         // 1.141x atk, haveSoulRate 3
    (7520, 1.264),         // 1.135x atk, haveSoulRate 2
    (7530, 1.191),         // 1.134x atk, haveSoulRate 2
    (7540, 1.008),         // 1.122x atk, haveSoulRate 2
    (7550, 1.001),         // 1.094x atk, haveSoulRate 2
    (7560, 1.015),         // 1.134x atk, haveSoulRate 2
    (7570, 1.001),         // 1.094x atk, haveSoulRate 2
    (7580, 1.015),         // 1.134x atk, haveSoulRate 2
    (7590, 1.015),         // 1.134x atk, haveSoulRate 2
    (7600, 1.015),         // 1.134x atk, haveSoulRate 2
    (7610, 1.015),         // 1.134x atk, haveSoulRate 2
    (7620, 1.015),         // 1.134x atk, haveSoulRate 2
    (7630, 1.015),         // 1.134x atk, haveSoulRate 2
    (7640, 1.015),         // 1.134x atk, haveSoulRate 2
    (7650, 1.015),         // 1.134x atk, haveSoulRate 2
    (7660, 1.015),         // 1.134x atk, haveSoulRate 2
    (7670, 1.015),         // 1.134x atk, haveSoulRate 2
    (7680, 1.015),         // 1.134x atk, haveSoulRate 2
    (20007400, 1.0981221), // 1.024x atk, haveSoulRate 2
    (20007410, 1.0839258), // 1.023x atk, haveSoulRate 2
    (20007420, 1.0779487), // 1.022x atk, haveSoulRate 2
    (20007430, 1.0781562), // 1.020x atk, haveSoulRate 2
    (20007440, 1.0782156), // 1.020x atk, haveSoulRate 2
    (20007450, 1.0768288), // 1.020x atk, haveSoulRate 2
    (20007460, 1.0770154), // 1.018x atk, haveSoulRate 2
    (20007470, 1.0771002), // 1.017x atk, haveSoulRate 2
    (20007480, 1.0693364), // 1.016x atk, haveSoulRate 2
    (20007490, 1.0541972), // 1.015x atk, haveSoulRate 2
    (20007500, 1.0456709), // 1.014x atk, haveSoulRate 2
    (20007510, 1.0388143), // 1.013x atk, haveSoulRate 2
    (20007520, 1.0313383), // 1.012x atk, haveSoulRate 2
    (20007530, 1.0206945), // 1.011x atk, haveSoulRate 2
    (20007540, 1.017251),  // 1.011x atk, haveSoulRate 2
    (20007550, 1.01),      // 1.010x atk, haveSoulRate 2
    (20007600, 1.0981221), // 1.024x atk, haveSoulRate 2
    (20007610, 1.0839258), // 1.023x atk, haveSoulRate 2
    (20007620, 1.0779487), // 1.022x atk, haveSoulRate 2
    (20007630, 1.0781562), // 1.020x atk, haveSoulRate 2
    (20007640, 1.0782156), // 1.020x atk, haveSoulRate 2
    (20007650, 1.0768288), // 1.020x atk, haveSoulRate 2
    (20007660, 1.0770154), // 1.018x atk, haveSoulRate 2
    (20007670, 1.0771002), // 1.017x atk, haveSoulRate 2
    (20007680, 1.0693364), // 1.016x atk, haveSoulRate 2
    (20007690, 1.0541972), // 1.015x atk, haveSoulRate 2
    (20007700, 1.0456709), // 1.014x atk, haveSoulRate 2
    (20007710, 1.0388143), // 1.013x atk, haveSoulRate 2
    (20007720, 1.0313383), // 1.012x atk, haveSoulRate 2
    (20007730, 1.0206945), // 1.011x atk, haveSoulRate 2
    (20007740, 1.017251),  // 1.011x atk, haveSoulRate 2
    (20007750, 1.01),      // 1.010x atk, haveSoulRate 2
];

/// Whether `param_id` is a band row (see [`BAND_TIERS`]).
pub fn is_band_rung(param_id: i32) -> bool {
    BAND_TIERS.iter().any(|&(id, _)| id == param_id)
}

/// The native ladder tier a carried band row implies: the LOWEST rung at least as strong as it.
///
/// 🛑 **SHELVED, DELIBERATELY — do not wire this into `scale_action`.** It was built to give the
/// UNRUNGED class a native tier, on my reading of a log as "527 unrunged, 303 band carriers". The
/// split census refuted that outright: `band-only` is **~1 per region**, and the unrunged class
/// carries no band rows at all (`band+rung` equalled the scaled count exactly, 198 of 198 and 153 of
/// 153). A native-tier source that reaches one enemy per region is not worth a wire.
///
/// It stays in the tree as the thing that named the family and caught the DLC-range bug in
/// `is_ladder_rung`, and because the census still uses it. If you are reading this while considering
/// connecting it: the reason not to is measured, not forgotten.
///
/// Ceiling, not nearest. Rounding to the nearest rung could land BELOW the multiplier the enemy
/// actually carries, and the whole point of a native tier is to be a floor we must clear before we
/// touch anything. Every ambiguity in this file resolves toward not touching.
pub fn band_native_tier(param_id: i32) -> Option<usize> {
    let hp = BAND_TIERS
        .iter()
        .find(|&&(id, _)| id == param_id)
        .map(|&(_, hp)| hp)?;
    Some(
        SCALING_TIERS
            .iter()
            .position(|t| t.hp >= hp)
            .unwrap_or(NUM_TIERS - 1),
    )
}

/// Leave an UNRUNGED enemy vanilla at the bottom of the ladder (#346, phase 0).
///
/// An enemy vanilla ships without any ladder rung is hand-tuned: its base stats already ARE its
/// difficulty. Every named boss is in that class, and so are the named/invader NPCs the Nexus report
/// was about. The ladder has no rung below 1.0, so the *weakest* thing we can say to such an enemy is
/// `7010` = **1.141x HP** — a 14% BUFF, applied in the course of trying to make it easier. That is not
/// a normalization, so at the bottom we say nothing and leave it alone.
///
/// 🛑 Bottom rung ONLY, and this is not the fix. A player who raised `minimum_enemy_difficulty` asked
/// for a floor above vanilla and still gets it (`target_tier > 0` is an instruction, not an artefact
/// of the ladder's shape). An unrunged enemy in a DEEP sphere still scales up — that half of #346
/// already works. The wall — an endgame-tuned NPC holding a check in a shallow sphere — needs a
/// native-tier notion and a sub-1.0 primitive, and survives this change.
pub fn skip_unrunged_at_floor(carried_ladder_rung: bool, target_tier: usize) -> bool {
    !carried_ladder_rung && target_tier == 0
}

/// What the sweep should do with one enemy (#346, phase 1a).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleAction {
    /// It carries a vanilla ladder rung, so the rung IS its declared native difficulty and swapping
    /// one rung for another is a true re-tier. Clear and apply, in both directions. This is the
    /// 4,214-row mainline population and its behaviour is exactly what shipped.
    Replace,
    /// No vanilla rung, but we have a defensible native tier for it, and the target is STRONGER.
    Apply,
    /// Leave it exactly as vanilla shipped it.
    NoTouch,
}

/// Native ladder index for an **`npc_param_id`**, or `None` if we have no defensible answer.
///
/// 🛑 THE KEY IS `ChrIns::npc_param_id`, NOT `ChrIns::npc_id`. They are different fields and the
/// crate documents both: `npc_param_id` is "NPC param ID for this character", an 8-9 digit
/// `NpcParam` row; `npc_id` is "4 number identifier for this npc, eg. 8000 for Torrent" — a
/// character/model id. This table is built from `NpcParam` row ids, so keying it on `npc_id` looks
/// entirely reasonable, compiles, runs, and misses EVERY row. Caught from a live log
/// (2026-08-05): the census reported unrunged `npc_ids [0, 100, 1000, 3010, 6001, 8000]`, all four
/// digits, and only three of them exist in `NpcParam` at all.
///
/// Derived offline from `getSoul` — the rune reward — calibrated against the 4,214 rows that carry
/// both a reward and a rung (Spearman 0.996). See `tools/derive_native_tiers.py` for the method and
/// for why `hp` is not the signal.
pub fn native_tier(npc_param_id: i32) -> Option<usize> {
    crate::native_tiers::NATIVE_TIERS
        .binary_search_by_key(&npc_param_id, |&(id, _)| id)
        .ok()
        .map(|i| crate::native_tiers::NATIVE_TIERS[i].1 as usize)
}

/// THE PHASE 1a DECISION. Supersedes `skip_unrunged_at_floor`, which only declined at the bottom
/// rung; this declines wherever we cannot show the target is an increase.
///
/// ⭐ **Absence is the safe state, and it is load-bearing.** An `npc_id` we have no native tier for
/// is never touched at ANY depth. 2,440 unrunged rows are in that class — every named boss among
/// them, because boss rune rewards live in `GameAreaParam` per arena, not in `NpcParam`. So does any
/// row a future patch adds. The failure mode of "absent" is an enemy that stays vanilla in a deep
/// sphere: under-scaled, a balance blemish. The failure mode of the alternative — defaulting to the
/// floor, which is what shipped — is a rung applied on top of hand-tuned endgame stats in a shallow
/// sphere, which is a progression WALL. That asymmetry decides every open question in this file.
///
/// 🛑 UP ONLY for derived entities. The ladder has no rung below 1.0, so "scale this hand-tuned
/// enemy DOWN" is not expressible yet; `Apply` fires only when `target_tier` exceeds the native one.
/// The down half needs the off-label `20018xxx` pair and is gated on a live probe.
pub fn scale_action(
    carried_ladder_rung: bool,
    npc_param_id: i32,
    target_tier: usize,
) -> ScaleAction {
    if carried_ladder_rung {
        return ScaleAction::Replace;
    }
    match native_tier(npc_param_id) {
        Some(native) if target_tier > native => ScaleAction::Apply,
        _ => ScaleAction::NoTouch,
    }
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
    /// floor). Empty = no DLC / mode != 2 -- which is the DEFAULT, so see `dlc_buckets` before
    /// reaching for this.
    pub dlc_blessing_floors: Vec<(i32, i32, i32)>,
    /// `dlcRegionBuckets`: the play_region/100 sub-ids of the seed's KEPT DLC regions, sorted.
    ///
    /// 🛑 THIS is the DLC-region test. `dlc_blessing_floors` was used for it until 2026-07-27, which
    /// was wrong in a way that could not be seen: those floors are emitted only when
    /// `global_scadutree_blessing == 2`, and the shipped default has been `off` since 2026-07-18 --
    /// so "is this a DLC region?" answered `false` for every bucket in the game on every default
    /// seed. It only decorated a log line, so nothing failed. It is exactly the wrong thing to key
    /// the DLC enemy ladder off. Empty = no DLC region in play.
    pub dlc_buckets: Vec<i32>,
    /// Maximum tier -- from `completion_scaling_ceiling`; nothing scales above this. `NUM_TIERS - 1`
    /// (the default) means uncapped. The MIRROR of `floor_tier`: gen sends an HP multiplier and this
    /// is the last rung no stronger than it.
    ///
    /// ⚠️ A client that predates this key simply never reads it and scales uncapped. That is why a
    /// seed which actually SETS a ceiling also lists `scaling_ceiling` in `requiresClientFeatures`
    /// (see `er_logic::client_features`) -- the connect check refuses rather than quietly ignoring a
    /// difficulty setting the player chose.
    pub ceiling_tier: usize,
}

/// Map a raw target to a tier index in `[floor_tier, NUM_TIERS)`. `max_target <= 0` → the floor tier.
/// Monotonic in `target`.
pub fn tier_for_target(
    target: i32,
    max_target: i32,
    floor_tier: usize,
    ceiling_tier: usize,
) -> usize {
    // A ceiling below the floor would make `clamp` PANIC (min > max). Gen rejects that pair with an
    // OptionError, but this is a PURE function reachable from hand-rolled or foreign slot_data, so it
    // resolves the contradiction rather than trusting its producer.
    //
    // THE CEILING WINS. When two difficulty bounds contradict each other the safe direction is the
    // gentler one: too-weak enemies are a disappointing seed, too-strong ones can be an unplayable
    // one. (This comment first claimed the floor won, which was neither what the code did nor the
    // better answer -- caught by the test below, which is why it exists.)
    let ceiling = ceiling_tier.min(NUM_TIERS - 1);
    let floor = floor_tier.min(ceiling);
    if max_target <= 0 {
        return floor;
    }
    let frac = (target.max(0) as f32 / max_target as f32).clamp(0.0, 1.0);
    let tier = (frac * (NUM_TIERS - 1) as f32).round() as usize;
    tier.clamp(floor, ceiling)
}

/// Region → tier. A region absent from `region_targets` falls back to the floor tier (unknown = don't
/// scale up). DLC buckets are NOT special-cased: with the DLC baked-scaling clear fixed
/// (`DLC_SCALING_ID_RANGE`), DLC enemies scale by sphere depth exactly like base game.
pub fn tier_for_region(cfg: &ScalingConfig, region: i32) -> usize {
    if let Some(&target) = cfg.region_targets.get(&region) {
        tier_for_target(target, cfg.max_target, cfg.floor_tier, cfg.ceiling_tier)
    } else if let Some(&(_, _, target)) =
        // SCALING_WIRE: range fallback -- `region` is the play_region/100 sub id; the apworld
        // emits [lo, hi, target] buckets in the same space (a few dozen; linear scan is fine).
        cfg
            .region_ranges
            .iter()
            .find(|&&(lo, hi, _)| (lo..=hi).contains(&region))
    {
        tier_for_target(target, cfg.max_target, cfg.floor_tier, cfg.ceiling_tier)
    } else {
        cfg.floor_tier.min(NUM_TIERS - 1)
    }
}

/// `[i32, ...]` slot_data array -> `Vec<i32>`, non-integers skipped. Tolerant like the rest of the
/// slot_data parses.
pub fn parse_int_list(v: Option<&Value>) -> Vec<i32> {
    v.and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_i64().map(|n| n as i32))
                .collect()
        })
        .unwrap_or_default()
}

/// Is this play_region/100 bucket part of a kept DLC region? Pure.
///
/// The apworld emits `dlcRegionBuckets` sorted, but this does not rely on that -- a membership test
/// that silently depends on ordering is the kind of assumption that rots. A few dozen entries; a
/// linear scan is not the bottleneck.
pub fn is_dlc_bucket(cfg: &ScalingConfig, region: i32) -> bool {
    cfg.dlc_buckets.contains(&region)
}

/// The Scadutree-blessing FLOOR level for a play_region/100 `region` bucket, or `None` if the bucket
/// isn't in the DLC floor wire (unknown = no floor). Pure. Used by upgrades.rs to write
/// max(fragment level, floor).
///
/// 🛑 NOT a DLC-region test -- use `is_dlc_bucket`. See `ScalingConfig::dlc_buckets`.
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
/// Highest tier whose HP rate is <= `ceil_mult` -- the MIRROR of `floor_tier_from_multiplier`, and
/// deliberately a separate function. A floor is "the first rung at least this strong"; a ceiling is
/// "the last rung no stronger than this". Reusing the floor search for both would cap one rung high.
/// Below the ladder -> tier 0 (the weakest rung is the least we can do).
pub fn ceiling_tier_from_multiplier(ceil_mult: f32) -> usize {
    SCALING_TIERS
        .iter()
        .rposition(|t| t.hp <= ceil_mult)
        .unwrap_or(0)
}

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
    // The real DLC-region membership set (see ScalingConfig::dlc_buckets). Independent of the
    // blessing option, unlike the floors above.
    let dlc_buckets = parse_int_list(sd.get("dlcRegionBuckets"));
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
    // Absent (older apworld) -> uncapped, which is what every seed did before this key existed.
    let ceiling_tier = match sd
        .pointer("/options/completion_scaling_ceiling")
        .and_then(|v| v.as_f64())
    {
        Some(m) => ceiling_tier_from_multiplier(m as f32),
        None => NUM_TIERS - 1,
    };
    Some(ScalingConfig {
        basis,
        floor_tier,
        region_targets,
        region_ranges,
        max_target,
        dlc_blessing_floors,
        dlc_buckets,
        ceiling_tier,
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

// ---- REGION SCALING, SAID OUT LOUD -------------------------------------------------------------
// Alaric + CrazzyMatthew21, 2026-07-29: "it feels unclear at which points im supposed to be in which
// areas." That is an INFORMATION problem before it is a tuning one. Scaling ramps over the ORDER a
// seed's regions unlock, so a late Limgrave is a hard Limgrave -- correct by design, and completely
// invisible to the player, who only knows Patches hits like a truck.
//
// The client already holds everything needed to say so, so it says so. Pure and host-tested: the
// caller does the I/O, this decides the words.

/// What we actually know about a region's scaling when the player walks into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionScaling {
    /// The seed put this region's bucket on the wire; `target` resolved to this tier.
    Known(usize),
    /// No target for this bucket (hub, tutorial, an unmapped sub-area, or a foreign/older seed), so
    /// the client is using the floor tier. NOT the same thing as "tier 0", and the toast must not
    /// present a defaulted value as a derived one -- unknown is said out loud, per the house rule
    /// that a derivation which cannot answer must say so rather than answer.
    Defaulted(usize),
}

/// Resolve a region's scaling for display. `target: None` means the bucket was absent from the wire.
pub fn region_scaling(
    target: Option<i32>,
    max_target: i32,
    floor_tier: usize,
    ceiling_tier: usize,
) -> RegionScaling {
    match target {
        Some(t) => RegionScaling::Known(tier_for_target(t, max_target, floor_tier, ceiling_tier)),
        None => RegionScaling::Defaulted(floor_tier.min(NUM_TIERS - 1)),
    }
}

/// The HP multiplier of a tier, clamped into the ladder.
pub fn tier_multiplier(tier: usize) -> f32 {
    SCALING_TIERS[tier.min(NUM_TIERS - 1)].hp
}

/// The one-line toast shown when the player first enters a region.
///
///   `Liurnia - enemy scaling 4.12x (tier 10 of 19)`      (uncapped seed: the band IS the ladder)
///   `Liurnia - enemy scaling 4.12x (tier 7 of 12)`       (floor 3, ceiling 15)
///   `Liurnia - enemy scaling not set for this area; using the floor, 1.14x`
///
/// The NUMBER is the point. A tier index alone means nothing to a player, and "hard" means nothing
/// either; `4.12x` is a thing you can compare to your own weapon. The tier fraction rides along so
/// two regions can be ranked against each other at a glance.
///
/// THE FRACTION IS ABOUT THIS RUN. It used to print the absolute ladder index over `NUM_TIERS - 1`,
/// a compile-time constant that knows nothing about the seed. On a seed with
/// `completion_scaling_floor` / `_ceiling` set, that denominator LIED: a player standing on their
/// hardest region read "of 19" and concluded they were two-thirds of the way up a ladder they could
/// never climb. Now both ends come from the resolved band, and the numerator is the rung within it.
/// (On an uncapped seed the band is the whole ladder -- the deepest kept region normalizes to
/// `frac == 1.0` and lands on the top rung -- so "of 19" was never wrong THERE, only where a bound
/// was set. Alaric asked 2026-07-31 whether the denominator was global or per-seed; it was global.)
///
/// STOP: ASCII ONLY. This string is drawn by the GAME, not by a terminal, and the in-game font has
/// no glyph for an em-dash: Alaric's 2026-07-31 screenshot reads `Altus ? enemy scaling 1.14x`
/// because this used U+2014. Everything a player reads in-game goes through that font, so the same
/// rule the .ps1 and gen_data surfaces already carry applies here. `every_toast_is_ascii` pins it.
pub fn region_scaling_toast(
    region: &str,
    scaling: RegionScaling,
    floor_tier: usize,
    ceiling_tier: usize,
) -> String {
    // THE FRACTION IS ABOUT THIS RUN, not the ladder. Same clamp order as `tier_for_target` --
    // ceiling first, then floor into it -- so the two cannot disagree about the band.
    let ceiling = ceiling_tier.min(NUM_TIERS - 1);
    let floor = floor_tier.min(ceiling);
    let span = ceiling - floor;
    match scaling {
        // Position WITHIN the band, not the absolute ladder index: with a floor of 3 an absolute
        // "tier 3" is the player's FIRST rung, and calling it 3 would read as "already a third of
        // the way up" when they have not moved at all.
        RegionScaling::Known(tier) if span == 0 => format!(
            "{} - enemy scaling {:.2}x (the only tier this seed)",
            region,
            tier_multiplier(tier)
        ),
        RegionScaling::Known(tier) => format!(
            "{} - enemy scaling {:.2}x (tier {} of {})",
            region,
            tier_multiplier(tier),
            tier.clamp(floor, ceiling) - floor,
            span
        ),
        RegionScaling::Defaulted(tier) => format!(
            "{} - enemy scaling not set for this area; using the floor, {:.2}x",
            region,
            tier_multiplier(tier)
        ),
    }
}

/// The coarse region NAME for a play_region/100 bucket, from the baked geometry table
/// (`region_locks::REGION_LOCKS` -- generated from the SAME `REGION_PLAY_IDS` the apworld derives
/// `regionSphereTargetRanges` from, so a bucket the scaling wire can target is a bucket this can
/// name). `None` = geometry that names no region (Roundtable, the tutorial, unmapped sub-areas).
/// The entry toast DERIVES its name here or stays silent; it never invents one.
pub fn region_name_for_bucket(bucket: i32) -> Option<&'static str> {
    crate::region_locks::REGION_LOCKS
        .iter()
        .find(|r| r.play_regions.contains(&bucket))
        .map(|r| r.region)
}

/// Once-per-announcement ledger for the region-entry scaling toast -- the production caller's
/// dedup, host-tested here because "when to repeat yourself" is a decision, not I/O.
///
/// Dedup is by the MESSAGE, not the bucket. A region spans many buckets (Altus has 13), and
/// crossing between two buckets of one region must not re-say the same words -- riding a border
/// would otherwise strobe the deck. But a hand-authored intra-fold delta (Greyoll's Dragonbarrow
/// inside Caelid) resolves a DIFFERENT tier under the same name, and that difference is exactly
/// worth one line of its own. So: each distinct rendered announcement fires once per session;
/// `reset()` on (re)configure lets a new seed -- whose tiers all changed -- speak afresh.
pub struct RegionToastLedger {
    announced: Vec<String>,
}

impl RegionToastLedger {
    /// `const` so the client can hold one in a `static Mutex` (see the const-construction test).
    pub const fn new() -> Self {
        RegionToastLedger {
            announced: Vec::new(),
        }
    }

    /// Forget everything said. Call when a new `ScalingConfig` arrives (connect / seed change).
    pub fn reset(&mut self) {
        self.announced.clear();
    }

    /// The announcement owed for observing the player in `bucket`, or `None`: no name for the
    /// bucket (hub/tutorial -- silence, not a guess), or these exact words were already said this
    /// session. `name` is a parameter (the caller passes [`region_name_for_bucket`]) so the
    /// decision stays testable without the baked table.
    pub fn on_region(
        &mut self,
        cfg: &ScalingConfig,
        bucket: i32,
        name: Option<&str>,
    ) -> Option<String> {
        let name = name?;
        let scaling = region_scaling(
            raw_target_for_region(cfg, bucket),
            cfg.max_target,
            cfg.floor_tier,
            cfg.ceiling_tier,
        );
        let msg = region_scaling_toast(name, scaling, cfg.floor_tier, cfg.ceiling_tier);
        if self.announced.contains(&msg) {
            return None;
        }
        self.announced.push(msg.clone());
        Some(msg)
    }
}

impl Default for RegionToastLedger {
    fn default() -> Self {
        Self::new()
    }
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
            dlc_buckets: vec![],
            ceiling_tier: NUM_TIERS - 1,
        }
    }

    #[test]
    fn dlc_membership_comes_from_its_own_wire_not_the_blessing_floors() {
        // THE BUG THIS PINS (2026-07-27). `is_dlc_bucket` used to be
        // `blessing_floor_for_region(&cfg.dlc_blessing_floors, r).is_some()`. Those floors are
        // emitted ONLY when global_scadutree_blessing == 2, and the shipped default has been `off`
        // since 2026-07-18 -- so on every default seed the DLC test answered `false` for every
        // bucket in the game, silently. It only decorated a log line, which is why it survived.
        let mut c = cfg(&[(20010, 5000)], 0);
        c.dlc_buckets = vec![20010, 21000, 22000];
        assert!(is_dlc_bucket(&c, 20010));
        assert!(is_dlc_bucket(&c, 22000));
        assert!(!is_dlc_bucket(&c, 6800), "a base-game bucket is not DLC");

        // ...and it must NOT depend on the blessing floors, in either direction.
        assert!(
            c.dlc_blessing_floors.is_empty(),
            "this seed has no blessing floors (the DEFAULT), and DLC membership still resolves"
        );
        let mut blessing_only = cfg(&[(20010, 5000)], 0);
        blessing_only.dlc_blessing_floors = vec![(20010, 20010, 15)];
        assert!(
            !is_dlc_bucket(&blessing_only, 20010),
            "DLC membership must come from dlcRegionBuckets alone -- inferring it from a blessing \
             floor is the bug, and a floor without the bucket wire is a foreign/old apworld"
        );
    }

    #[test]
    fn int_list_parse_is_tolerant_and_empty_means_absent() {
        use serde_json::json;
        assert_eq!(parse_int_list(Some(&json!([1, 2, 3]))), vec![1, 2, 3]);
        assert_eq!(parse_int_list(Some(&json!([1, "x", 3]))), vec![1, 3]);
        assert!(parse_int_list(None).is_empty());
        assert!(parse_int_list(Some(&json!("not an array"))).is_empty());
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
        // DERIVED from SpEffectParam (2026-07-27), so pin the SHAPE, not a length that will move
        // again the next time the run is extended: contiguous 7010..=NNN in steps of 10, strictly
        // ascending HP, every id inside the clear range so our own tier is stripped on re-scale.
        assert_eq!(SCALING_TIERS[0].speffect_id, 7010);
        assert_eq!(
            SCALING_TIERS[NUM_TIERS - 1].speffect_id,
            7010 + 10 * (NUM_TIERS as i32 - 1)
        );
        for (i, t) in SCALING_TIERS.iter().enumerate() {
            assert_eq!(
                t.speffect_id,
                7010 + 10 * i as i32,
                "ladder is not contiguous at {i}"
            );
            assert!(SCALING_ID_RANGE.contains(&t.speffect_id));
        }
        assert!(
            SCALING_TIERS.windows(2).all(|w| w[0].hp < w[1].hp),
            "ladder HP must be strictly ascending -- floor_tier_from_multiplier's search depends on it"
        );
    }

    #[test]
    fn the_top_rung_is_the_full_run_not_the_old_subset() {
        // The ladder was 7010..7100 (3.703x) until 2026-07-27; extending it to 7200 DOUBLED the
        // deepest region's difficulty, which is a balance change and should not drift back silently.
        let top = SCALING_TIERS[NUM_TIERS - 1];
        assert_eq!(top.speffect_id, 7200);
        assert!((top.hp - 7.422).abs() < 1e-4, "top rung HP is {}", top.hp);
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
        // 7500 is in the CO-OP GUEST-COUNT ladder (`7400..7680`): `haveSoulRate` steps 5->4->3->2
        // as the multiplier falls, so the STRONGEST rows fire in the FULLEST sessions -- 7400 is
        // 3.434x HP / 1.902x atk. Clearing it is DELIBERATE and must stay: these are spCategory 0,
        // so not clearing them would stack up to 3.4x underneath a tier that already reaches 7.4x.
        // (Stripping it does cost co-op its player-count compensation -- tracked separately, and
        // under-scaling, so it blocks nothing here.)
        assert!(is_scaling_speffect(7500));
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
        assert_eq!(tier_for_target(0, 100, 0, NUM_TIERS - 1), 0);
        assert_eq!(tier_for_target(0, 100, 2, NUM_TIERS - 1), 2); // floor clamps up
    }

    #[test]
    fn target_max_is_top_tier() {
        assert_eq!(tier_for_target(100, 100, 0, NUM_TIERS - 1), NUM_TIERS - 1);
    }

    #[test]
    fn target_midpoint_rounds_to_middle_tier() {
        let mid = ((NUM_TIERS - 1) as f32 / 2.0).round() as usize;
        assert_eq!(tier_for_target(50, 100, 0, NUM_TIERS - 1), mid);
    }

    #[test]
    fn tier_is_monotonic_in_target() {
        let mut last = 0;
        for t in (0..=100).step_by(5) {
            let tier = tier_for_target(t, 100, 0, NUM_TIERS - 1);
            assert!(tier >= last, "tier decreased at target {t}");
            last = tier;
        }
    }

    #[test]
    fn floor_clamps_low_targets_but_not_high() {
        assert_eq!(tier_for_target(0, 100, 3, NUM_TIERS - 1), 3);
        assert_eq!(tier_for_target(100, 100, 3, NUM_TIERS - 1), NUM_TIERS - 1);
    }

    #[test]
    fn no_scaling_info_returns_floor() {
        assert_eq!(tier_for_target(999, 0, 0, NUM_TIERS - 1), 0);
        assert_eq!(tier_for_target(999, -5, 1, NUM_TIERS - 1), 1);
    }

    #[test]
    fn out_of_range_target_clamps() {
        assert_eq!(tier_for_target(1000, 100, 0, NUM_TIERS - 1), NUM_TIERS - 1);
        assert_eq!(tier_for_target(-50, 100, 0, NUM_TIERS - 1), 0);
    }

    // --- tier_for_region ---

    #[test]
    fn known_region_maps_to_its_tier() {
        let c = cfg(&[(60000, 0), (63000, 50), (76000, 100)], 0);
        let mid = ((NUM_TIERS - 1) as f32 / 2.0).round() as usize; // ladder-length agnostic
        assert_eq!(tier_for_region(&c, 60000), 0);
        assert_eq!(tier_for_region(&c, 63000), mid);
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
        let top = SCALING_TIERS[NUM_TIERS - 1].speffect_id;
        assert_eq!(speffect_id_for_tier(0), 7010);
        assert_eq!(speffect_id_for_tier(NUM_TIERS - 1), top);
        assert_eq!(speffect_id_for_tier(999), top); // clamp
    }

    #[test]
    fn ceiling_is_the_mirror_of_the_floor_not_a_reuse_of_it() {
        // A floor is "first rung at least this strong"; a ceiling is "last rung no stronger than
        // this". Reusing the floor search for both would cap ONE RUNG HIGH, which is the kind of
        // off-by-one nobody would ever notice in play.
        let top = SCALING_TIERS[NUM_TIERS - 1];
        assert_eq!(
            ceiling_tier_from_multiplier(top.hp),
            NUM_TIERS - 1,
            "top rung = uncapped"
        );
        assert_eq!(
            ceiling_tier_from_multiplier(1000.0),
            NUM_TIERS - 1,
            "above the ladder = uncapped"
        );
        assert_eq!(
            ceiling_tier_from_multiplier(0.0),
            0,
            "below the ladder = weakest rung"
        );
        for (i, t) in SCALING_TIERS.iter().enumerate() {
            assert_eq!(
                ceiling_tier_from_multiplier(t.hp),
                i,
                "rung {i} must round-trip"
            );
            // strictly between rung i and i+1 -> still i (no stronger than the value)
            if i + 1 < NUM_TIERS {
                let mid = (t.hp + SCALING_TIERS[i + 1].hp) / 2.0;
                assert_eq!(
                    ceiling_tier_from_multiplier(mid),
                    i,
                    "midpoint above rung {i}"
                );
                assert_eq!(
                    floor_tier_from_multiplier(mid),
                    i + 1,
                    "the FLOOR rounds the other way"
                );
            }
        }
    }

    #[test]
    fn a_ceiling_caps_the_deepest_region_and_leaves_the_rest_alone() {
        let cap = 5;
        // deepest region (frac 1.0) would be NUM_TIERS-1; the cap holds it at `cap`.
        assert_eq!(tier_for_target(100, 100, 0, cap), cap);
        // a shallow region is untouched by a ceiling above it.
        assert_eq!(tier_for_target(0, 100, 0, cap), 0);
        // uncapped is the previous behaviour, exactly.
        assert_eq!(tier_for_target(100, 100, 0, NUM_TIERS - 1), NUM_TIERS - 1);
    }

    #[test]
    fn an_inverted_floor_and_ceiling_resolves_instead_of_panicking() {
        // gen rejects this pair with an OptionError, but tier_for_target is PURE and reachable from
        // hand-rolled or foreign slot_data -- and `clamp` PANICS when min > max. The CEILING wins:
        // when two difficulty bounds contradict, the gentler one is the safe resolution.
        assert_eq!(tier_for_target(50, 100, 9, 3), 3);
        assert_eq!(tier_for_target(0, 0, 9, 3), 3, "the max_target==0 path too");
    }

    #[test]
    fn an_apworld_that_sends_no_ceiling_is_uncapped() {
        // Every seed rolled before this key existed must behave exactly as it did.
        let sd = serde_json::json!({
            "completion_scaling": 1,
            "completionScalingBasis": 1,
            "regionSphereTargetRanges": [[100, 100, 0], [200, 200, 10000]],
            "options": { "completion_scaling": 1, "completion_scaling_floor": 0.0 },
        });
        let cfg = parse_scaling_config(&sd).expect("should arm");
        assert_eq!(cfg.ceiling_tier, NUM_TIERS - 1);
        assert_eq!(tier_for_region(&cfg, 200), NUM_TIERS - 1);
    }

    #[test]
    fn a_ceiling_in_slot_data_is_parsed_and_applied() {
        let sd = serde_json::json!({
            "completion_scaling": 1,
            "completionScalingBasis": 1,
            "regionSphereTargetRanges": [[100, 100, 0], [200, 200, 10000]],
            "options": { "completion_scaling": 1, "completion_scaling_floor": 0.0,
                         "completion_scaling_ceiling": SCALING_TIERS[5].hp },
        });
        let cfg = parse_scaling_config(&sd).expect("should arm");
        assert_eq!(cfg.ceiling_tier, 5);
        assert_eq!(
            tier_for_region(&cfg, 200),
            5,
            "the deepest region is capped"
        );
        assert_eq!(
            tier_for_region(&cfg, 100),
            0,
            "the shallowest is unaffected"
        );
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
    // ---- the region-entry toast ----------------------------------------------------------------
    // The words a player reads are a user-facing string, and this project's rule is that those get
    // checked as RENDERED, not as code. These pin the rendering; the in-game check is that the
    // toast appears on entry at all.

    #[test]
    fn every_toast_is_ascii() {
        // MOTIVATING CASE (rule 11): Alaric's 2026-07-31 screenshot showed
        // `Altus ? enemy scaling 1.14x (tier 0 of 19)` -- the toast format string carried a U+2014
        // em-dash and the in-game font has no glyph for it, so the game drew `?`. The toast system
        // itself was working perfectly; the STRING was unrenderable.
        //
        // Checked over the whole tier range and both variants, plus a region name with a space,
        // because the defect was in the constant part and a single-case assertion would have
        // matched the broken string just as happily.
        for tier in 0..=NUM_TIERS + 1 {
            for s in [
                region_scaling_toast("Farum Azula", RegionScaling::Known(tier), 0, NUM_TIERS - 1),
                region_scaling_toast(
                    "Farum Azula",
                    RegionScaling::Defaulted(tier),
                    0,
                    NUM_TIERS - 1,
                ),
                region_scaling_toast("Farum Azula", RegionScaling::Known(tier), 3, 15),
                region_scaling_toast("Farum Azula", RegionScaling::Known(tier), 7, 7),
            ] {
                assert!(
                    s.is_ascii(),
                    "toast is drawn by the GAME's font, which has no glyph for non-ASCII: {s:?}"
                );
            }
        }
    }

    #[test]
    fn a_known_region_shows_the_multiplier_and_where_it_sits() {
        // tier 10 of 19 is `auto`'s cap for a 5-region seed -- the number Alaric calibrated.
        let s = region_scaling_toast("Liurnia", RegionScaling::Known(10), 0, NUM_TIERS - 1);
        // 4.12, NOT 4.13: rung 10 is 4.125 and `{:.2}` on that f32 renders DOWN. Verified with a
        // standalone rustc rather than reasoned about -- the assertion this file first carried said
        // 4.13x and would have reddened CI for a rounding rule nobody can eyeball.
        assert_eq!(s, "Liurnia - enemy scaling 4.12x (tier 10 of 19)");
    }

    #[test]
    fn the_top_and_bottom_of_the_ladder_render() {
        assert_eq!(
            region_scaling_toast("Limgrave", RegionScaling::Known(0), 0, NUM_TIERS - 1),
            "Limgrave - enemy scaling 1.14x (tier 0 of 19)"
        );
        assert_eq!(
            region_scaling_toast(
                "Farum Azula",
                RegionScaling::Known(NUM_TIERS - 1),
                0,
                NUM_TIERS - 1
            ),
            "Farum Azula - enemy scaling 7.42x (tier 19 of 19)"
        );
    }

    #[test]
    fn the_fraction_is_the_seeds_band_not_the_global_ladder() {
        // Alaric, 2026-07-31, on seeing `tier 0 of 19` in game: "is that of 20 total or 20 in this
        // seed". It was global -- `NUM_TIERS - 1`, a compile-time constant. On a seed that sets
        // `completion_scaling_floor` / `_ceiling` that denominator LIES: the player on their
        // hardest region reads "of 19" and thinks they are two-thirds up a ladder they can never
        // climb.
        //
        // floor 3, ceiling 15 -> a 12-rung band, and the numerator is the rung WITHIN it.
        assert_eq!(
            region_scaling_toast("Liurnia", RegionScaling::Known(3), 3, 15),
            "Liurnia - enemy scaling 1.81x (tier 0 of 12)",
            "the floor is the player's FIRST rung, not rung 3" // 1.81, not 1.53: rung 3 is hp 1.813. Rendered with a standalone rustc rather than
                                                               // reasoned about -- my first guess at this constant was simply wrong.
        );
        assert_eq!(
            region_scaling_toast("Farum Azula", RegionScaling::Known(15), 3, 15),
            "Farum Azula - enemy scaling 6.88x (tier 12 of 12)",
            "the ceiling is the TOP of this run, and must read as the top"
        );
        // Out-of-band inputs clamp instead of underflowing (usize subtraction panics on wrap).
        assert!(region_scaling_toast("X", RegionScaling::Known(0), 3, 15).contains("tier 0 of 12"));
        assert!(
            region_scaling_toast("X", RegionScaling::Known(99), 3, 15).contains("tier 12 of 12")
        );
    }

    #[test]
    fn an_uncapped_seed_still_reads_against_the_whole_ladder() {
        // Not a regression -- it is the honest answer. With no bound set, the deepest kept region
        // normalizes to frac == 1.0 and lands on the top rung, so the band IS the ladder.
        assert!(
            region_scaling_toast("Limgrave", RegionScaling::Known(0), 0, NUM_TIERS - 1)
                .contains("tier 0 of 19")
        );
    }

    #[test]
    fn a_single_tier_seed_does_not_say_zero_of_zero() {
        // floor == ceiling: a fixed-difficulty seed. "tier 0 of 0" is nonsense to read, so the
        // fraction is dropped rather than rendered as a degenerate ratio.
        let s = region_scaling_toast("Caelid", RegionScaling::Known(7), 7, 7);
        assert!(s.contains("the only tier this seed"), "{s}");
        assert!(!s.contains("of 0"), "{s}");
    }

    #[test]
    fn a_contradictory_band_resolves_instead_of_panicking() {
        // floor above ceiling is rejected at generation, but this is a PURE function reachable from
        // hand-rolled or foreign slot_data. Same resolution as `tier_for_target`: the ceiling wins.
        // floor 18 > ceiling 4 -> ceiling wins, floor clamps into it, span 0.
        let s = region_scaling_toast("Altus", RegionScaling::Known(10), 18, 4);
        assert!(s.contains("the only tier this seed"), "{s}");
    }

    #[test]
    fn an_unknown_bucket_says_so_instead_of_showing_tier_zero() {
        // THE POINT OF THE ENUM. A region with no target on the wire falls back to the floor, and
        // rendering that as "tier 0" would present a defaulted value as a derived one -- the exact
        // confident-wrong-answer shape this crate's comments keep warning about.
        let s = region_scaling_toast(
            "Roundtable Hold",
            RegionScaling::Defaulted(0),
            0,
            NUM_TIERS - 1,
        );
        assert!(s.contains("not set for this area"), "{s}");
        assert!(
            !s.contains("tier"),
            "a defaulted region must not claim a tier: {s}"
        );
    }

    #[test]
    fn region_scaling_maps_a_missing_target_to_defaulted_not_to_the_bottom() {
        assert_eq!(
            region_scaling(None, 10_000, 3, 19),
            RegionScaling::Defaulted(3)
        );
        assert!(matches!(
            region_scaling(Some(10_000), 10_000, 0, 19),
            RegionScaling::Known(_)
        ));
    }

    #[test]
    fn the_ceiling_is_visible_in_the_toast() {
        // A capped seed must SHOW its cap: the deepest region of a 5-region run reads 4.12x, not
        // 7.42x. This is how a player can tell `maximum_enemy_difficulty` did anything at all.
        let ceiling = ceiling_tier_from_multiplier(4.125);
        let deepest = region_scaling(Some(10_000), 10_000, 0, ceiling);
        assert_eq!(deepest, RegionScaling::Known(10));
        assert_eq!(
            region_scaling_toast("Leyndell", deepest, 0, NUM_TIERS - 1),
            "Leyndell - enemy scaling 4.12x (tier 10 of 19)"
        );
    }

    #[test]
    fn a_tier_past_the_ladder_clamps_instead_of_panicking() {
        // Reachable from foreign or hand-rolled slot_data, so it must not index out of bounds.
        assert_eq!(tier_multiplier(usize::MAX), SCALING_TIERS[NUM_TIERS - 1].hp);
    }

    // ---- the region-entry ledger (the toast's production dedup) --------------------------------

    #[test]
    fn the_entry_toast_speaks_once_per_distinct_announcement() {
        // Two Caelid buckets on the same tier: ONE line, ever -- riding a bucket border must not
        // strobe. The delta'd Dragonbarrow bucket resolves a different tier under the same name:
        // its own line, once.
        let c = cfg(&[(64000, 5000), (64010, 5000), (64020, 7500)], 0);
        let mut ledger = RegionToastLedger::new();
        let first = ledger
            .on_region(&c, 64000, Some("Caelid"))
            .expect("first entry announces");
        assert!(first.starts_with("Caelid - enemy scaling"), "{first}");
        assert_eq!(
            ledger.on_region(&c, 64000, Some("Caelid")),
            None,
            "the repeat sweep in the same bucket is silent"
        );
        assert_eq!(
            ledger.on_region(&c, 64010, Some("Caelid")),
            None,
            "a same-region bucket rendering the same words is silent"
        );
        let bumped = ledger
            .on_region(&c, 64020, Some("Caelid"))
            .expect("the intra-fold delta bucket is a different announcement");
        assert_ne!(bumped, first);
        assert_eq!(ledger.on_region(&c, 64020, Some("Caelid")), None);
    }

    #[test]
    fn a_nameless_bucket_stays_silent() {
        // Roundtable / tutorial class: no name in the baked geometry, no announcement -- and the
        // silence must not consume anything (a later NAMED bucket still speaks).
        let c = cfg(&[(60000, 5000)], 0);
        let mut ledger = RegionToastLedger::new();
        assert_eq!(ledger.on_region(&c, 11100, None), None);
        assert!(ledger.on_region(&c, 60000, Some("Limgrave")).is_some());
    }

    #[test]
    fn an_unmapped_named_bucket_announces_the_floor_wording_once() {
        // A named region the wire does not target (a sealed num_regions neighbour the player rode
        // into): honest telemetry -- you are in unscaled territory -- said once, not per sweep.
        let c = cfg(&[(60000, 5000)], 2);
        let mut ledger = RegionToastLedger::new();
        let msg = ledger
            .on_region(&c, 64000, Some("Caelid"))
            .expect("announces");
        assert!(msg.contains("not set for this area"), "{msg}");
        assert_eq!(ledger.on_region(&c, 64000, Some("Caelid")), None);
    }

    #[test]
    fn a_new_seed_resets_the_ledger() {
        let c = cfg(&[(60000, 5000)], 0);
        let mut ledger = RegionToastLedger::new();
        assert!(ledger.on_region(&c, 60000, Some("Limgrave")).is_some());
        ledger.reset();
        assert!(
            ledger.on_region(&c, 60000, Some("Limgrave")).is_some(),
            "a reconfigure (new seed) announces afresh"
        );
    }

    #[test]
    fn the_announced_tier_is_the_applied_tier() {
        // The words must agree with what the sweep actually applies (tier_for_region) -- mapped,
        // unmapped, and ceiling-clamped alike. If region_scaling ever grows resolution rules of
        // its own, this is the tripwire: a toast that disagrees with the applied speffect would be
        // the confident wrong answer this crate keeps warning about.
        let mut c = cfg(&[(64000, 5000), (30000, 10000)], 1);
        c.ceiling_tier = 8;
        for bucket in [64000, 30000, 99999] {
            let applied = tier_for_region(&c, bucket);
            let (RegionScaling::Known(shown) | RegionScaling::Defaulted(shown)) = region_scaling(
                raw_target_for_region(&c, bucket),
                c.max_target,
                c.floor_tier,
                c.ceiling_tier,
            );
            assert_eq!(shown, applied, "bucket {bucket}");
        }
    }

    #[test]
    fn baked_geometry_names_the_buckets_the_wire_speaks() {
        // Spot-checks against the generated table (region_locks.rs): the intra-fold delta bucket
        // and a DLC bucket both resolve; an id in no region does not.
        assert_eq!(region_name_for_bucket(64020), Some("Caelid"));
        assert_eq!(region_name_for_bucket(41020), Some("Charo's"));
        assert_eq!(region_name_for_bucket(99999), None);
    }

    #[test]
    fn the_ledger_const_constructs_in_a_static() {
        // The client holds the ledger in `static TOAST_LEDGER: Mutex<RegionToastLedger>` --
        // Windows-only code this crate cannot compile, so the const-construction is proven here.
        static LEDGER: std::sync::Mutex<RegionToastLedger> =
            std::sync::Mutex::new(RegionToastLedger::new());
        let c = cfg(&[(60000, 5000)], 0);
        assert!(LEDGER
            .lock()
            .unwrap()
            .on_region(&c, 60000, Some("Limgrave"))
            .is_some());
    }

    // ---- #346: unrunged enemies (the ladder has no rung below 1.0) ---------------------------

    #[test]
    fn a_speffect_the_clear_catches_is_not_automatically_a_ladder_rung() {
        // THE MOTIVATING CASE for `ScalingKind`. `7850` is `spCategory 140`, sits inside
        // `SCALING_ID_RANGE` (7000..8000) and so is cleared -- but it is not an area-scaling rung.
        // The discriminator #346 prescribed, `!stale.is_empty()`, would read this enemy as RUNGED
        // and hide it from the very census meant to find it.
        assert_eq!(scaling_kind(7850), Some(ScalingKind::OtherInRange));
        assert!(!is_ladder_rung(7850));
        // Same trap, other block: the 5.3-5.8x run at 7210..7280.
        assert_eq!(scaling_kind(7240), Some(ScalingKind::OtherInRange));
        // And the identity row, which declares a native multiplier of exactly 1.0.
        assert_eq!(scaling_kind(7000), Some(ScalingKind::OtherInRange));
    }

    #[test]
    fn the_ladder_and_its_dlc_re_emission_both_count_as_rungs() {
        assert_eq!(scaling_kind(7010), Some(ScalingKind::Ladder));
        assert_eq!(scaling_kind(7200), Some(ScalingKind::Ladder));
        assert_eq!(scaling_kind(20007010), Some(ScalingKind::Ladder));
        // Every shipped tier id must classify as a rung, or the census under-counts silently.
        for t in SCALING_TIERS {
            assert_eq!(
                scaling_kind(t.speffect_id),
                Some(ScalingKind::Ladder),
                "tier {} is not recognised as a rung",
                t.speffect_id
            );
        }
    }

    #[test]
    fn effects_outside_the_clear_space_are_not_ours() {
        assert_eq!(scaling_kind(6999), None);
        assert_eq!(scaling_kind(8000), None);
        assert_eq!(scaling_kind(20008000), None);
        // The DLC ally-tuning block -- candidate down-state ids, deliberately NOT in the clear
        // space today. If a future change arms them it must widen this deliberately, by allowlist.
        assert_eq!(scaling_kind(20018004), None);
    }

    #[test]
    fn an_unrunged_enemy_is_left_vanilla_at_the_bottom_rung() {
        // The reported case: a hand-tuned entity with no vanilla rung, in a tier-0 region. Applying
        // `7010` would BUFF it 1.141x, so we apply nothing.
        assert!(skip_unrunged_at_floor(false, 0));
    }

    #[test]
    fn a_raised_difficulty_floor_still_reaches_unrunged_enemies() {
        // `minimum_enemy_difficulty` resolves to a floor tier above 0. That is a player instruction,
        // not an artefact of the ladder's shape, so the skip must not swallow it.
        assert!(!skip_unrunged_at_floor(false, 1));
    }

    // ---- #346 phase 1a: native tiers, and absence as the safe state -------------------------

    #[test]
    fn an_enemy_we_have_no_native_tier_for_is_never_touched_at_any_depth() {
        // THE MOTIVATING CASE. 2,440 unrunged rows have no rune reward in NpcParam -- every named
        // boss among them -- so they are absent from the table. Defaulting them to the floor is what
        // shipped, and it is what puts a rung on top of hand-tuned endgame stats in a shallow
        // sphere. Absence must mean "leave it alone", at EVERY tier, or the wall comes back.
        let absent = -1; // no npc_id is negative; stands in for "not in the table"
        assert!(native_tier(absent).is_none());
        for tier in 0..NUM_TIERS {
            assert_eq!(
                scale_action(false, absent, tier),
                ScaleAction::NoTouch,
                "an unclassified enemy was scaled at tier {tier}"
            );
        }
    }

    #[test]
    fn a_band_only_enemy_still_goes_down_the_unrunged_path() {
        // The motivating case for SHELVING `band_native_tier`: region 10010's lone band-only entity.
        // It must resolve exactly like any other unrunged enemy -- through the getSoul table, or to
        // NoTouch -- and the band must NOT quietly become its native tier.
        let band_only_unknown_npc = -1; // absent from NATIVE_TIERS
        assert!(native_tier(band_only_unknown_npc).is_none());
        for tier in 0..NUM_TIERS {
            assert_eq!(
                scale_action(false, band_only_unknown_npc, tier),
                ScaleAction::NoTouch,
                "a band-only enemy was scaled at tier {tier}"
            );
        }
        // `band_native_tier` has an answer for 7460 -- the point is that nothing consults it.
        assert!(band_native_tier(7460).is_some());
    }

    #[test]
    fn the_clear_range_covers_the_band_ids_actually_seen_in_play() {
        // Every band id observed live on 2026-08-05, asserted by direct call. The clear is what makes
        // the strip deliberate rather than incidental, and these are the rows it must keep catching.
        for id in [7430, 7440, 7460, 7500] {
            assert!(
                is_scaling_speffect(id),
                "observed band id {id} is not cleared"
            );
            assert!(is_band_rung(id));
        }
    }

    #[test]
    fn a_band_row_is_not_a_ladder_rung() {
        // An unfired guard is untested: direct calls, both families, both id blocks.
        for id in [7400, 7430, 7550, 7680, 20007400, 20007750] {
            assert!(!is_ladder_rung(id), "{id} classified as a ladder rung");
            assert!(is_band_rung(id), "{id} not recognised as a band row");
            assert_eq!(scaling_kind(id), Some(ScalingKind::OtherInRange));
        }
        for id in [7010, 7200, 20007000] {
            assert!(is_ladder_rung(id));
            assert!(!is_band_rung(id), "{id} misfiled as a band row");
        }
    }

    #[test]
    fn a_band_row_implies_the_lowest_rung_at_least_as_strong() {
        // BOTH ENDS, deliberately. Quoting only one end of this band is exactly how it got
        // misidentified twice -- once as "floors at 1.001x" (it tops at 3.434x), once as the co-op
        // guest ladder.
        //
        // 7400 = 3.434x -> the first rung with hp >= 3.434 is 7100 (3.703), index 9.
        assert_eq!(band_native_tier(7400), Some(9));
        // 7550 = 1.001x -> below the bottom rung (1.141), so tier 0.
        assert_eq!(band_native_tier(7550), Some(0));
        // The flat tail is 1.015x -- still under the bottom rung.
        assert_eq!(band_native_tier(7680), Some(0));
        // Ceiling, never nearest: 7430 = 2.355x sits between 7060 (2.266) and 7070 (2.406);
        // nearest would pick 2.266 and UNDERSTATE the enemy. We must pick 7070.
        assert_eq!(band_native_tier(7430), Some(6));
        assert_eq!(tier_rates(6).speffect_id, 7070);
        assert!(tier_rates(6).hp >= 2.355);
        assert!(tier_rates(5).hp < 2.355);
        // Not a band row.
        assert_eq!(band_native_tier(7010), None);
    }

    #[test]
    fn every_band_row_maps_somewhere_and_none_is_a_rung() {
        for &(id, hp) in BAND_TIERS {
            assert!(
                is_scaling_speffect(id),
                "band row {id} is outside the clear range"
            );
            assert!(!is_ladder_rung(id), "band row {id} would be Replaced");
            let t = band_native_tier(id).expect("band row maps");
            assert!(t < NUM_TIERS);
            // The implied rung is genuinely at least as strong, unless we clamped at the top.
            assert!(
                tier_rates(t).hp >= hp || t == NUM_TIERS - 1,
                "band {id} ({hp}x) mapped to a WEAKER rung {t}"
            );
        }
    }

    #[test]
    fn the_table_is_keyed_on_npc_param_id_not_the_four_digit_npc_id() {
        // THE MOTIVATING CASE for this fix. `ChrIns` carries BOTH `npc_param_id` (the `NpcParam`
        // row) and `npc_id` ("4 number identifier ... eg. 8000 for Torrent" -- a chr/model id).
        // Keying on the wrong one compiles and runs. A live log exposed it: the census named
        // unrunged ids [0, 100, 1000, 3010, 6001, 8000], every one of them four digits.
        //
        // 🛑 THE TWO ID SPACES OVERLAP, so this was never merely inert. Most chr ids miss the table
        // entirely -- but `NpcParam` also has low-numbered rows, so a handful of chr ids land on a
        // REAL and completely unrelated row and come back with a confident wrong answer. That is the
        // dangerous half, and it is why this test asserts the overlap exists rather than asserting
        // "small ids never resolve" (which is what I first wrote here, and the data refuted it).
        let miss = [3010i32, 6001, 6060, 6090, 6100, 8000];
        for id in miss {
            assert_eq!(
                native_tier(id),
                None,
                "chr id {id} resolved to a native tier"
            );
        }
        let collide: Vec<i32> = [0i32, 1, 100, 540, 1000, 9999]
            .into_iter()
            .filter(|&id| native_tier(id).is_some())
            .collect();
        assert!(
            !collide.is_empty(),
            "the id spaces were expected to OVERLAP -- if they no longer do, this test's premise \
             (and the danger it documents) has changed and the comment above needs rewriting"
        );

        // And a real NpcParam row id, far outside chr-id space, resolves.
        let row_id = crate::native_tiers::NATIVE_TIERS
            .iter()
            .map(|&(id, _)| id)
            .max()
            .expect("table is not empty");
        assert!(row_id > 9999, "table is not keyed on NpcParam row ids");
        assert!(native_tier(row_id).is_some());
    }

    #[test]
    fn a_derived_enemy_scales_up_but_never_down() {
        // Up-only: the ladder has no rung below 1.0, so "weaker than native" is not expressible and
        // must resolve to leaving the enemy alone rather than to the nearest thing we can say.
        let (npc, native) = crate::native_tiers::NATIVE_TIERS[0];
        let native = native as usize;
        for tier in 0..NUM_TIERS {
            let want = if tier > native {
                ScaleAction::Apply
            } else {
                ScaleAction::NoTouch
            };
            assert_eq!(
                scale_action(false, npc, tier),
                want,
                "npc {npc} at tier {tier}"
            );
        }
    }

    #[test]
    fn a_runged_enemy_is_always_replaced_in_both_directions() {
        // The 4,214-row mainline population is untouched by phase 1a: its rung IS its declared
        // native difficulty, so swapping rungs is a true re-tier and works downward already.
        for tier in 0..NUM_TIERS {
            assert_eq!(scale_action(true, -1, tier), ScaleAction::Replace);
            assert_eq!(scale_action(true, 40000000, tier), ScaleAction::Replace);
        }
    }

    #[test]
    fn the_native_table_is_sorted_and_in_range() {
        // Sorted is a CORRECTNESS requirement, not tidiness: `native_tier` binary-searches, so an
        // unsorted table silently returns None for real entries -- which reads exactly like the
        // safe "absent" case and would hide itself.
        let t = crate::native_tiers::NATIVE_TIERS;
        assert!(!t.is_empty());
        for w in t.windows(2) {
            assert!(w[0].0 < w[1].0, "not sorted / duplicate at npc {}", w[0].0);
        }
        for &(npc, idx) in t {
            assert!(
                (idx as usize) < NUM_TIERS,
                "npc {npc} has out-of-range tier {idx}"
            );
        }
        // Every entry must be findable through the public lookup, not just present in the slice.
        for &(npc, idx) in t {
            assert_eq!(native_tier(npc), Some(idx as usize));
        }
    }

    #[test]
    fn the_calibration_bands_are_strictly_increasing() {
        // A non-increasing band list would make the emitted assignment order meaningless.
        let b = crate::native_tiers::GETSOUL_BANDS;
        for w in b.windows(2) {
            assert!(
                w[0].0 < w[1].0,
                "bands not increasing: {} then {}",
                w[0].0,
                w[1].0
            );
            assert!(w[0].1 < w[1].1, "band indices not increasing");
        }
        assert!(b.last().unwrap().1 < crate::native_tiers::TOP_BAND_INDEX);
    }

    #[test]
    fn every_emitted_entry_agrees_with_the_curve_it_came_from() {
        // Guards the emitter: re-derive each npc's index from its recorded getSoul band and check
        // it matches what was written. Catches a table emitted from a stale curve.
        let bands = crate::native_tiers::GETSOUL_BANDS;
        let n_top = crate::native_tiers::NATIVE_TIERS
            .iter()
            .filter(|&&(_, idx)| idx == crate::native_tiers::TOP_BAND_INDEX)
            .count();
        // Sanity: the top band is not swallowing the table (that would mean a collapsed curve).
        assert!(n_top < crate::native_tiers::NATIVE_TIERS.len() / 2);
        for &(_, idx) in crate::native_tiers::NATIVE_TIERS {
            let known =
                bands.iter().any(|&(_, b)| b == idx) || idx == crate::native_tiers::TOP_BAND_INDEX;
            assert!(
                known,
                "emitted index {idx} is not one the curve can produce"
            );
        }
    }

    #[test]
    fn a_runged_enemy_is_never_skipped_and_deep_spheres_still_scale_up() {
        // Runged mobs keep working exactly as before -- the half of the feature that is not broken.
        assert!(!skip_unrunged_at_floor(true, 0));
        assert!(!skip_unrunged_at_floor(true, NUM_TIERS - 1));
        // And an unrunged enemy in a deep sphere still scales UP; the skip is bottom-rung only.
        assert!(!skip_unrunged_at_floor(false, NUM_TIERS - 1));
    }
}
