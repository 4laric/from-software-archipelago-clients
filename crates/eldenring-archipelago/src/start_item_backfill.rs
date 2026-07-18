//! Inventory-verified start-item backfill: grant any `startItems` entry that isn't actually in the
//! bag. A last-resort backstop for whatever the primary start-item paths dropped -- verifies against
//! the BAG, not any bookkeeping flag. Live case: no healing flask on a Roundtable-hub start; the
//! flask WAS in `startItems`, but the RECONCILER (which owns start-item goods, `apply=...,goods,...`)
//! converged without placing it, and the old boolean-gated drain had already stood down. Logic +
//! flask-family handling live in `er_logic::start_backfill`.
//!
//! Runs ONCE per client launch (in-memory latch), in-world, AFTER an in-world SETTLE (so the
//! reconciler/drain have taken their pass first -> on a healthy save it finds nothing missing and
//! never double-grants), snapshotting the inventory fresh each tick. Inventory verification is the
//! anti-double-grant guarantee: anything a primary path placed reads as present and is skipped.
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

/// Per-tick until done. `settled` = the world has loaded + the primary start-item paths have had
/// time to run (in-world settle, same signal `apply_start_flags`/the drain use). Once settled and
/// in-world with the inventory populated, grant any `startItems` NOT in the bag.
///
/// GATE FIX (2026-07-18): originally gated on the persisted `start_items_granted` boolean, on the
/// theory that a stale-TRUE boolean made the old drain skip. WRONG for the live case: start-item
/// GOODS (the flask) are now owned by the RECONCILER (`apply=flags,goods,ledger`), the old drain
/// stands down, so `start_items_granted` never latches TRUE -> the backfill never ran. Now gated on
/// an in-world SETTLE instead, so it runs as a true backstop AFTER the reconciler converges,
/// independent of whichever primary path (drain or reconciler) dropped the item. Inventory
/// verification is what prevents a double-grant: anything the reconciler did place reads as present
/// and is skipped; only genuinely-absent startItems are granted.
pub fn tick(settled: bool) {
    if DONE.load(Ordering::Relaxed) || !settled || !crate::flags::in_world() {
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
    // Always log the decision (once, on the settled run) so a "nothing granted" outcome is visible
    // in diagnosis -- this is the line whose ABSENCE told us the old gate was wrong.
    log::info!(
        "start-item backfill: scanned {} inventory id(s), {}/{} startItems absent -> granting {:?}",
        present.len(),
        missing.len(),
        items.len(),
        missing
            .iter()
            .map(|&f| format!("{:#010x}", f as u32))
            .collect::<Vec<_>>()
    );
    if !missing.is_empty() {
        let mut granted = 0u32;
        for &fid in &missing {
            let ok = crate::detour::grant_full_id(fid, 1);
            if ok {
                granted += 1;
            }
            log::info!(
                "start-item backfill: grant {:#010x} -> {}",
                fid as u32,
                if ok { "ok" } else { "FAILED (not placed)" }
            );
        }
        log::info!(
            "start-item backfill: granted {}/{} absent startItems (backstop after reconciler converge)",
            granted,
            missing.len()
        );
    }
    DONE.store(true, Ordering::Relaxed);
}
