//! `attunement_replay` -- headless timeline for the attunement-release boss payout
//! (SPEC-gf-boss-lock-tracker.md "Attunement-release design"). Twin of
//! [`crate::region_lock_replay`] / [`crate::flagpoll_baseline_replay`]: it replays the exact
//! save-load / reconnect timeline the client's 5b poll walks, asserting the deferred boss payout and
//! the attunement bloom survive a reconnect that re-snapshots from the SERVER checked set without
//! double-releasing or re-bannering.
//!
//! Model of the client's per-region gate (`core.rs`, section 5b, `region_attunement`):
//!  - a boss kill wants to pay out `payout` checks (its own check + dungeon-sweep members);
//!  - the payout is HELD while the checked members are fewer than `threshold` (DEFER into `pending`);
//!  - the poll the region crosses the threshold BURST-RELEASES `pending` (mark_checked -> the server
//!    set), lights the bloom once, and banners; a reconnect drops the session latches (`pending` /
//!    `attuned_latched` / `bloom_lit`) but the SERVER set persists and is replayed, so priming
//!    re-derives "already attuned" WITHOUT a second banner or a double release.

#[cfg(test)]
mod replay {
    use crate::attunement::{attuned, newly_attuned};
    use std::collections::HashSet;

    /// One region's attunement gate + a boss whose payout it holds, plus the authoritative server
    /// checked set. Releasing a payout check feeds back into `server_checked` (server truth), which
    /// is exactly what a reconnect replays.
    struct Region {
        members: HashSet<i64>,
        threshold: u32,
        payout: Vec<i64>, // the boss's deferred checks (boss + sweep members)
        server_checked: HashSet<i64>,
        // --- session-scoped state (dropped + rebuilt on reconnect) ---
        pending: HashSet<i64>,
        attuned_latched: bool,
        bloom_lit: bool,
        primed: bool,
        banners: Vec<String>,
        released_total: usize,
    }

    impl Region {
        fn new(members: &[i64], threshold: u32, payout: &[i64]) -> Self {
            Region {
                members: members.iter().copied().collect(),
                threshold,
                payout: payout.to_vec(),
                server_checked: HashSet::new(),
                pending: HashSet::new(),
                attuned_latched: false,
                bloom_lit: false,
                primed: false,
                banners: Vec::new(),
                released_total: 0,
            }
        }

        fn is_attuned(&self) -> bool {
            attuned(&self.members, self.threshold, |m| self.server_checked.contains(&m))
        }

        /// Collect an in-region member check (this is what BUILDS attunement).
        fn collect_member(&mut self, id: i64) {
            self.server_checked.insert(id);
        }

        /// Prime once on the first in-world poll: an already-attuned region blooms silently (no
        /// banner) so a reconnect stays quiet. Mirrors the client's `attunement_primed` block.
        fn prime(&mut self) {
            if self.primed {
                return;
            }
            if self.is_attuned() {
                self.bloom_lit = true;
                self.attuned_latched = true;
            }
            self.primed = true;
        }

        /// One 5b poll: partition the boss payout by attunement, deferring or releasing, and fire the
        /// once-only bloom on the rising edge.
        fn poll(&mut self) {
            self.prime();
            let attuned_now = self.is_attuned();
            // Partition the payout: only checks not already on the server set are live candidates.
            for loc in self.payout.clone() {
                if self.server_checked.contains(&loc) {
                    continue; // already delivered (reconnect replay filters these)
                }
                if attuned_now {
                    self.server_checked.insert(loc); // release: mark_checked -> server set
                    self.released_total += 1;
                } else {
                    self.pending.insert(loc);
                }
            }
            // Burst-release anything still pending once attuned (redundant with the loop above but
            // mirrors the client's explicit drain + gives the release banner its count).
            if attuned_now && !self.pending.is_empty() {
                let drained: Vec<i64> = self.pending.drain().collect();
                let n = drained.len();
                for loc in drained {
                    self.server_checked.insert(loc);
                }
                self.banners.push(format!("released {n}"));
            }
            // Attunement bloom: once-only rising edge (suppressed until primed).
            if self.primed && newly_attuned(self.attuned_latched, attuned_now) {
                self.bloom_lit = true;
                self.attuned_latched = true;
                self.banners.push("attuned".to_string());
            }
        }

