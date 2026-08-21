//! The Serpent-Hunter's wave moveset, gated to the Rykard FIGHT (#345; reverses the
//! "everywhere, deliberately" ruling that shipped 2026-08-07).
//!
//! THE FACT THIS RESTS ON, AND ITS PROVENANCE. The spear's wind waves are NOT a property of the
//! weapon in vanilla: `EquipParamWeapon` row 17030000 ships `-1` in all three RESIDENT SpEffect
//! fields, and the game turns the moveset on from somewhere else during the Rykard fight. Applying
//! SpEffect **1908** to the wielder enables it -- Digones, "Keep Serpent-Hunter Spear special
//! moveset" (Nexus 642). Measured 2026-08-07 19:56:29: the resident-slot param write binds at
//! EQUIP time only, so the client applies 1908 to the PLAYER directly.
//!
//! ⭐ THE RULING, 2026-08-21 (#345, Alaric): the waves are ON only while the Rykard fight is
//! ACTIVE. The first ruling made them global -- "'only in the vanilla arena' is not a behaviour
//! worth preserving", because enemy rando can move Rykard into any arena (measured: a Rykard
//! healthbar in play region 68410) and ARENA-keyed gating would miss him. That reasoning stands;
//! the conclusion changed because the gate is FIGHT-keyed, not arena-keyed: `healthbar_shows`
//! follows Rykard wherever the randomiser puts him (#594's lesson, reused from the grant). With
//! the spear a randomized item any player can receive early, an always-on screen-wide wave attack
//! is a balance hole everywhere except the one fight it exists for.
//!
//! This module is the PURE half: one total decision function the client crate obeys. The I/O
//! (SpEffect application/removal, the row probe) lives in the game crate.

/// The SpEffect that gives the spear its Rykard-fight moveset. See the module doc for provenance.
pub const WAVE_SPEFFECT: i32 = 1908;

/// `EquipParamWeapon` row id of the Serpent-Hunter, mirrored from
/// [`crate::boss_grants::SERPENT_HUNTER_BASE`] so this module reads standalone.
pub const SERPENT_HUNTER_ROW: i32 = crate::boss_grants::SERPENT_HUNTER_BASE;

/// What the per-tick wave keeper should do to the player's SpEffect list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveAction {
    /// Put 1908 on the player.
    Apply,
    /// Take 1908 off the player.
    Remove,
    /// Touch nothing this tick.
    Nothing,
}

/// Decide the wave action. Total over every read outcome; the reads are the caller's.
///
/// * `fight_on` -- `healthbar_shows(RYKARD_CHR_ID, ..)`: `None` is a FAILED READ and freezes the
///   hand (never apply, never strip, off one blink -- the same discipline `probe_fight` keeps).
/// * `holds` -- does the bag hold a Serpent-Hunter (`None` = bag unreadable).
/// * `present` -- is 1908 on the player right now.
///
/// 🛑 NEVER strips mid-fight, even on an unreadable bag: taking the moveset away DURING the one
/// fight it exists for is the harm case this feature must be incapable of. Outside the fight the
/// effect has no business on the player regardless of what the bag says, so `Remove` ignores
/// `holds` entirely.
pub fn wave_action(fight_on: Option<bool>, holds: Option<bool>, present: bool) -> WaveAction {
    match fight_on {
        None => WaveAction::Nothing,
        Some(true) => {
            if holds == Some(true) && !present {
                WaveAction::Apply
            } else {
                WaveAction::Nothing
            }
        }
        Some(false) => {
            if present {
                WaveAction::Remove
            } else {
                WaveAction::Nothing
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_the_fight_a_held_spear_gets_the_waves() {
        // MOTIVATING CASE (rule 11), unchanged from the first ruling: boblerrr 2026-08-07 15:23,
        // "it equipped but weapon dont work" -- in the fight, spear in hand, effect absent.
        assert_eq!(
            wave_action(Some(true), Some(true), false),
            WaveAction::Apply
        );
        // ...and once present, the keeper is quiet.
        assert_eq!(
            wave_action(Some(true), Some(true), true),
            WaveAction::Nothing
        );
    }

    #[test]
    fn outside_the_fight_the_waves_come_off() {
        // THE #345 CASE, the inversion of the one above: fight over, effect still on the player.
        assert_eq!(
            wave_action(Some(false), Some(true), true),
            WaveAction::Remove
        );
        // `holds` is irrelevant to removal -- the effect has no business there either way.
        assert_eq!(wave_action(Some(false), None, true), WaveAction::Remove);
        assert_eq!(
            wave_action(Some(false), Some(false), true),
            WaveAction::Remove
        );
        // Nothing to remove, nothing to do.
        assert_eq!(
            wave_action(Some(false), Some(true), false),
            WaveAction::Nothing
        );
    }

    #[test]
    fn a_failed_healthbar_read_freezes_the_hand() {
        // 🛑 A blink mid-fight must not strip, and a blink outside must not apply.
        assert_eq!(wave_action(None, Some(true), false), WaveAction::Nothing);
        assert_eq!(wave_action(None, Some(true), true), WaveAction::Nothing);
    }

    #[test]
    fn mid_fight_nothing_is_ever_stripped() {
        // 🛑 The harm case this feature must be incapable of: bag unreadable (or the spear
        // dropped mid-fight) while the bar is up -- the effect STAYS.
        assert_eq!(wave_action(Some(true), None, true), WaveAction::Nothing);
        assert_eq!(
            wave_action(Some(true), Some(false), true),
            WaveAction::Nothing
        );
    }

    #[test]
    fn an_empty_bag_in_the_fight_gets_nothing() {
        // The grant path is what hands the spear over; the keeper never applies to a bare hand.
        assert_eq!(
            wave_action(Some(true), Some(false), false),
            WaveAction::Nothing
        );
        assert_eq!(wave_action(Some(true), None, false), WaveAction::Nothing);
    }

    #[test]
    fn the_row_and_speffect_ids_are_the_published_ones() {
        // Pinned in exact values: these two numbers ARE the mechanism, and a typo in either is a
        // silent no-op in game with nothing in the log to say so.
        assert_eq!(WAVE_SPEFFECT, 1908);
        assert_eq!(SERPENT_HUNTER_ROW, 17_030_000);
    }
}
