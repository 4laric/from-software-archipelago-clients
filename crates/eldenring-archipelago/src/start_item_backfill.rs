//! Inventory-verified start-item backfill: grant any `startItems` entry that isn't actually in the
//! bag. Backstops the persisted `start_items_granted` boolean, which lies for a character first
//! played before an item was added to `startItems` (live case: no healing flask on a Roundtable-hub
//! start -- the flask was in `startItems`, but the boolean said "already granted"). Logic +
//! flask-family handling live in `er_logic::start_backfill`.
//!
//! Runs ONCE per client launch (in-memory latch), in-world, AFTER the normal start-item drain has
//! had a chance to run (so on a fresh save it finds nothing missing and no double-grant), snapshot-
//! ting the inventory fresh each tick.
//!
//! TRADEOFF: an absent start item is re-granted on the next launch. For permanent items (flask,
//! weapons, key items) that's exactly right. A STACKABLE CONSUMABLE the player used up would refill
//! on relaunch -- if unwanted, gate this behind a persisted hash of the `startItems` content
//! (follow-up); today it re-runs per launch by design so a stale boolean can't strand an item.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{GameDataMan, ItemCategory};
use fromsoftware_shared::FromStatic;

static START_ITEMS: Mutex<Vec<i32>> = Mutex::new(Vec::new());
static DONE: AtomicBool = AtomicBool::new(false);

/// Set from slot_data `startItems` at connect (the same list `startgrants` parses).
pub fn set_start_items(items: Vec<i32>) {
    if let Ok(mut g) = START_ITEMS.lock() {
        *g = items;
    }
    DONE.store(false, Ordering::Relaxed);
}

/// FullID for a held item id: `(category<<28) | row`, matching the `startItems` / `grant_full_id`
/// encoding (`er_codec` category nibbles).
fn full_id_of(cat: ItemCategory, row: u32) -> u32 {
    let nibble: u32 = match cat {
        ItemCategory::Weapon => 0x0000_0000,
        ItemCategory::Protector => 0x1000_0000,
        ItemCategory::Accessory => 0x2000_0000,
        ItemCategory::Goods => 0x4000_0000,
        ItemCategory::Gem => 0x8000_0000,
    };
    nibble | (row & 0x0FFF_FFFF)
}

/// Per-tick until done: once the normal start-item drain has completed (`start_items_granted`) and
/// we're in-world with the inventory populated, grant any `startItems` not in the bag. Gating on
/// `start_items_granted` means this only ever acts as the backstop for a STALE boolean -- it never
/// races or doubles a fresh drain (which runs while the boolean is still false).
pub fn tick(start_items_granted: bool) {
    if DONE.load(Ordering::Relaxed) || !start_items_granted || !crate::flags::in_world() {
        return;
    }
    let items = match START_ITEMS.lock() {
        Ok(g) if !g.is_empty() => g.clone(),
        _ => return, // no startItems (or lock poisoned) -- nothing to backfill
    };

    // SAFETY: FD4 singleton; read on the single-threaded FrameBegin tick. Same path as inventory.rs.
    let Ok(gdm) = (unsafe { GameDataMan::instance() }) else {
        return;
    };
    let pgd = gdm.main_player_game_data.as_ref();

    // Snapshot the held inventory as FullIDs.
    let mut present: HashSet<u32> = HashSet::new();
    for entry in pgd.equipment.equip_inventory_data.items_data.items() {
        // `entry.item_id` is a valid `ItemId` here (not `OptionalItemId`), so category()/param_id()
        // return the values directly -- same access inventory.rs::scan_synthetics uses.
        present.insert(full_id_of(
            entry.item_id.category(),
            entry.item_id.param_id(),
        ));
    }
    if present.is_empty() {
        return; // inventory holder not populated yet -- retry next tick (don't latch)
    }

    let missing = er_logic::start_backfill::missing_start_items(&present, &items);
    if !missing.is_empty() {
        let mut granted = 0u32;
        for &fid in &missing {
            if crate::detour::grant_full_id(fid, 1) {
                granted += 1;
            }
        }
        log::info!(
            "start-item backfill: {}/{} startItems absent from inventory, granted {} (backstop for \
             the stale start_items_granted boolean)",
            missing.len(),
            items.len(),
            granted
        );
    }
    DONE.store(true, Ordering::Relaxed);
}
