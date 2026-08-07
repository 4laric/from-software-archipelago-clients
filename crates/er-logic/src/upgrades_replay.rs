//! `upgrades_replay` — headless timeline replay for the auto_upgrade RECONNECT GRANT-BURST.
//!
//! Sibling of [`crate::start_backfill`], for the auto_upgrade
//! feature. The decision itself is already pure + unit-tested in [`crate::upgrades`]
//! (`apply_auto_upgrade(hook, on, full_id)`: raise-only, cap-clamped, identity when off / off-world
//! / non-weapon / unresolvable). Those tests fire ONE call. This module adds the dimension they
//! miss and that the replay tier exists for: the STATEFUL sequence around a reconnect.
//!
//! auto_upgrade runs inside `detour.rs grant_full_id`, so it fires on EVERY granted item — including
//! the reconnect RE-GRANT BURST, where the whole received-item history is replayed back. The hazard
//! class is the same as the flask double-grant / start-item clobber: a per-grant side effect that
//! must be idempotent and monotone under that burst. Two properties matter and neither is covered by
//! a single-shot unit test:
//!   1. BURST IDEMPOTENCY — re-granting an already-upgraded weapon during the reconnect burst is a
//!      no-op: the result never climbs past the weapon's cap and never lowers what is already held.
//!   2. TRANSIENT-MISS SAFETY — if the bag can't be walked on some ticks (the load is mid-flight so
//!      `highest_held_level` reads None), the grant returns the input UNCHANGED (identity), never a
//!      LOWERED id, so a transient read miss can never downgrade a weapon; it recovers next tick.
//!
//! The module reuses the real `er_logic::upgrades::apply_auto_upgrade` through the `GameHook` seam,
//! defines its OWN evolving-bag timeline model (never touches the shared `FakeGame`), and replays
//! the reconnect grant burst.

#[cfg(test)]
mod replay {
    use crate::hook::GameHook;
    use crate::upgrades::apply_auto_upgrade;
    use std::collections::HashMap;

    // One upgradeable normal-track weapon: base id, normal cap +25.
    const WEAPON_BASE: i32 = 1_000_000;
    const NORMAL_CAP: i32 = 25;

    /// A game model whose BAG evolves across the timeline: as upgraded weapons land, the highest
    /// held level on a track rises; a load-in-flight window makes the bag briefly un-walkable.
    struct UpgradeGame {
        in_world: bool,
        /// base -> (reinforce cap, is_somber).
        track_cap: HashMap<i32, (i32, bool)>,
        /// highest +N held on the normal / somber track.
        held_normal: i32,
        held_somber: i32,
        /// When false the bag can't be walked (mid-load) -> highest_held_level reads None.
        bag_walkable: bool,
    }

    impl UpgradeGame {
        fn new() -> Self {
            let mut track_cap = HashMap::new();
            track_cap.insert(WEAPON_BASE, (NORMAL_CAP, false));
            UpgradeGame {
                in_world: true,
                track_cap,
                held_normal: 0,
                held_somber: 0,
                bag_walkable: true,
            }
        }
        /// A weapon at level `lvl` on `somber?` lands in the bag, raising the held high-water mark.
        fn weapon_enters_bag(&mut self, somber: bool, lvl: i32) {
            if somber {
                self.held_somber = self.held_somber.max(lvl);
            } else {
                self.held_normal = self.held_normal.max(lvl);
            }
        }
    }

    impl GameHook for UpgradeGame {
        fn get_event_flag(&self, _flag: u32) -> bool {
            false
        }
        fn set_event_flag(&mut self, _flag: u32, _on: bool) {}
        fn try_set_event_flag(&mut self, _flag: u32, _on: bool) -> bool {
            true
        }
        fn in_world(&self) -> bool {
            self.in_world
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
        fn weapon_track_and_cap(&self, base: i32) -> Option<(i32, bool)> {
            self.track_cap.get(&base).copied()
        }
        fn highest_held_level(&self, somber: bool) -> Option<i32> {
            if !self.bag_walkable {
                return None; // bag mid-load -> can't resolve safely (the transient-miss window)
            }
            Some(if somber {
                self.held_somber
            } else {
                self.held_normal
            })
        }
        fn scadutree_blessing(&self) -> Option<i32> {
            None
        }
        fn set_scadutree_blessing(&mut self, _level: i32) {}
    }

