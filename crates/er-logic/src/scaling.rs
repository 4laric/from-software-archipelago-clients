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
    /// One of OUR down-state rows (`DOWNSTATE_IDS`). Not vanilla's, not in either range: it is
    /// there because a previous sweep put it there, so it is ours to clear and re-derive.
    Downstate,
}

/// Classify one carried `param_id`. `None` = not ours to clear.
pub fn scaling_kind(param_id: i32) -> Option<ScalingKind> {
    if is_ladder_rung(param_id) {
        Some(ScalingKind::Ladder)
    } else if is_downstate_id(param_id) {
        Some(ScalingKind::Downstate)
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

/// The ladder index a carried BASE-GAME rung stands for, or `None`. The inverse of
/// `speffect_id_for_tier`.
///
/// 🛑 BASE GAME ONLY, ON PURPOSE. The DLC re-emission (`20007000..20007350`) is a far STEEPER curve
/// at the same indices -- `20007010` is 7.84x where base index 0 is 1.141x -- so folding a DLC rung
/// into a base-indexed statistic would silently overstate the area it was read from. A DLC rung is
/// not a sample; a DLC region simply reports a smaller one.
pub fn ladder_tier(param_id: i32) -> Option<usize> {
    SCALING_TIERS.iter().position(|t| t.speffect_id == param_id)
}

/// Fewest vanilla-shaped neighbours that may stand in for an AREA.
pub const MIN_AREA_SAMPLE: u32 = 8;

/// The area's own declared difficulty, read off the enemies vanilla placed in it (#346, phase 1b).
///
/// ⭐⭐⭐ THE SIGNAL IS THE NEIGHBOURS, because the enemy we care about has nothing to read. An
/// unrunged, reward-less enemy -- Vyke, Gideon, every hand-tuned NPC -- is absent from `NATIVE_TIERS`
/// by construction (it is derived from `getSoul`, which they do not have) and carries no band of its
/// own. Nothing ON such an enemy declares a strength. What vanilla DOES declare is how hard it
/// considered the ground the enemy is standing on.
///
/// 🛑🛑 IT MUST BE FED ONLY VANILLA-SHAPED ENEMIES, AND THAT IS THE WHOLE TRAP. We REPLACE rungs, so
/// from a region's second sweep onward the enemies around us carry OUR target rather than vanilla's.
/// A statistic over them is a statistic over our own output: it converges on the tier we already
/// chose and looks beautifully stable while measuring nothing at all. The discriminator is the BAND
/// -- we strip it, and vanilla pairs it with a rung essentially always (band+rung was 198 of 198 and
/// 153 of 153 in the 2026-08-05 census) -- so an enemy carrying BOTH is one we have not yet
/// processed, and only those may be counted. See the same failure in a different costume:
/// a generator whose input contains its own output.
///
/// Weighted MEDIAN, not mean: an area is allowed to contain one outlier without the outlier becoming
/// the area -- which is exactly the Vyke case, an endgame invader parked in a mid-game field.
/// Returns `None` below `MIN_AREA_SAMPLE`: a handful of enemies inside a fog gate is not an area, and
/// declining to answer is far cheaper than answering wrongly, because the answer's whole purpose is
/// to license touching something we currently leave alone.
pub fn area_tier_from_histogram(hist: &[u32]) -> Option<usize> {
    let total: u32 = hist.iter().sum();
    if total < MIN_AREA_SAMPLE {
        return None;
    }
    let half = total / 2;
    let mut seen = 0u32;
    for (idx, &n) in hist.iter().enumerate() {
        seen += n;
        if seen > half {
            return Some(idx);
        }
    }
    None
}

/// The phase-1b DOWN-STATE rows: the only rows in the game that can scale an enemy BELOW vanilla.
///
/// SWEPT 2026-08-06 across all 11,325 `SpEffectParam` rows — every row with `maxHpRate < 1` or
/// `physicsAttackPowerRate < 1`, each full-column-diffed against the identity row `7000`. Usable
/// means `effectEndurance -1` (infinite), `conditionHp -1` (unconditional), `spCategory 0` (so they
/// STACK), no `vfxId`/`iconId`/`stateInfo`/`cycleOccurrenceSpEffectId`, and the `effectTarget*`
/// flags left at 1.
///
/// | id | attack | HP | non-default diffs vs `7000` |
/// |---|---|---|---|
/// | `20018008` | 0.70 | 1.00 | 5 — the attack rates, and nothing else |
/// | `20018027` | 0.45 | 3.00 | 6 — the attack rates, and nothing else |
/// | `20018002` | 0.30 | 1.00 | 6 — the rates, plus `targetPriority 0 -> -0.5` |
/// | `20018004` | 1.00 | 0.25 | 3 — `maxHpRate`, plus `targetPriority 0 -> 1` |
///
/// ⭐ `20018027`'s 3x HP is not a defect, it is the CANCELLER: 3.0 x 0.25 = 0.75, so pairing it with
/// `20018004` lands a near-neutral HP while halving the attack gap the original three-state design
/// left open (it jumped 0.70 -> 0.30 on the axis this phase buckets by).
///
/// ⭐⭐⭐ `20018004` IS THE ONLY CLEAN HP-DOWN ROW IN THE GAME, and that is now verified rather than
/// assumed. The other 19 sub-1.0 `maxHpRate` rows are all disqualified, and the reasons are recorded
/// so nobody re-derives them: `1420` (0.5x) zeroes all seven `effectTarget*` flags AND doubles
/// `defEnemyDmgCorrectRate`; `330800`/`500135`/`6083000`/`6083200`/`6160000` are player talismans
/// (`iconId` + `addXStatus` + `effectTargetEnemy 0`); the rest are timed (`effectEndurance` 10-120)
/// or a different `spCategory`.
///
/// 🛑 TWO ATTACK ROWS THAT LOOK USABLE AND ARE NOT. `18684`/`18685` (attack **0.0**) zero every
/// `effectTarget*` flag and every `*DamageCutRate` — a total nullifier that can apply to nothing.
/// `6109000` (0.95x) is a talisman: `iconId`, `cycleOccurrenceSpEffectId`, `addLuckStatus 8`,
/// `effectTargetEnemy 0`. `19389` (0.35x) rides a `maxHpRate 2`. Don't reach for any of them.
pub const DOWNSTATE_IDS: [i32; 4] = [20018002, 20018004, 20018008, 20018027];

/// One expressible DOWN state: the ids applied together, and what they compose to.
///
/// `spCategory 0` rows stack MULTIPLICATIVELY — the same property that forces the sweep to clear the
/// `70xx` before applying its own rung — so a subset of `DOWNSTATE_IDS` is simply a product.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DownState {
    /// Applied together; cleared as a SET. Sorted, so `settled_on_downstate` can compare cheaply.
    pub ids: &'static [i32],
    /// Composed all-element attack multiplier. ⭐ THE PRIMARY AXIS — see `down_state_for`.
    pub attack: f32,
    /// Composed max-HP multiplier. Secondary: HP governs how long a fight takes, not whether you
    /// get past it, and a slog is a blemish where a wall is a progression stop.
    pub hp: f32,
}

/// Every state the four rows compose to that is actually usable, descending by attack.
///
/// 🛑 TWO FAMILIES ARE DELIBERATELY ABSENT. Subsets containing `20018027` WITHOUT `20018004` carry
/// its 3x HP uncancelled — those are BUFFS, and offering them here would let a down decision make an
/// enemy tankier. And `{20018004}` alone is attack-neutral, so on the primary axis it is a no-op;
/// the honest answer in that case is `None`, not a state that does nothing to the wall.
pub const DOWN_STATES: &[DownState] = &[
    DownState {
        ids: &[20018008],
        attack: 0.7,
        hp: 1.0,
    },
    DownState {
        ids: &[20018004, 20018008],
        attack: 0.7,
        hp: 0.25,
    },
    DownState {
        ids: &[20018004, 20018027],
        attack: 0.45,
        hp: 0.75,
    },
    DownState {
        ids: &[20018004, 20018008, 20018027],
        attack: 0.315,
        hp: 0.75,
    },
    DownState {
        ids: &[20018002],
        attack: 0.3,
        hp: 1.0,
    },
    DownState {
        ids: &[20018002, 20018004],
        attack: 0.3,
        hp: 0.25,
    },
    DownState {
        ids: &[20018002, 20018008],
        attack: 0.21,
        hp: 1.0,
    },
    DownState {
        ids: &[20018002, 20018004, 20018008],
        attack: 0.21,
        hp: 0.25,
    },
    DownState {
        ids: &[20018002, 20018004, 20018027],
        attack: 0.135,
        hp: 0.75,
    },
    DownState {
        ids: &[20018002, 20018004, 20018008, 20018027],
        attack: 0.0945,
        hp: 0.75,
    },
];

/// Leave the enemy vanilla when the ladder's own step between the two tiers is weaker than this.
///
/// ⭐⭐⭐ THE COARSEST TOOL WE HAVE IS 0.70x, AND THAT IS THE WHOLE REASON THIS CONSTANT EXISTS.
/// Anything the ladder wants between 0.70 and 1.0 has no expressible answer: we either overshoot or
/// do nothing. At 0.90 the 34 pairs whose ladder step is under 10% are left alone — firing a 30%
/// nerf at a 5% problem is not a fix — while everything the ladder treats as a real difference still
/// moves, INCLUDING the motivating case.
///
/// 🛑 DO NOT RAISE IT TO 0.85 WITHOUT RE-READING `altus_at_sphere_zero_is_scaled_down`. At 0.85 the
/// Altus case (ground 7, target 5, want 0.879) falls into the deadband and this whole phase no-ops
/// the defect it was built for. That is not a tuning preference, it is the acceptance test.
///
/// Alaric's call, 2026-08-06, after being shown 1.0 / 0.95 / 0.90 / 0.85 side by side.
pub const DOWN_DEADBAND: f32 = 0.90;

/// How far ABOVE the ladder's asked-for ratio a state may still be chosen.
///
/// 🔥 CAUGHT IN THE FIRST LIVE LOG (2026-08-06). `523210014`, native tier 9 at target 0, wanted
/// 0.444 with a 0.45 state sitting right there — and a strict `<=` pushed it down a whole lattice
/// step to 0.315. **A 30% extra cut for a 1.4% miss.** The table has a worse case: native 10 -> 1
/// wants **0.450 exactly** and is still refused 0.45, because these are f32 ratios of f32 rates and
/// exact equality was never going to hold.
///
/// ⭐ 2% fixes 7 of the 156 acting pairs and moves median overshoot 1.214 -> 1.177. The acted count
/// does NOT change, so it buys precision at zero coverage cost — the deadband still decides whether
/// we act at all, and this only decides which state we reach for once we have.
pub const DOWN_TOLERANCE: f32 = 0.02;

/// Which down state moves an enemy sitting at `native` to look like one at `target`.
///
/// ⭐⭐⭐ BUCKET BY ATTACK, NOT BY HP. Every reported death in the 2026-08-04/05 Nexus thread is
/// damage TAKEN, and boblerrr is the control ("didn't seem too bad, but I also didn't get hit by
/// them"). Attack is the axis a player hits a wall on; HP only sets how long the fight runs.
///
/// The target ratio is the LADDER'S OWN, `attack(target) / attack(native)` — not a ratio anyone
/// argued for. Round DOWN on attack (never leave it stronger than the ladder asked), break ties on
/// HP, then on fewest ids.
///
/// 🛑 THIS IS RELATIVE WHERE `Apply` IS ABSOLUTE, AND THE ASYMMETRY IS DELIBERATE. `Apply` puts the
/// target RUNG on an enemy whose base already encodes its native strength, so it multiplies on top —
/// the known hazard documented on `presumed_native_tier`. A down state cannot work that way: there
/// is no rung to replace, so the only correct move is the RATIO between the two tiers. Fixing
/// `Apply` to match is a separate change and is not attempted here.
pub fn down_state_for(native: usize, target: usize) -> Option<&'static DownState> {
    if target >= native {
        return None;
    }
    let n = SCALING_TIERS.get(native)?;
    let t = SCALING_TIERS.get(target)?;
    let want_attack = t.attack / n.attack;
    if want_attack > DOWN_DEADBAND {
        return None; // the ladder's step is smaller than anything we can express
    }
    let want_hp = t.hp / n.hp;
    // Strongest state that does not UNDER-reduce; if the gap outruns the lattice, its floor.
    let attack = DOWN_STATES
        .iter()
        .map(|s| s.attack)
        .filter(|&a| a <= want_attack * (1.0 + DOWN_TOLERANCE))
        .fold(f32::NEG_INFINITY, f32::max);
    let attack = if attack.is_finite() {
        attack
    } else {
        DOWN_STATES.last()?.attack
    };
    DOWN_STATES
        .iter()
        .filter(|s| s.attack == attack)
        .min_by(|a, b| {
            (a.hp - want_hp)
                .abs()
                .partial_cmp(&(b.hp - want_hp).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.ids.len().cmp(&b.ids.len()))
        })
}

