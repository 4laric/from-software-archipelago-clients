//! `boss_grants` -- give the player the tool a specific BOSS assumes they arrived holding,
//! wherever that boss happens to be.
//!
//! MOTIVATING CASE (rule 11), issue #413. Rykard's second phase is built around the
//! **Serpent-Hunter**, a unique great spear that vanilla parks in Volcano Manor on the way to him.
//! A randomiser scatters that spear into the multiworld, so the fight can demand a tool the player
//! has no way to hold. boblerrr, 2026-08-06: *"rykard without serpent hunter is some bs"*.
//!
//! # 🛑 KEYED ON THE CHARACTER, NOT THE PLACE -- and that is the whole design
//!
//! A first cut keyed this on Rykard's ARENA (play_region bucket 16000, m16). **Alaric rejected it,
//! 2026-08-06: it has to fire whenever you fight Rykard, no matter where Rykard is.** An enemy
//! randomiser moves bosses between arenas, so a place key grants the spear to whoever inherited
//! his room and gives Rykard's actual opponent nothing. The place is the one thing about this
//! fight that is NOT stable.
//!
//! What IS stable is the character. `NpcParam.nameId` is a **PlaceName** id -- the boss-healthbar
//! label -- and PlaceName [`RYKARD_PLACE_NAME_ID`] (160000) is "Rykard, Lord of Blasphemy",
//! carried by exactly one row in all 7,039 of `NpcParam`: [`RYKARD_NPC_PARAM_IDS`]. A character
//! brings its `NpcParam` row with it wherever it spawns, so this follows him.
//!
//! ⭐ This is the same keying `AREA_EXCLUDED` already uses ("keyed PER CHARACTER via `nameId`, NOT
//! per row"), for the same reason: the row says what an instance is, the character says who it is.
//!
//! # Three properties, each load-bearing
//!
//! 1. **The grant never collects the check.** The Serpent-Hunter's vanilla obtained-flag is
//!    [`SERPENT_HUNTER_CHECK_FLAG`] (16007690) -- exactly what apworld check **7771816** is keyed
//!    on. Latching on it the way `unique_grants` latches the Steed Whistle on 60100 would SEND the
//!    check the moment we granted the copy, paying the multiworld for an item the player never
//!    found. That is the filed "start grant flags are check flags" defect. A test greps this
//!    module's own body for the flag verbs so the property cannot rot.
//!
//! 2. **The latch is POSSESSION.** A bag read for any reinforce level of the base row. Nothing
//!    allocated, nothing persisted; correct across reload, save-scum, a fresh character, or the
//!    pool copy arriving first.
//!
//! 3. **"Don't know" is never "no".** BOTH inputs are `Option<bool>`, and either one being `None`
//!    (character set unwalkable / bag unresolvable this tick) must behave like "do nothing". The
//!    other direction duplicates a unique weapon every tick until the read comes back. A missed
//!    grant simply retries on the next one.

use crate::hook::GameHook;

/// Serpent-Hunter, base (+0) weapon row. Reinforce levels occupy `base ..= base + 25` inside the
/// 100-wide stride, which is why possession is tested on the BASE.
pub const SERPENT_HUNTER_BASE: i32 = 17030000;

/// ER's weapon id stride per smithing level. Mirrors `eldenring-archipelago`'s `REINFORCE_STEP`;
/// duplicated because that crate does not build off Windows.
pub const REINFORCE_STEP: i32 = 100;

/// PlaceName id "Rykard, Lord of Blasphemy" -- the boss-healthbar label `NpcParam.nameId` points
/// at. Recorded so the derivation below can be re-run rather than trusted.
pub const RYKARD_PLACE_NAME_ID: i32 = 160000;

/// Every `NpcParam` row whose `nameId` is [`RYKARD_PLACE_NAME_ID`]. Derived from the vanilla
/// `NpcParam.csv` in `gen_inputs.db`, 2026-08-06: exactly ONE row of 7,039.
///
/// 🔎 OPEN, and cheap to settle: the phase-1 God-Devouring Serpent has an `NpcName` entry
/// (904710000) but NO PlaceName row, so it is either the same character or a row with `nameId 0`
/// that this cannot see. If it is separate, the grant lands at the phase transition instead of at
/// the encounter start -- still in time to matter, since phase 2 is what the spear is for. The
/// scaling census already prints `npc_param_id`s per region, so ONE log from that fight names
/// every row in the arena and closes this.
pub const RYKARD_NPC_PARAM_IDS: &[i32] = &[500020079];

