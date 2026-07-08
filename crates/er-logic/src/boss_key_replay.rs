//! `boss_key_replay` -- headless timeline for the Boss Keys (mode B) deferred boss-OWN-check
//! release (SPEC-gf-boss-lock-tracker.md "Boss Key: <Boss>"). Twin of [`crate::attunement_replay`]:
//! it replays the save-load / reconnect timeline the client's 5b poll walks for a boss whose OWN
//! check is gated behind a "Boss Key: <Boss>" item, asserting the held check survives a reconnect
//! that re-snapshots from the SERVER checked set + the cumulative received-name set without
//! double-releasing or re-bannering.
//!
//! Model of the client's per-boss gate (`core.rs`, section 5b, boss-key block):
//!  - a boss kill wants to send its OWN check (`boss_ap_id`); the dungeon-sweep members are held by
//!    the separate `sweepLockGates` path (see [`crate::sweep_gate`]) and are out of scope here;
//!  - the check is HELD while its gate key is not in the received set (DEFER into `pending`);
//!  - the poll the key lands BURST-RELEASES `pending` (mark_checked -> the server set) and banners;
//!  - a reconnect drops the session `pending`/`primed` latches, but the SERVER set + received set
//!    persist and are replayed, so priming re-seeds a still-held seal silently (no re-banner) and an
//!    already-released check stays out of `pending` (no double release).

#[cfg(test)]
mod replay {
    use crate::sweep_gate::gate_open;
    use std::collections::HashSet;

    /// One gated boss: its OWN check id + gate key, the authoritative server checked set, and the
    /// cumulative received-name set. Releasing the check feeds the server set (server truth), which
    /// is exactly what a reconnect replays.
    struct Boss {
        own_check: i64,
        gate: Option<String>,
        felled: bool,
        received: HashSet<String>,
        server_checked: HashSet<i64>,
        // --- session-scoped state (dropped + rebuilt on reconnect) ---
        pending: HashSet<i64>,
        primed: bool,
        seal_banners: usize,
        release_banners: usize,
    }

    impl Boss {
        fn new(own_check: i64, gate: Option<&str>) -> Self {
            Boss {
                own_check,
                gate: gate.map(|g| g.to_string()),
                felled: false,
                received: HashSet::new(),
                server_checked: HashSet::new(),
                pending: HashSet::new(),
                primed: false,
                seal_banners: 0,
                release_banners: 0,
            }
        }

        fn kill(&mut self) {
            self.felled = true;
        }

        fn receive(&mut self, name: &str) {
            self.received.insert(name.to_string());
        }

        /// The real decision seam: is this boss's gate open given the received set.
        fn gate_now(&self) -> bool {
            gate_open(self.gate.as_deref(), |n| self.received.contains(n))
        }

        /// Prime once, the first in-world poll: a boss felled in a PRIOR session whose key is still
        /// unreceived and whose check is not yet on the server set is seeded into `pending` SILENTLY,
        /// so a reconnect re-derives the seal without a banner. Mirrors the client's boss_key_primed.
        fn prime(&mut self) {
            if self.primed {
                return;
            }
            if self.felled && !self.gate_now() && !self.server_checked.contains(&self.own_check) {
                self.pending.insert(self.own_check);
            }
            self.primed = true;
        }

        /// One 5b poll. The locationFlags poll re-offers the boss's OWN check every tick while it is
        /// felled and unsent (the server set filters an already-sent check). Gate-open -> send;
        /// gate-closed -> defer (a first insert is the once-per-boss seal banner). Then burst-release
        /// any pending once the key is held.
        fn poll(&mut self) {
            self.prime();
            let candidate = self.felled && !self.server_checked.contains(&self.own_check);
            if candidate {
                if self.gate_now() {
                    self.server_checked.insert(self.own_check); // direct send (key already held)
                } else if self.pending.insert(self.own_check) {
                    self.seal_banners += 1; // first seal this session
                }
            }
            if self.gate_now() && !self.pending.is_empty() {
                let drained: Vec<i64> = self.pending.drain().collect();
                for loc in drained {
                    self.server_checked.insert(loc);
                }
                self.release_banners += 1;
            }
        }

