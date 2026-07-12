//! shop_stock.rs — reroll the INFINITE-STOCK shop rows to high-impact consumables, per seed.
//!
//! 455 ShopLineupParam rows carry NO `eventFlag_forStock`, and every one is `sellQuantity == -1`
//! (unlimited). No flag means no way to observe a purchase, so they can never be AP checks. That is not
//! a bug — nothing touches them today, they simply sell their vanilla ware forever (which is why a
//! merchant still stocks a vanilla Flail in a randomised seed).
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
//! PRICE IS LOAD-BEARING — it is not decoration. Those 455 rows carry the price of the item they USED to
//! sell: the 116 Gem (Ash of War) rows cost 1 RUNE, and 166 of the 332 armor rows are FREE. Write a
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

    // SAFETY: FD4 singleton; game thread, in-world (caller gates). instance_mut/get_mut are the crate's
    // sanctioned mutable access to the live RW param table -- same path shop_sell/shop_flags use.
    let repo = match unsafe { SoloParamRepository::instance_mut() } {
        Ok(r) => r,
        Err(_) => return false, // repo not up yet -- retry
    };

    let mut n = 0usize;
    for (row_id, (gid, etype, price)) in roll {
        let Some(row) = repo.get_mut::<ShopLineupParam>(row_id) else {
            continue;
        };
        // Idempotent: skip rows already rerolled (run() is re-armed on tick).
        if row.equip_id() == gid && row.equip_type() == etype && row.value() == price {
            continue;
        }
        row.set_equip_id(gid);
        row.set_equip_type(etype);
        // `set_value` CONFIRMED to exist by the Windows build 2026-07-11 -- the raw +0x04 write is
        // not needed (VALUE_OFF is kept only as documentation of the row layout).
        row.set_value(price);
        // sellQuantity stays -1: infinite stock is the whole point.
        n += 1;
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
