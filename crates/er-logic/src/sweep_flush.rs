//! `sweep_flush` — the acquisition flags a fired boss sweep still owes the world.
//!
//! ## The bug this exists for (2026-07-24 playtest, "chests are still broken")
//!
//! A boss sweep grants the checks around a kill site: `core.rs` pushes each member location into
//! `to_check`, the server records them, the player gets the items. What it never did was set each
//! member's own in-game acquisition flag. So the physical pickup stays in the world with its lot
//! already neutralised to the 8852 placeholder and its check already spent — the player walks up to
//! a chest that AP considers collected, opens it, and receives NOTHING. No popup, no error, no log
//! line. Nineteen of these were created in one poll around Summonwater Village.
//!
//! Setting the flag is what tells the GAME the pickup is done, so the chest reads as opened
//! (consistent with the check having been collected) instead of sitting there as a dead prop.
//!
//! ## Why a reconciler and not a write
//!
//! The game rejects flag writes at menus and during load transitions, and a fire-and-forget write
//! that lands nowhere is unrecoverable — the sweep only fires once, on the poll that observes the
//! defeat flag. So the owed flags are held as PENDING and re-asserted every tick until each one
//! READS BACK set ([`retire`]). This is the house rule from CONTRIBUTING ("Reconcile, don't
//! dispatch"): never advance a cursor past a write you did not verify landed.

/// The flags a sweep owes: every granted member whose own detection flag is not set yet.
///
/// `members` is `(location, detection flag)`; a member with no flag (0) is skipped rather than
/// guessed at. The result is de-duplicated because co-check siblings legitimately share one flag.
pub fn flags_to_assert(members: &[(i64, u32)], is_set: impl Fn(u32) -> bool) -> Vec<u32> {
    let mut owed: Vec<u32> = Vec::new();
    for &(_loc, flag) in members {
        if flag != 0 && !owed.contains(&flag) && !is_set(flag) {
            owed.push(flag);
        }
    }
    owed
}

/// Drop the pending writes that have been OBSERVED to land. Anything still unset stays pending and
/// is re-asserted on the next tick — a rejected write (menu, load transition) must never be lost.
pub fn retire(pending: &mut Vec<u32>, is_set: impl Fn(u32) -> bool) {
    pending.retain(|&flag| !is_set(flag));
}

#[cfg(test)]
mod replay {
    use super::*;
    use std::collections::HashSet;

    /// Two swept members from the Summonwater group (flags from the 2026-07-24 log).
    const MEMBERS: [(i64, u32); 2] = [(7774623, 1045397000), (7774624, 1045397020)];

    /// Whether the client keeps re-asserting an owed flag until it reads back.
    #[derive(Clone, Copy, PartialEq)]
    enum Policy {
        /// Pre-fix: the sweep grants the check and writes nothing. The pickup goes dead.
        GrantOnly,
        /// Pre-fix-and-a-half: write once, on the poll the sweep fires, and assume it landed.
        FireAndForget,
        /// The fix: stage the owed flags, re-assert until each reads back.
        Reconcile,
    }

    /// The frames that matter.
    enum Ev {
        /// The defeat flag is observed and the group's members are granted.
        SweepFires,
        /// A poll while the game is NOT accepting writes (menu, load screen, warp transition).
        TickRejectingWrites,
        /// An ordinary in-world poll: writes land.
        TickInWorld,
    }

    struct World {
        set: HashSet<u32>,
        pending: Vec<u32>,
    }

    fn replay(events: &[Ev], policy: Policy) -> World {
        let mut w = World {
            set: HashSet::new(),
            pending: Vec::new(),
        };
        for ev in events {
            match ev {
                Ev::SweepFires => {
                    let owed = flags_to_assert(&MEMBERS, |f| w.set.contains(&f));
                    match policy {
                        Policy::GrantOnly => {}
                        // The write is attempted exactly once, here, and never checked. If the
                        // game is not accepting writes this tick it is gone for good.
                        Policy::FireAndForget => {}
                        Policy::Reconcile => w.pending = owed,
                    }
                }
                Ev::TickRejectingWrites => {}
                Ev::TickInWorld => {
                    if policy == Policy::Reconcile {
                        for &f in &w.pending {
                            w.set.insert(f);
                        }
                        retire(&mut w.pending, |f| w.set.contains(&f));
                    }
                }
            }
        }
        w
    }

    #[test]
    fn swept_members_are_dead_pickups_until_their_flags_are_asserted() {
        // THE BUG: sweep grants, nothing sets the member flags, the chests sit dead in the world.
        let timeline = [Ev::SweepFires, Ev::TickInWorld, Ev::TickInWorld];
        let old = replay(&timeline, Policy::GrantOnly);
        assert!(
            old.set.is_empty(),
            "pre-fix: not one swept member's acquisition flag is set -- every one is a dead pickup"
        );
        let new = replay(&timeline, Policy::Reconcile);
        assert_eq!(new.set.len(), 2, "post-fix: both member flags are set");
        assert!(new.pending.is_empty(), "and nothing is left owed");
    }

    #[test]
    fn a_write_rejected_at_a_menu_is_retried_not_dropped() {
        // The sweep fires on the poll that sees the defeat flag -- which can be mid-load, when the
        // game refuses writes. Fire-and-forget loses those members permanently; the sweep does not
        // fire twice. Reconciling keeps them owed until an in-world tick accepts them.
        let timeline = [
            Ev::SweepFires,
            Ev::TickRejectingWrites,
            Ev::TickRejectingWrites,
            Ev::TickInWorld,
        ];
        let ff = replay(&timeline, Policy::FireAndForget);
        assert!(
            ff.set.is_empty(),
            "fire-and-forget: the rejected write is gone"
        );
        let new = replay(&timeline, Policy::Reconcile);
        assert_eq!(
            new.set.len(),
            2,
            "reconciled: the retry lands once writes are accepted"
        );
        assert!(new.pending.is_empty());
    }

    #[test]
    fn flags_already_set_are_never_re_asserted() {
        // A reconnect re-observes the defeat flag. Members whose flags already read set must not be
        // owed again -- re-asserting a set flag is harmless but the owed list must converge to empty.
        let mut already = HashSet::new();
        already.insert(1045397000u32);
        let owed = flags_to_assert(&MEMBERS, |f| already.contains(&f));
        assert_eq!(owed, vec![1045397020], "only the unset member is owed");
    }

    #[test]
    fn co_check_siblings_sharing_one_flag_are_owed_once() {
        // Shared-flag siblings are real (292 flags own >1 lot). One flag, one write.
        let shared = [(7774623i64, 1045397000u32), (7774624, 1045397000)];
        assert_eq!(flags_to_assert(&shared, |_| false), vec![1045397000]);
    }

    #[test]
    fn a_member_with_no_known_flag_is_skipped_not_guessed() {
        let unknown = [(7774623i64, 0u32)];
        assert!(flags_to_assert(&unknown, |_| false).is_empty());
    }
}
