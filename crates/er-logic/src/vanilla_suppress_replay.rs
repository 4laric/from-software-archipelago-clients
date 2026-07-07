//! `vanilla_suppress_replay` — headless timeline replay for the vanilla-pickup SUPPRESSION seam.
//!
//! Twin of [`crate::start_grant_replay`] and [`crate::region_lock_replay`], for the shared-flag
//! re-pickup class. The suppression DECISION already lives, pure and host-tested, in
//! [`crate::vanilla_suppress::should_suppress`] (`fn(mapped_flags: &[u32], collected: &HashSet<u32>)
//! -> bool`). What that single-tick test can't show is the SEQUENCING failure: a shared acquisition
//! flag being set by picking up ONE location and then, on a LATER tick, standing in for its 224
//! neighbours. This module models that timeline — pick up loc A, then pick up loc B on the same flag,
//! across a reconnect — so the collected-set fix is provable offline and stays a regression guard.
//!
//! WHY THIS EXISTS (Traveler's Clothes leak, 2026-07-03; er-vanilla-suppress-collected-set-fix):
//! ~224 acquisition flags cover 605 ER datapackage locations — armor sets, NPC-corpse bundles, boss
//! remembrance drops share ONE flag across many distinct item ids / locations (e.g. Traveler's
//! Clothes item 0x100f90c4, flag 15007980, is one of a large shared-flag lot). The game sets that
//! shared flag AT or BEFORE the bag-add. So a suppressor keyed on the LIVE event flag ("is the flag
//! set right now?") reads `true` the instant ANY one location on the flag is touched, and thereafter
//! treats every OTHER location on that flag as an already-done "re-pickup" — passing the vanilla ware
//! through instead of suppressing it (the observed leak). The fix keys on the server COLLECTED set
//! (the checked-location set, bridged loc -> acquisition flag via `checkItemFlags` / `locationFlags`):
//! a location is only ever "done" once ITS OWN check was reported, so a shared-flag neighbour that
//! was never collected still suppresses. See [`crate::vanilla_suppress`] module docs.
//!
//! The two policies contrasted here:
//! * FLAG-KEYED (buggy leak): suppress only while NO mapped flag is live-set on the game — i.e. once
//!   the shared flag is set by a neighbour, stop suppressing. This is the pre-fix live-flag test,
//!   reconstructed here so the leak is reproducible offline. It is NOT production code.
//! * COLLECTED-SET-KEYED (fixed): delegate straight to the real
//!   [`crate::vanilla_suppress::should_suppress`] against the server checked-set. This is the shipped
//!   decision; the harness just drives it through a timeline.
//!
//! No new production logic is added — the fixed path reuses `should_suppress` verbatim.

/// Which discriminator the suppressor keys on, threaded through the replay like `reconcile_gate` /
/// `latch_on_observed` in the sibling replay modules.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SuppressKeying {
    /// Pre-fix: key on the LIVE event flag. Once the shared acquisition flag is set (by ANY location
    /// on it), suppression stops for EVERY location on that flag — the leak. Reconstructed for the
    /// regression guard; not shipped.
    LiveFlag,
    /// Fixed: key on the server COLLECTED set via [`crate::vanilla_suppress::should_suppress`]. A
    /// location suppresses until ITS OWN check is collected, regardless of shared-flag neighbours.
    CollectedSet,
}

#[cfg(test)]
mod replay {
    use super::SuppressKeying;
    use crate::hook::GameHook;
    use crate::vanilla_suppress::should_suppress;
    use std::collections::{HashMap, HashSet};

    // A shared-flag lot faithful to the pinned bug: two DISTINCT locations (distinct item ids) that
    // map to the SAME acquisition flag. Picking up either sets the one shared flag.
    // (er-vanilla-suppress-collected-set-fix; Traveler's Clothes lot, flag 15007980.)
    const SHARED_FLAG: u32 = 15_007_980;
    /// Location A — the item actually picked up first (e.g. Traveler's Clothes, id 0x100f90c4).
    const LOC_A: u32 = 0x100f90c4;
    /// Location B — a DIFFERENT location on the SAME shared flag (e.g. Traveler's Manchettes). It is
    /// the innocent neighbour that must NOT be suppressed just because A was picked up.
    const LOC_B: u32 = 0x100f9128;

