//! `upgrades_replay` — headless timeline replay for the auto_upgrade RECONNECT GRANT-BURST.
//!
//! Sibling of [`crate::start_grant_replay`] / [`crate::flask_grant_replay`], for the auto_upgrade
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
            Some(if somber { self.held_somber } else { self.held_normal })
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
        assert_eq!(apply_auto_upgrade(&g, true, WEAPON_BASE + 20), WEAPON_BASE + 20);
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
        assert_eq!(levels, vec![8, 8, 25, 25], "monotone, capped at +25, no over-raise");
        assert!(levels.iter().all(|&l| l <= NORMAL_CAP));
        assert!(levels.windows(2).all(|w| w[1] >= w[0]), "never lowers across the burst");
    }

    #[test]
    fn transient_bag_miss_never_lowers_a_weapon() {
        // A weapon is granted and reaches +12. Then the bag goes un-walkable (load in flight) and
        // the same weapon is re-granted (reconnect). With the bag unreadable, auto_upgrade must
        // return the input UNCHANGED (identity) -- never a LOWERED id -- and recover once the bag is
        // walkable again. This is the down-flicker guard in pure form.
        let out = replay(
            &[
                Ev::Grant(WEAPON_BASE + 12),   // held -> 12
                Ev::BagWalkable(false),        // load in flight
                Ev::Grant(WEAPON_BASE + 12),   // re-grant: bag unreadable -> identity (+12), NOT lowered
                Ev::Grant(WEAPON_BASE),        // a fresh +0 during the miss -> identity (+0), NOT guessed
                Ev::BagWalkable(true),         // bag back
                Ev::Grant(WEAPON_BASE),        // now resolves to the held +12 again
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
            &[Ev::Grant(WEAPON_BASE), Ev::Grant(WEAPON_BASE + 3), Ev::Grant(WEAPON_BASE)],
            false,
        );
        assert_eq!(out, vec![WEAPON_BASE, WEAPON_BASE + 3, WEAPON_BASE]);
    }
}
