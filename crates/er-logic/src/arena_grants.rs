//! `arena_grants` -- hand the player the tool a specific ARENA assumes they walked in holding.
//!
//! MOTIVATING CASE (CONTRIBUTING rule 11), issue #413. Rykard, Lord of Blasphemy is a two-phase
//! fight whose second phase vanilla expects you to answer with the **Serpent-Hunter**, a unique
//! great spear that sits on a corpse in Volcano Manor. Under a randomiser that spear is a CHECK
//! like any other, so it can be anywhere in the multiworld -- and boblerrr, 2026-08-06: *"rykard
//! without serpent hunter is some bs"*. The fix is not to un-randomise the check; it is to make
//! the ARENA supply the tool it assumes, the way the game itself does.
//!
//! # Three properties, and why each is load-bearing
//!
//! 1. **The grant never collects the check.** The Serpent-Hunter's vanilla obtained-flag is
//!    [`SERPENT_HUNTER_CHECK_FLAG`] (16007690) -- which is exactly what apworld check **7771816**
//!    (`Mt. Gelmir :: Serpent-Hunter`) is keyed on. Latching this grant on that flag the way
//!    `unique_grants` latches the Steed Whistle on 60100 would SEND the check the moment we
//!    granted the copy, silently paying the multiworld an item the player never found. That is the
//!    already-filed "start grant flags are check flags" defect, and this module must never repeat
//!    it. Nothing here reads or writes 16007690; a test below enforces that mechanically.
//!
//! 2. **The latch is POSSESSION, not a flag.** Idempotency comes from a bag read -- does the
//!    player already hold any reinforce level of the base row? -- supplied by the caller
//!    (`eldenring-archipelago::upgrades::holds_weapon_base`). No flag is allocated, nothing
//!    persists, and the decision is correct after a reload, a save-scum, a fresh character, or a
//!    pool copy arriving first. Sell it and you get another next time you walk in; "do not take it
//!    back" is a constraint on US, not on the player.
//!
//! 3. **An unreadable bag is NOT "no".** `holds == None` means the walk could not resolve this
//!    tick, and it must never be read as "the player has nothing", because that direction
//!    DUPLICATES a unique weapon on every tick until the bag comes back. A missed grant simply
//!    retries; a wrong grant is permanent clutter. That asymmetry decides the match arm.
//!
//! # What this is NOT keyed on
//!
//! It is keyed on the ARENA (a place we control), never on the ENTITY standing in it. Under an
//! enemy randomiser Rykard can be anywhere and anything can be in his arena -- so such a player may
//! get a free spear in m16 and meet Rykard without one. That is inherent to keying on place, it is
//! the ruling's choice, and it is why a rig running one cannot be this feature's acceptance test.

use crate::hook::GameHook;

/// Serpent-Hunter, base (+0) weapon row. Reinforce levels occupy `base ..= base + 25` inside the
/// 100-wide id stride, which is why possession is tested on the BASE and not on this id.
pub const SERPENT_HUNTER_BASE: i32 = 17030000;

/// ER's weapon id stride per smithing level. Mirrors `eldenring-archipelago`'s `REINFORCE_STEP`;
/// duplicated rather than shared because that crate does not build off Windows.
pub const REINFORCE_STEP: i32 = 100;

/// Rykard's map as 5-digit play_region BUCKETS. `16000` is m16, Volcano Manor -- the whole legacy
/// dungeon, not the boss room. Deliberate: the spear is no use once the fog gate is behind you, so
/// the grant should land on the walk in.
pub const RYKARD_ARENA_SUBS: &[i32] = &[16000];

/// THE FLAG THIS MODULE MUST NEVER TOUCH -- see property 1. Check 7771816 is keyed on it, so
/// setting it pays the multiworld for an item the player never found.
pub const SERPENT_HUNTER_CHECK_FLAG: u32 = 16007690;

/// Interior play regions are 7-digit (`bucket * 100 + sub`); normalise to the 5-digit bucket the
/// area tables and the kick-watch both key on.
pub fn play_region_sub(play_region: i32) -> i32 {
    if play_region >= 1_000_000 {
        play_region / 100
    } else {
        play_region
    }
}