/// 🛑 THE FLAG THIS MODULE MUST NEVER TOUCH -- see property 1.
pub const SERPENT_HUNTER_CHECK_FLAG: u32 = 16007690;

/// Does `row` (a resolved weapon param row, base + level) belong to `base`?
pub fn is_level_of(row: i32, base: i32) -> bool {
    row - (row % REINFORCE_STEP) == base
}

/// Is `npc_param_id` one of the character's rows?
pub fn is_rykard(npc_param_id: i32) -> bool {
    RYKARD_NPC_PARAM_IDS.contains(&npc_param_id)
}

/// THE DECISION. `Some(full_id)` = grant now; `None` = do nothing.
///
/// `present` = is the character loaded, `holds` = does the player already have one. Either input
/// being `None` means the read failed this tick, and both failure modes resolve to "do nothing"
/// (property 3).
pub fn boss_grant_action(present: Option<bool>, holds: Option<bool>) -> Option<i32> {
    match (present, holds) {
        (Some(true), Some(false)) => Some(SERPENT_HUNTER_BASE),
        _ => None,
    }
}

/// Production adapter. A `false` from `grant_full_id` means no inventory pointer this tick; we say
/// nothing and the next tick retries, which is why the POSSESSION latch and not a "we tried" flag
/// is what stops the second copy.
pub fn tick(hook: &mut dyn GameHook, present: Option<bool>, holds: Option<bool>) -> Option<String> {
    let full_id = boss_grant_action(present, holds)?;
    if hook.grant_full_id(full_id, 1) {
        Some("Serpent-Hunter granted for Rykard".to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rykard_is_one_known_character_row() {
        assert_eq!(RYKARD_NPC_PARAM_IDS, &[500020079]);
        assert_eq!(RYKARD_PLACE_NAME_ID, 160000);
        assert!(is_rykard(500020079));
        assert!(!is_rykard(500020078));
        assert!(!is_rykard(0));
    }

    #[test]
    fn reinforce_levels_all_belong_to_the_base() {
        for level in 0..=25 {
            assert!(is_level_of(SERPENT_HUNTER_BASE + level, SERPENT_HUNTER_BASE));
        }
        assert!(!is_level_of(SERPENT_HUNTER_BASE + 100, SERPENT_HUNTER_BASE));
        assert!(!is_level_of(SERPENT_HUNTER_BASE - 1, SERPENT_HUNTER_BASE));
    }

    #[test]
    fn decision_truth_table() {
        let want = Some(SERPENT_HUNTER_BASE);
        assert_eq!(boss_grant_action(Some(true), Some(false)), want);
        assert_eq!(boss_grant_action(Some(true), Some(true)), None);
        assert_eq!(boss_grant_action(Some(false), Some(false)), None);
        assert_eq!(boss_grant_action(Some(false), Some(true)), None);
    }

    /// PROPERTY 3, its own test on BOTH inputs: the wrong arm duplicates a unique weapon forever.
    #[test]
    fn an_unknown_read_never_grants() {
        for holds in [Some(true), Some(false), None] {
            assert_eq!(boss_grant_action(None, holds), None, "unknown presence granted");
        }
        for present in [Some(true), Some(false), None] {
            assert_eq!(boss_grant_action(present, None), None, "unknown bag granted");
        }
    }

    /// PROPERTY 1, enforced mechanically: no flag verb may appear in this module's body, because
    /// the only flag in scope is the one check 7771816 is keyed on.
    #[test]
    fn the_check_flag_is_never_read_or_written() {
        assert_eq!(SERPENT_HUNTER_CHECK_FLAG, 16007690);
        let src = include_str!("boss_grants.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        for verb in ["get_event_flag", "set_event_flag"] {
            assert!(
                !body.contains(verb),
                "boss_grants calls {verb} -- the only flag in scope is check 7771816's, and \
                 setting it collects a check the player never found"
            );
        }
    }

    /// Repeat ticks while the boss is loaded settle: one grant, then the bag read stops it.
    #[test]
    fn repeat_ticks_grant_exactly_once() {
        let mut held = false;
        let mut grants = 0;
        for _ in 0..10 {
            if boss_grant_action(Some(true), Some(held)).is_some() {
                grants += 1;
                held = true;
            }
        }
        assert_eq!(grants, 1);
    }
}