    /// One frame of the grant timeline.
    #[derive(Clone, Copy)]
    enum Ev {
        /// The game grants a weapon FullID (server -> client). auto_upgrade runs; the resulting
        /// weapon then LANDS in the bag (raising the held high-water mark).
        Grant(i32),
        /// Toggle bag walkability (models CSInventory not walkable right after a load).
        BagWalkable(bool),
    }

    /// Replay a timeline with auto_upgrade `on`, recording the upgraded FullID auto_upgrade returned
    /// for every Grant (in order). Each granted weapon then enters the bag at its RETURNED level, so
    /// the bag evolves exactly as it would live.
    fn replay(events: &[Ev], on: bool) -> Vec<i32> {
        let mut g = UpgradeGame::new();
        let mut out = Vec::new();
        for &ev in events {
            match ev {
                Ev::BagWalkable(v) => g.bag_walkable = v,
                Ev::Grant(full_id) => {
                    let up = apply_auto_upgrade(&g, on, full_id);
                    out.push(up);
                    // The granted weapon lands in the bag at whatever level it ended up (raise-only).
                    if let Some((_cap, somber)) = g.weapon_track_and_cap(WEAPON_BASE) {
                        let lvl = up - WEAPON_BASE;
                        if (0..=NORMAL_CAP).contains(&lvl) {
                            g.weapon_enters_bag(somber, lvl);
                        }
                    }
                }
            }
        }
        out
    }

    #[test]
    fn reconnect_burst_is_idempotent_and_never_over_raises() {
        // The player has hand-upgraded to +12; then a reconnect re-grants a fresh +0 of the same
        // weapon SEVERAL times (the received-item burst). Each grant must bump the fresh +0 to the
        // held +12 and no further — never climbing toward the +25 cap on repeats, never lowering.
        let mut g = UpgradeGame::new();
        g.held_normal = 12; // player already holds a +12 on this track

        // First grant of a fresh +0 -> +12.
        assert_eq!(apply_auto_upgrade(&g, true, WEAPON_BASE), WEAPON_BASE + 12);

        // Reconnect burst: the SAME fresh +0 re-granted repeatedly is stable at +12 every time.
        for _ in 0..5 {
            assert_eq!(
                apply_auto_upgrade(&g, true, WEAPON_BASE),
                WEAPON_BASE + 12,
                "re-grant during the reconnect burst must be a stable no-op at the held level"
            );
        }
        // And a grant already at/above the target is returned unchanged (raise-only).
        assert_eq!(
            apply_auto_upgrade(&g, true, WEAPON_BASE + 20),
            WEAPON_BASE + 20
        );
    }

    #[test]
    fn evolving_bag_burst_is_monotone_and_capped() {
        // Timeline: grant +8 (lands, held=8), grant a fresh +0 (-> +8 from the bag), grant +25
        // (lands at the cap), then a reconnect re-grants +0 (-> +25 now). Sequence must be
        // monotone non-decreasing and never exceed the +25 cap.
        let out = replay(
            &[
                Ev::Grant(WEAPON_BASE + 8),  // held -> 8
                Ev::Grant(WEAPON_BASE),      // fresh +0 -> +8
                Ev::Grant(WEAPON_BASE + 25), // held -> 25 (the cap)
                Ev::Grant(WEAPON_BASE),      // reconnect re-grant -> +25
            ],
            true,
        );
        let levels: Vec<i32> = out.iter().map(|&id| id - WEAPON_BASE).collect();
        assert_eq!(
            levels,
            vec![8, 8, 25, 25],
            "monotone, capped at +25, no over-raise"
        );
        assert!(levels.iter().all(|&l| l <= NORMAL_CAP));
        assert!(
            levels.windows(2).all(|w| w[1] >= w[0]),
            "never lowers across the burst"
        );
    }

