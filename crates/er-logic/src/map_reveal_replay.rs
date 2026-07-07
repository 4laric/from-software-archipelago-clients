//! `map_reveal_replay` — headless timeline replay for the map-reveal-on-connect bug.
//!
//! Twin of [`crate::start_grant_replay`] / [`crate::region_lock_replay`], for the map-reveal timing
//! bug. When a region unlocks (on connect, or on receiving its `<Region> Lock`), the client must
//! SET that region's map-REVEAL FLAGS — never GRANT the map-piece ITEM. The buggy path fired
//! `lockNotifyItems` REGION_MAP_ITEM grants on connect, so map-piece items materialized in the
//! player's inventory (a fresh seed "gives you map pieces just for connecting"). That is the
//! 2026-07-03 bug (er-map-pieces-granted-on-connect): the fix is to set reveal flags, not grant
//! items. See the converged client's `region.rs::open_on_received_name`, which correctly sets
//! `lock_reveal_flags` and grants nothing.
//!
//! SECOND, coupled defect (er-underground-map-quadrant-flags): the underground (Underworld) map
//! LAYER never paints even when its per-region FRAGMENT flags (62060-62064) are set, unless the
//! VIEW-unlock flag **82001** is also set (confirmed live 2026-07-04 via CE flag-logger bisection;
//! the client was missing it). So the underground region's reveal-flag set MUST include 82001.
//!
//! This module lifts the "which flags does a region unlock set" decision into pure, host-tested code
//! ([`map_reveal_flags`]) and replays the connect timeline through a game model that tracks BOTH set
//! flags AND granted items — so "no map-piece item ever lands in the bag" is provable offline and
//! stays a regression, exactly like the Torch-clobber and front-door-latch replays.

/// Base-game per-region world-map FRAGMENT reveal flags, mirroring the live client's
/// `MAP_REVEAL_FLAGS_BASE` (`eldenring-ap` `startgrants.rs`). A region unlock sets its own
/// fragment flag(s); the underground region additionally needs the view-unlock flag (below).
///
/// Representative regions only (this is an illustrative, host-tested seam, not the full table):
/// - `"Limgrave"`   -> 62010, 62011, 62012 (Limgrave W, Weeping, Limgrave E)  [MAP_REVEAL_FLAGS_BASE]
/// - `"Caelid"`     -> 62040, 62041        (Caelid, Dragonbarrow)             [MAP_REVEAL_FLAGS_BASE]
/// - `"Underground"`-> 62060..=62064 fragments + **82001** VIEW-unlock        [UNDERGROUND_MAP_VIEW_UNLOCK]
///
/// The underground row is the whole point: without 82001 the underground map never displays even
/// with its fragment flags set (er-underground-map-quadrant-flags).
const LIMGRAVE_REVEAL: &[u32] = &[62010, 62011, 62012];
const CAELID_REVEAL: &[u32] = &[62040, 62041];
/// Underground (Underworld) map FRAGMENT flags, mirroring `startgrants.rs` MAP_REVEAL_FLAGS_BASE.
const UNDERGROUND_FRAGMENTS: &[u32] = &[62060, 62061, 62062, 62063, 62064];
/// Underground (Underworld) map VIEW-unlock flag — distinct from the fragment flags; without it the
/// underground map layer never paints. `startgrants.rs::UNDERGROUND_MAP_VIEW_UNLOCK`.
/// (er-underground-map-quadrant-flags: confirmed live 2026-07-04.)
pub const UNDERGROUND_MAP_VIEW_UNLOCK: u32 = 82001;

