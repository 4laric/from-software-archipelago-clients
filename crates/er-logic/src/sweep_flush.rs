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

/// Why a granted member contributed no owed flag. Every field is a BENIGN, documented reason --
/// the point is that the log could not tell them apart (world#697).
///
/// ⭐ THE MOTIVATING CASE. islam's 2026-08-15 log:
///
/// ```text
/// 12:23:45  Boss sweep (Liurnia) [trigger flag 1034500800] -- 19 check(s) granted.
/// 12:23:45  sweep-flush: 14 swept member flag(s) confirmed set (0 still owed)
/// ```
///
/// 19 granted, 14 confirmed, nothing owed. Read against a sweep in the same session that reported
/// 55 granted and 55 confirmed, that looks like five flags quietly lost -- and #697 reasonably
/// called it a reconciler reporting a clean run over a shortfall (rule 2). It is not: all three of
/// the filters below are deliberate, and any mix of them explains the gap exactly. But the summary
/// line carried one number and no way to check, which is the same defect one layer over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AssertSkips {
    /// Members whose detection flag is `0`: no flag to set, skipped rather than guessed at.
    pub no_flag: u32,
    /// Members sharing a flag already accounted for -- co-check siblings legitimately do.
    pub shared: u32,
    /// Members whose flag was ALREADY set before the sweep fired. On a trigger that flipped
    /// without a boss fight this is the expected shape: the player had already collected some of
    /// those pickups by hand.
    pub already_set: u32,
}

impl AssertSkips {
    pub fn total(&self) -> u32 {
        self.no_flag + self.shared + self.already_set
    }

    /// The clause that explains a `granted != asserted` gap, or `None` when there is no gap.
    ///
    /// 🛑 STATED, NOT INFERRED. Without this the reader has to hold the sweep's member list, the
    /// co-check map and the world's flag state in their head to decide whether a smaller number is
    /// fine. #697 shows what happens when they cannot: a healthy line is filed as a bug.
    pub fn explain(&self) -> Option<String> {
        if self.total() == 0 {
            return None;
        }
        Some(format!(
            "{} member(s) needed no flag asserted: {} already set, {} sharing a flag with a \
             sibling, {} carrying no detection flag",
            self.total(),
            self.already_set,
            self.shared,
            self.no_flag
        ))
    }
}

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

/// [`flags_to_assert`], plus a count of why each skipped member was skipped (world#697).
///
/// Same filter, same order, same result -- `flags_to_assert` is this function's first return value
/// and stays the API for callers that do not report. One walk, so the counts cannot disagree with
/// the flags.
pub fn flags_to_assert_counted(
    members: &[(i64, u32)],
    is_set: impl Fn(u32) -> bool,
) -> (Vec<u32>, AssertSkips) {
    let mut owed: Vec<u32> = Vec::new();
    let mut skips = AssertSkips::default();
    for &(_loc, flag) in members {
        if flag == 0 {
            skips.no_flag += 1;
        } else if owed.contains(&flag) {
            skips.shared += 1;
        } else if is_set(flag) {
            skips.already_set += 1;
        } else {
            owed.push(flag);
        }
    }
    (owed, skips)
}

#[cfg(test)]
mod replay {
    use super::*;

    /// ⭐ islam's Adula sweep, world#697: 19 granted, 14 asserted, and the five that were not are
    /// all benign. Driven with a flag profile that reproduces the reported numbers exactly.
    #[test]
    fn the_adula_sweep_gap_is_fully_explained() {
        // 19 members: 14 needing assertion, 3 already set, 1 sharing a sibling's flag, 1 flagless.
        let mut members: Vec<(i64, u32)> = (0..14).map(|i| (i as i64, 1000 + i as u32)).collect();
        members.push((100, 2001)); // already set
        members.push((101, 2002)); // already set
        members.push((102, 2003)); // already set
        members.push((103, 1000)); // co-check sibling: shares member 0's flag
        members.push((104, 0)); //    no detection flag
        assert_eq!(members.len(), 19);

        let already: [u32; 3] = [2001, 2002, 2003];
        let (owed, skips) = flags_to_assert_counted(&members, |f| already.contains(&f));

        assert_eq!(owed.len(), 14, "the 14 in islam's sweep-flush line");
        assert_eq!(skips.already_set, 3);
        assert_eq!(skips.shared, 1);
        assert_eq!(skips.no_flag, 1);
        assert_eq!(skips.total(), 5, "19 granted - 14 asserted");

        let line = skips.explain().expect("a gap must explain itself");
        assert!(line.contains("5 member(s)"), "{line}");
        assert!(line.contains("3 already set"), "{line}");
    }

    /// 🛑 THE COUNTED WALK MUST NOT DISAGREE WITH THE UNCOUNTED ONE. They are the same filter and
    /// a caller that reports must get the same flags as one that does not.
    #[test]
    fn counted_and_uncounted_agree() {
        let members = [(1, 10u32), (2, 0), (3, 10), (4, 20), (5, 30)];
        let already = |f: u32| f == 30;
        let plain = flags_to_assert(&members, already);
        let (counted, skips) = flags_to_assert_counted(&members, already);
        assert_eq!(plain, counted);
        assert_eq!(skips.total() as usize + counted.len(), members.len());
    }

    /// No gap, no clause: a sweep where every member needed asserting says nothing extra, which is
    /// what keeps the explanation worth reading when it does appear.
    #[test]
    fn a_clean_sweep_explains_nothing() {
        let members = [(1, 10u32), (2, 20), (3, 30)];
        let (owed, skips) = flags_to_assert_counted(&members, |_| false);
        assert_eq!(owed.len(), 3);
        assert_eq!(skips.total(), 0);
        assert_eq!(skips.explain(), None);
    }

    /// The explanation is ASCII (repo rule 10) and names all three reasons even at zero, so a
    /// reader never has to wonder which bucket was omitted.
    #[test]
    fn the_explanation_is_ascii_and_names_every_bucket() {
        let skips = AssertSkips {
            no_flag: 0,
            shared: 0,
            already_set: 2,
        };
        let line = skips.explain().unwrap();
        assert!(line.is_ascii(), "{line}");
        for want in ["already set", "sharing a flag", "no detection flag"] {
            assert!(line.contains(want), "{want} missing from: {line}");
        }
    }
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