    #[test]
    fn transient_bag_miss_never_lowers_a_weapon() {
        // A weapon is granted and reaches +12. Then the bag goes un-walkable (load in flight) and
        // the same weapon is re-granted (reconnect). With the bag unreadable, auto_upgrade must
        // return the input UNCHANGED (identity) -- never a LOWERED id -- and recover once the bag is
        // walkable again. This is the down-flicker guard in pure form.
        let out = replay(
            &[
                Ev::Grant(WEAPON_BASE + 12), // held -> 12
                Ev::BagWalkable(false),      // load in flight
                Ev::Grant(WEAPON_BASE + 12), // re-grant: bag unreadable -> identity (+12), NOT lowered
                Ev::Grant(WEAPON_BASE), // a fresh +0 during the miss -> identity (+0), NOT guessed
                Ev::BagWalkable(true),  // bag back
                Ev::Grant(WEAPON_BASE), // now resolves to the held +12 again
            ],
            true,
        );
        let levels: Vec<i32> = out.iter().map(|&id| id - WEAPON_BASE).collect();
        assert_eq!(
            levels,
            vec![12, 12, 0, 12],
            "a transient bag miss returns the input unchanged (never lowers); recovers after"
        );
    }

    #[test]
    fn off_burst_is_identity() {
        // With the feature off, no grant in the burst is ever touched, regardless of the bag.
        let out = replay(
            &[
                Ev::Grant(WEAPON_BASE),
                Ev::Grant(WEAPON_BASE + 3),
                Ev::Grant(WEAPON_BASE),
            ],
            false,
        );
        assert_eq!(out, vec![WEAPON_BASE, WEAPON_BASE + 3, WEAPON_BASE]);
    }
}

// ================================================================================================
// #296 / #302 / #303 — the AUTO_EQUIP QUEUE must hold the id that ENTERS THE BAG.
// ================================================================================================
//
// auto_equip queues a received FullID and, on a later tick, looks it up in the inventory by EXACT
// FullID (`eldenring-archipelago/src/auto_equip.rs`, `owned.get(&full)`). The grant path runs
// `apply_auto_upgrade` on its way into the bag, so with `auto_upgrade` ON an upgradeable weapon is
// QUEUED as `base + 0` and LANDS as `base + N`. The lookup misses, the id goes back on
// `still_pending`, and it retries for the rest of the session.
//
// Protectors are identity under `apply_auto_upgrade`, so armour is untouched -- which is exactly
// boblerrr's report: "armor still auto-equips fine -- it's specifically weapons that fail." His
// 2026-08-03 log has 8 successful equips: 7 protectors, plus one weapon (param 52080000,
// Lordsworn's Bolt) that is AMMUNITION and so has no reinforce run for auto_upgrade to raise.
// Zero upgradeable weapons equipped all session.
//
// The fix routes `enqueue` through the same predicate the grant runs, so the queue and the bag
// come from ONE call. Since the 2026-08-04 inert-test audit (F1) that routing is a NAMED er-logic
// seam -- `crate::auto_equip::enqueue_id` -- because the first version of this module compared two
// local aliases of `apply_auto_upgrade` and could not fail: deleting the fix at the Windows enqueue
// site left the whole workspace green. These tests now put the two PRODUCTION paths on the two
// sides of every assert: `queued` is `enqueue_id` (what `auto_equip::enqueue` stores, via
// `upgrades.rs enqueue_upgrade_id`) and `bagged` is `apply_auto_upgrade` (what `detour.rs
// grant_full_id` puts in the bag) -- plus the EXACT expected id, so agreement cannot be satisfied
// by both sides drifting together. Neutralise the upgrade application inside `enqueue_id` and
// `post_fix_queue_matches_the_bag_for_an_upgraded_weapon` and
// `queue_matches_the_bag_at_every_held_level` go red (mutation-verified before landing).

// THE SECOND HALF OF THE SAME QUESTION (#413, boblerrr 2026-08-07 18:31:38).
//
// `auto_equip_queue_matches_bag` below pins the queue against THE GRANT PATH: `bagged` is
// `apply_auto_upgrade`, i.e. what a grant is about to deposit. That premise is exactly right for a
// RECEIVE and it is the reason those tests could not witness #101's defect -- the fight-equip
// queues for an item that is ALREADY in the bag, banked at whatever the target was on the day it
// arrived, with no grant coming to reconcile anything. Both sides of every assert down there are
// computed from TODAY's target, so a bag holding yesterday's level is not representable.
//
// These tests model the thing that module structurally cannot: a bag whose contents were fixed in
// the past, and a target that has moved since.

#[cfg(test)]
mod fight_equip_queues_what_the_bag_actually_holds {
    use crate::auto_equip::held_row_to_equip;
    use crate::boss_grants::SERPENT_HUNTER_BASE;
    use crate::hook::GameHook;
    use crate::upgrades::apply_auto_upgrade;

