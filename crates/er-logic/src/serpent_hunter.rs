//! The Serpent-Hunter's wave, forced on (#413 follow-up; boblerrr 2026-08-07 15:23 "it equipped
//! but weapon dont work" -> Alaric "oh like it doesn't have the special effect?" -> "exactly").
//!
//! THE FACT THIS RESTS ON, AND ITS PROVENANCE. The spear's wind waves are NOT a property of the
//! weapon in vanilla: `EquipParamWeapon` row 17030000 ships `-1` in all three of its RESIDENT
//! SpEffect fields, and the game turns the moveset on from somewhere else during the Rykard fight.
//! Writing SpEffect **1908** into one of those three fields is the community's standing fix --
//! Digones, "Keep Serpent-Hunter Spear special moveset" (Nexus 642), whose install notes are
//! verbatim: "change the ID of -1 to 1908 to one of three fields 'passive SpEffect'. That will
//! make the weapon have the moveset used in the fight."
//!
//! 🛑🛑 WHAT IS *NOT* MEASURED, STATED UP FRONT BECAUSE THE PROBE EXISTS FOR IT. Nothing here has
//! measured HOW VANILLA APPLIES 1908 -- whether the arena puts it on the player, or the wave is
//! gated on the TARGET being the God-Devouring Serpent. The wikis say the effect does not fire
//! against other serpents, which leans target-gated, and if it IS target-gated then a resident
//! SpEffect on the wielder may not be sufficient on its own. That is exactly why the client half
//! logs the player's whole active SpEffect list during the fight: if this write does not work,
//! the log says what the game actually had, and the next step is a reading rather than a guess.
//!
//! ⚠️ The Nexus mod also ships `sfxbnd_commoneffects.ffxbnd`, which implies the wave's SFX is not
//! resident outside Rykard's map. A pure-runtime client cannot ship that file. It should not
//! matter for the case we care about -- the player is INSIDE the fight, where the SFX is already
//! loaded -- but a seed that wants the waves everywhere is a different, larger job.

/// The SpEffect that gives the spear its Rykard-fight moveset. See the module doc for provenance.
pub const WAVE_SPEFFECT: i32 = 1908;

/// `EquipParamWeapon` row id of the Serpent-Hunter, mirrored from
/// [`crate::boss_grants::SERPENT_HUNTER_BASE`] so this module reads standalone.
pub const SERPENT_HUNTER_ROW: i32 = crate::boss_grants::SERPENT_HUNTER_BASE;

/// The "no SpEffect" sentinel in an `EquipParamWeapon` resident slot.
///
/// 🛑 ONLY `-1`, never `0`. SpEffect row 0 is a REAL row in ER, so treating 0 as free would let
/// this clobber a weapon that legitimately carries it. The mod's own notes say the vanilla
/// Serpent-Hunter slots read `-1`, so the narrow sentinel loses us nothing on the row we target.
pub const EMPTY_SLOT: i32 = -1;

/// What to do with the three resident SpEffect slots of a weapon row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentWrite {
    /// The SpEffect is already in a slot -- do nothing. Keeps the write idempotent across the
    /// per-tick retry AND across a map load that restored the vanilla row and then got rewritten.
    AlreadyPresent(usize),
    /// Write it into this slot index (0-based over `resident_sp_effect_id{,1,2}`).
    Slot(usize),
    /// Every slot is occupied by something else. 🛑 We do NOT clobber: a resident SpEffect we did
    /// not put there is a vanilla behaviour (or another mod's), and silently dropping it to force
    /// a cosmetic-ish moveset is a worse bug than the one being fixed. The caller logs this loudly
    /// -- on the vanilla row it cannot happen, so if it ever fires, something else edited the row
    /// first and that is the news.
    NoFreeSlot,
}

/// Decide which resident slot takes `want`, given the row's current three.
///
/// Total and order-stable: presence beats writing, and the lowest free index wins so a re-run
/// after a param reload lands in the same slot and the log stays comparable across sessions.
pub fn resident_write(resident: [i32; 3], want: i32) -> ResidentWrite {
    if let Some(i) = resident.iter().position(|&v| v == want) {
        return ResidentWrite::AlreadyPresent(i);
    }
    match resident.iter().position(|&v| v == EMPTY_SLOT) {
        Some(i) => ResidentWrite::Slot(i),
        None => ResidentWrite::NoFreeSlot,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vanilla_row_takes_slot_zero() {
        // MOTIVATING CASE (rule 11). The mod's notes say the vanilla Serpent-Hunter carries -1 in
        // all three, which is the shape this has to handle first.
        assert_eq!(
            resident_write([-1, -1, -1], WAVE_SPEFFECT),
            ResidentWrite::Slot(0)
        );
    }

    #[test]
    fn a_row_we_already_wrote_is_left_alone() {
        // The client retries per tick until the params are up, and re-arms after every map load.
        // Without this the same id would be written into all three slots over three ticks.
        for i in 0..3 {
            let mut r = [-1; 3];
            r[i] = WAVE_SPEFFECT;
            assert_eq!(
                resident_write(r, WAVE_SPEFFECT),
                ResidentWrite::AlreadyPresent(i)
            );
        }
    }

    #[test]
    fn an_occupied_slot_is_never_clobbered() {
        // 🛑 Vanilla content in a resident slot outranks our moveset write.
        assert_eq!(
            resident_write([100, 200, 300], WAVE_SPEFFECT),
            ResidentWrite::NoFreeSlot
        );
        // ...and a partially-occupied row takes the first FREE index, not index 0.
        assert_eq!(
            resident_write([100, -1, -1], WAVE_SPEFFECT),
            ResidentWrite::Slot(1)
        );
        assert_eq!(
            resident_write([100, 200, -1], WAVE_SPEFFECT),
            ResidentWrite::Slot(2)
        );
    }

    #[test]
    fn zero_is_a_real_speffect_row_and_not_a_free_slot() {
        // 🛑 THE REGRESSION THIS EXISTS FOR. `0` is a valid SpEffect row id in ER; treating it as
        // "empty" would delete it. A row of zeroes therefore has NO free slot.
        assert_eq!(
            resident_write([0, 0, 0], WAVE_SPEFFECT),
            ResidentWrite::NoFreeSlot
        );
        assert_eq!(
            resident_write([0, -1, 0], WAVE_SPEFFECT),
            ResidentWrite::Slot(1)
        );
    }

    #[test]
    fn the_row_and_speffect_ids_are_the_published_ones() {
        // Pinned in exact values: these two numbers ARE the fix, and a typo in either is a silent
        // no-op in game with nothing in the log to say so.
        assert_eq!(WAVE_SPEFFECT, 1908);
        assert_eq!(SERPENT_HUNTER_ROW, 17_030_000);
    }
}
