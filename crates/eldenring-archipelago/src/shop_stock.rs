//! shop_stock.rs — reroll the INFINITE-STOCK shop rows to high-impact consumables, per seed.
//!
//! 🛑 THE 455-ROW STORY BELOW WAS WRONG AND IS KEPT ONLY AS THE CORRECTION. The world used to select
//! these rows with `eventFlag_forStock == 0`, which is the exact INVERSE of a shop check, and got 455
//! rows: the Alter-Garments armour-conversion menu, the Ash-of-War duplication menu, and debug rows.
//! MENUS, not shelves. No player can browse them, so the reroll rerolled nothing anyone could buy —
//! and it corrupted the two menus a player CAN open. Retargeted 2026-07-29 to the 14 real browsable
//! unlimited GOODS shelves (equipType 3, mtrlId -1, costType 0, sellQuantity -1, no release gate,
//! and a stock flag that is PRESENT): Patches' Glass Shards, Kale's Throwing Daggers, Iji's Somber
//! Smithing Stones, the poison-dart and throwing-knife racks. Ammo shelves are deliberately excluded
//! so arrow builds keep their supply line.
//!
//! Alaric's idea (2026-07-11): don't make them checks. REROLL them. Each seed the apworld draws a
//! high-impact consumable for every infinite row (`features/shop_stock.py`, pool =
//! `filler_curation.CATEGORIES` — the same curated roster the filler recipe uses, unforked), and ships
//! the result as slot_data `shopInfiniteStock`:
//!
//!     { "<ShopLineupParam row id>": [goodsId, equipType, price] }
//!
//! We just apply it. GOODS ONLY, deliberately: infinite stock is only interesting for what you CONSUME.
//!
//! PRICE IS LOAD-BEARING — it is not decoration. A shelf carries the price of the item it USED to
//! sell, and a cheap shelf handed an expensive consumable is an infinite dispenser. (The figures in
//! the next sentence describe the retired 455-row set; the principle is unchanged for the 14.) Write a
//! consumable into one of those and leave the price alone, and every seed ships an infinite free Rune
//! Arc / Stonesword Key / smithing stone dispenser. With 282 near-free slots the odds that at least one
//! lands something economy-breaking are ~1 — that is not "some seeds you get lucky", it is a guaranteed
//! dominant strategy in every seed. So the apworld derives a price from the item itself (what a vanilla
//! shop charges for it -> basicPrice -> sellValue*10) and we write it alongside the id. The reroll then
//! costs what it is WORTH, and the economy is neutral by construction.
//!
//! CROSS-TYPE: an armor/gem/weapon row rewritten to a GOODS item is a cross-category rewrite — the exact
//! thing SHOP_CTD_GUARD used to block. That guard was removed 2026-07-11 (its 3x CTD repro is believed
//! confounded by the bag-add nulling that was live then and is dead code now). This feature RIDES on
//! that being true. If the shop-buyout playtest CTDs, this comes out with the guard.
//!
//! `sellQuantity` is left at -1 on purpose: the point is that the stock is infinite.
//! Idempotent (skips rows already equal); re-armed on tick like the other param passes.

#![allow(dead_code)]

use eldenring::cs::{ShopLineupParam, SoloParamRepository};
use fromsoftware_shared::FromStatic; // brings SoloParamRepository::instance_mut into scope
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// (goods row id, equipType, price) -- what one infinite-stock row was rerolled to.
type StockRow = (i32, u8, i32);
/// ShopLineupParam row id -> its rerolled ware. From slot_data `shopInfiniteStock`.
type StockTable = HashMap<u32, StockRow>;

static ROLL: Mutex<Option<StockTable>> = Mutex::new(None);
static DONE: AtomicBool = AtomicBool::new(false);

/// Byte offset of `value` (i32, the rune price) in a SHOP_LINEUP_PARAM row (Paramdex def):
/// equipId@+0x00, **value@+0x04**, mtrlId@+0x08, eventFlag_forStock@+0x0C, ...
/// Only needed if the crate has no typed `set_value`; see the note in `apply`.
const VALUE_OFF: usize = 0x04;

/// Called from net.rs at connect with the parsed slot_data map.
pub fn configure(roll: HashMap<u32, (i32, u8, i32)>) {
    let n = roll.len();
    *ROLL.lock().unwrap() = Some(roll);
    DONE.store(false, Ordering::Relaxed);
    log::info!("shop-stock: configured {n} infinite-stock row(s) for reroll");
}

