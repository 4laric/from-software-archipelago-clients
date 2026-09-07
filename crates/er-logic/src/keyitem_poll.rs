//! `keyitem_poll` — stop a CLIENT-SET obtained flag from being read back as a PICKUP.
//!
//! The reported bug (2026-09-07): a player killed Black Knight Garrew, the boss sweep delivered the
//! Rold Medallion as one of its items, and the very next flag poll ALSO sent
//! `Leyndell :: Rold Medallion` (AP loc 7770556) — a check nobody had done.
//!
//! Mechanism. Event flag 400001 wears two hats at once:
//!   * it is that location's poll flag (`locationFlags[7770556] = 400001`), and
//!   * it is the Grand-Lift-of-Rold OBTAINED flag that `keyitems::KEY_ITEM_ACQUIRE_FLAGS` sets
//!     whenever the Rold Medallion is RECEIVED from the pool (`set_acquire_flags`, re-asserted
//!     every settled tick by `tick_keyitem_flags`), because the lift reads the flag, not inventory.
//!
//! The receive sets the flag; the poll sees a set flag and reports a pickup. Same shape for the
//! Bell / Whetstone Knife / Crafting Kit (60110 / 60130 / 60120 -> locs 7770012 / 7770014 /
//! 7770013), the Drawing-Room Key (400072), and any great-rune restore flag (191-196) a seed
//! happens to poll.
//!
//! Why not the whetblade fix. [`crate::whetblade`] solved its identical collision by REPOINTING the
//! check onto a client-owned flag and rewriting the lot's `getItemFlagId` so a real pickup sets the
//! new flag. That needs a lot to rewrite. These items are handed out by ESD/EMEVD
//! (`DirectlyGivePlayerItem` — Melina at the Erdtree Sanctuary, Renna, Kalé) or by a shop row that
//! is deliberately NOT natively rewritten ([`crate::shop_echo::echo_dedup_eligible`] exempts every
//! client-settable flag), so there is no `getItemFlagId` to move and the vanilla flag stays the
//! only signal a genuine acquisition produces. Blanket-suppressing these locations from the poll
//! would therefore make the GENUINE check undetectable — trading a false collect for a lost check.
//!
//! THE DISCRIMINATOR IS WHO WROTE THE FLAG, and the client knows: it only ever writes an obtained
//! flag that reads UNSET (`tick_keyitem_flags` uses the flag itself as its latch). So:
//!   * flag already set when the receive arrives  -> the client wrote nothing, the flag is the
//!     player's own acquisition, the poll reports it exactly as before;
//!   * flag flipped unset -> set BY US on a receive -> record it ([`ClientWrites`]) and the poll
//!     must never read that flag back as a pickup.
//!
//! This is the IN-SESSION half of the same idea `flag_poll_baseline` implements ACROSS sessions:
//! the baseline holds every watched flag already set at the first in-world poll, so a receive in an
//! earlier session is covered there and this record does not need to persist.
//!
//! Ordering safety: a genuine acquisition and our write cannot be confused, because our write is
//! conditional on the flag reading false. If the player earns the check first, the flag is already
//! set and no write (hence no suppression) is recorded; the poll fires on the very next tick.

use std::collections::{BTreeSet, HashMap, HashSet};

/// Checks whose poll flag is ALSO a flag the client itself sets on a receive — computed once at
/// configure from the seed's merged `location_flags` and `keyitems::all_acquire_flags()`.
///
/// Membership alone suppresses NOTHING (see [`poll_suppressed`]): it only marks the locations where
/// a client write is possible, so a flag the client never actually wrote keeps its ordinary poll
/// behaviour. Sorted by location id for a stable log line.
pub fn colliding_checks(
    location_flags: &HashMap<i64, u32>,
    client_flags: &HashSet<u32>,
) -> Vec<(i64, u32)> {
    let mut out: Vec<(i64, u32)> = location_flags
        .iter()
        .filter(|(_, flag)| client_flags.contains(flag))
        .map(|(&loc, &flag)| (loc, flag))
        .collect();
    out.sort_unstable();
    out
}

/// The one configure-time line naming the collisions, or `None` when this seed has none (a seed
/// that polls none of these locations must stay silent rather than log an empty list).
pub fn configure_line(collisions: &[(i64, u32)]) -> Option<String> {
    if collisions.is_empty() {
        return None;
    }
    let list = collisions
        .iter()
        .map(|(loc, flag)| format!("{loc} (flag {flag})"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "key-item poll guard: {} check(s) suppressed from the flag poll when their flag is set by \
         a receive: {list}",
        collisions.len()
    ))
}

