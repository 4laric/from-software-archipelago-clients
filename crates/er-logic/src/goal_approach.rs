//! Warn a player standing at the goal arena that the ending will not count yet (world#694).
//!
//! # The motivating case
//!
//! bobler, Discord, 2026-08-15, on a seed where the goal region opened early:
//!
//! > it should still on either ashen or enir always so it should always give you this last **or it
//! > just ends randomly in a weird way**
//!
//! That "weird way" is the shipped behaviour working as designed. `goalRequiredItems` folds the
//! kept Region Locks into `goal.rs`'s `item_goals` and `is_met` waits for them -- so a player who
//! reaches Radagon one Lock deep (**25% of seeds** at the rolled 6-region default, measured in
//! world#232) fights the boss, watches the ending, takes the credits, and lands in a post-ending
//! save with no victory sent. #233 names that outcome and accepts it.
//!
//! Expected, and still bad: the player has spent the ending, and the one irreversible thing in the
//! run happened at the wrong time.
//!
//! # 🛑 THIS IS OPTION C, AND IT DELIBERATELY BLOCKS NOTHING
//!
//! Alaric's ruling, 2026-08-15: *"i think C for 694 for now, the permanent fix is B but im not sure
//! exactly how to do it"*.
//!
//! **The complaint is SURPRISE, not reachability.** A player who knows the ending will not count
//! can walk in anyway; one who does not know burns it and finds out from a spoiler log. So this
//! module warns and returns -- it has no authority over anything, and therefore no way to fail
//! closed.
//!
//! That matters because option B -- gate the arena -- is this repo's known softlock shape. #589
//! records that disarming `leyndell_gate` is a softlock and `area_locks` is on record as born
//! softlocked, and the datamine on #694 killed the gentler version of B outright: Enir Ilim's gate
//! reads `PlayerHasItem(Goods, 2008021)` at five sites and **zero flags**, and the Erdtree needs no
//! key at all, so there is no vanilla flag to withhold. B is the KICK, which is exactly the
//! mechanism with a filed precedent for making seeds unwinnable. A toast has none of that.
//!
//! # Why the count and not the names
//!
//! Per the ruling: *"Name the count, not the regions: the count is the actionable number and the
//! names are a hint the player may not have paid for."* A multiworld player can buy hints; handing
//! out which regions still owe a Lock would give away placement for free.

/// Region Locks the goal still needs that the player has not received.
///
/// 🛑 LOCKS ONLY. `item_goals` is the union of `goalRequiredItems` (the kept Region Locks) and
/// `great_rune_items`, and a `great_runes` seed needs both -- but the two gates must not shadow
/// each other, so this counts the Lock half and says so. Filtered on the `" Lock"` suffix, the same
/// convention [`crate::region_lock::region_of_lock_item`] strips.
///
/// `natural_progression: true` mints no Lock items at all, so `item_goals` carries none and this is
/// 0 -- the notice is absent rather than permanently shut, which is the constraint #694 sets out.
pub fn outstanding_locks(item_goals: &[String], has_item: &dyn Fn(&str) -> bool) -> usize {
    item_goals
        .iter()
        .filter(|n| n.ends_with(" Lock"))
        .filter(|n| !has_item(n))
        .count()
}

/// The line the player sees. ASCII only -- it is an in-game toast (repo rule 10).
///
/// `None` at zero outstanding, so a player who has everything is told nothing.
pub fn approach_warning(outstanding: usize) -> Option<String> {
    if outstanding == 0 {
        return None;
    }
    Some(format!(
        "{outstanding} Region Lock(s) outstanding -- the ending will not count yet."
    ))
}

/// Says it once per arrival at the arena, not once per tick and not once per reload.
///
/// Wraps [`crate::region_lock::EnforcementLatch`]'s rising edge rather than re-deriving one: the
/// kick has used that latch for the same job since it shipped, and a second hand-rolled edge
/// detector is how two copies drift.
#[derive(Debug, Clone, Default)]
pub struct ApproachNotice {
    latch: crate::region_lock::EnforcementLatch,
}

impl ApproachNotice {
    pub const fn new() -> Self {
        Self {
            latch: crate::region_lock::EnforcementLatch::new(),
        }
    }

    /// `Some(line)` on the tick the player arrives at the goal arena still owing Locks.
    ///
    /// Re-arms when they leave, so walking out and back in warns again -- which is the right
    /// behaviour for a notice whose whole job is to be seen before an irreversible act.
    pub fn poll(
        &mut self,
        in_goal_arena: bool,
        item_goals: &[String],
        has_item: &dyn Fn(&str) -> bool,
    ) -> Option<String> {
        let outstanding = outstanding_locks(item_goals, has_item);
        if !self.latch.fire(in_goal_arena && outstanding > 0) {
            return None;
        }
        approach_warning(outstanding)
    }
}