/// Is this enemy ALREADY carrying exactly `state` and nothing else in the clear space?
///
/// 🛑 SET EQUALITY, for the same reason `settled_on_target` is. A down state that re-applied every
/// sweep would churn the enemy's effect list forever and never converge, and the census would show
/// it being "scaled" on every pass — which is exactly how the `residue 306` bug read.
pub fn settled_on_downstate(carried: &[i32], state: &DownState) -> bool {
    carried.len() == state.ids.len() && state.ids.iter().all(|id| carried.contains(id))
}

/// Whether `param_id` is one of the down-state rows.
pub fn is_downstate_id(param_id: i32) -> bool {
    DOWNSTATE_IDS.contains(&param_id)
}

/// What `is_scaling_speffect` must become when phase 1b arms: the ranges, OR the explicit pair.
///
/// 🛑 AN ALLOWLIST, NEVER A WIDENED RANGE. `20018xxx` is the DLC's ally-tuning block; widening
/// `DLC_SCALING_ID_RANGE` to swallow it would strip legitimate effects off DLC summons. Written and
/// tested now so that arming 1b is a one-line swap against a predicate that already has direct-call
/// coverage, rather than a new predicate written under time pressure.
pub fn is_scaling_speffect_with_downstates(param_id: i32) -> bool {
    is_scaling_speffect(param_id) || is_downstate_id(param_id)
}

/// Is this enemy ALREADY in the state the sweep wants, i.e. carrying `target` and nothing else in
/// the clear space?
///
/// 🛑 THE `AND NOTHING ELSE` IS THE WHOLE POINT, and it was missing. `scale_one` used to return as
/// soon as the enemy *contained* the target, before the clear -- so an enemy carrying the target rung
/// PLUS anything else in `7000..8000` kept the extra forever, because every later sweep took the same
/// short-circuit.
///
/// It hid because it is invisible at floor 0. The target was `7010`, which almost nothing carries
/// natively, so the census read `residue 0`. Raising the default floor to tier 5 made the target
/// `7060` -- exactly what Liurnia's enemies carry natively -- and **306 of them** short-circuited in
/// one sweep, keeping their band: `7060 x 7460` = 2.266 x 1.845 = **4.18x** against the 2.266x every
/// peer on the same tier got. Not a stacking bug in the applier; a convergence bug in the skip.
///
/// The invariant `scale_one` documents is "carries EXACTLY `target`". This is that sentence, executable.
pub fn settled_on_target(carried: &[i32], target: i32) -> bool {
    !carried.is_empty() && carried.iter().all(|&id| id == target)
}