    /// B's OWN acquisition flag, distinct from the shared lot flag — so B is a real, separately
    /// collectable check that merely SHARES one flag with A (the coupling that caused the leak), not a
    /// literal alias of A. Collecting A must not release B; only B's own collection releases it.
    const LOC_B_OWN_FLAG: u32 = 15_007_981;

    /// Bridge a location to its acquisition flag(s) — the `checkItemFlags` / `locationFlags` mapping
    /// the live client loads from slot_data. A and B SHARE `SHARED_FLAG` (the coupling that is the
    /// bug), but B also carries its own flag, so the collected-set can still tell the two apart.
    fn mapped_flags(loc: u32) -> Vec<u32> {
        match loc {
            LOC_A => vec![SHARED_FLAG],
            LOC_B => vec![SHARED_FLAG, LOC_B_OWN_FLAG],
            _ => vec![],
        }
    }

    /// A game model that — unlike the single-tick `FakeGame` — tracks BOTH the live acquisition flags
    /// (set by the game at bag-add) AND, separately, the server COLLECTED set (checked locations,
    /// populated only by a flag-poll tick that runs STRICTLY AFTER a check is reported). Keeping the
    /// two apart is the whole point: the flag is set immediately, the collected-set lags a poll.
    struct SuppressGame {
        /// Live game acquisition flags (CSEventFlagMan). Set by the game at/around bag-add.
        flags: HashMap<u32, bool>,
        /// Server checked-set, as acquisition flags (what `should_suppress` consumes). A location's
        /// flag enters this only when its OWN check has been reported AND a later poll pulled it.
        collected: HashSet<u32>,
        /// Locations whose check has been reported to the server but not yet pulled by a poll.
        reported_pending_poll: HashSet<u32>,
        /// Ordered transcript of locations whose vanilla ware LEAKED (bag-add passed through).
        leaked: Vec<u32>,
    }

    impl SuppressGame {
        fn new() -> Self {
            SuppressGame {
                flags: HashMap::new(),
                collected: HashSet::new(),
                reported_pending_poll: HashSet::new(),
                leaked: Vec::new(),
            }
        }

        fn flag_set(&self, flag: u32) -> bool {
            self.flags.get(&flag).copied().unwrap_or(false)
        }

        /// A flag-poll tick: pull every reported-but-unpolled location's acquisition flags into the
        /// collected-set. Mirrors the client poll that lags the report by at least one tick.
        fn poll_collected(&mut self) {
            let pending: Vec<u32> = self.reported_pending_poll.drain().collect();
            for loc in pending {
                for f in mapped_flags(loc) {
                    self.collected.insert(f);
                }
            }
        }

        /// The reconnect / save-load: live acquisition flags are volatile game state and drop, but the
        /// server COLLECTED set is authoritative server state and PERSISTS. This asymmetry is exactly
        /// why keying on the collected-set is reconnect-safe and keying on the live flag is not.
        fn reconnect(&mut self) {
            self.flags.clear();
        }

        /// Attempt the vanilla bag-add for `loc` under `keying`. The game sets the shared acquisition
        /// flag at bag-add time (as the real game does). Under LiveFlag the suppressor's own check
        /// then reads that flag; under CollectedSet it consults the server checked-set. If suppression
        /// is DECLINED the vanilla ware passes through (recorded as a leak).
        fn pickup(&mut self, loc: u32, keying: SuppressKeying) {
            let flags = mapped_flags(loc);
            let suppress = match keying {
                // Pre-fix live-flag test: suppress only while NO mapped flag is already live-set.
                // For a shared-flag lot, a neighbour's earlier pickup has already set the flag, so
                // this returns false and leaks. Empty mapped-flags -> not a check -> never suppress.
                SuppressKeying::LiveFlag => !flags.is_empty() && !flags.iter().any(|&f| self.flag_set(f)),
                // Fixed path: the REAL decision against the server checked-set.
                SuppressKeying::CollectedSet => should_suppress(&flags, &self.collected),
            };
            // The game sets the shared acquisition flag as part of the bag-add, regardless.
            for &f in &flags {
                self.set_event_flag(f, true);
            }
            if suppress {
                // Suppressed: the vanilla ware is withheld; the check is what got picked up. Report it
                // to the server (a later poll will fold it into the collected-set).
                self.reported_pending_poll.insert(loc);
            } else {
                // Not suppressed: the vanilla ware entered the bag — a leak unless it was a genuine
                // re-pickup of an already-collected check.
                self.leaked.push(loc);
            }
        }

