//! `flask_grant_replay` — headless timeline replay for the start-item GRANT DEDUP across a reload.
//!
//! Third in the family with [`crate::start_grant_replay`] and [`crate::region_lock_replay`], for the
//! next timing bug in the same untested surface. The grant *decision* — grant the start-items queue
//! exactly once per save — already lives in the pure, host-tested [`crate::grants::drain_start_items`],
//! whose dedup is the persisted [`crate::save_state::SaveState::start_items_granted`] flag. What has no
//! host test is the *lifetime* of that guard across a mid-tutorial reload: if the once-per-save latch
//! is reset by the reload, the drain re-queues from slot_data and grants the flasks a SECOND time.
//!
//! WHY THIS EXISTS (flask double-grant, er-flask-double-grant-reconnect, 2026-07-05): a fresh Grafted
//! Scion start dies in the tutorial and the game reloads. If the once-per-save dedup lived only in a
//! SESSION latch (reset by that reload) instead of PERSISTED save state, the post-reload tick re-runs
//! the start-item drain and the player ends up with 6 heal + 2 FP flasks from a 3 + 1 start — exactly
//! the doubling reported in-game. The fix is the same shape as the Torch clobber and the region-lock
//! bloom: the dedup must latch on OBSERVED PERSISTED state, not a proxy that a reload wipes.
//!
//! A dedup-lifetime bug is invisible to slot_data / shape validation AND to the single-tick `FakeGame`
//! (which never reloads). This module models the game-state TIMELINE — receive start items, reload,
//! tick — so the fix is provable offline, on any host, and stays a regression. It reuses the REAL
//! `drain_start_items`; the pure decision here is captured by [`start_guard_survives_reload`].

/// Does the once-per-save start-item dedup survive a save/new-game reload? `true` = the guard is
/// persisted in save state (the fix — a tutorial-death reload can't re-grant); `false` = the guard is
/// a session-only latch that the reload resets (the bug — the drain re-fires and flasks double).
/// Pure, host-tested home for the policy the Windows `core.rs` must honor: keep the dedup in the
/// persisted `SaveState`, never in a re-initialized session field.
pub fn start_guard_survives_reload(persist_guard: bool) -> bool {
    persist_guard
}

#[cfg(test)]
mod replay {
    use super::*;
    use crate::grants::drain_start_items;
    use crate::hook::GameHook;
    use crate::save_state::SaveState;
    use std::collections::{HashMap, VecDeque};

    /// Illustrative start-item FullIDs. The live client resolves the real flask goods ids from
    /// slot_data; the harness only needs stable tokens to count through the timeline. 3 heal + 1 FP
    /// mirrors the reported starting loadout (Flask of Crimson Tears x3, Flask of Cerulean Tears x1).
    const HEAL_FLASK_FULL_ID: i32 = 1001;
    const FP_FLASK_FULL_ID: i32 = 1002;
    const HEAL_START_QTY: i32 = 3;
    const FP_START_QTY: i32 = 1;

    /// The start-item queue the client re-derives from slot_data every tick (before the persisted
    /// dedup drops it). One entry per stacked flask, matching the reported 3 + 1 loadout.
    fn start_queue() -> VecDeque<(i32, i32)> {
        [
            (HEAL_FLASK_FULL_ID, HEAL_START_QTY),
            (FP_FLASK_FULL_ID, FP_START_QTY),
        ]
        .into_iter()
        .collect()
    }

    /// A game model that — unlike the single-tick `FakeGame` — tracks the LIVE inventory contents so
    /// a re-fired grant ACCUMULATES (the double). This is the state the flask double-grant lives in.
    struct ClobberGame {
        /// Inventory pointer captured (start items can only place once this is true).
        inventory_ready: bool,
        /// What is ACTUALLY in the bag right now; a re-fired grant pushes flasks a second time.
        live_inventory: Vec<(i32, i32)>,
        flags: HashMap<u32, bool>,
    }

    impl ClobberGame {
        fn new() -> Self {
            ClobberGame {
                inventory_ready: true,
                live_inventory: Vec::new(),
                flags: HashMap::new(),
            }
        }

        /// Total quantity of a FullID currently held, summed across every grant that landed. A
        /// once-granted flask reads its start qty; a double-granted flask reads twice that.
        fn qty_of(&self, full_id: i32) -> i32 {
            self.live_inventory
                .iter()
                .filter(|&&(id, _)| id == full_id)
                .map(|&(_, qty)| qty)
                .sum()
        }
    }