/// What the sweep should do with one enemy (#346, phases 1a and 1b).
///
/// 🛑 `Eq` IS GONE ON PURPOSE: `Down` carries f32 rates. Nothing needs total equality here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScaleAction {
    /// It carries a vanilla ladder rung, so the rung IS its declared native difficulty and swapping
    /// one rung for another is a true re-tier. Clear and apply, in both directions. This is the
    /// 4,214-row mainline population and its behaviour is exactly what shipped.
    Replace,
    /// No vanilla rung, but we have a defensible native tier for it, and the target is STRONGER.
    Apply,
    /// No vanilla rung, we have a defensible native tier, and the target is WEAKER. Clear, then
    /// apply the down state — and NOT the region's rung, which would multiply on top of a base that
    /// already encodes the native tier. Phase 1b.
    Down(&'static DownState),
    /// It already carries a down state from an earlier sweep and we can no longer say what its
    /// native tier is, so the state stands. Behaviourally identical to `NoTouch` — nothing is
    /// mutated — but it is a DIFFERENT FACT and the census must not conflate them.
    ///
    /// 🛑🛑 THIS EXISTS BECAUSE THE DOWN PATH HAS NO ANCHOR AND THE UP PATH DOES. An enemy the sweep
    /// scales UP carries a ladder RUNG afterwards, so the next sweep reads it as runged and
    /// `Replace` re-derives it forever. A down state is not a rung, so the next sweep re-derives
    /// from `presumed_native_tier` — which, on a converged region, answers `None`: the area
    /// histogram counts only rung-AND-band carriers and our own sweep stripped them, BY DESIGN.
    ///
    /// The first live log (2026-08-06, region 0) shows it exactly: `down-scaled 23 (settled 0)`,
    /// then `down-scaled 0 (settled 5)` with `left vanilla` 19 -> 37. **18 + 5 = 23 and 19 + 18 =
    /// 37.** The 18 area-placed enemies kept their state only because `NoTouch` happens to return
    /// before the clear. That is correct behaviour resting on an accident, and it reported itself as
    /// "left vanilla" — the opposite of what happened.
    KeepDown,
    /// It carries a down state that is no longer the right answer, and there is no new state to put
    /// in its place. Strip ours and apply nothing.
    ///
    /// ⭐ SAFE TO STRIP WITHOUT REPLACING, unlike every other path in this file. The usual rule —
    /// never clear an enemy and then decline to re-apply — protects VANILLA state, whose loss is
    /// irreversible because the sweep re-derives everything from what the enemy currently carries.
    /// A down state is additive and ours; removing it restores exactly what vanilla shipped.
    ///
    /// 🛑 WITHOUT THIS A DOWN STATE IS UNREMOVABLE. `down_state_for` returns `None` for 34 of the
    /// 190 tier pairs (the deadband), so an enemy placed at one target and re-swept at a target only
    /// slightly below its native tier would otherwise keep a state that is now far too strong a cut.
    ClearDown,
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
/// ⭐ BOTH DIRECTIONS, since 2026-08-06. The ladder still has no rung below 1.0 — `Apply` fires only
/// when `target_tier` exceeds the native one — but the down half is now expressible off-label
/// through `DOWN_STATES`, so a derived entity standing ABOVE its region's target is brought down
/// rather than left alone. See `down_state_for` for why that path is RELATIVE and `Apply` absolute.
pub fn scale_action(
    carried_ladder_rung: bool,
    carried_downstate: bool,
    npc_param_id: i32,
    target_tier: usize,
    area_tier: Option<usize>,
) -> ScaleAction {
    if carried_ladder_rung {
        return ScaleAction::Replace;
    }
    // ⭐ TWO ATTRIBUTIONS, BECAUSE THE CARVE-OUT IS DIRECTIONAL. `up` is what we are willing to
    // claim when the move would strengthen the enemy (conservative — `AREA_EXCLUDED` applies);
    // `down` is what we claim when it would weaken it (the area vouches for anyone). They differ
    // only for a named, unrewarded character with no rune reward, which is exactly the hand-tuned
    // class this whole issue is about.
    let up = presumed_native_tier(npc_param_id, area_tier);
    let down = presumed_native_tier_down(npc_param_id, area_tier);
    match up {
        Some(native) if target_tier > native => return ScaleAction::Apply,
        _ => {}
    }
    match down {
        // 🛑 THE DOWN HALF, ARMED 2026-08-06. Ground ABOVE target used to fall to `NoTouch` and the
        // comment below said so; that is the Altus reading (target 5, area index 7, 61 unrunged left
        // at full Altus strength beside neighbours normalised to 2.266x). `None` here still means
        // NoTouch — the deadband, or a tier pair the lattice cannot express.
        Some(native) if native > target_tier => match down_state_for(native, target_tier) {
            Some(state) => ScaleAction::Down(state),
            // In the deadband: no cut is warranted, so a state carried from an earlier sweep is now
            // wrong and must come off.
            None if carried_downstate => ScaleAction::ClearDown,
            None => ScaleAction::NoTouch,
        },
        // Native EQUALS the target: it is exactly where it should be, so any state we put on it
        // earlier is now a pure nerf.
        Some(_) if carried_downstate => ScaleAction::ClearDown,
        // 🛑 WE CANNOT PLACE IT *AND* IT ALREADY CARRIES ONE OF OUR STATES. Re-deriving is not an
        // option, and stripping would be worse than either: the state is the only surviving record
        // that this enemy was placed, so clearing it would silently restore full vanilla strength in
        // a region we already decided was too strong. Classify from what it CARRIES.
        None if carried_downstate => ScaleAction::KeepDown,
        _ => ScaleAction::NoTouch,
    }
}

/// The strength we are willing to attribute to an enemy that carries no rung: its own `getSoul`
/// tier if it has one, otherwise **the ground it is standing on**.
///
/// ⭐⭐⭐ ONE LEVEL PER REGION IS THE GOAL, and the area index is what makes the leftovers reachable.
/// The 2026-08-05 Weeping census is the case: target tier 11, area index 1, **523 enemies at the tier
/// and 69 left at vanilla in the same field**. Those 69 have no rung and no rune reward, so
/// `native_tier` cannot place them and they sat out the region's difficulty entirely. The area index
/// places them the way it places anything else vanilla put on that ground.
///
/// 🛑🛑 THIS ATTRIBUTES A STRENGTH WE DID NOT MEASURE ON THE ENEMY ITSELF, and for a hand-tuned boss
/// that attribution is wrong in the dangerous direction: its base already assumes the end of the
/// game, so a delta computed from the ground multiplies on top of endgame stats. That is the shape of
/// the v0.3.4 one-shotting reports. Shipped deliberately and without a boss carve-out (Alaric,
/// 2026-08-05: "all as a class"), with the sweep NAMING what it moves this way so the first log says
/// which entities were picked up — rather than a player saying it.
///
/// 🛑 `None` area tier keeps the old behaviour exactly: too few untouched neighbours to say, so
/// nothing is attributed and the enemy stays vanilla.
pub fn presumed_native_tier(npc_param_id: i32, area_tier: Option<usize>) -> Option<usize> {
    native_tier(npc_param_id).or_else(|| area_tier.filter(|_| area_may_vouch_for(npc_param_id)))
}

/// The same attribution, for a move that would make the enemy **WEAKER** — and here the area may
/// vouch for ANYONE, including the named, unrewarded characters `AREA_EXCLUDED` refuses upward.
///
/// ⭐⭐⭐ THE CARVE-OUT WAS DIRECTION-BLIND, AND ITS OWN JUSTIFICATION IS NOT. `area_may_vouch_for`
/// exists because an area-derived delta multiplied on top of tuning that already assumes the endgame
/// is the v0.3.4 one-shotting bug — Vyke came out "crazy strong". Every word of that reasoning is
/// about scaling UP. Downward the sign flips: attributing the ground to a hand-tuned character makes
/// it weaker, and this file's axiom is that under-scaling is a balance blemish where over-scaling is
/// a progression WALL. Refusing to move it down does not protect anything; it just leaves the wall.
///
/// 🛑 THE CASE THAT DECIDES IT IS NOT VYKE (Alaric, 2026-08-06). Vyke sits in Liurnia and comes out
/// merely reasonable, which is the mild end of the distribution and a bad place to set the rule.
/// Set it at **Okina in a sphere-0 Mountaintops**, or **Ancient Dragon Man on a Gravesite Plain
/// start** — a hand-tuned endgame duel that a randomised spine can hand you with starting gear, and
/// which `AREA_EXCLUDED` was pinning at full strength precisely because it is named and carries no
/// rune reward. **275 of the 411 excluded rows have no `getSoul` tier either**, so before this they
/// were `NoTouch` in BOTH directions: unreachable by every mechanism we have.
///
/// ⭐ The Vyke guard is untouched. Upward attribution still goes through `presumed_native_tier`, so
/// nothing here can reproduce the bug the carve-out was added for — that bug was an up-scale, and
/// this function is only ever consulted when the answer would be a cut.
pub fn presumed_native_tier_down(npc_param_id: i32, area_tier: Option<usize>) -> Option<usize> {
    native_tier(npc_param_id).or(area_tier)
}

/// May the AREA speak for this enemy's strength? False for the named, unrewarded rows.
///
/// 🛑🛑 THE CARVE-OUT, ADDED FROM PLAY (2026-08-05). Vyke is unrunged and `getSoul`-less, so the
/// area placed him -- tier 11 in a Liurnia whose ground reads index 5 -- and in play he came out
/// "crazy strong". That is the v0.3.4 failure exactly: an area-derived delta multiplied on top of
/// tuning that already assumes you meet him late. The ground is a fair statement about the trash
/// standing on it and a false one about the thing vanilla deliberately parked there.
///
/// ⭐ `AREA_EXCLUDED` is keyed PER CHARACTER (`nameId`), not per row: a named character with ANY
/// unrunged, reward-less row has all of its unrunged rows in the set. Vyke owns a 500-reward row and
/// three reward-less ones; Gideon three at 4000 and one at 0 — keyed per row, which variant the game
/// instantiated would decide how that character scales, and nothing in the data marks them as the
/// same character. 411 rows across 137 characters.
///
/// 🛑 It is NOT a general boss exclusion, and the difference is load-bearing: a named enemy carrying
/// a rung still takes `Replace`, in both directions. Only the AREA's vouching is refused.
/// ⭐ Nor is it a class ban: ordinary enemies are NAMELESS (6323 of 7039 rows have `nameId` 0), so
/// every trash enemy the fallback exists for is untouched.
pub fn area_may_vouch_for(npc_param_id: i32) -> bool {
    crate::native_tiers::AREA_EXCLUDED
        .binary_search(&npc_param_id)
        .is_err()
}

/// Did this enemy reach the region's tier only because the AREA vouched for it? The sweep logs these
/// by id; see `presumed_native_tier` for why that census matters more than the count.
pub fn placed_by_area(npc_param_id: i32, area_tier: Option<usize>) -> bool {
    native_tier(npc_param_id).is_none() && area_tier.is_some() && area_may_vouch_for(npc_param_id)
}

/// The same question for the DOWN path, where the area vouches for everyone.
///
/// 🛑 THE CENSUS MATTERS MORE HERE THAN IT DOES UPWARD. These are the named, hand-tuned characters
/// we are now moving on their neighbours' evidence rather than their own — the population that was
/// deliberately untouchable until 2026-08-06. If a down-scale ever lands wrong, this list is where
/// it shows up first, in our log rather than in a report.
pub fn placed_by_area_down(npc_param_id: i32, area_tier: Option<usize>) -> bool {
    native_tier(npc_param_id).is_none() && area_tier.is_some()
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

/// Map a raw target to a tier index in `[floor_tier, ceiling_tier]`. `max_target <= 0` → the floor
/// tier. Monotonic in `target`.
///
/// 🛑🛑 IT USED TO NORMALISE TO THE LADDER AND CLAMP AFTERWARDS, WHICH THREW AWAY THE TOP OF EVERY
/// CAPPED RAMP. `round(frac * 19).clamp(floor, ceiling)` spends the run's depth fraction on the full
/// 20-rung ladder and then discards whatever exceeds the ceiling — so on a capped seed the deepest
/// regions do not merely *reach* the cap, they SATURATE on it and become indistinguishable.
///
/// Measured on boblerrr's 2026-08-07 session (`completion_scaling_ceiling: 4.844` = tier 11, six
/// distinct sphere targets on a 2000 grid):
///
/// | target | raw `frac * 19` | applied |
/// |--------|-----------------|---------|
/// | 0      | 0.0             | 0       |
/// | 2000   | 3.8             | 4       |
/// | 4000   | 7.6             | 8       |
/// | 6000   | 11.4            | **11**  |
/// | 8000   | 15.2            | **11**  |
/// | 10000  | 19.0            | **11**  |
///
/// Three different depths, one tier: **the top 40% of that seed's spine was flat**, and the player
/// reported it as "I reached tier 11 too fast" and "it skipped tier 2". Both symptoms are this one
/// bug — coarse steps early because the ramp climbs at 1.9 tiers per 1000 target, then no steps at
/// all once it hits the cap.
///
/// ⭐ `region_scaling_toast` ALREADY had the right model: it prints the rung as a position *within*
/// `[floor, ceiling]`, because a tier index means nothing outside the band the seed chose. This
/// function now agrees with the toast instead of contradicting it — before this fix a player on any
/// of those three regions read the same `tier 11 of 11`.
///
/// 🛑 Lowering the ceiling ALONE makes saturation worse, not better: a lower cap is reached at a
/// shallower target, so the flat zone grows. The band has to be re-spread, which is what this does.
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
    // NORMALISE TO THE BAND, NOT THE LADDER. `frac` is this run's depth; the band [floor, ceiling]
    // is the range the seed actually chose. Multiplying by `NUM_TIERS - 1` and clamping afterwards
    // spends the fraction on rungs the seed forbade, and every one of those lands on the ceiling --
    // so the deepest regions all collapse onto the same tier. See the saturation note above.
    let tier = floor + (frac * (ceiling - floor) as f32).round() as usize;
    tier.clamp(floor, ceiling)
}

/// Region → tier, or **`None` when the region is unmapped**. DLC buckets are NOT special-cased: with
/// the DLC baked-scaling clear fixed (`DLC_SCALING_ID_RANGE`), DLC enemies scale by sphere depth
/// exactly like base game.
///
/// 🛑🛑 IT USED TO RETURN THE FLOOR TIER FOR AN UNMAPPED REGION, AND THAT WAS A GUESS WEARING A
/// NUMBER'S CLOTHES. The comment called it "unknown = don't scale up", which was true only while the
/// floor was 0; at any other floor it scales an unidentified region to a tier nobody chose. The
/// 2026-08-06 log shows the cost: `sub 0` (nothing resolved yet, at connect) and `sub 10010` (Chapel,
/// not in the wire) both floored, and **198 and 42 enemies were swept in them** — including, since
/// #346 phase 1b, DOWN states.
///
/// ⭐⭐⭐ THIS FILE ALREADY DECIDED THIS QUESTION, ONE LEVEL DOWN. `presumed_native_tier` returns
/// `None` for an enemy it cannot place, and `scale_action` leaves it exactly as vanilla shipped it —
/// "absence is the safe state, and it is load-bearing". A region we cannot identify is the same fact
/// about a bigger object, and it now gets the same answer.
///
/// ⭐⭐⭐ THE EXPOSED POPULATION IS SMALL, AND CHECKED. It is NOT "sealed regions" -- those are not
/// walkable. The apworld emits `areaLockFlags` for **all 30 regions / 116 buckets**, and a SEALED
/// region's range is keyed to an open flag that is never received and never lit, so the kick-watch
/// permanently ejects the player (`features/area_locks.py`, the 2026-07-08 dead-drop fix). And the
/// scaling wire loops `for pid in REGION_PLAY_IDS[region]`, so a KEPT region has **every** one of
/// its buckets wired -- there are no unscaled pockets inside a region you can fight through.
///
/// What is left is the space outside `REGION_PLAY_IDS` entirely: Roundtable Hold, the Chapel of
/// Anticipation (`sub 10010`, verified absent from the table), and the transient `sub 0` at connect
/// before the game resolves a region. Those are exactly the places that should never have carried a
/// difficulty statement, which is why this is a fix and not a trade.
pub fn tier_for_region(cfg: &ScalingConfig, region: i32) -> Option<usize> {
    if let Some(&target) = cfg.region_targets.get(&region) {
        Some(tier_for_target(
            target,
            cfg.max_target,
            cfg.floor_tier,
            cfg.ceiling_tier,
        ))
    } else if let Some(&(_, _, target)) =
        // SCALING_WIRE: range fallback -- `region` is the play_region/100 sub id; the apworld
        // emits [lo, hi, target] buckets in the same space (a few dozen; linear scan is fine).
        cfg
            .region_ranges
            .iter()
            .find(|&&(lo, hi, _)| (lo..=hi).contains(&region))
    {
        Some(tier_for_target(
            target,
            cfg.max_target,
            cfg.floor_tier,
            cfg.ceiling_tier,
        ))
    } else {
        None
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
    /// No target for this bucket (hub, tutorial, an unmapped sub-area, an unkept region, or a
    /// foreign/older seed), so **nothing is applied there at all**.
    ///
    /// 🛑 IT USED TO CARRY THE FLOOR TIER, AND THAT WENT STALE THE MOMENT THE SWEEP STOPPED USING
    /// IT (2026-08-06, `tier_for_region` -> `Option`). An announcement that named a multiplier the
    /// sweep no longer applies would be the confident wrong answer this enum exists to prevent —
    /// worse than the "tier 0" rendering it was built to stop, because it would be actively false
    /// rather than merely misleading. Carrying no number is what makes that impossible.
    Unscaled,
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
        None => RegionScaling::Unscaled,
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
///   `Liurnia - enemy scaling not set for this area; enemies here are unchanged`
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
        RegionScaling::Unscaled => format!(
            "{} - enemy scaling not set for this area; enemies here are unchanged",
            region
        ),
    }
}

/// The same sentence [`region_scaling_toast`] says ONCE on entry, available to be ASKED at any
/// time -- the tracker's persistent "what is my scaling here" row.
///
/// ⭐ THE ENTRY TOAST IS NOT A QUERYABLE SURFACE, AND THAT IS WHAT THIS FIXES. It fires once per
/// region per session and is then gone, so a player who was not looking has no way to get the
/// answer back. bobler asked "how to view my scaling" on 2026-08-07 after a morning of watching
/// our toasts scroll past -- the information was being delivered and was still unavailable.
///
/// 🛑 `name: None` IS THE ANSWER, NOT A FAILURE. [`region_name_for_bucket`] returns `None` for the
/// geometry that names no region -- Roundtable, the tutorial, unmapped sub-areas -- and those are
/// exactly the places the sweep declines outright (`tier_for_region -> None`, PR #82). The entry
/// toast stays SILENT there, correctly: an announcement nobody asked for should not fire on a
/// non-event. A row the player deliberately opened is the opposite case, and silence there reads
/// as a broken feature rather than as an answer. So this is the first live path by which
/// [`RegionScaling::Unscaled`] can reach a player at all: the sweep returns before the toast is
/// ever built, and `on_region` refuses an unnamed bucket, so the variant was unsayable in-game.
///
/// The `(None, Known(_))` pairing cannot arise -- `region_locks::REGION_LOCKS` and the apworld's
/// `regionSphereTargetRanges` are generated from the same `REGION_PLAY_IDS`, so a bucket the wire
/// can target is a bucket we can name (`an_unnamed_bucket_is_always_unscaled` pins the direction
/// that matters). It is still rendered as unscaled rather than asserted on: a row that panics is
/// worse than a row that under-claims.
///
/// STOP: ASCII ONLY, for the reason on [`region_scaling_toast`] -- this is drawn by the same font.
pub fn region_scaling_line(
    name: Option<&str>,
    scaling: RegionScaling,
    floor_tier: usize,
    ceiling_tier: usize,
) -> String {
    match name {
        Some(n) => region_scaling_toast(n, scaling, floor_tier, ceiling_tier),
        // Deliberately NOT routed through `region_scaling_toast` with a placeholder name: every
        // phrasing of that ("This area - ... not set for this area") reads as a bug.
        None => "This spot is not part of a scaled region; enemies here are unchanged".to_string(),
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
        assert_eq!(tier_for_region(&c, 6850), Some(NUM_TIERS - 1)); // DLC: full tier, uncapped
        assert_eq!(tier_for_region(&c, 64000), Some(NUM_TIERS - 1)); // base: identical treatment
                                                                     // floor_tier still applies to DLC buckets like any other.
        c.floor_tier = 2;
        c.region_ranges = vec![(6850, 6850, 0)]; // shallow DLC bucket
        c.max_target = 100;
        assert_eq!(tier_for_region(&c, 6850), Some(2));
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
        // Stormveil sub 10000 -> low tier; Liurnia sub 62400 -> top tier; unmapped -> no answer.
        assert!(tier_for_region(&c, 10000) < tier_for_region(&c, 62400));
        assert_eq!(tier_for_region(&c, 99999), None);
        assert_eq!(tier_for_region(&c, 62400), Some(NUM_TIERS - 1));
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
        assert_eq!(tier_for_region(&c, 60000), Some(0));
        assert_eq!(tier_for_region(&c, 63000), Some(mid));
        assert_eq!(tier_for_region(&c, 76000), Some(NUM_TIERS - 1));
    }

    #[test]
    fn an_unmapped_region_gets_no_answer_rather_than_the_floor() {
        // 🛑 THE PREMISE MOVED 2026-08-06 AND THE NAME MOVED WITH IT. This test used to be called
        // `unknown_region_falls_back_to_floor` and asserted exactly that. The fallback was a GUESS,
        // and it was invisible while the floor was 0 because tier 0 is nearly vanilla — with the
        // floor at 2, as this fixture has it, an unidentified region was being scaled to 1.656x HP
        // on no evidence at all.
        //
        // The bug the old test was written to catch — an unmapped region silently taking some OTHER
        // region's tier — is still caught, and more strictly: `None` cannot be mistaken for a tier.
        let c = cfg(&[(60000, 100)], 2);
        assert_eq!(tier_for_region(&c, 99999), None);
        // ⭐ NOT VACUOUS: the same config still answers for the region it does know, at a tier that
        // is not the floor. A change that made this function return `None` for everything would
        // pass the assertion above and fail here.
        assert_eq!(tier_for_region(&c, 60000), Some(NUM_TIERS - 1));

        // The floor still governs MAPPED regions -- this change is about identification, not about
        // the floor.
        let shallow = cfg(&[(60000, 0)], 2);
        assert_eq!(tier_for_region(&shallow, 60000), Some(2));
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

    /// MOTIVATING CASE (rule 11), from boblerrr's `archipelago-2026-08-07 (8).log`: six distinct
    /// sphere targets on a 2000 grid under `completion_scaling_ceiling: 4.844` (= tier 11). The old
    /// `round(frac * 19).clamp(..)` gave 0, 4, 8, 11, 11, 11 -- the deepest THREE regions collapsed
    /// onto one tier, and the player felt it as "I reached tier 11 too fast".
    ///
    /// THE ASSERTION IS SATURATION, NOT SPECIFIC RUNGS. Pinning the exact tiers would freeze the
    /// rounding and make any future ramp reshape look like a regression; what must never come back
    /// is a capped ramp whose top is flat.
    #[test]
    fn a_capped_ramp_does_not_saturate_at_its_ceiling() {
        let cap = 11;
        let tiers: Vec<usize> = [0, 2000, 4000, 6000, 8000, 10000]
            .iter()
            .map(|&t| tier_for_target(t, 10000, 0, cap))
            .collect();

        // the band's ends are still exactly the band's ends
        assert_eq!(tiers[0], 0, "shallowest region sits on the floor");
        assert_eq!(tiers[5], cap, "deepest region sits on the ceiling");

        // and the interior is SPREAD: every step strictly climbs, so no two depths tie.
        for w in tiers.windows(2) {
            assert!(
                w[1] > w[0],
                "a capped ramp must keep climbing across the whole spine, got {tiers:?}"
            );
        }
    }

    /// The gap this suite had: EVERY capped assertion was an endpoint (`frac` 0 or 1), and every
    /// interior assertion was uncapped -- where the old and new formulas are algebraically
    /// identical. So the one combination that mattered, capped AND interior, was untested, which is
    /// precisely where the saturation lived. A test that only checks endpoints cannot see a curve's
    /// shape.
    #[test]
    fn the_interior_of_a_capped_ramp_is_distributed_across_the_band() {
        // halfway up the run is halfway up the BAND, not halfway up the ladder then clamped.
        assert_eq!(tier_for_target(50, 100, 0, 10), 5);
        assert_eq!(tier_for_target(25, 100, 0, 8), 2);
        // a floor shifts the band without compressing it
        assert_eq!(tier_for_target(0, 100, 4, 12), 4);
        assert_eq!(tier_for_target(50, 100, 4, 12), 8);
        assert_eq!(tier_for_target(100, 100, 4, 12), 12);
    }

    /// A lower cap must not buy a flatter curve -- that was the trap in "just bring the ceiling
    /// down". Under the old formula a tighter band saturated EARLIER; under this one it stays
    /// spread, which is what makes lowering the cap a usable knob at all.
    #[test]
    fn a_tighter_ceiling_still_spreads_across_the_spine() {
        for cap in [6, 9, 11, 15] {
            let tiers: Vec<usize> = [0, 2000, 4000, 6000, 8000, 10000]
                .iter()
                .map(|&t| tier_for_target(t, 10000, 0, cap))
                .collect();
            assert_eq!(tiers[0], 0, "cap {cap}: floor end");
            assert_eq!(tiers[5], cap, "cap {cap}: ceiling end");
            assert!(
                tiers.windows(2).all(|w| w[1] > w[0]),
                "cap {cap} saturates: {tiers:?}"
            );
        }
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
        assert_eq!(tier_for_region(&cfg, 200), Some(NUM_TIERS - 1));
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
            Some(5),
            "the deepest region is capped"
        );
        assert_eq!(
            tier_for_region(&cfg, 100),
            Some(0),
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
                region_scaling_toast("Farum Azula", RegionScaling::Unscaled, 0, NUM_TIERS - 1),
                region_scaling_toast("Farum Azula", RegionScaling::Known(tier), 3, 15),
                region_scaling_toast("Farum Azula", RegionScaling::Known(tier), 7, 7),
                // The tracker row is drawn by the SAME font, so it is bound by the same rule --
                // including the unnamed-bucket sentence, which has no other caller to catch it.
                region_scaling_line(
                    Some("Farum Azula"),
                    RegionScaling::Known(tier),
                    0,
                    NUM_TIERS - 1,
                ),
                region_scaling_line(None, RegionScaling::Unscaled, 0, NUM_TIERS - 1),
            ] {
                assert!(
                    s.is_ascii(),
                    "toast is drawn by the GAME's font, which has no glyph for non-ASCII: {s:?}"
                );
            }
        }
    }

    #[test]
    fn the_row_repeats_the_toast_verbatim_for_a_named_region() {
        // MOTIVATING CASE (rule 11): bobler asked "how to view my scaling" on 2026-08-07 having
        // watched our toasts all morning -- the entry announcement is not a queryable surface.
        // The fix is to say the SAME sentence on demand, and the whole point of routing the row
        // through `region_scaling_toast` is that the two can never drift into disagreeing about
        // the same region. A second format string would have been the bug.
        for tier in 0..=NUM_TIERS + 1 {
            for (floor, ceiling) in [(0, NUM_TIERS - 1), (3, 15), (7, 7)] {
                assert_eq!(
                    region_scaling_line(
                        Some("Liurnia"),
                        RegionScaling::Known(tier),
                        floor,
                        ceiling
                    ),
                    region_scaling_toast("Liurnia", RegionScaling::Known(tier), floor, ceiling),
                );
            }
        }
    }

    #[test]
    fn an_unnamed_bucket_never_states_a_number() {
        // The Chapel / Roundtable / unmapped case, and the reason the row exists at all: the sweep
        // returns at `tier_for_region -> None` BEFORE any toast is built, so before this there was
        // no path by which a player could be told "nothing is applied here". Saying it is the
        // answer to the 2026-08-07 tutorial question in one glance.
        //
        // 🛑 The assertion is the same one `RegionScaling::Unscaled` was rebuilt around (PR #82):
        // NO tier and NO multiplier, because any number here would be a difficulty statement about
        // a region we deliberately refused to make one about.
        let s = region_scaling_line(None, RegionScaling::Unscaled, 0, NUM_TIERS - 1);
        assert!(
            !s.contains('x'),
            "the row must not name a multiplier: {s:?}"
        );
        assert!(!s.contains("tier"), "the row must not name a tier: {s:?}");
        assert!(
            s.contains("unchanged"),
            "the row must say what is true: {s:?}"
        );
    }

    #[test]
    fn an_unnamed_bucket_is_always_unscaled() {
        // `(None, Known(_))` cannot arise -- REGION_LOCKS and the apworld's
        // regionSphereTargetRanges are generated from the same REGION_PLAY_IDS, so a bucket the
        // wire can target is one we can name. This pins the DIRECTION that matters if that ever
        // stops being true: an unnameable bucket must degrade to "unchanged" rather than leak a
        // multiplier for a place whose name we could not resolve.
        for tier in 0..=NUM_TIERS + 1 {
            assert_eq!(
                region_scaling_line(None, RegionScaling::Known(tier), 0, NUM_TIERS - 1),
                region_scaling_line(None, RegionScaling::Unscaled, 0, NUM_TIERS - 1),
            );
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
        // THE POINT OF THE ENUM, and the point moved on 2026-08-06. It used to be "do not render the
        // floor as though it were derived"; now the sweep does not apply anything in an unmapped
        // region, so the toast must not name a MULTIPLIER either. A number in this string would be
        // a promise the sweep no longer keeps.
        let s = region_scaling_toast("Roundtable Hold", RegionScaling::Unscaled, 0, NUM_TIERS - 1);
        assert!(s.contains("not set for this area"), "{s}");
        assert!(
            !s.contains("tier"),
            "an unscaled region must not claim a tier: {s}"
        );
        assert!(
            !s.contains('x'),
            "an unscaled region must not claim a multiplier either: {s}"
        );
        // ⭐ And it must still SAY something -- silence would read as a missing feature.
        assert!(s.contains("Roundtable Hold"), "{s}");
    }

    #[test]
    fn region_scaling_maps_a_missing_target_to_defaulted_not_to_the_bottom() {
        // 🛑 The floor is passed in and deliberately IGNORED: an unmapped region is not floored any
        // more, it is untouched, and a variant carrying `3` here is what would let that drift back.
        assert_eq!(region_scaling(None, 10_000, 3, 19), RegionScaling::Unscaled);
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
    fn a_grace_warp_re_announces_the_same_region() {
        // MOTIVATING CASE (rule 11): bobler, 2026-08-07, asked for the scaling toast to fire again
        // when he warps to a grace "even within region". The ledger is MESSAGE-keyed and per
        // session, so re-entering the region you are already in was silent -- and a fast travel is
        // exactly the moment a player is re-orienting and wants to be told again.
        //
        // 🛑 THE POINT IS THE *SAME* REGION. A different region already re-announced (different
        // message), so a test that warped somewhere else would pass without the feature existing.
        let c = cfg(&[(60000, 5000)], 0);
        let mut ledger = RegionToastLedger::new();
        let first = ledger
            .on_region(&c, 60000, Some("Limgrave"))
            .expect("announces");
        assert_eq!(
            ledger.on_region(&c, 60000, Some("Limgrave")),
            None,
            "still deduped in place"
        );
        ledger.reset(); // what the LuaWarp hook now calls
        assert_eq!(
            ledger.on_region(&c, 60000, Some("Limgrave")),
            Some(first),
            "a grace warp re-announces the region you warped INTO, even if you never left it"
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
            let shown = match region_scaling(
                raw_target_for_region(&c, bucket),
                c.max_target,
                c.floor_tier,
                c.ceiling_tier,
            ) {
                RegionScaling::Known(t) => Some(t),
                // ⭐ THE INVARIANT SURVIVED THE PREMISE CHANGE BECAUSE IT WAS NEVER ABOUT NUMBERS.
                // "Announced == applied" now includes announcing NOTHING when nothing is applied,
                // and `99999` is in this list precisely to exercise that arm.
                RegionScaling::Unscaled => None,
            };
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
        // ⭐ THE DLC ALLY-TUNING BLOCK IS NOT OURS -- except for the four rows we apply. Armed
        // 2026-08-06 by ALLOWLIST, which is the property this half of the test exists to pin: the
        // ids we use are `Downstate`, and their immediate neighbours in the same block are still
        // nothing to do with us. A range widened to swallow the block would fail here.
        for id in DOWNSTATE_IDS {
            assert_eq!(scaling_kind(id), Some(ScalingKind::Downstate), "{id}");
        }
        for id in [20018000, 20018001, 20018003, 20018007, 20018010, 20018035] {
            assert_eq!(scaling_kind(id), None, "{id} is not ours");
        }
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
                scale_action(false, false, absent, tier, None),
                ScaleAction::NoTouch,
                "an unclassified enemy was scaled at tier {tier}"
            );
        }
    }

    // ---- the Nexus thread, made executable (2026-08-04/05) --------------------------------------

    #[test]
    fn two_bosses_in_one_fight_no_longer_get_wildly_different_scaling() {
        // THE MOTIVATING CASE, and the thread's cleanest complaint -- lizzymagala, 2026-08-05:
        // "2 bosses that are part of the same fight can have wildly different scaling with one being
        // super squishy and weak and the other being insanely tanky and straight up one shotting".
        //
        // That is a duo where one member carries a vanilla rung and one does not. Under v0.3.4 the
        // runged one was normalised to the region tier while the unrunged one had the SAME tier
        // multiplied on top of hand-tuned, near-endgame base stats -- so one got weaker and the other
        // got far stronger, from one line of code.
        //
        // 🛑 What this pins is that they no longer diverge THAT way. It does NOT pin that they feel
        // alike: the unrunged one keeps vanilla strength, which is the residual phase 1b exists for.
        // Do not "fix" this test by making NoTouch scale something.
        let unplaceable = -1; // absent from NATIVE_TIERS, like every getSoul-less named boss
        for tier in 0..NUM_TIERS {
            assert_eq!(
                scale_action(true, false, unplaceable, tier, None),
                ScaleAction::Replace
            );
            assert_eq!(
                scale_action(false, false, unplaceable, tier, None),
                ScaleAction::NoTouch
            );
        }
    }

    #[test]
    fn a_hand_tuned_npc_is_never_multiplied_at_any_depth() {
        // lavakoala6, 2026-08-04: Gideon "1 shots from 50 vigor and he has soo much health", and
        // Vyke "definitely Mountaintop scaling while still in sphere 2". Both are unrunged with no
        // rune reward in NpcParam, so both resolve here -- and the sponge and the one-shots came from
        // the same multiplier, applied to stats that already assumed the end of the game.
        for tier in 0..NUM_TIERS {
            assert_eq!(
                scale_action(false, false, -1, tier, None),
                ScaleAction::NoTouch
            );
        }
    }

    #[test]
    fn the_area_places_an_enemy_its_own_reward_cannot() {
        // THE MOTIVATING CASE (Weeping, 2026-08-05): region target tier 11, area index 1, and 69
        // enemies with no rung and no getSoul sitting at vanilla while 523 neighbours carry the
        // tier. With the area vouching for them they take the same path as everything else.
        let unplaceable = -1; // absent from NATIVE_TIERS
        assert_eq!(
            scale_action(false, false, unplaceable, 11, Some(1)),
            ScaleAction::Apply
        );
        // ...and the region's own level is what they reach -- the area index is the enemy's PRESUMED
        // NATIVE strength, never a cap on the region (clamping the tier toward the area index would
        // drag every region back to vanilla geography, which is the thing the mod exists to override).
        assert_eq!(presumed_native_tier(unplaceable, Some(1)), Some(1));
        assert!(placed_by_area(unplaceable, Some(1)));
    }

    #[test]
    fn the_area_does_not_speak_for_a_carved_out_character() {
        // 🛑 THE VYKE CASE, from play (2026-08-05). Liurnia region 62010: target tier 11, area index
        // 5, and the area placed two NAMED rows -- 523040020 (nameId 130400) and 523360012
        // (nameId 133600). Alaric fought the result: "crazy strong". Their bases already assume a
        // late encounter, so an area-derived delta lands on top of endgame tuning.
        let named = crate::native_tiers::AREA_EXCLUDED;
        assert!(!named.is_empty(), "the exclusion set must not be empty");
        let vyke_shaped = named[0];
        assert!(!area_may_vouch_for(vyke_shaped));
        assert_eq!(presumed_native_tier(vyke_shaped, Some(5)), None);
        assert_eq!(
            scale_action(false, false, vyke_shaped, 11, Some(5)),
            ScaleAction::NoTouch
        );
        assert!(!placed_by_area(vyke_shaped, Some(5)));
    }

    #[test]
    fn a_carved_out_character_is_carved_out_in_all_of_its_rows() {
        // ⭐⭐⭐ PER CHARACTER, NOT PER ROW. Gideon owns four unrunged rows: 523240070 has no reward,
        // and 523240000 / 523240079 / 523240179 each carry getSoul 4000. Keyed per row, the first
        // would be left vanilla and the other three placed by their own derived tier -- so which
        // Gideon you met would decide how Gideon scaled. All four are in the set.
        for id in [523240000, 523240070, 523240079, 523240179] {
            assert!(
                !area_may_vouch_for(id),
                "{id} is a Gideon row; the character is carved out, so every row is"
            );
        }
        // The cost, asserted so it cannot be quietly reverted: a REWARDED row is excluded here even
        // though `native_tier` could place it. That is deliberate -- it lands in the under-scaled
        // direction, a blemish rather than a wall.
        assert!(
            native_tier(523240000).is_some(),
            "fixture must still carry its own tier"
        );
        assert_eq!(
            presumed_native_tier(523240000, Some(5)),
            native_tier(523240000)
        );
    }

    #[test]
    fn the_carve_out_does_not_swallow_ordinary_enemies() {
        // ⭐ THE WHOLE POINT of keying on nameId: ordinary enemies are NAMELESS (6323 of 7039 rows
        // have nameId 0), so this excludes 275 rows and not the class. A gate that kept only NAMED
        // enemies would have excluded ~90% of the game -- the exact trash the fallback exists for.
        let unnamed_unrewarded = -1; // absent from NATIVE_TIERS and from the exclusion set
        assert!(area_may_vouch_for(unnamed_unrewarded));
        assert_eq!(
            scale_action(false, false, unnamed_unrewarded, 11, Some(1)),
            ScaleAction::Apply
        );
        assert!(
            crate::native_tiers::AREA_EXCLUDED.len() < 600,
            "this is a carve-out, not a class ban"
        );
    }

    #[test]
    fn the_exclusion_set_is_sorted_so_binary_search_is_valid() {
        // area_may_vouch_for binary-searches it; an unsorted table would return silent WRONG
        // answers for most ids rather than failing, which is the same shape as the npc_id/
        // npc_param_id bug in #63.
        let named = crate::native_tiers::AREA_EXCLUDED;
        assert!(
            named.windows(2).all(|w| w[0] < w[1]),
            "must be sorted and duplicate-free"
        );
    }

    #[test]
    fn a_carved_out_enemy_still_scales_when_placed_without_the_area() {
        // 🛑 NOT A GENERAL BOSS EXCLUSION. The carve-out refuses the AREA's vouching, nothing else:
        // a named enemy that carries a rung is still a true re-tier, in both directions.
        let named = crate::native_tiers::AREA_EXCLUDED;
        let vyke_shaped = named[0];
        for tier in 0..NUM_TIERS {
            assert_eq!(
                scale_action(true, false, vyke_shaped, tier, Some(5)),
                ScaleAction::Replace
            );
        }
    }

    #[test]
    fn no_area_reading_means_the_old_behaviour_exactly() {
        // `None` is not "assume weak" -- it is "we did not see enough untouched neighbours to say".
        // Attributing a strength on no evidence is how the unrunged class got buffed in v0.3.4.
        for tier in 0..NUM_TIERS {
            assert_eq!(
                scale_action(false, false, -1, tier, None),
                ScaleAction::NoTouch
            );
        }
        assert_eq!(presumed_native_tier(-1, None), None);
        assert!(!placed_by_area(-1, None));
    }

    #[test]
    fn an_enemys_own_reward_outranks_the_ground_it_stands_on() {
        // A getSoul-derived tier is measured ON the enemy; the area index is measured on its
        // neighbours. When both exist the enemy's own wins, at whatever value -- including when that
        // makes it NoTouch and the area would have moved it.
        let table = crate::native_tiers::NATIVE_TIERS;
        let placed = table[table.len() - 1].0; // a real, high-index row
        let own = native_tier(placed).expect("fixture must be in the table");
        assert_eq!(presumed_native_tier(placed, Some(0)), Some(own));
        assert!(!placed_by_area(placed, Some(0)));
    }

    #[test]
    fn altus_at_sphere_zero_is_scaled_down() {
        // ⭐⭐⭐ THE MOTIVATING CASE, AND THEREFORE THE ACCEPTANCE TEST (CONTRIBUTING rule 11).
        // Altus at sphere 0, measured 2026-08-05: target tier 5, area index 7 off a 302-enemy
        // sample. Every runged neighbour was normalised DOWN to 2.266x while **61 unrunged enemies
        // stood at full Altus strength** in the same field -- lizzymagala's "one super squishy and
        // weak, the other insanely tanky", which is not a bug in the tier but `NoTouch` doing
        // exactly what it promised in a region whose ground is above the target.
        //
        // Until 2026-08-06 this test asserted `NoTouch` and was named for it. That was an honest
        // statement of a missing primitive, not a property worth keeping.
        let down = match scale_action(false, false, -1, 5, Some(7)) {
            ScaleAction::Down(s) => s,
            other => panic!("Altus 7 -> 5 must scale DOWN, got {other:?}"),
        };
        assert_eq!(down.ids, &[20018008]);
        // The ladder's own step between those two rungs is 1.758/2.0 = 0.879 attack. 0.70 is the
        // closest we can express without leaving it stronger than that.
        assert_eq!(down.attack, 0.7);

        // 🛑 THE DEADBAND MUST NOT SWALLOW THIS CASE. At a cutoff of 0.85 it does, and the whole
        // phase silently no-ops the defect it was built for. If DOWN_DEADBAND is ever lowered, this
        // is the assertion that should stop it.
        assert!(
            SCALING_TIERS[5].attack / SCALING_TIERS[7].attack <= DOWN_DEADBAND,
            "the Altus step must stay expressible"
        );

        // Ground EQUAL to the target is still nothing to do -- there is no gap to close.
        assert_eq!(
            scale_action(false, false, -1, 7, Some(7)),
            ScaleAction::NoTouch
        );
    }

    #[test]
    fn a_gap_too_small_to_express_is_left_vanilla() {
        // ⭐ The other half of the deadband. Tier 6 -> 5 wants 0.960 attack; our coarsest tool is
        // 0.70, so "closest available" would fire a 30% nerf at a 4% problem. Leaving it alone is
        // the honest answer and it matches how `presumed_native_tier` declines rather than guesses.
        assert!(SCALING_TIERS[5].attack / SCALING_TIERS[6].attack > DOWN_DEADBAND);
        assert_eq!(
            scale_action(false, false, -1, 5, Some(6)),
            ScaleAction::NoTouch
        );
        assert_eq!(down_state_for(6, 5), None);
        // Tiers 17/18/19 share an attack rate outright, so the ladder itself expresses no
        // difference there and neither may we.
        assert_eq!(down_state_for(19, 18), None);
        assert_eq!(down_state_for(18, 17), None);
    }

    #[test]
    fn a_down_state_never_leaves_the_enemy_stronger_than_the_ladder_asked() {
        // The rounding rule, over the whole ladder: where we act at all, we act by AT LEAST as much
        // as the step between the two rungs. Over-reducing is a balance blemish; under-reducing
        // leaves the wall standing, which is the failure this phase exists to remove.
        let mut acted = 0;
        for native in 0..NUM_TIERS {
            for target in 0..native {
                let want = SCALING_TIERS[target].attack / SCALING_TIERS[native].attack;
                let Some(state) = down_state_for(native, target) else {
                    continue;
                };
                acted += 1;
                // 🛑 THE PREMISE MOVED 2026-08-06 AND THIS SENTENCE MOVED WITH IT. It used to read
                // "at least as much as the step asked for", strictly. That strictness is what sent
                // a want of 0.444 down to 0.315 past an available 0.45, and what refused an exact
                // 0.450. The property worth keeping is that we never under-reduce by anything a
                // player could feel -- DOWN_TOLERANCE is 2%, and the bound is still a bound.
                assert!(
                    state.attack <= want * (1.0 + DOWN_TOLERANCE) + f32::EPSILON,
                    "tier {native} -> {target}: wanted {want}, state gives {}",
                    state.attack
                );
            }
        }
        // 🛑 A VACUOUS PASS IS THE FAILURE MODE HERE -- a deadband of 1.0 would make every pair
        // `None` and this loop would assert nothing at all.
        assert!(
            acted > 100,
            "only {acted} pairs acted; the deadband has eaten the phase"
        );
        assert_eq!(down_state_for(5, 5), None, "no gap, no state");
        assert_eq!(
            down_state_for(0, 5),
            None,
            "target ABOVE native is the Apply path"
        );
    }

    #[test]
    fn a_near_tie_does_not_cost_a_whole_lattice_step() {
        // 🔥 FROM THE FIRST LIVE 1b LOG (2026-08-06). npc_param 523210014, native tier 9, target 0:
        // the ladder wanted 0.444 and a 0.45 state was right there, but a strict `<=` reached past
        // it to 0.315 -- a 30% extra cut to cover a 1.4% miss.
        let want = SCALING_TIERS[0].attack / SCALING_TIERS[9].attack;
        assert!(
            (0.44..0.45).contains(&want),
            "fixture drifted: want is {want}"
        );
        let s = down_state_for(9, 0).expect("must act");
        assert_eq!(s.attack, 0.45, "a 1.4% miss must not cost a lattice step");

        // The case that shows it was never a tuning preference: this pair wants 0.450 EXACTLY and
        // was still refused, because these are f32 ratios of f32 rates.
        let exact = SCALING_TIERS[1].attack / SCALING_TIERS[10].attack;
        assert!((exact - 0.45).abs() < 0.001, "fixture drifted: {exact}");
        assert_eq!(down_state_for(10, 1).expect("must act").attack, 0.45);

        // 🛑 AND IT MUST NOT BECOME A SECOND DEADBAND. The tolerance decides WHICH state, never
        // WHETHER -- the acted-pair count is unchanged by it.
        let acted = (0..NUM_TIERS)
            .flat_map(|n| (0..n).map(move |t| (n, t)))
            .filter(|&(n, t)| down_state_for(n, t).is_some())
            .count();
        assert_eq!(
            acted, 156,
            "the tolerance changed COVERAGE, which it must not"
        );
    }

    #[test]
    fn a_named_hand_tuned_enemy_comes_down_on_the_area_but_never_up() {
        // ⭐⭐⭐ THE CASE ALARIC SET THE RULE ON (2026-08-06): "imagine this is a mountaintops fight
        // and I have to fight Okina. or a gravesite plain start and I'm fighting Ancient Dragon
        // Man." Vyke in Liurnia came out merely reasonable, which is the mild end of the
        // distribution and the wrong place to calibrate. A randomised spine can open on a region
        // whose hand-tuned duels assume the endgame, and `AREA_EXCLUDED` was pinning every one of
        // them at full strength.
        let excluded = *crate::native_tiers::AREA_EXCLUDED
            .iter()
            .find(|&&id| native_tier(id).is_none())
            .expect("the affected class is the excluded rows with no getSoul tier");

        // The guard that carve-out exists for, UNCHANGED: the area may never make it stronger.
        assert!(!area_may_vouch_for(excluded));
        assert_eq!(presumed_native_tier(excluded, Some(5)), None);
        for tier in 6..NUM_TIERS {
            assert_eq!(
                scale_action(false, false, excluded, tier, Some(5)),
                ScaleAction::NoTouch,
                "the area must never up-scale a named unrewarded enemy (tier {tier})"
            );
        }

        // ...and the half that changed: deep ground, shallow target, so it comes DOWN.
        assert_eq!(presumed_native_tier_down(excluded, Some(11)), Some(11));
        let down = match scale_action(false, false, excluded, 0, Some(11)) {
            ScaleAction::Down(s) => s,
            other => panic!("Okina in a sphere-0 region must come down, got {other:?}"),
        };
        assert!(down.attack < 1.0);
        // 🛑 And it must be NAMED in the census -- these are moved on their neighbours' evidence,
        // not their own, so the log is where a bad one has to surface first.
        assert!(placed_by_area_down(excluded, Some(11)));
        assert!(
            !placed_by_area(excluded, Some(11)),
            "the UP census must still refuse it, or the two lists stop meaning different things"
        );
    }

    #[test]
    fn letting_the_area_vouch_downward_changes_nothing_for_an_ordinary_enemy() {
        // ⭐ NON-VACUITY IN THE OTHER DIRECTION. If `presumed_native_tier_down` had simply replaced
        // the guarded one everywhere, this test would still pass -- so it also pins that an enemy
        // the area ALREADY vouched for is decided identically by both, and that a row with its own
        // getSoul tier ignores the area entirely.
        let ordinary = 7100; // not in AREA_EXCLUDED, no table tier
        assert!(area_may_vouch_for(ordinary));
        assert_eq!(native_tier(ordinary), None);
        assert_eq!(
            presumed_native_tier(ordinary, Some(9)),
            presumed_native_tier_down(ordinary, Some(9))
        );

        let (own, tier) = *crate::native_tiers::NATIVE_TIERS
            .iter()
            .find(|&&(_, t)| t > 0)
            .expect("need a table row");
        assert_eq!(
            presumed_native_tier_down(own, Some(19)),
            Some(tier as usize)
        );

        // No area reading at all: still nothing to attribute, in either direction.
        assert_eq!(presumed_native_tier_down(-1, None), None);
        assert_eq!(
            scale_action(false, false, -1, 0, None),
            ScaleAction::NoTouch
        );
    }

    #[test]
    fn a_carried_down_state_is_kept_when_we_can_no_longer_place_the_enemy() {
        // ⭐⭐⭐ THE MOTIVATING CASE, from the first live 1b log (2026-08-06, region 0):
        // `down-scaled 23 (settled 0)` then `down-scaled 0 (settled 5)`, with `left vanilla` going
        // 19 -> 37. 18 + 5 = 23 and 19 + 18 = 37 -- the 18 area-placed enemies stopped resolving,
        // because the area histogram counts only rung-AND-band carriers and our own sweep had
        // stripped them. They kept their state, but ONLY because `NoTouch` returns before the clear,
        // and they reported themselves as "left vanilla" -- the opposite of what happened.
        let unplaceable = -1;
        assert_eq!(presumed_native_tier(unplaceable, None), None);
        assert_eq!(
            scale_action(false, true, unplaceable, 0, None),
            ScaleAction::KeepDown
        );
        // Same enemy, no state carried: genuinely untouched, and it must still say so.
        assert_eq!(
            scale_action(false, false, unplaceable, 0, None),
            ScaleAction::NoTouch
        );
        // 🛑 KeepDown is the answer ONLY when we cannot place it. Where we can, the placement wins
        // and the state is re-derived like any other.
        let (npc, native) = crate::native_tiers::NATIVE_TIERS[0];
        let native = native as usize;
        assert!(!matches!(
            scale_action(false, true, npc, native, None),
            ScaleAction::KeepDown
        ));
    }

    #[test]
    fn a_down_state_that_is_no_longer_warranted_comes_off() {
        // 🛑 WITHOUT `ClearDown` A DOWN STATE IS UNREMOVABLE, and the deadband makes that reachable:
        // `down_state_for` declines 34 of the 190 pairs, so an enemy placed at one target and
        // re-swept at a target just below its native tier would keep a cut nothing now justifies.
        let (npc, native) = *crate::native_tiers::NATIVE_TIERS
            .iter()
            .find(|&&(_, t)| (t as usize) >= 6)
            .expect("need a mid-ladder fixture");
        let native = native as usize;

        // Deep enough below to warrant a state...
        assert!(matches!(
            scale_action(false, true, npc, 0, None),
            ScaleAction::Down(_)
        ));
        // ...one rung below is inside the deadband, so the carried state must come OFF.
        assert_eq!(down_state_for(native, native - 1), None);
        assert_eq!(
            scale_action(false, true, npc, native - 1, None),
            ScaleAction::ClearDown
        );
        // Exactly at its native tier: nothing is warranted at all.
        assert_eq!(
            scale_action(false, true, npc, native, None),
            ScaleAction::ClearDown
        );
        // ⭐ And with no state carried, both of those are simply NoTouch -- `ClearDown` must never
        // become a reason to touch an enemy we have never touched.
        assert_eq!(
            scale_action(false, false, npc, native - 1, None),
            ScaleAction::NoTouch
        );
        assert_eq!(
            scale_action(false, false, npc, native, None),
            ScaleAction::NoTouch
        );
    }

    #[test]
    fn no_down_state_can_buff_anything() {
        // 🛑 THE TABLE IS THE GUARD. `20018027` carries a 3x HP rate that only `20018004` cancels,
        // so a subset built without it is a BUFF -- and a down decision that made an enemy tankier
        // would be the wall direction, arrived at through the fix for walls.
        for state in DOWN_STATES {
            assert!(state.attack < 1.0, "{state:?} does not reduce attack");
            assert!(state.hp <= 1.0, "{state:?} BUFFS hp");
            assert!(!state.ids.is_empty());
            // Every id we can apply must be one the clear can see, or it strands forever.
            for id in state.ids {
                assert!(is_downstate_id(*id), "{id} is applied but not cleared");
                assert!(is_scaling_speffect_with_downstates(*id));
            }
            // Sorted, because `settled_on_downstate` and the census both read them as a set.
            assert!(
                state.ids.windows(2).all(|w| w[0] < w[1]),
                "{state:?} is unsorted"
            );
        }
    }

    #[test]
    fn a_down_state_is_settled_only_on_exact_set_equality() {
        let s = down_state_for(7, 5).expect("the Altus state");
        assert!(settled_on_downstate(&[20018008], s));
        // Order must not matter...
        let two = down_state_for(11, 0).expect("a deep gap");
        assert!(two.ids.len() >= 2);
        let mut rev: Vec<i32> = two.ids.to_vec();
        rev.reverse();
        assert!(settled_on_downstate(&rev, two));
        // ...but a SUPERSET is not settled. An enemy holding the state plus a stale rung must fall
        // through and be cleared -- the `residue 306` lesson, in the down half.
        assert!(!settled_on_downstate(&[20018008, 7060], s));
        assert!(!settled_on_downstate(&[], s));
        assert!(!settled_on_downstate(&[20018002], s));
    }

    #[test]
    fn ladder_tier_is_the_inverse_of_speffect_id_for_tier() {
        for tier in 0..NUM_TIERS {
            assert_eq!(ladder_tier(speffect_id_for_tier(tier)), Some(tier));
        }
        // A BAND id is not a rung and must not answer -- band and ladder ids differ by exactly 400,
        // so an off-by-one-family bug here would read as a plausible tier rather than as nothing.
        assert_eq!(ladder_tier(7460), None);
        // Nor may the DLC ladder answer on the base index: 20007010 is 7.84x, base index 0 is 1.141x.
        assert_eq!(ladder_tier(20007010), None);
        assert_eq!(ladder_tier(0), None);
    }

    #[test]
    fn an_area_needs_enough_neighbours_to_be_an_area() {
        let mut hist = [0u32; NUM_TIERS];
        // One fog-gated room's worth of enemies is not a statement about a region.
        hist[6] = MIN_AREA_SAMPLE - 1;
        assert_eq!(area_tier_from_histogram(&hist), None);
        hist[6] = MIN_AREA_SAMPLE;
        assert_eq!(area_tier_from_histogram(&hist), Some(6));
        assert_eq!(area_tier_from_histogram(&[0u32; NUM_TIERS]), None);
    }

    #[test]
    fn the_area_is_the_median_so_one_outlier_is_not_the_area() {
        // 🛑 THE MOTIVATING SHAPE, and the reason this is not a mean. Vyke is an endgame-tuned
        // invader standing in a mid-game field: a lone index-19 entry among twenty index-5
        // neighbours. A mean would drag the field toward him -- i.e. the outlier would license
        // scaling the field UP to meet the outlier, which is the wall direction. The median leaves
        // the field at 5, which is the answer that makes him the thing that stands out.
        let mut hist = [0u32; NUM_TIERS];
        hist[5] = 20;
        hist[NUM_TIERS - 1] = 1;
        assert_eq!(area_tier_from_histogram(&hist), Some(5));
    }

    #[test]
    fn the_area_median_is_weighted_by_population() {
        // Two rungs, unequal populations: the answer follows the bodies, not the distinct ids. (The
        // existing `rung_band_pairs` sample is DEDUPLICATED and capped at 12, so a median over it
        // would be a median over distinct rungs -- which is why the census carries its own
        // histogram instead of reusing that field.)
        let mut hist = [0u32; NUM_TIERS];
        hist[3] = 3;
        hist[9] = 30;
        assert_eq!(area_tier_from_histogram(&hist), Some(9));
        // Exactly balanced: the upper half wins, deliberately. Ties resolve toward NOT weakening.
        let mut tie = [0u32; NUM_TIERS];
        tie[4] = 10;
        tie[8] = 10;
        assert_eq!(area_tier_from_histogram(&tie), Some(8));
    }

    #[test]
    fn downstates_are_cleared_but_their_block_is_not() {
        // ⭐ ARMED 2026-08-06, and this test inverted in the same change exactly as its previous
        // version instructed. The down-state rows sit OUTSIDE both clear ranges, so the sweep could
        // not see them: a state applied before 1b would have stranded across reconnects and seed
        // changes, because the clear that removes everything else walked straight past it.
        //
        // 🛑 THE PROPERTY THAT MATTERS IS STILL THE ALLOWLIST. If this fails, either the ids and the
        // clear have drifted apart, or someone widened a RANGE far enough to swallow the DLC
        // ally-tuning block -- and the second is much worse, because it would strip legitimate
        // effects off DLC summons with no symptom on our side at all.
        for id in DOWNSTATE_IDS {
            assert!(is_downstate_id(id));
            assert!(
                !is_scaling_speffect(id),
                "the clear can now see {id} -- see this test's note"
            );
            assert!(is_scaling_speffect_with_downstates(id));
        }
        // The successor predicate must be a strict extension: everything cleared today stays cleared.
        for id in [7010, 7200, 7460, 7850, 20007000, 20007400] {
            assert!(is_scaling_speffect_with_downstates(id));
        }
        // ...and must NOT have become a range that swallows the block the rows live in. 🛑 These
        // are the NEIGHBOURS of the four we use, in the same block, deliberately: an allowlist
        // leaves them alone and a widened range does not.
        for id in [20018000, 20018001, 20018003, 20018007, 20018010, 20018035] {
            assert!(
                !is_scaling_speffect_with_downstates(id),
                "{id} is not ours to clear"
            );
        }
    }

    #[test]
    fn an_enemy_carrying_the_target_and_a_band_is_not_settled() {
        // THE MOTIVATING CASE, straight off the 2026-08-05 floor-25 smoke test: target 7060 (the new
        // default floor tier) on a Liurnia enemy whose vanilla rung IS 7060, still carrying its band.
        // `contains` says leave it alone; that is how it kept 4.18x while its peers sat at 2.266x.
        assert!(!settled_on_target(&[7060, 7460], 7060));
        // ...and the state that IS settled: the target, alone.
        assert!(settled_on_target(&[7060], 7060));
        // Order must not matter.
        assert!(!settled_on_target(&[7460, 7060], 7060));
        // Nothing carried at all is NOT settled -- the enemy still needs the tier applied.
        assert!(!settled_on_target(&[], 7060));
        // A different rung is not the target.
        assert!(!settled_on_target(&[7010], 7060));
        // Duplicates of the target only are still settled (the engine can list one twice).
        assert!(settled_on_target(&[7060, 7060], 7060));
    }

    #[test]
    fn settling_is_invisible_at_the_old_floor_which_is_why_this_hid() {
        // Regression-proof the reason it went unnoticed: at floor 0 the target is 7010 and an enemy
        // carrying its own native rung plus a band never contains the target at all, so it took the
        // NORMAL path and was cleared correctly. The bug needed a floor above 0 to appear.
        assert!(!settled_on_target(&[7060, 7460], 7010));
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
                scale_action(false, false, band_only_unknown_npc, tier, None),
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
    fn a_derived_enemy_scales_up_by_rung_and_down_by_state() {
        // 🛑 THE DIRECTIONS USE DIFFERENT MACHINERY AND THAT IS THE PROPERTY. Up is a RUNG applied
        // absolutely (`Apply`); down is a RATIO between two rungs, expressed off-label
        // (`Down`), because there is no rung below 1.0 to apply. What must never happen is `Apply`
        // firing downward: the rung would multiply on top of a base that already encodes the
        // native tier, which is an up-scale wearing a down-scale's name.
        let (npc, native) = crate::native_tiers::NATIVE_TIERS[0];
        let native = native as usize;
        for tier in 0..NUM_TIERS {
            match scale_action(false, false, npc, tier, None) {
                ScaleAction::Apply => assert!(
                    tier > native,
                    "Apply must never fire at or below native (tier {tier}, native {native})"
                ),
                ScaleAction::Down(s) => {
                    assert!(
                        tier < native,
                        "Down must never fire at or above native (tier {tier}, native {native})"
                    );
                    assert!(s.attack < 1.0);
                }
                ScaleAction::NoTouch => {}
                // Both are conditioned on a carried down state, and this enemy carries none, so
                // reaching either here would mean the guard has stopped guarding.
                ScaleAction::KeepDown | ScaleAction::ClearDown => {
                    panic!("no state is carried, so nothing can be kept or cleared")
                }
                ScaleAction::Replace => panic!("an unrunged enemy can never be Replaced"),
            }
        }
    }

    #[test]
    fn a_runged_enemy_is_always_replaced_in_both_directions() {
        // The 4,214-row mainline population is untouched by phase 1a: its rung IS its declared
        // native difficulty, so swapping rungs is a true re-tier and works downward already.
        for tier in 0..NUM_TIERS {
            assert_eq!(
                scale_action(true, false, -1, tier, None),
                ScaleAction::Replace
            );
            assert_eq!(
                scale_action(true, false, 40000000, tier, None),
                ScaleAction::Replace
            );
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
