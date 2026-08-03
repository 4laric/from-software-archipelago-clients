//! `vanilla_suppress_replay` — headless timeline replay for the vanilla-pickup SUPPRESSION seam.
//!
//! Twin of [`crate::start_backfill`] and [`crate::region_lock_replay`], for the shared-flag
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
                SuppressKeying::LiveFlag => {
                    !flags.is_empty() && !flags.iter().any(|&f| self.flag_set(f))
                }
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
        assert!(
            !g.leaked(LOC_A),
            "re-pickup before the poll must still suppress"
        );

        let g = replay(
            &[
                Ev::PickUpSharedFlagLoc(LOC_A),
                Ev::Poll,                       // A folded into the collected-set
                Ev::PickUpSharedFlagLoc(LOC_A), // now a genuine re-pickup -> passes
                Ev::PickUpSharedFlagLoc(LOC_B), // neighbour, never collected -> still suppresses
            ],
            SuppressKeying::CollectedSet,
        );
        assert!(
            g.leaked(LOC_A),
            "after its own check is collected, a re-pickup of A must pass"
        );
        assert!(
            !g.leaked(LOC_B),
            "B was never collected -> must keep suppressing"
        );
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
                Ev::Poll,                       // A now in the collected-set
                Ev::Reconnect, // live flags wiped; collected-set (A's flag) persists
                Ev::PickUpSharedFlagLoc(LOC_B), // uncollected neighbour -> suppress
                Ev::PickUpSharedFlagLoc(LOC_A), // collected -> pass
            ],
            SuppressKeying::CollectedSet,
        );
        assert!(
            !g.leaked(LOC_B),
            "an uncollected loc must suppress across a reconnect"
        );
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
        let g = replay(
            &[Ev::PickUpSharedFlagLoc(non_check)],
            SuppressKeying::CollectedSet,
        );
        assert!(
            g.leaked(non_check),
            "a non-check id must pass under collected-set keying"
        );
        let g = replay(
            &[Ev::PickUpSharedFlagLoc(non_check)],
            SuppressKeying::LiveFlag,
        );
        assert!(
            g.leaked(non_check),
            "a non-check id must pass under live-flag keying"
        );
    }
}

// ================================================================================================
// #321 — the id-keyed suppressor eats a vanilla weapon from a NON-CHECK source.
// ================================================================================================
//
// MOTIVATING CASE (CONTRIBUTING rule 11), boblerrr on the Nexus page, 2026-08-03:
//   "if you want to leave a weapon on the ground to upgrade it like you can in matt's randomizer
//    it outright delete the weapon instead"
//
// 🛑 The issue title blames `auto_upgrade`. It is not, and the direction is BACKWARDS:
// `apply_auto_upgrade` has one production caller, `detour::grant_full_id_outcome`, so it runs only
// on an AP GRANT and never on a world pickup. And `checkItemFlags` is keyed on the VANILLA ware id
// (`base + 0`), so a weapon auto_upgrade raised to `base + N` is not a key and CANNOT be suppressed.
// auto_upgrade ON makes this bug LESS likely, not more.
//
// What destroys the item is the ID-KEYED VANILLA SUPPRESSOR in `detour::add_item_detour`:
// `check_item_flags_lookup(raw_id)` -> `should_suppress` -> `return 0`, so the original AddItemFunc
// never runs. The client hooks four game functions and owns no item-removal primitive; withholding a
// bag-add is the only way it can make an item not exist.
//
// 🛑 THE FALSE PREMISE, `check_lots.rs` module header (lines 28-30), verbatim:
//     "GOODS slots only. Weapon/armor check wares stay on the id-keyed suppressor, which is already
//      sound for them: a weapon is essentially never farmable, so it lives in the check-only set and
//      cannot eat a legitimate source."
// `enemy_drops.rs` refutes it in the same tree: 4891 enemy lots carry no flag and are therefore
// farmable, and the reroll rewrites "only the GOODS slots -- weapon/armor/talisman drop slots keep
// their vanilla contents." So a farmable enemy CAN drop a vanilla weapon that backs a check.
//
// ⚠️ The reporter's own source is NOT modelled as a drop. Elden Ring has no leave-on-the-ground verb
// (Discard destroys; Store goes to the chest), so "leave a weapon on the ground" most likely means
// leaving a world pickup uncollected and returning later. The SOURCE is unestablished and the fix
// does not depend on it.
//
// ================================================================================================
// WHAT SHIPS, AND WHAT REMAINS
// ================================================================================================
//
// TWO changes, neither of which closes this on its own:
//
//   1. WORLD SIDE — drop every id whose every backing check is repointed at the placeholder.
//      1289 armed ids -> 211. Nothing left to suppress there, so arming it was pure downside.
//   2. CLIENT SIDE — for the 211-id residue (the LOT-LESS EMEVD-award checks, which have no source
//      to neutralise), also release an id once its own acquisition flag FIRES, not only once the
//      server reports it collected. Legal only because no emitted flag is mapped by two ids; gated
//      at connect on `flags_are_unshared` and by a world regen test.
//
// 🛑 THE GAP THAT REMAINS, pinned by `before_the_award_fires_the_copy_is_still_destroyed` below: a
// copy picked up BEFORE the award fires is still eaten. The disarm caps the window; it does not
// close it. Weapon 0x6acfc0 -- the id in boblerrr's 23:43:54 log line, single flag 220550, location
// 7774813, not lot-covered -- is one of the 211. Do not report #321 as fixed.