    /// The bag as of bobler's 18:31:38 tick: the Serpent-Hunter banked at +0, and a normal-track
    /// high-water mark that has since climbed to +3.
    struct Bag {
        held_normal: i32,
    }

    impl GameHook for Bag {
        fn get_event_flag(&self, _f: u32) -> bool {
            false
        }
        fn set_event_flag(&mut self, _f: u32, _on: bool) {}
        fn try_set_event_flag(&mut self, _f: u32, _on: bool) -> bool {
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
        fn weapon_track_and_cap(&self, base: i32) -> Option<(i32, bool)> {
            (base == SERPENT_HUNTER_BASE).then_some((25, false))
        }
        fn highest_held_level(&self, somber: bool) -> Option<i32> {
            Some(if somber { 0 } else { self.held_normal })
        }
        fn scadutree_blessing(&self) -> Option<i32> {
            None
        }
        fn set_scadutree_blessing(&mut self, _l: i32) {}
    }

    #[test]
    fn the_motivating_case_in_exact_values() {
        // boblerrr 2026-08-07 18:31:38, verbatim. The spear in the bag is +0; the target is +3.
        let bag = Bag { held_normal: 3 };
        let in_the_bag = [SERPENT_HUNTER_BASE];

        // What #101 queued: the receive path's raise. His log line is the proof it ran --
        // `auto_upgrade: 0x103db70 -> 0x103db73 (enqueue)`.
        let what_101_queued = apply_auto_upgrade(&bag, true, SERPENT_HUNTER_BASE);
        assert_eq!(what_101_queued, 17_030_003);
        // ...and 17030003 is in nobody's bag, which is why the drain retried in silence forever.
        assert!(!in_the_bag.contains(&what_101_queued));

        // What the fix queues: the row the bag reports.
        let queued = held_row_to_equip(SERPENT_HUNTER_BASE, in_the_bag.iter().copied());
        assert_eq!(queued, Some(17_030_000));
        assert!(in_the_bag.contains(&queued.unwrap()));
    }

    #[test]
    fn a_queued_id_the_bag_does_not_hold_is_the_bug_shape() {
        // The invariant, stated as a property rather than one pair of numbers: whatever this
        // returns must be an id the bag literally contains, at EVERY target the run can reach.
        // Mutation-verified before landing: dropping the `b == base` filter, swapping `max_by_key`
        // for `min_by_key`, or returning `None` each turns this module red.
        for target in 0..=25 {
            let bag = Bag {
                held_normal: target,
            };
            let in_the_bag = [SERPENT_HUNTER_BASE];
            let queued = held_row_to_equip(SERPENT_HUNTER_BASE, in_the_bag.iter().copied())
                .expect("a bag holding the base row must resolve");
            assert!(
                in_the_bag.contains(&queued),
                "target +{target} queued {queued:#x}, which the bag does not hold"
            );
            // The raise would have named a row that is not there, for every target above +0.
            let raised = apply_auto_upgrade(&bag, true, SERPENT_HUNTER_BASE);
            assert_eq!(raised != queued, target > 0, "target +{target}");
        }
    }