        /// A reconnect / ER save reload: session latches drop; server set + received set persist.
        fn reconnect(&mut self) {
            self.pending.clear();
            self.primed = false;
            self.seal_banners = 0;
            self.release_banners = 0;
        }
    }

    #[test]
    fn kill_without_key_holds() {
        let mut b = Boss::new(900, Some("Boss Key: Godrick"));
        b.poll(); // first in-world poll: prime with the boss still alive -> nothing seeded
        b.kill();
        b.poll(); // killed THIS session, key not held -> sealed
        assert!(!b.gate_now());
        assert!(b.pending.contains(&900), "own check held pending the key");
        assert!(!b.server_checked.contains(&900), "nothing sent while sealed");
        assert_eq!(b.seal_banners, 1);
        assert_eq!(b.release_banners, 0);
        // A further poll must NOT re-seal (once per boss, not every poll).
        b.poll();
        assert_eq!(b.seal_banners, 1, "no second seal banner");
        assert!(!b.server_checked.contains(&900));
    }

    #[test]
    fn key_after_kill_burst_releases_once() {
        let mut b = Boss::new(900, Some("Boss Key: Godrick"));
        b.poll(); // prime, alive
        b.kill();
        b.poll(); // sealed
        assert_eq!(b.pending.len(), 1);
        b.receive("Boss Key: Godrick");
        b.poll(); // key held -> burst release
        assert!(b.server_checked.contains(&900), "check released to the server set");
        assert!(b.pending.is_empty());
        assert_eq!(b.release_banners, 1);
        // No second release / no re-send on the next poll.
        let released_before = b.release_banners;
        b.poll();
        assert_eq!(b.release_banners, released_before, "no second release banner");
        assert!(b.server_checked.contains(&900));
    }

    #[test]
    fn reconnect_re_derives_without_double_release() {
        // Key already held at the kill -> released immediately; reconnect must not re-release.
        let mut b = Boss::new(900, Some("Boss Key: Godrick"));
        b.poll(); // prime, alive
        b.kill();
        b.receive("Boss Key: Godrick");
        b.poll(); // gate open at kill -> direct send
        assert!(b.server_checked.contains(&900));
        let snapshot = b.server_checked.clone();
        b.reconnect(); // session latches drop; server set + received persist (replayed)
        assert_eq!(
            b.server_checked, snapshot,
            "server set is authoritative across reconnect"
        );
        b.poll(); // already on the server set -> not re-offered, not re-released
        assert_eq!(b.release_banners, 0, "nothing re-released on reconnect");
        assert_eq!(b.seal_banners, 0, "nothing re-sealed on reconnect");
        assert!(b.pending.is_empty());
    }

    #[test]
    fn reconnect_while_still_sealed_re_seeds_silently() {
        // Killed, key never received, then reconnect before the key lands: priming re-seeds the seal
        // into pending WITHOUT a banner, and the check stays held.
        let mut b = Boss::new(900, Some("Boss Key: Godrick"));
        b.poll(); // prime, alive
        b.kill();
        b.poll(); // sealed this session (banner 1)
        assert_eq!(b.seal_banners, 1);
        b.reconnect();
        b.poll(); // prime re-seeds pending silently; candidate already pending -> no new banner
        assert!(b.pending.contains(&900), "seal re-derived on reconnect");
        assert!(!b.server_checked.contains(&900));
        assert_eq!(b.seal_banners, 0, "reconnect-seeded seal does not re-banner");
        // Key arrives post-reconnect -> burst release fires exactly once.
        b.receive("Boss Key: Godrick");
        b.poll();
        assert!(b.server_checked.contains(&900));
        assert!(b.pending.is_empty());
        assert_eq!(b.release_banners, 1);
    }

    #[test]
    fn ungated_boss_sends_immediately() {
        // gate = None: the boss's OWN check is never held -- it goes straight to the server set.
        let mut b = Boss::new(900, None);
        b.kill();
        b.poll();
        assert!(b.gate_now());
        assert!(b.server_checked.contains(&900), "ungated check sent at once");
        assert!(b.pending.is_empty());
        assert_eq!(b.seal_banners, 0);
        assert_eq!(b.release_banners, 0);
    }
}