/// Apply the reroll. Returns false if the param repo isn't up yet (caller retries next tick).
pub fn run() -> bool {
    if DONE.load(Ordering::Relaxed) {
        return true;
    }
    let roll: Vec<(u32, (i32, u8, i32))> = match ROLL.lock().unwrap().as_ref() {
        Some(m) if !m.is_empty() => m.iter().map(|(k, v)| (*k, *v)).collect(),
        Some(_) => {
            DONE.store(true, Ordering::Relaxed); // feature off / empty roll: nothing to do
            return true;
        }
        None => return true, // not configured (non-greenfield seed)
    };

    let mut n = 0usize;
    let mut missing = 0usize;
    let mut todo: Vec<u32> = Vec::new();
    let mut rolled: Vec<(u32, i32, i32)> = Vec::new();

    // The `instance_mut()` borrow is scoped to THIS BLOCK on purpose. shop_flags' stock-flag
    // helpers re-enter the repo through `instance()`, so the flag pass below must not run while a
    // mutable borrow of the same table is alive.
    {
        // SAFETY: FD4 singleton; game thread, in-world (caller gates). instance_mut/get_mut are the
        // crate's sanctioned mutable access to the live RW param table -- same path shop_sell uses.
        let repo = match unsafe { SoloParamRepository::instance_mut() } {
            Ok(r) => r,
            Err(_) => return false, // repo not up yet -- retry
        };
        for (row_id, (gid, etype, price)) in roll {
            let Some(row) = repo.get_mut::<ShopLineupParam>(row_id) else {
                missing += 1;
                continue;
            };
            // NOTE the flag is read and cleared in a SECOND PASS below, not here: shop_flags'
            // read/write_stock_flag re-enter the repo through `instance()`, and doing that while this
            // loop holds the `instance_mut()` borrow would alias the same table through two paths.
            let ware_ok =
                row.equip_id() == gid && row.equip_type() == etype && row.value() == price;
            if ware_ok {
                // Ware already correct; the flag pass below still gets a look at it, because a row can
                // be correctly rerolled and STILL be invisible if its stock flag survived.
                todo.push(row_id);
                continue;
            }
            row.set_equip_id(gid);
            row.set_equip_type(etype);
            // `set_value` CONFIRMED to exist by the Windows build 2026-07-11 -- the raw +0x04 write is
            // not needed (VALUE_OFF is kept only as documentation of the row layout).
            row.set_value(price);
            // sellQuantity stays -1: infinite stock is the whole point.
            //
            // 🛑 ZERO THE STOCK FLAG. Paramdex calls eventFlag_forStock the "flag holding the count"
            // (個数保持イベントフラグ) -- on a purchasable row it is SAVE STATE, not a static property.
            // Leaving a rewritten unlimited shelf pointing at a live counter makes its visibility depend
            // on that save's history, and on 2026-07-29 it did: Alaric's Iji shelf 100226 showed its
            // reroll while 100225, param-identical after the write, did not appear at all. Same for
            // Patches' 100104. Both invisible rows were the ones we most wanted seen (they had rolled
            // Golden Rune [1] below worth). Zeroing severs visibility from save history -- matt's
            // randomizer zeroes this field on every infinite shop entry it writes
            // (PermutationWriter.cs:748/780), and our own former 455-row set were all flag-0 rows,
            // which DID render their rerolled wares in game.
            //
            // The world-side predicate still SELECTS on eventFlag_forStock > 0: that clause is what
            // separates the 14 real shelves from the 455 menus. Selecting on it and then clearing it
            // are not in tension -- one is how we find a shelf, the other is how we make it browsable.
            todo.push(row_id);
            rolled.push((row_id, gid, price));
            n += 1;
        }
    } // instance_mut() borrow ends here

    let mut cleared = 0usize;
    for row_id in todo {
        match crate::shop_flags::write_stock_flag(row_id, 0) {
            Some(0) => {}
            Some(old) => {
                cleared += 1;
                log::info!("shop-stock: row {row_id} stock flag {old} -> 0 (was hiding the shelf)");
            }
            None => log::warn!("shop-stock: row {row_id} stock flag not writable"),
        }
    }
    for (row_id, gid, price) in rolled {
        log::info!("shop-stock: row {row_id} -> goods {gid} @{price}");
    }
    if cleared > 0 {
        log::info!("shop-stock: cleared {cleared} stale stock flag(s)");
    }
    if missing > 0 {
        log::info!(
            "shop-stock: {missing} configured row(s) absent from ShopLineupParam \
             (DLC rows on a non-DLC install?)"
        );
    }
    log::info!(
        "shop-stock: rerolled {n} infinite-stock slot(s) to consumables (priced from the item)"
    );
    DONE.store(true, Ordering::Relaxed);
    true
}

/// Re-arm after a reconnect / new seed so a fresh roll is applied.
pub fn reset() {
    DONE.store(false, Ordering::Relaxed);
}