    #[test]
    fn several_levels_held_takes_the_strongest() {
        // Raise-only means the target is a floor, so the best copy is the one the intent points
        // at -- and it is the only tie-break that does not depend on inventory ORDER.
        let bag = [
            SERPENT_HUNTER_BASE + 3,
            SERPENT_HUNTER_BASE,
            SERPENT_HUNTER_BASE + 9,
            SERPENT_HUNTER_BASE + 1,
        ];
        assert_eq!(
            held_row_to_equip(SERPENT_HUNTER_BASE, bag.iter().copied()),
            Some(SERPENT_HUNTER_BASE + 9)
        );
        // Order-independence, stated: reversing the walk cannot change the answer.
        assert_eq!(
            held_row_to_equip(SERPENT_HUNTER_BASE, bag.iter().rev().copied()),
            Some(SERPENT_HUNTER_BASE + 9)
        );
    }

    #[test]
    fn an_empty_bag_and_a_neighbouring_row_both_decline() {
        assert_eq!(held_row_to_equip(SERPENT_HUNTER_BASE, []), None);
        // A different weapon row, and one that is only a near-miss on the base arithmetic.
        assert_eq!(
            held_row_to_equip(
                SERPENT_HUNTER_BASE,
                [
                    1_000_000,
                    SERPENT_HUNTER_BASE - 100,
                    SERPENT_HUNTER_BASE + 100
                ]
            ),
            None
        );
    }

    #[test]
    fn a_protector_in_the_walk_is_never_mistaken_for_a_level() {
        // Category nibble 0x1. `decode_weapon_id` guards on it, so the row arithmetic never runs
        // on a protector -- a caller that hands this the whole bag gets None, not a wrong slot.
        const PROTECTOR: i32 = 0x1003_3450;
        assert_eq!(held_row_to_equip(SERPENT_HUNTER_BASE, [PROTECTOR]), None);
        assert_eq!(
            held_row_to_equip(SERPENT_HUNTER_BASE, [PROTECTOR, SERPENT_HUNTER_BASE + 2]),
            Some(SERPENT_HUNTER_BASE + 2)
        );
    }

    #[test]
    fn the_receive_path_is_deliberately_left_alone() {
        // The fix must NOT become "stop raising everywhere". A received weapon still has a grant
        // coming that deposits `base + target`, so its queue entry still has to be raised --
        // that is #296/#302/#303, and the module below is what pins it.
        let bag = Bag { held_normal: 3 };
        assert_eq!(
            apply_auto_upgrade(&bag, true, SERPENT_HUNTER_BASE),
            17_030_003
        );
    }
}

#[cfg(test)]
mod auto_equip_queue_matches_bag {
    use crate::auto_equip::enqueue_id;
    use crate::hook::GameHook;
    use crate::upgrades::apply_auto_upgrade;

    /// One upgradeable normal-track weapon at +0, cap +25.
    const WEAPON_BASE: i32 = 1_000_000;
    /// A protector FullID (category nibble 0x1) -- identity under apply_auto_upgrade.
    const PROTECTOR: i32 = 0x1003_3450;

    /// Minimal bag model: a track/cap table and a per-track high-water mark. Deliberately its own
    /// model rather than the reconnect harness above -- this asks a different question (do two
    /// call sites agree?) and should not inherit that timeline's state.
    struct Bag {
        held_normal: i32,
    }

    impl GameHook for Bag {
        fn get_event_flag(&self, _f: u32) -> bool {
            false
        }
        fn set_event_flag(&mut self, _f: u32, _on: bool) {}
        fn try_set_event_flag(&mut self, _f: u32, _on: bool) -> bool {
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
        fn weapon_track_and_cap(&self, base: i32) -> Option<(i32, bool)> {
            (base == WEAPON_BASE).then_some((25, false))
        }
        fn highest_held_level(&self, somber: bool) -> Option<i32> {
            Some(if somber { 0 } else { self.held_normal })
        }
        fn scadutree_blessing(&self) -> Option<i32> {
            None
        }
        fn set_scadutree_blessing(&mut self, _l: i32) {}
    }