        /// A reconnect / ER save reload: session latches drop, the SERVER set persists (replayed).
        fn reconnect(&mut self) {
            self.pending.clear();
            self.attuned_latched = false;
            self.bloom_lit = false;
            self.primed = false;
            self.banners.clear();
            self.released_total = 0;
        }
    }

    #[test]
    fn not_attuned_holds_the_payout() {
        // threshold 3; collect only 2 members, then the boss dies -> payout is DEFERRED, nothing
        // released, no bloom.
        let mut r = Region::new(&[1, 2, 3, 4, 5], 3, &[900, 901]);
        r.collect_member(1);
        r.collect_member(2);
        r.poll(); // boss dead, under threshold
        assert!(!r.is_attuned());
        assert_eq!(r.released_total, 0);
        assert_eq!(r.pending.len(), 2, "both payout checks held");
        assert!(!r.bloom_lit);
        assert!(r.banners.is_empty());
    }

    #[test]
    fn crossing_the_threshold_releases_once_and_blooms_once() {
        let mut r = Region::new(&[1, 2, 3, 4, 5], 3, &[900, 901]);
        r.collect_member(1);
        r.collect_member(2);
        r.poll(); // deferred
        assert_eq!(r.pending.len(), 2);
        // Collect the 3rd member -> attune. Next poll burst-releases + blooms.
        r.collect_member(3);
        r.poll();
        assert!(r.is_attuned());
        assert!(r.pending.is_empty(), "payout drained on crossing");
        assert!(r.bloom_lit);
        assert!(r.banners.contains(&"attuned".to_string()));
        assert!(r.server_checked.contains(&900) && r.server_checked.contains(&901));
        let released_after_cross = r.released_total;
        assert!(released_after_cross >= 2);
        // A further poll must NOT re-release or re-banner.
        let banners_before = r.banners.len();
        r.poll();
        assert_eq!(r.released_total, released_after_cross, "no second release");
        assert_eq!(r.banners.len(), banners_before, "no second banner");
    }

    #[test]
    fn reconnect_re_derives_without_double_release_or_rebanner() {
        let mut r = Region::new(&[1, 2, 3, 4, 5], 3, &[900, 901]);
        r.collect_member(1);
        r.collect_member(2);
        r.collect_member(3);
        r.poll(); // attune + release + bloom
        assert!(r.server_checked.contains(&900));
        assert!(r.bloom_lit);
        // Reconnect: session latches drop; the server set (incl. released payout) persists.
        let checked_snapshot = r.server_checked.clone();
        r.reconnect();
        assert_eq!(
            r.server_checked, checked_snapshot,
            "server set is authoritative across reconnect"
        );
        r.poll(); // priming sees already-attuned -> bloom, NO banner, NO re-release
        assert!(r.is_attuned());
        assert!(r.bloom_lit, "bloom re-derived on reconnect");
        assert!(
            r.banners.is_empty(),
            "already-attuned region stays quiet on reconnect (no re-banner)"
        );
        assert_eq!(r.released_total, 0, "payout already on the server set -> nothing re-released");
        assert!(r.pending.is_empty());
    }

    #[test]
    fn reconnect_while_still_under_threshold_keeps_holding() {
        // Deferred, then reconnect before attuning: the payout is re-derived as still-held (the
        // server set never got the payout), nothing leaks.
        let mut r = Region::new(&[1, 2, 3, 4, 5], 3, &[900, 901]);
        r.collect_member(1);
        r.poll(); // 1/3 -> deferred
        r.reconnect();
        r.poll(); // still 1/3 -> still deferred
        assert!(!r.is_attuned());
        assert_eq!(r.released_total, 0);
        assert!(!r.server_checked.contains(&900));
        assert_eq!(r.pending.len(), 2);
    }
}