/// Is the player standing in one of `subs`? `None` (no resolvable region) is never "yes".
pub fn in_arena(play_region: Option<i32>, subs: &[i32]) -> bool {
    match play_region {
        Some(pr) => subs.contains(&play_region_sub(pr)),
        None => false,
    }
}

/// Does `row` (a resolved weapon param row, base + level) belong to `base`?
pub fn is_level_of(row: i32, base: i32) -> bool {
    row - (row % REINFORCE_STEP) == base
}

/// THE DECISION. `Some(full_id)` = grant it now; `None` = do nothing.
///
/// `holds` is the caller's bag read: `Some(true)` already has one, `Some(false)` does not, `None`
/// could not tell -- and `None` must behave like "already has one" (property 3).
pub fn arena_grant_action(play_region: Option<i32>, holds: Option<bool>) -> Option<i32> {
    if !in_arena(play_region, RYKARD_ARENA_SUBS) {
        return None;
    }
    match holds {
        Some(false) => Some(SERPENT_HUNTER_BASE),
        _ => None,
    }
}

/// Production adapter: read the region off the hook, act, and report what was granted.
///
/// A `false` from `grant_full_id` means no inventory pointer this tick -- we say nothing and the
/// next tick tries again, which is why the POSSESSION latch, and not a "we tried" flag, is what
/// stops the second copy.
pub fn tick(hook: &mut dyn GameHook, holds: Option<bool>) -> Option<String> {
    let full_id = arena_grant_action(hook.play_region_id(), holds)?;
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
    fn seven_digit_interior_regions_normalise_to_the_bucket() {
        assert_eq!(play_region_sub(1_600_000), 16000);
        assert_eq!(play_region_sub(16000), 16000);
        assert_eq!(play_region_sub(6_200_000), 62000);
    }

    #[test]
    fn the_arena_is_matched_in_both_id_shapes() {
        assert!(in_arena(Some(16000), RYKARD_ARENA_SUBS));
        assert!(in_arena(Some(1_600_000), RYKARD_ARENA_SUBS));
        assert!(!in_arena(Some(62000), RYKARD_ARENA_SUBS));
        assert!(!in_arena(None, RYKARD_ARENA_SUBS));
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
        let arena = Some(1_600_000);
        let want = Some(SERPENT_HUNTER_BASE);
        assert_eq!(arena_grant_action(arena, Some(false)), want);
        assert_eq!(arena_grant_action(arena, Some(true)), None);
        assert_eq!(arena_grant_action(arena, None), None);
        assert_eq!(arena_grant_action(Some(62000), Some(false)), None);
        assert_eq!(arena_grant_action(None, Some(false)), None);
    }

    /// PROPERTY 3, its own test because the wrong arm duplicates a unique weapon forever.
    #[test]
    fn an_unreadable_bag_never_grants() {
        for _ in 0..50 {
            assert_eq!(arena_grant_action(Some(1_600_000), None), None);
        }
    }

    /// PROPERTY 1, enforced mechanically rather than by review: no flag verb may appear in this
    /// module's body, because the only flag in scope here is the one check 7771816 is keyed on.
    #[test]
    fn the_check_flag_is_never_read_or_written() {
        assert_eq!(SERPENT_HUNTER_CHECK_FLAG, 16007690);
        let src = include_str!("arena_grants.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        for verb in ["get_event_flag", "set_event_flag"] {
            assert!(
                !body.contains(verb),
                "arena_grants calls {verb} -- the only flag in scope is check 7771816's, and \
                 setting it collects a check the player never found"
            );
        }
    }

    /// Repeat ticks in the arena settle: one grant, then the bag read stops it.
    #[test]
    fn repeat_ticks_grant_exactly_once_once_the_bag_reflects_it() {
        let mut held = false;
        let mut grants = 0;
        for _ in 0..10 {
            if arena_grant_action(Some(1_600_000), Some(held)).is_some() {
                grants += 1;
                held = true;
            }
        }
        assert_eq!(grants, 1);
    }
}