    /// What `enqueue` stored BEFORE the fix: the raw received id. Kept as the documented bug shape.
    fn queued_pre_fix(_b: &Bag, full_id: i32) -> i32 {
        full_id
    }
    /// What production `enqueue` QUEUES: the er-logic seam the Windows crate calls
    /// (`auto_equip.rs enqueue` -> `upgrades.rs enqueue_upgrade_id` -> this fn).
    fn queued(b: &Bag, full_id: i32) -> i32 {
        enqueue_id(b, true, full_id)
    }
    /// What the grant actually puts in the bag (`detour.rs grant_full_id` -> `apply_auto_upgrade`).
    fn bagged(b: &Bag, full_id: i32) -> i32 {
        apply_auto_upgrade(b, true, full_id)
    }

    #[test]
    fn pre_fix_queue_misses_an_auto_upgraded_weapon() {
        // Documents the bug in EXACT values. (The earlier form was a bare `assert_ne!` -- a
        // negative with two causes, which stayed green under a wrong-constant mutation of the
        // whole mechanism.) Holding a +5: the pre-fix queue stored base+0 while the grant landed
        // base+5, so the exact-FullID lookup could never hit.
        let b = Bag { held_normal: 5 };
        assert_eq!(queued_pre_fix(&b, WEAPON_BASE), WEAPON_BASE);
        assert_eq!(bagged(&b, WEAPON_BASE), WEAPON_BASE + 5);
    }

    #[test]
    fn post_fix_queue_matches_the_bag_for_an_upgraded_weapon() {
        let b = Bag { held_normal: 5 };
        assert_eq!(
            queued(&b, WEAPON_BASE),
            WEAPON_BASE + 5,
            "the enqueue seam must raise the queued id to the held level",
        );
        assert_eq!(
            queued(&b, WEAPON_BASE),
            bagged(&b, WEAPON_BASE),
            "#296/#302/#303: the queued id must be the id auto_equip will find in the bag",
        );
    }

    #[test]
    fn the_fix_is_inert_when_there_is_nothing_to_raise() {
        // Target +0: pre-fix and post-fix agree, so a fresh character sees no behaviour change.
        let b = Bag { held_normal: 0 };
        assert_eq!(queued(&b, WEAPON_BASE), WEAPON_BASE);
        assert_eq!(bagged(&b, WEAPON_BASE), WEAPON_BASE);
    }

    #[test]
    fn the_enqueue_seam_is_identity_when_auto_upgrade_is_off() {
        // Off means off: with auto_upgrade off the queue holds exactly the received id, even
        // with a higher weapon in the bag.
        let b = Bag { held_normal: 5 };
        assert_eq!(enqueue_id(&b, false, WEAPON_BASE), WEAPON_BASE);
    }

    #[test]
    fn a_protector_is_unaffected_by_the_fix() {
        // Identity under apply_auto_upgrade -- which is WHY armour kept working. Must stay identity.
        let b = Bag { held_normal: 5 };
        assert_eq!(queued(&b, PROTECTOR), PROTECTOR);
        assert_eq!(bagged(&b, PROTECTOR), PROTECTOR);
    }

    #[test]
    fn queue_matches_the_bag_at_every_held_level() {
        // Applying the predicate at enqueue time is only correct if the grant, moments later, sees
        // the same target. Production shares the 1500ms UPGRADE_TARGETS cache; here we assert the
        // weaker sufficient property -- for any bag state the two production paths agree, AND on
        // the exact id, so the assert cannot be satisfied by both sides drifting together.
        for lvl in [0, 1, 5, 10, 25] {
            let b = Bag { held_normal: lvl };
            assert_eq!(
                queued(&b, WEAPON_BASE),
                WEAPON_BASE + lvl,
                "queued id wrong at held level {lvl}",
            );
            assert_eq!(
                queued(&b, WEAPON_BASE),
                bagged(&b, WEAPON_BASE),
                "queue/bag disagreed at held level {lvl}",
            );
        }
    }
}