/// May the flag poll turn `flag` reading SET at `loc` into a check report?
///
/// `false` only when BOTH halves hold: the location is a known collision (configure time) AND the
/// client actually flipped that flag itself this session (`client_written`). Anything else — an
/// unrelated check, a collision whose flag the client never wrote — is untouched, which is what
/// keeps a genuine ESD/shop acquisition of the same item reporting normally.
pub fn poll_suppressed(
    loc: i64,
    flag: u32,
    collisions: &HashMap<i64, u32>,
    client_written: &BTreeSet<u32>,
) -> bool {
    collisions.get(&loc) == Some(&flag) && client_written.contains(&flag)
}

/// Record of the obtained flags the CLIENT flipped unset -> set this session.
///
/// The arm (`keyitems.rs`) owns the live instance and feeds it at the one place that writes: a flag
/// that already read set is NOT recorded, because the client did not cause it. Session-scoped on
/// purpose — see the module doc on `flag_poll_baseline` covering earlier sessions.
///
/// Backed by a `BTreeSet` so [`ClientWrites::empty`] is `const` and the arm can hold it in a plain
/// `static Mutex<_>` beside the other keyitems tables (`HashSet::new` is not const).
#[derive(Debug, Default, Clone)]
pub struct ClientWrites {
    written: BTreeSet<u32>,
}

impl ClientWrites {
    /// A record with nothing in it, usable as a `static Mutex<ClientWrites>` initialiser.
    pub const fn empty() -> Self {
        ClientWrites {
            written: BTreeSet::new(),
        }
    }

    /// Note that the client set `flag` because it had read UNSET. Idempotent.
    pub fn record(&mut self, flag: u32) {
        self.written.insert(flag);
    }

    /// Every flag the client has written this session.
    pub fn flags(&self) -> &BTreeSet<u32> {
        &self.written
    }

    /// Forget everything (seed change / disconnect).
    pub fn clear(&mut self) {
        self.written.clear();
    }
}

#[cfg(test)]
mod replay {
    use super::*;

    /// The reported case: "Leyndell :: Rold Medallion" and the Grand Lift obtained flag.
    const ROLD_LOC: i64 = 7_770_556;
    const ROLD_FLAG: u32 = 400_001;
    /// The Crafting Kit, same collision class through a Kalé shop row (client#335).
    const KIT_LOC: i64 = 7_770_013;
    const KIT_FLAG: u32 = 60_120;
    /// An ordinary check whose flag nothing but the world sets — the control.
    const ORDINARY_LOC: i64 = 7_770_099;
    const ORDINARY_FLAG: u32 = 510_800;

    fn seed_poll() -> HashMap<i64, u32> {
        HashMap::from([
            (ROLD_LOC, ROLD_FLAG),
            (KIT_LOC, KIT_FLAG),
            (ORDINARY_LOC, ORDINARY_FLAG),
        ])
    }

    /// What the arm passes in: every flag `keyitems::all_acquire_flags()` can set.
    fn acquire_flags() -> HashSet<u32> {
        HashSet::from([ROLD_FLAG, KIT_FLAG, 60_110, 60_130, 400_072, 191, 192])
    }

    /// One frame of the session timeline.
    #[derive(Clone, Copy)]
    enum Ev {
        /// An AP item arrives whose obtained flag is `flag`; `keyitems` sets the flag IF it reads
        /// unset, and records the write when it did.
        Receive(u32),
        /// The player genuinely earns a check: the GAME sets the flag (Melina's ESD hand-over, a
        /// shop purchase, a world pickup).
        GenuineAcquire(u32),
        /// A flag-poll tick.
        Poll,
    }