        fn leaked(&self, loc: u32) -> bool {
            self.leaked.contains(&loc)
        }
    }

    // SuppressGame is a full GameHook (per the harness contract), though the suppression seam itself
    // only reads the flag map. The other verbs are inert stubs — this model owns no upgrade / grant /
    // region behaviour.
    impl GameHook for SuppressGame {
        fn get_event_flag(&self, flag: u32) -> bool {
            self.flag_set(flag)
        }
        fn set_event_flag(&mut self, flag: u32, on: bool) {
            self.flags.insert(flag, on);
        }
        fn try_set_event_flag(&mut self, flag: u32, on: bool) -> bool {
            self.flags.insert(flag, on);
            true
        }
        fn in_world(&self) -> bool {
            true
        }
        fn play_region_id(&self) -> Option<i32> {
            None
        }
        fn grant_full_id(&mut self, _full_id: i32, _qty: i32) -> bool {
            true
        }
        fn player_hp(&self) -> Option<i32> {
            None
        }
        fn weapon_track_and_cap(&self, _base: i32) -> Option<(i32, bool)> {
            None
        }
        fn highest_held_level(&self, _somber: bool) -> Option<i32> {
            None
        }
        fn scadutree_blessing(&self) -> Option<i32> {
            None
        }
        fn set_scadutree_blessing(&mut self, _level: i32) {}
    }

    /// One frame of the pickup timeline.
    #[derive(Clone, Copy)]
    enum Ev {
        /// A vanilla bag-add fires for a location on the shared-flag lot.
        PickUpSharedFlagLoc(u32),
        /// A flag-poll tick folds reported checks into the server collected-set (lags the report).
        Poll,
        /// A reconnect / save-load: live acquisition flags drop, the collected-set persists.
        Reconnect,
        /// Idle tick.
        Tick,
    }

    /// Replay a timeline under one keying policy. Returns the final game (inspect `leaked` /
    /// `collected`). `LiveFlag` reproduces the pre-fix leak; `CollectedSet` drives the shipped fix.
    fn replay(events: &[Ev], keying: SuppressKeying) -> SuppressGame {
        let mut g = SuppressGame::new();
        for &ev in events {
            match ev {
                Ev::PickUpSharedFlagLoc(loc) => g.pickup(loc, keying),
                Ev::Poll => g.poll_collected(),
                Ev::Reconnect => g.reconnect(),
                Ev::Tick => {}
            }
        }
        g
    }

    #[test]
    fn shared_flag_neighbor_leaks_under_flag_keying() {
        // Pre-fix: pick up LOC_A (sets the shared flag), then pick up LOC_B on the SAME flag. The
        // live-flag suppressor sees the flag already set and passes B's vanilla ware straight through
        // — the exact Traveler's Clothes-class leak. Documents the bug.
        let timeline = [
            Ev::PickUpSharedFlagLoc(LOC_A), // A: flag not yet set -> suppressed, flag now set
            Ev::Tick,
            Ev::PickUpSharedFlagLoc(LOC_B), // B: shared flag already set -> WRONGLY passes through
        ];
        let g = replay(&timeline, SuppressKeying::LiveFlag);
        assert!(
            !g.leaked(LOC_A),
            "A was the first pickup on the flag -> suppressed even under the buggy keying",
        );
        assert!(
            g.leaked(LOC_B),
            "regression guard: the neighbour B leaks under live-flag keying (documents the bug)",
        );
    }

    #[test]
    fn collected_set_keying_isolates_shared_flag_locs() {
        // Fixed: same timeline, but nothing has been COLLECTED yet (A was only just reported; no poll
        // ran). B's own check is uncollected, so it suppresses despite the shared flag being live-set.
        let timeline = [
            Ev::PickUpSharedFlagLoc(LOC_A),
            Ev::Tick,
            Ev::PickUpSharedFlagLoc(LOC_B),
        ];
        let g = replay(&timeline, SuppressKeying::CollectedSet);
        assert!(!g.leaked(LOC_A), "first pickup must suppress");
        assert!(
            !g.leaked(LOC_B),
            "the shared-flag neighbour must still suppress — only its OWN collection can release it",
        );
    }

