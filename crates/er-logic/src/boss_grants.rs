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

/// Rykard's CHR id. Both phases are this one character: `NpcName` **904710000**
/// ("God-Devouring Serpent") and **904710001** ("Rykard, Lord of Blasphemy") are name index 0 and
/// 1 of chr `c4710`, and `c4700` has **no `NpcParam` rows at all**. So the serpent is not a second
/// character and this key covers the whole fight -- which matters, because the spear is the answer
/// to BOTH phases, not just the second (Alaric, 2026-08-06).
pub const RYKARD_CHR_ID: i32 = 4710;

/// 🛑🛑 HOW I GOT THIS WRONG THE FIRST TIME, because the same trap is one keystroke away.
///
/// I keyed this on `NpcParam.nameId`, having found that row `500020079` carries `nameId 160000`
/// and that **PlaceName** 160000 is "Rykard, Lord of Blasphemy". Both facts are true and the
/// conclusion was still wrong: `nameId` indexes **NpcName**, not PlaceName, and NpcName 160000 is
/// the **Twin Maiden Husks**. Two id spaces, one collision, and the shipped build granted the
/// spear in Roundtable Hold.
///
/// The check that would have caught it in one line: resolve the SAME id in both tables and see
/// that they disagree, or resolve a handful of OTHER `nameId`s (134800 Millicent, 121600 Blaidd,
/// 130900 Patches -- all NpcName, none in PlaceName). A single id that resolves in the table you
/// expected is not evidence that the table is the right one.
///
/// Boss rows carry `nameId 0`; a boss healthbar name comes from the EMEVD `DisplayBossHealthBar`
/// call, not from `NpcParam`. So there was never going to be a name-keyed answer here.
///
/// What replaced it is structural: `NpcParam` ids are CHR-ENCODED, `CCCC____`. Verified on two
/// independent characters -- `c4710` has 6 rows (47100000, 47100038, 47101000, 47101038, 47102000,
/// 47109000) and Torrent `c8000` has exactly one (80000000).
pub const RYKARD_NPC_PARAM_ROWS: &[i32] = &[
    47100000, 47100038, 47101000, 47101038, 47102000, 47109000,
];

/// 🛑 THE FLAG THIS MODULE MUST NEVER TOUCH -- see property 1.
pub const SERPENT_HUNTER_CHECK_FLAG: u32 = 16007690;

/// Does `row` (a resolved weapon param row, base + level) belong to `base`?
pub fn is_level_of(row: i32, base: i32) -> bool {
    row - (row % REINFORCE_STEP) == base
}

/// Does this `NpcParam` row belong to chr `chr_id`? Ids are chr-encoded `CCCC____`.
///
/// ⭐ A PREFIX test, not a membership test in [`RYKARD_NPC_PARAM_ROWS`]: a patch that adds a
/// seventh `c4710` row should be covered automatically, and the enumerated list is documentation
/// of what exists today rather than the gate.
///
/// 🛑 `npc_param_id`, NEVER `npc_id`. `npc_id` is the 4-digit CHR id (4710 here) and the two id
/// spaces OVERLAP -- passing one where the other belongs is how phase 1a shipped a silently wrong
/// native tier.
pub fn is_character(npc_param_id: i32, chr_id: i32) -> bool {
    npc_param_id / 10_000 == chr_id
}

/// Is this row one of Rykard's, in either phase?
pub fn is_rykard(npc_param_id: i32) -> bool {
    is_character(npc_param_id, RYKARD_CHR_ID)
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
    fn every_known_rykard_row_matches_and_neighbours_do_not() {
        for &row in RYKARD_NPC_PARAM_ROWS {
            assert!(is_rykard(row), "{row} is a c4710 row");
        }
        // Torrent (c8000, row 80000000) is the second character the CCCC____ convention was
        // verified on; it must never match.
        assert!(!is_rykard(80000000));
        assert!(is_character(80000000, 8000));
        assert!(!is_rykard(47110000), "c4711 is a different character");
        assert!(!is_rykard(0));
    }

    /// THE REGRESSION. 500020079 is the Twin Maiden Husks -- the row a PlaceName/NpcName id
    /// collision put here first, which granted the spear in Roundtable Hold.
    #[test]
    fn the_twin_maiden_husks_row_is_not_rykard() {
        assert!(!is_rykard(500020079));
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