    /// Replay a timeline, returning the locations the poll reported, in order.
    ///
    /// `guard = false` is today's poll (any set flag is a check — the bug); `guard = true` adds the
    /// collision set + [`poll_suppressed`]. Both dedup already-reported locations, mirroring the
    /// server checked-set: the bug is the FIRST report, not a repeat.
    fn replay(events: &[Ev], guard: bool) -> Vec<i64> {
        let poll_map = seed_poll();
        let collisions: HashMap<i64, u32> = colliding_checks(&poll_map, &acquire_flags())
            .into_iter()
            .collect();
        let mut game: HashSet<u32> = HashSet::new();
        let mut writes = ClientWrites::empty();
        let mut reported: Vec<i64> = Vec::new();
        for &ev in events {
            match ev {
                Ev::Receive(flag) => {
                    // keyitems.rs: the flag IS the latch — write only when it reads unset, and
                    // that is exactly the case the client caused.
                    if !game.contains(&flag) {
                        game.insert(flag);
                        writes.record(flag);
                    }
                }
                Ev::GenuineAcquire(flag) => {
                    game.insert(flag);
                }
                Ev::Poll => {
                    for (&loc, &flag) in &poll_map {
                        if !game.contains(&flag) || reported.contains(&loc) {
                            continue;
                        }
                        if guard && poll_suppressed(loc, flag, &collisions, writes.flags()) {
                            continue;
                        }
                        reported.push(loc);
                    }
                    reported.sort_unstable();
                }
            }
        }
        reported
    }

    #[test]
    fn collisions_are_exactly_the_client_settable_checks() {
        assert_eq!(
            colliding_checks(&seed_poll(), &acquire_flags()),
            vec![(KIT_LOC, KIT_FLAG), (ROLD_LOC, ROLD_FLAG)],
            "only checks whose poll flag a receive can set may enter the guard"
        );
    }

    #[test]
    fn a_seed_without_these_checks_logs_nothing() {
        let poll = HashMap::from([(ORDINARY_LOC, ORDINARY_FLAG)]);
        let collisions = colliding_checks(&poll, &acquire_flags());
        assert!(collisions.is_empty());
        assert!(configure_line(&collisions).is_none());
    }

    #[test]
    fn configure_line_names_every_collision_once() {
        let line = configure_line(&colliding_checks(&seed_poll(), &acquire_flags())).unwrap();
        assert!(line.contains("2 check(s)"), "{line}");
        assert!(line.contains("7770556 (flag 400001)"), "{line}");
        assert!(line.contains("7770013 (flag 60120)"), "{line}");
    }

    /// Regression guard, documenting the 2026-09-07 report: the sweep hands over the Rold
    /// Medallion, keyitems sets 400001 so the Grand Lift works, and the unguarded poll reads its
    /// own write back as a pickup of "Leyndell :: Rold Medallion".
    #[test]
    fn receive_falsely_collects_without_the_guard() {
        let fired = replay(&[Ev::Receive(ROLD_FLAG), Ev::Poll], false);
        assert!(
            fired.contains(&ROLD_LOC),
            "regression guard: the pre-fix poll reports the receive as a check, fired={fired:?}"
        );
    }

    #[test]
    fn receive_no_longer_collects_the_check() {
        let fired = replay(
            &[Ev::Receive(ROLD_FLAG), Ev::Poll, Ev::Poll, Ev::Poll],
            true,
        );
        assert!(
            fired.is_empty(),
            "a flag the client wrote itself must never be read back as a pickup, fired={fired:?}"
        );
    }

    /// The other half of the fix: suppression must not eat the real check. A player who has NOT
    /// received the medallion and talks to Melina after Morgott gets 400001 from her ESD; the
    /// client wrote nothing, so the poll reports the location exactly once.
    #[test]
    fn genuine_acquisition_still_reports_exactly_once() {
        let fired = replay(
            &[
                Ev::Poll,
                Ev::GenuineAcquire(ROLD_FLAG),
                Ev::Poll,
                Ev::Poll,
                // The pool later delivers the same item; the flag already reads set, so nothing
                // is written and nothing changes.
                Ev::Receive(ROLD_FLAG),
                Ev::Poll,
            ],
            true,
        );
        assert_eq!(fired, vec![ROLD_LOC]);
    }

    /// The genuine check survives even when it is earned AFTER a receive on a DIFFERENT collision
    /// flag — the guard is per-flag, never a blanket over the whole class.
    #[test]
    fn a_receive_does_not_suppress_a_sibling_check() {
        let fired = replay(
            &[
                Ev::Receive(ROLD_FLAG),
                Ev::GenuineAcquire(KIT_FLAG),
                Ev::Poll,
            ],
            true,
        );
        assert_eq!(fired, vec![KIT_LOC]);
    }

    #[test]
    fn unrelated_checks_are_untouched() {
        let fired = replay(
            &[
                Ev::Receive(ROLD_FLAG),
                Ev::GenuineAcquire(ORDINARY_FLAG),
                Ev::Poll,
            ],
            true,
        );
        assert_eq!(fired, vec![ORDINARY_LOC]);
    }
}