    #[test]
    fn genuine_repickup_passes_only_after_own_collection() {
        // A location's vanilla ware should pass on a GENUINE re-pickup — after its own check has been
        // reported AND a poll folded it into the collected-set. Before the poll it still suppresses;
        // after, it passes. (The neighbour B, never collected, keeps suppressing throughout.)
        let g = replay(
            &[
                Ev::PickUpSharedFlagLoc(LOC_A), // reported, pending poll
                Ev::PickUpSharedFlagLoc(LOC_A), // pre-poll re-pickup: still uncollected -> suppress
            ],
            SuppressKeying::CollectedSet,
        );
        assert!(!g.leaked(LOC_A), "re-pickup before the poll must still suppress");

        let g = replay(
            &[
                Ev::PickUpSharedFlagLoc(LOC_A),
                Ev::Poll,                       // A folded into the collected-set
                Ev::PickUpSharedFlagLoc(LOC_A), // now a genuine re-pickup -> passes
                Ev::PickUpSharedFlagLoc(LOC_B), // neighbour, never collected -> still suppresses
            ],
            SuppressKeying::CollectedSet,
        );
        assert!(g.leaked(LOC_A), "after its own check is collected, a re-pickup of A must pass");
        assert!(!g.leaked(LOC_B), "B was never collected -> must keep suppressing");
    }

    #[test]
    fn collected_set_survives_reconnect_while_live_flag_is_transient() {
        // Reconnect asymmetry: collect A (its flag is in the server checked-set), then reconnect —
        // the live acquisition flags drop but the collected-set persists. A re-pickup of B (never
        // collected) must STILL suppress, and a re-pickup of A (collected, persisted) must pass —
        // even though the live shared flag was wiped by the reconnect.
        let g = replay(
            &[
                Ev::PickUpSharedFlagLoc(LOC_A),
                Ev::Poll,       // A now in the collected-set
                Ev::Reconnect,  // live flags wiped; collected-set (A's flag) persists
                Ev::PickUpSharedFlagLoc(LOC_B), // uncollected neighbour -> suppress
                Ev::PickUpSharedFlagLoc(LOC_A), // collected -> pass
            ],
            SuppressKeying::CollectedSet,
        );
        assert!(!g.leaked(LOC_B), "an uncollected loc must suppress across a reconnect");
        assert!(
            g.leaked(LOC_A),
            "a collected loc's re-pickup passes across a reconnect (collected-set persists)",
        );

        // Contrast: under live-flag keying the reconnect WIPES the discriminator entirely — after the
        // reconnect the flag is unset, so B suppresses by luck, but there is no persistent record that
        // A was ever collected, so A ALSO suppresses (a genuine re-pickup is now wrongly withheld).
        // Neither location leaks here, but the policy has lost A's collected state — the transience
        // the fix removes.
        let g = replay(
            &[
                Ev::PickUpSharedFlagLoc(LOC_A),
                Ev::Reconnect,
                Ev::PickUpSharedFlagLoc(LOC_A),
            ],
            SuppressKeying::LiveFlag,
        );
        assert!(
            !g.leaked(LOC_A),
            "live-flag keying loses A's collected state on reconnect and wrongly re-suppresses it",
        );
    }

    #[test]
    fn empty_mapped_flags_never_suppresses_either_way() {
        // A non-check id (no mapped flags) is not a check under EITHER policy -> its ware always
        // passes. Guards the degenerate branch of both keyings.
        let non_check = 0xDEAD_BEEFu32;
        let g = replay(&[Ev::PickUpSharedFlagLoc(non_check)], SuppressKeying::CollectedSet);
        assert!(g.leaked(non_check), "a non-check id must pass under collected-set keying");
        let g = replay(&[Ev::PickUpSharedFlagLoc(non_check)], SuppressKeying::LiveFlag);
        assert!(g.leaked(non_check), "a non-check id must pass under live-flag keying");
    }
}
