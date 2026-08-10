//! The vetted no-op `SpEffectParam` rows this client repurposes — the single source of truth.
//!
//! WHY THIS FILE EXISTS. Two features already repurpose a vanilla `SpEffectParam` row by rewriting
//! its fields at runtime (`no_equip_load`, `no_fall_damage`). Until now the "vetted safe set" was
//! prose in two module docstrings, which means the set was not a set: nothing stopped a third
//! feature from silently claiming a row another feature was already mutating, and the two would
//! fight every tick with no error anywhere. A row is a SHARED RESOURCE. Claim it here or not at all.
//!
//! WHAT MAKES A ROW ELIGIBLE. All four, no exceptions:
//!
//! 1. **No-op in vanilla** — every field already at its neutral value, so applying the unmodified
//!    row to the player does nothing at all.
//! 2. **Silent** — `iconId = -1`, `vfxId = -1`: no status icon, no particle, nothing on screen.
//! 3. **Permanent** — `effectEndurance = -1`. A row with a finite duration expires out from under
//!    us; see `scadu_blessing` for what that costs (the vanilla blessing rungs are `0.05`s and are
//!    only alive because the engine re-applies them every tick, inside the DLC only).
//! 4. **Unreferenced** — the row id appears EXACTLY ONCE across all 239 vanilla param tables: as its
//!    own row. Nothing else in the regulation points at it, so editing it cannot reach any item,
//!    enemy, spell, or system.
//!
//! HOW TO VERIFY A CANDIDATE, without a game install:
//!
//! ```text
//! python tools/verify_safe_speffect_row.py <id>        # in the er-archipelago repo
//! ```
//!
//! It reads the full Smithbox param dump out of `gen_inputs.db` and checks all four properties,
//! including the cross-reference sweep over every table. Run it and paste the output in the PR —
//! do not reason from "the id next door was fine" (`prefer-datamine-over-runtime-read`).

/// `no_equip_load` — `allItemWeightChangeRate -> 0` (always light-roll).
pub const NO_EQUIP_LOAD: i32 = 20_012_080;

/// `no_fall_damage` — `fallDamageRate -> 0`.
pub const NO_FALL_DAMAGE: i32 = 20_010_827;

/// `scadu_blessing` — the Scadutree blessing clone (18 rate fields + `effectEndurance = -1`).
///
/// Verified 2026-07-31 against `gen_inputs.db` (`tools/verify_safe_speffect_row.py 20012081`):
/// field-identical to [`NO_EQUIP_LOAD`] in its vanilla state (so no-op, silent, permanent), and the
/// literal `20012081` occurs exactly once across all 239 param tables — only as its own row.
pub const SCADU_BLESSING: i32 = 20_012_081;

/// `traps::NoFlask` -- `changeHp/MpEstusFlaskCorrectRate -> 0` with a FINITE `effectEndurance`.
///
/// Verified 2026-08-10 (`python tools/verify_safe_speffect_row.py 20012082`):
///   ok    no-op, silent, permanent (identical to 20012080)
///   ok    unreferenced: occurs exactly once across all 239 param tables (itself)
///
/// 🛑 Note the tension this row is the first to carry, and it is not a contradiction. Eligibility
/// demands `effectEndurance -1` IN VANILLA -- so the row cannot expire out from under a feature
/// that wants it permanent -- while the trap writes a finite endurance itself at fire time. The
/// duration IS the param field; that is the whole mechanism (`er_logic::traps`).
pub const TRAP_NO_FLASK: i32 = 20_012_082;

/// Every claimed row, for the duplicate-claim test below. Add here in the same commit that claims.
pub const CLAIMED: [(i32, &str); 4] = [
    (NO_EQUIP_LOAD, "no_equip_load"),
    (NO_FALL_DAMAGE, "no_fall_damage"),
    (SCADU_BLESSING, "scadu_blessing"),
    (TRAP_NO_FLASK, "traps::no_flask"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the file: two features must never claim the same row. This is cheap and it
    /// is the one thing a human reviewer will not catch by eye in a diff that touches three modules.
    #[test]
    fn no_row_is_claimed_twice() {
        for (i, (row_a, owner_a)) in CLAIMED.iter().enumerate() {
            for (row_b, owner_b) in CLAIMED.iter().skip(i + 1) {
                assert_ne!(
                    row_a, row_b,
                    "SpEffect row {row_a} is claimed by both `{owner_a}` and `{owner_b}` — they will \
                     overwrite each other's fields every tick"
                );
            }
        }
    }

    /// Pins the constants against a careless edit. These ids are load-bearing: a typo'd digit is a
    /// row that may NOT be a vanilla no-op, and the failure mode is a silent gameplay change.
    #[test]
    fn claimed_ids_are_the_verified_ones() {
        assert_eq!(NO_EQUIP_LOAD, 20012080);
        assert_eq!(NO_FALL_DAMAGE, 20010827);
        assert_eq!(SCADU_BLESSING, 20012081);
        assert_eq!(TRAP_NO_FLASK, 20012082);
    }

    /// 🛑 Our clone row must not collide with the vanilla Scadutree ladder (`20000100..=20000120`).
    /// Staying outside that range is one of the three reasons Lever D is immune to whatever the
    /// engine uses to scope the blessing to the DLC.
    #[test]
    fn scadu_clone_is_outside_the_vanilla_ladder() {
        assert!(!(20000100..=20000120).contains(&SCADU_BLESSING));
    }
}
