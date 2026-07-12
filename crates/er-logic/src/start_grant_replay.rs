//! `start_grant_replay` — a headless timeline-replay test tier for start-item GRANT SEQUENCING.
//!
//! The grant *decisions* (once-per-save, all-or-nothing on a missing inventory pointer) already live
//! in the pure, host-tested [`crate::grants::drain_start_items`]. The grant *sequencing/timing* —
//! WHEN it is safe to fire the drain — lives only in the Windows glue (`eldenring-ap` `core.rs`
//! `update_live`) and had no host test. That is exactly where the 2026-07-06 Torch-clobber bug hid,
//! and the whole class of timing bugs (flask double-grant, torrent hand-off) shares that untested
//! surface.
//!
//! WHY THIS EXISTS (Torch clobber, 2026-07-06): `USE_STATIC_INVENTORY_PRIME` lets the client capture
//! the inventory pointer during the LOAD SCREEN, so the start-item grant can fire *before* the
//! save / new-game bulk inventory load completes. That bulk load then REPLACES the bag and wipes the
//! just-granted Torch; because the grant latched `start_items_granted` with no read-back, it never
//! retries and the Torch is gone forever. The fix (`patch_greenfield_start_item_clobber.py`) defers
//! the grant until the inventory is genuinely live: a real game AddItem has fired (bulk load done)
//! OR we've been in-world >= 8s.
//!
//! A sequencing bug is invisible to slot_data / shape validation AND to the single-tick `FakeGame`
//! (which has no notion of a later clobber). This module models the game-state TIMELINE — load
//! screen, bulk-load clobber, real pickup — so the fix is provable offline, on any host, and stays
//! a regression. It reuses the REAL `drain_start_items`; the only new production logic is the pure
//! [`start_items_settled`] gate, which the Windows `core.rs` should call instead of its inline copy.

/// The inventory is SETTLED (safe to place start items without a clobber) once the game has driven a
/// real AddItem (`real_pickup_seen` — proves the save / new-game bulk load replace is done) OR we've
/// been continuously in-world at least [`START_ITEM_SETTLE_MS`] (fallback when the player triggers no
/// pickup). Pure, host-tested home for the gate the Windows `core.rs` currently inlines.
pub const START_ITEM_SETTLE_MS: u64 = 8_000;

/// Pure clobber-avoidance gate for the start-item drain. See [`START_ITEM_SETTLE_MS`].
pub fn start_items_settled(real_pickup_seen: bool, in_world_ms: u64) -> bool {
    real_pickup_seen || in_world_ms >= START_ITEM_SETTLE_MS
}

#[cfg(test)]
mod replay {
    use super::*;
    use crate::grants::drain_start_items;
    use crate::hook::GameHook;
    use crate::save_state::SaveState;
    use std::collections::{HashMap, VecDeque};

    /// Illustrative start-item FullID. The live client resolves the real Torch goods id from
    /// slot_data; the harness only needs a stable token to track through the timeline.
    const TORCH_FULL_ID: i32 = 2008;

    /// A game model that — unlike the single-tick `FakeGame` — tracks the LIVE inventory contents and
    /// can replay a save / new-game bulk load that CLOBBERS items granted before it. This is the
    /// state the Torch-clobber bug actually lives in.
    struct ClobberGame {
        in_world: bool,
        /// Inventory pointer captured (the static prime sets this true during the load screen).
        inventory_ready: bool,
        /// What is ACTUALLY in the bag right now (a bulk load clears this).
        live_inventory: Vec<(i32, i32)>,
        /// Set once the game itself drives AddItem (a real pickup / the post-load populate).
        real_pickup_seen: bool,
        flags: HashMap<u32, bool>,
    }

    impl ClobberGame {
        fn new() -> Self {
            ClobberGame {
                in_world: false,
                inventory_ready: false,
                live_inventory: Vec::new(),
                real_pickup_seen: false,
                flags: HashMap::new(),
            }
        }

        /// The save / new-game inventory load lands: it REPLACES the bag (wiping any grant made
        /// during the load screen) and is itself the first real AddItem (inventory now live).
        fn bulk_inventory_load(&mut self) {
            self.live_inventory.clear();
            self.real_pickup_seen = true;
        }