    impl GameHook for ClobberGame {
        fn get_event_flag(&self, flag: u32) -> bool {
            self.flags.get(&flag).copied().unwrap_or(false)
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
        fn grant_full_id(&mut self, full_id: i32, qty: i32) -> bool {
            if !self.inventory_ready {
                return false; // no inventory pointer yet -> caller requeues the whole start queue
            }
            self.live_inventory.push((full_id, qty));
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

    /// One frame of the session timeline.
    #[derive(Clone, Copy)]
    enum Ev {
        /// The client re-derives the start-item queue from slot_data and attempts the real drain
        /// (as `update_live` does every tick).
        ReceiveStartItems,
        /// A plain tick: re-derive + drain again (the queue is rebuilt each frame, so the persisted
        /// dedup is the only thing stopping a re-grant).
        Tick,
        /// The Grafted Scion tutorial death reloads the save/new-game. The persisted SaveState
        /// survives; a SESSION-only latch would be reset here. This is the reload that doubles flasks.
        TutorialDeathReload,
        /// The inventory pointer is (re)captured or lost — models the drain being unable to place
        /// this tick (all-or-nothing requeue).
        InventoryReady(bool),
    }

    /// Replay a timeline, attempting the real start-item drain each tick (as `update_live` does).
    ///
    /// `persist_guard = false` reproduces the bug: the once-per-save dedup lives in a SESSION latch
    /// that the tutorial-death reload resets, so the post-reload drain re-fires and flasks double.
    /// `persist_guard = true` keeps the dedup in the PERSISTED `SaveState` (the fix), so the reload
    /// can't re-grant. Returns the final game so tests can count the bag.
    fn replay(events: &[Ev], persist_guard: bool) -> ClobberGame {
        let mut g = ClobberGame::new();
        // The persisted guard. On a reload with the SESSION-only latch (the bug), the client
        // re-initializes this to Default -> `start_items_granted = false` -> the drain re-fires.
        let mut save = SaveState::default();
        for &ev in events {
            match ev {
                Ev::ReceiveStartItems | Ev::Tick => {}
                Ev::InventoryReady(v) => g.inventory_ready = v,
                Ev::TutorialDeathReload => {
                    if !start_guard_survives_reload(persist_guard) {
                        // Session-only latch: the reload wipes the dedup, so the persisted flag the
                        // real drain reads is reset and the start-item grant fires a SECOND time.
                        save.start_items_granted = false;
                    }
                    // With the fix, `save` is untouched across the reload (persisted), so the drain
                    // sees `start_items_granted == true` and clears the re-derived queue without
                    // granting.
                }
            }
            // Every tick the client re-derives the queue from slot_data; the persisted
            // `start_items_granted` latch is what dedups it. Mirror that exactly.
            let mut queue = start_queue();
            drain_start_items(&mut g, &mut queue, &mut save);
        }
        g
    }

    #[test]
    fn flasks_double_without_persisted_guard() {
        // Pre-fix (session-only latch): the drain fires once on receipt, then the tutorial-death
        // reload resets the latch and the next tick fires it AGAIN -> 6 heal + 2 FP from a 3 + 1
        // start. Reproduces er-flask-double-grant-reconnect.
        let timeline = [
            Ev::ReceiveStartItems,      // first grant: 3 heal + 1 FP
            Ev::Tick,                   // persisted latch would dedup here...
            Ev::TutorialDeathReload,    // ...but the session latch is reset
            Ev::Tick,                   // second grant: another 3 heal + 1 FP
        ];
        let g = replay(&timeline, false);
        assert_eq!(
            g.qty_of(HEAL_FLASK_FULL_ID),
            HEAL_START_QTY * 2,
            "regression guard: the reset session latch double-grants heal flasks (documents the bug)"
        );
        assert_eq!(
            g.qty_of(FP_FLASK_FULL_ID),
            FP_START_QTY * 2,
            "regression guard: the reset session latch double-grants FP flasks (documents the bug)"
        );
    }

    #[test]
    fn flasks_granted_once_with_persisted_guard() {
        // With the fix the dedup lives in the PERSISTED SaveState: the reload leaves
        // `start_items_granted == true`, so the post-reload drain clears the re-derived queue
        // without granting. The player keeps exactly the 3 + 1 start.
        let timeline = [
            Ev::ReceiveStartItems,
            Ev::Tick,
            Ev::TutorialDeathReload,
            Ev::Tick,
            Ev::Tick, // extra reconnect-style re-derives must still not re-grant
        ];
        let g = replay(&timeline, true);
        assert_eq!(
            g.qty_of(HEAL_FLASK_FULL_ID),
            HEAL_START_QTY,
            "persisted guard must grant heal flasks exactly once across the reload"
        );
        assert_eq!(
            g.qty_of(FP_FLASK_FULL_ID),
            FP_START_QTY,
            "persisted guard must grant FP flasks exactly once across the reload"
        );
    }

    #[test]
    fn no_grant_until_inventory_is_ready_then_grants_once() {
        // The drain is all-or-nothing: while the inventory pointer is missing it must place NOTHING
        // and leave the persisted latch unset, so the flasks land in full exactly once when the bag
        // comes live — never a partial or a miss that later doubles.
        let timeline = [
            Ev::InventoryReady(false),
            Ev::ReceiveStartItems, // can't place -> nothing granted, latch stays false
            Ev::Tick,
            Ev::InventoryReady(true),
            Ev::Tick, // now it lands, in full
        ];
        let g = replay(&timeline, true);
        assert_eq!(g.qty_of(HEAL_FLASK_FULL_ID), HEAL_START_QTY, "must grant in full once ready");
        assert_eq!(g.qty_of(FP_FLASK_FULL_ID), FP_START_QTY, "must grant in full once ready");
    }

    #[test]
    fn multiple_reloads_with_persisted_guard_never_re_grant() {
        // A player who dies to the Scion tutorial repeatedly must still end up with a single start
        // loadout: the persisted guard holds across every reload.
        let timeline = [
            Ev::ReceiveStartItems,
            Ev::TutorialDeathReload,
            Ev::Tick,
            Ev::TutorialDeathReload,
            Ev::Tick,
            Ev::TutorialDeathReload,
            Ev::Tick,
        ];
        let g = replay(&timeline, true);
        assert_eq!(g.qty_of(HEAL_FLASK_FULL_ID), HEAL_START_QTY, "no re-grant across repeated reloads");
        assert_eq!(g.qty_of(FP_FLASK_FULL_ID), FP_START_QTY, "no re-grant across repeated reloads");
    }

    #[test]
    fn pure_policy_semantics() {
        assert!(!start_guard_survives_reload(false), "session latch does not survive a reload");
        assert!(start_guard_survives_reload(true), "persisted save-state guard survives a reload");
    }
}