#[cfg(test)]
mod non_check_source {
    use crate::vanilla_suppress::should_suppress_with_flag_disarm;
    use std::collections::HashSet;

    /// A WEAPON FullID that backs a LOT-LESS check, so it survives the world-side drop and stays in
    /// `checkItemFlags`. From the live log line above: category 0x0 | row 7_000_000, level 0.
    const WEAPON_FULL_ID: u32 = 0x006a_cfc0;
    /// Its check's acquisition flag. SYNTHETIC -- the real one is not claimed here; all that matters
    /// is that the id maps to exactly one.
    const WEAPON_CHECK_FLAG: u32 = 30_087_100;

    fn mapped_flags(item: u32) -> Vec<u32> {
        if item == WEAPON_FULL_ID {
            vec![WEAPON_CHECK_FLAG]
        } else {
            vec![]
        }
    }

    /// Where a suppressed bag-add sends the item.
    #[derive(Debug, PartialEq, Eq)]
    enum Fate {
        /// Entered the bag.
        InBag,
        /// Withheld, and an AP grant is on the way -- the CORRECT outcome for a check pickup.
        WithheldForGrant,
        /// Withheld with nothing behind it. The player is simply short one item.
        Destroyed,
    }

    struct Game {
        /// Server checked-set, as acquisition flags.
        collected: HashSet<u32>,
        /// Live game acquisition flags -- set by the EMEVD award when the check finally fires.
        fired: HashSet<u32>,
        /// Whether this seed's table permits the flag-set disarm.
        disarm: bool,
    }

    impl Game {
        fn new(disarm: bool) -> Self {
            Game {
                collected: HashSet::new(),
                fired: HashSet::new(),
                disarm,
            }
        }

        fn suppresses(&self, item: u32) -> bool {
            should_suppress_with_flag_disarm(
                &mapped_flags(item),
                &self.collected,
                self.disarm,
                &|f| self.fired.contains(&f),
            )
        }

        /// The lot-less check finally fires: the EMEVD award sets its acquisition flag.
        fn award_fires(&mut self) {
            self.fired.insert(WEAPON_CHECK_FLAG);
        }

        /// The player reaches the check itself. The award sets the flag and the AP grant delivers.
        fn pick_up_own_check(&mut self, item: u32) -> Fate {
            let suppress = self.suppresses(item);
            self.award_fires();
            if suppress {
                Fate::WithheldForGrant
            } else {
                Fate::InBag
            }
        }

        /// The same vanilla item arrives from something that is NOT its check -- an unflagged
        /// farmable enemy lot, or any other non-check origin. No grant is coming.
        fn pick_up_elsewhere(&self, item: u32) -> Fate {
            if self.suppresses(item) {
                Fate::Destroyed
            } else {
                Fate::InBag
            }
        }
    }

    #[test]
    fn own_check_pickup_is_withheld_for_the_grant() {
        // The control. Suppressing a CHECK pickup is correct and costs the player nothing. If this
        // ever fails the harness is broken rather than the client.
        let mut g = Game::new(true);
        assert_eq!(g.pick_up_own_check(WEAPON_FULL_ID), Fate::WithheldForGrant);
    }

    #[test]
    fn a_weapon_that_backs_no_check_is_never_touched() {
        // Bounds the blast radius: the suppressor only ever sees ids in `checkItemFlags`.
        let g = Game::new(true);
        assert_eq!(g.pick_up_elsewhere(0x0010_0000), Fate::InBag);
    }

    #[test]
    fn after_the_award_fires_a_non_check_copy_reaches_the_bag() {
        // THE SHIPPED GUARANTEE. Once the lot-less check has fired, its ware is no longer protecting
        // anything, so a copy from any other source must arrive -- without waiting on a collected-set
        // entry that only appears after the server round-trips, and never at all if the player skips
        // the check.
        let mut g = Game::new(true);
        g.award_fires();
        assert_eq!(g.pick_up_elsewhere(WEAPON_FULL_ID), Fate::InBag);
    }

    #[test]
    fn before_the_award_fires_the_copy_is_still_destroyed() {
        // 🛑 THE REMAINING GAP, asserted so it cannot be quietly forgotten. A farmed copy picked up
        // BEFORE the award is still eaten with no grant behind it. #321 is capped, not closed.
        // Closing it needs a source discriminator the AddItemFunc detour does not have.
        let g = Game::new(true);
        assert_eq!(g.pick_up_elsewhere(WEAPON_FULL_ID), Fate::Destroyed);
    }

    #[test]
    fn without_the_disarm_a_fired_award_does_not_release() {
        // Documents what the 211 residue ids cost on a seed that cannot enable the disarm (one whose
        // table maps a flag to two ids): the copy is eaten until the SERVER reports the check
        // collected, and forever if the player never does it.
        let mut g = Game::new(false);
        g.award_fires();
        assert_eq!(g.pick_up_elsewhere(WEAPON_FULL_ID), Fate::Destroyed);
        g.collected.insert(WEAPON_CHECK_FLAG);
        assert_eq!(g.pick_up_elsewhere(WEAPON_FULL_ID), Fate::InBag);
    }

    #[test]
    fn the_disarm_does_not_widen_a_multi_check_ware() {
        // An id backing two checks must still wait for BOTH to be released. One fired award is not
        // enough -- the merge semantics are unchanged by the disarm.
        let collected = HashSet::new();
        let fired = |f: u32| f == 100;
        assert!(should_suppress_with_flag_disarm(
            &[100, 200],
            &collected,
            true,
            &fired
        ));
    }
}