        fn holds(&self, full_id: i32) -> bool {
            self.live_inventory.iter().any(|&(id, _)| id == full_id)
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
            self.in_world
        }
        fn play_region_id(&self) -> Option<i32> {
            None
        }
        fn grant_full_id(&mut self, full_id: i32, qty: i32) -> bool {
            if !self.inventory_ready {
                return false; // no inventory pointer yet -> caller requeues
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
        /// Time passes (ms); the in-world clock only advances while in-world.
        Tick(u64),
        /// Load screen: the static prime captures the inventory pointer and the in-world flag flips
        /// true, but the save data hasn't replaced the bag yet (region still 0 in the real game).
        EnterLoadScreen,
        /// The save / new-game bulk inventory load completes -> clobber + real pickup seen.
        BulkInventoryLoad,
    }

    /// Replay a timeline, attempting the real start-item drain each tick (as `update_live` does).
    /// `reconcile_gate = false` reproduces today's pre-fix behavior (grant as soon as the inventory
    /// pointer exists); `true` gates the drain on [`start_items_settled`]. Returns the final game.
    fn replay(events: &[Ev], reconcile_gate: bool) -> ClobberGame {
        let mut g = ClobberGame::new();
        let mut save = SaveState::default();
        let mut in_world_ms: u64 = 0;
        for &ev in events {
            match ev {
                Ev::Tick(ms) => {
                    if g.in_world {
                        in_world_ms += ms;
                    }
                }
                Ev::EnterLoadScreen => {
                    g.in_world = true;
                    g.inventory_ready = true;
                    in_world_ms = 0;
                }
                Ev::BulkInventoryLoad => g.bulk_inventory_load(),
            }
            let settled = !reconcile_gate || start_items_settled(g.real_pickup_seen, in_world_ms);
            if settled {
                // The live client re-queues start items from slot_data each tick; the persisted
                // `start_items_granted` latch dedups. Mirror that here.
                let mut queue: VecDeque<(i32, i32)> = [(TORCH_FULL_ID, 1)].into_iter().collect();
                drain_start_items(&mut g, &mut queue, &mut save);
            }
        }
        g
    }

    #[test]
    fn torch_is_clobbered_without_the_reconcile_gate() {
        // Pre-fix: the grant fires the instant the inventory pointer is captured (load screen), then
        // the bulk load wipes it and the latched grant never retries. Reproduces the 2026-07-06 bug.
        let timeline = [
            Ev::EnterLoadScreen, // inventory_ready -> grant fires here, far too early
            Ev::Tick(3_000),
            Ev::BulkInventoryLoad, // clobbers the just-granted Torch
            Ev::Tick(5_000),
        ];
        let g = replay(&timeline, false);
        assert!(
            !g.holds(TORCH_FULL_ID),
            "regression guard: without the settle gate the Torch is clobbered (documents the bug)"
        );
    }

    #[test]
    fn torch_survives_with_the_reconcile_gate() {
        // With the gate the drain waits through the load screen and fires only AFTER the bulk load
        // marks the inventory live -> the Torch is placed post-clobber and survives.
        let timeline = [
            Ev::EnterLoadScreen,
            Ev::Tick(3_000),
            Ev::BulkInventoryLoad, // real_pickup_seen = true; the very next drain lands safely
            Ev::Tick(100),
        ];
        let g = replay(&timeline, true);
        assert!(
            g.holds(TORCH_FULL_ID),
            "with the settle gate the Torch must survive the save / new-game load"
        );
    }

    #[test]
    fn settle_gate_falls_back_to_8s_when_no_pickup_ever_fires() {
        // No bulk load / real pickup ever occurs (the player triggers nothing). The 8s fallback must
        // still arm the grant; before 8s it must keep waiting.
        let waiting = [Ev::EnterLoadScreen, Ev::Tick(START_ITEM_SETTLE_MS - 1)];
        assert!(
            !replay(&waiting, true).holds(TORCH_FULL_ID),
            "before the fallback window the grant must still be deferred"
        );

        let armed = [Ev::EnterLoadScreen, Ev::Tick(START_ITEM_SETTLE_MS)];
        assert!(
            replay(&armed, true).holds(TORCH_FULL_ID),
            "the 8s fallback must arm the grant when no pickup fires"
        );
    }

    #[test]
    fn pure_gate_semantics() {
        assert!(!start_items_settled(false, 0));
        assert!(!start_items_settled(false, START_ITEM_SETTLE_MS - 1));
        assert!(start_items_settled(false, START_ITEM_SETTLE_MS));
        assert!(
            start_items_settled(true, 0),
            "a real pickup settles the inventory immediately"
        );
    }
}