#[cfg(test)]
mod replay {
    use super::*;

    fn v(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    const KEPT: [&str; 3] = ["Liurnia Lock", "Rauh Base Lock", "Ashen Capital Lock"];

    /// ⭐ THE RED-FIRST ASSERTION, and it is bobler's case: the player reaches the goal arena one
    /// Lock deep and today is told nothing at all.
    #[test]
    fn arriving_at_the_arena_one_lock_deep_warns() {
        let mut n = ApproachNotice::new();
        let held = |name: &str| name != "Ashen Capital Lock";
        let line = n
            .poll(true, &v(&KEPT), &held)
            .expect("arriving with a Lock outstanding must warn");
        assert!(line.contains("1 Region Lock(s) outstanding"), "{line}");
        assert!(
            line.contains("will not count"),
            "the player has to know the ending is the thing at stake: {line}"
        );
    }

    /// 🛑 IT SAYS THE COUNT AND NOT THE REGIONS. Per the ruling: the names are a hint a multiworld
    /// player may not have paid for.
    #[test]
    fn the_warning_never_names_a_region() {
        let line = approach_warning(3).unwrap();
        for name in KEPT {
            assert!(!line.contains(name), "leaked a placement hint: {line}");
            assert!(
                !line.contains(crate::region_lock::region_of_lock_item(name)),
                "leaked a region name: {line}"
            );
        }
    }

    /// Once per arrival, not once per tick -- the notice runs on the same cadence as the kick.
    #[test]
    fn it_speaks_once_per_arrival() {
        let mut n = ApproachNotice::new();
        let held = |_: &str| false;
        assert!(n.poll(true, &v(&KEPT), &held).is_some());
        for _ in 0..600 {
            assert_eq!(n.poll(true, &v(&KEPT), &held), None, "one line per arrival");
        }
        // Leave and come back: a notice whose job is to precede an irreversible act says it again.
        assert_eq!(n.poll(false, &v(&KEPT), &held), None);
        assert!(n.poll(true, &v(&KEPT), &held).is_some());
    }

    /// 🛑 A PLAYER WHO HOLDS EVERY LOCK IS TOLD NOTHING. This is the case where the ending DOES
    /// count, and a warning there would be actively wrong.
    #[test]
    fn holding_every_lock_is_silent() {
        let mut n = ApproachNotice::new();
        assert_eq!(n.poll(true, &v(&KEPT), &|_| true), None);
        assert_eq!(outstanding_locks(&v(&KEPT), &|_| true), 0);
        assert_eq!(approach_warning(0), None);
    }

    /// 🛑 `natural_progression: true` MINTS NO LOCKS, so the notice is ABSENT rather than
    /// permanently shut -- #694's explicit constraint, and the shape #589 is about.
    #[test]
    fn natural_progression_is_silent_not_stuck() {
        let mut n = ApproachNotice::new();
        assert_eq!(n.poll(true, &[], &|_| false), None);
    }

    /// Great Runes share `item_goals` and are NOT Locks: a `great_runes` seed must not have one
    /// gate shadow the other, and the count has to stay the Lock count.
    #[test]
    fn great_runes_are_not_counted_as_locks() {
        let goals = v(&[
            "Godrick's Great Rune",
            "Malenia's Great Rune",
            "Liurnia Lock",
        ]);
        assert_eq!(
            outstanding_locks(&goals, &|_| false),
            1,
            "one Lock, two runes"
        );
        let mut n = ApproachNotice::new();
        let line = n.poll(true, &goals, &|_| false).unwrap();
        assert!(line.contains("1 Region Lock(s)"), "{line}");
    }

    /// Away from the arena nothing is said, however many Locks are outstanding -- this notice is
    /// about the moment before the ending, not a running todo list.
    #[test]
    fn elsewhere_in_the_world_it_is_quiet() {
        let mut n = ApproachNotice::new();
        assert_eq!(n.poll(false, &v(&KEPT), &|_| false), None);
    }

    /// In-game strings are ASCII (repo rule 10).
    #[test]
    fn the_warning_is_ascii() {
        for k in [1usize, 2, 17] {
            let line = approach_warning(k).unwrap();
            assert!(line.is_ascii(), "{line}");
        }
    }
}