/// The map-REVEAL FLAGS a region unlock should SET (never an item to grant). Pure, host-tested home
/// for the decision the buggy `lockNotifyItems`/REGION_MAP_ITEM path got wrong. Returns the region's
/// fragment flags; for the underground region it appends the 82001 view-unlock flag so the layer
/// actually paints. Unknown region -> empty (nothing to reveal, and — critically — nothing to grant).
pub fn map_reveal_flags(region: &str) -> Vec<u32> {
    match region {
        "Limgrave" => LIMGRAVE_REVEAL.to_vec(),
        "Caelid" => CAELID_REVEAL.to_vec(),
        "Underground" => {
            let mut v = UNDERGROUND_FRAGMENTS.to_vec();
            v.push(UNDERGROUND_MAP_VIEW_UNLOCK); // 82001 — er-underground-map-quadrant-flags
            v
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod replay {
    use super::*;
    use crate::hook::GameHook;
    use std::collections::HashMap;

    /// Illustrative REGION_MAP_ITEM FullID (the map-piece item the buggy path erroneously granted).
    /// The live client resolves the real map-piece goods id; the harness only needs a stable token
    /// to track through the timeline and prove it NEVER lands in the bag under the fix.
    const MAP_PIECE_FULL_ID: i32 = 8600;

    /// A game model that tracks BOTH set flags AND granted items, so the replay can assert the exact
    /// bug/fix distinction: the buggy path GRANTS a map-piece item; the fixed path SETS reveal flags
    /// and grants NOTHING.
    struct MapGame {
        flags: HashMap<u32, bool>,
        /// Ordered transcript of every grant that LANDED: (full_id, qty). A map-piece FullID appearing
        /// here on connect IS the bug.
        grants: Vec<(i32, i32)>,
        /// Inventory pointer captured (grants can land). True in these scenarios.
        inventory_ready: bool,
    }

    impl MapGame {
        fn new() -> Self {
            MapGame {
                flags: HashMap::new(),
                grants: Vec::new(),
                inventory_ready: true,
            }
        }
        fn is_set(&self, flag: u32) -> bool {
            self.flags.get(&flag).copied().unwrap_or(false)
        }
        /// Did a map-piece item land in the bag? (The bug's observable symptom.)
        fn holds_map_piece(&self) -> bool {
            self.grants.iter().any(|&(id, _)| id == MAP_PIECE_FULL_ID)
        }
    }

    impl GameHook for MapGame {
        fn get_event_flag(&self, flag: u32) -> bool {
            self.is_set(flag)
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
                return false; // no inventory pointer yet -> caller requeues
            }
            self.grants.push((full_id, qty));
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

    /// One frame of the connect timeline.
    #[derive(Clone, Copy)]
    enum Ev {
        /// Session connect settles (in-world, inventory live).
        Connect,
        /// A region's unlock apparatus fires (on connect, or on receiving its `<Region> Lock`).
        RegionUnlock(&'static str),
    }

    /// Replay a timeline. `reveal_flags_not_items = false` reproduces today's pre-fix behavior: a
    /// RegionUnlock GRANTS the REGION_MAP_ITEM map-piece (`lockNotifyItems`). `true` applies the fix:
    /// a RegionUnlock SETS the region's [`map_reveal_flags`] and grants NOTHING. Returns the game.
    fn replay(events: &[Ev], reveal_flags_not_items: bool) -> MapGame {
        let mut g = MapGame::new();
        for &ev in events {
            match ev {
                Ev::Connect => {}
                Ev::RegionUnlock(region) => {
                    if reveal_flags_not_items {
                        // FIX: set the region's reveal flags directly; grant no item.
                        for f in map_reveal_flags(region) {
                            g.set_event_flag(f, true);
                        }
                    } else {
                        // BUG: fire the lockNotifyItems REGION_MAP_ITEM grant (map piece -> inventory).
                        g.grant_full_id(MAP_PIECE_FULL_ID, 1);
                    }
                }
            }
        }
        g
    }

    #[test]
    fn map_piece_item_granted_on_connect_under_item_path() {
        // Pre-fix: a region unlock on connect fires the REGION_MAP_ITEM grant, so a map-piece FullID
        // lands in the inventory. Reproduces the 2026-07-03 "map pieces granted on connect" bug.
        let timeline = [Ev::Connect, Ev::RegionUnlock("Limgrave")];
        let g = replay(&timeline, false);
        assert!(
            g.holds_map_piece(),
            "regression guard: the item path grants a map-piece on connect (documents the bug)"
        );
    }

    #[test]
    fn reveal_flags_set_and_no_item_granted_under_fixed() {
        // Fix: the same unlock SETS the reveal flags and grants NOTHING.
        let timeline = [Ev::Connect, Ev::RegionUnlock("Limgrave")];
        let g = replay(&timeline, true);
        for f in map_reveal_flags("Limgrave") {
            assert!(g.is_set(f), "fixed path must set reveal flag {f}");
        }
        assert!(
            !g.holds_map_piece(),
            "fixed path must grant NO map-piece item (set reveal flags only)"
        );
        assert!(g.grants.is_empty(), "fixed path must grant nothing at all on a region unlock");
    }

    #[test]
    fn underground_unlock_sets_view_flag_82001() {
        // The underground region unlock must set BOTH the fragment flags AND the 82001 view-unlock,
        // or the underground map layer never paints (er-underground-map-quadrant-flags).
        let g = replay(&[Ev::Connect, Ev::RegionUnlock("Underground")], true);
        assert!(
            g.is_set(UNDERGROUND_MAP_VIEW_UNLOCK),
            "underground unlock must set the 82001 view-unlock flag"
        );
        for f in [62060, 62061, 62062, 62063, 62064] {
            assert!(g.is_set(f), "underground unlock must set fragment flag {f}");
        }
        assert!(!g.holds_map_piece(), "underground unlock must still grant no item");
    }

    #[test]
    fn pure_reveal_flag_semantics() {
        assert_eq!(map_reveal_flags("Limgrave"), vec![62010, 62011, 62012]);
        assert_eq!(map_reveal_flags("Caelid"), vec![62040, 62041]);
        // Underground = fragments + 82001, appended last.
        assert_eq!(
            map_reveal_flags("Underground"),
            vec![62060, 62061, 62062, 62063, 62064, UNDERGROUND_MAP_VIEW_UNLOCK]
        );
        assert!(
            map_reveal_flags("Underground").contains(&82001),
            "underground reveal set must include the 82001 view-unlock flag"
        );
        // Unknown region -> nothing to reveal AND nothing to grant.
        assert!(map_reveal_flags("Nowhere").is_empty());
    }
}
