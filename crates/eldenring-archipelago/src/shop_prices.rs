//! shop_prices.rs — write the per-seed rolled rune price onto a shop check row.
//!
//! `shop_sell` rewrites a check row's `equipId` to the AP reward and deliberately leaves `value`
//! alone: the slot costs what the slot cost, which is right when you are buying a randomised ITEM at
//! a merchant's price. It is wrong when the reward is a RUNE, because a rune is money — a 3500-rune
//! slot selling a Golden Rune [1] worth 2000 is not a gamble, it is a slot no player ever presses,
//! and the check behind it is never collected. (Alaric, playtest 2026-07-25.)
//!
//! The world rolls each such slot into `[0, 2x the rune's own derived worth]` and ships the result as
//! slot_data `shopRunePrices` (`features/rune_pricing.py`). This applies it. Nothing is decided here:
//! the roll must be per-seed reproducible, so it belongs to generation, and this pass is pure I/O.
//!
//! Only `value` is touched — never `equipId`, `equipType` or `sellQuantity`. That keeps it disjoint
//! from `shop_sell` (which owns the ware) and from `shop_repoint` (which owns the cosmetic
//! placeholder), so the three passes cannot fight over a row.
//!
//! Idempotent (skips rows already at the rolled price) and re-armed on the in-world edge: a map load
//! streams `ShopLineupParam` back in and reverts the write, exactly as it does for every other param
//! pass here.

#![allow(dead_code)]

use eldenring::cs::{ShopLineupParam, SoloParamRepository};
use fromsoftware_shared::FromStatic;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// ShopLineupParam row id -> rolled rune price. From slot_data `shopRunePrices`.
static PRICES: Mutex<Option<HashMap<u32, i32>>> = Mutex::new(None);
static DONE: AtomicBool = AtomicBool::new(false);

pub fn configure(prices: HashMap<u32, i32>) {
    log::info!(
        "shop-prices: configured {} rolled rune price(s)",
        prices.len()
    );
    *PRICES.lock().unwrap() = Some(prices);
    DONE.store(false, Ordering::Relaxed);
}

/// Re-arm after a load edge / reconnect (params revert). Same reason as check_lots/shop_sell.
pub fn reset() {
    DONE.store(false, Ordering::Relaxed);
}

/// Apply. Returns false if the param repo isn't up yet (caller retries next tick).
pub fn run() -> bool {
    if DONE.load(Ordering::Relaxed) {
        return true;
    }
    let prices: Vec<(u32, i32)> = match PRICES.lock().unwrap().as_ref() {
        Some(m) if !m.is_empty() => m.iter().map(|(k, v)| (*k, *v)).collect(),
        Some(_) => {
            DONE.store(true, Ordering::Relaxed); // feature off / no rune slots this seed
            return true;
        }
        None => return true, // not configured (non-greenfield seed)
    };

    // SAFETY: FD4 singleton; game thread, in-world (caller gates). Same sanctioned mutable access on
    // the live RW param table that shop_sell / shop_stock / shop_repoint use.
    let repo = match unsafe { SoloParamRepository::instance_mut() } {
        Ok(r) => r,
        Err(_) => return false, // repo not up yet -- retry next tick
    };

    // The write loop CONSUMES `prices`; the read-back below needs the same pairs.
    let verify: Vec<(u32, i32)> = prices.clone();
    let (mut n, mut missing) = (0usize, 0usize);
    for (row_id, price) in prices {
        let Some(row) = repo.get_mut::<ShopLineupParam>(row_id) else {
            // A configured row with no live ShopLineupParam entry is the interesting case: the world
            // thinks this check is a shop purchase and the game has no such row. Counted, not
            // swallowed -- a silently absent row is how a whole feature goes inert.
            missing += 1;
            continue;
        };
        if row.value() != price {
            row.set_value(price);
            n += 1;
        }
    }
    // READ-BACK VERIFY. `n` counts rows whose value DIFFERED and were written; it is a dispatch
    // count, and dispatch counts are what lied through the whole 2026-07-29 rune-price incident
    // (CONTRIBUTING rule 12: "a correct wire is not a correct feature"). Note the shape of the
    // count: `n == 0` on a later pass means every row ALREADY held the rolled price -- it is not a
    // DONE-latch artefact. That distinction cost a session to re-derive from the log, so assert it
    // here instead: re-read every configured row and report what the param actually holds.
    let mut ok = 0u32;
    let mut wrong: Vec<String> = Vec::new();
    for (row_id, price) in &verify {
        match repo.get_mut::<ShopLineupParam>(*row_id) {
            Some(row) => {
                let got = row.value();
                if got == *price {
                    ok += 1;
                } else {
                    wrong.push(format!("row {row_id}: wrote {price}, read back {got}"));
                }
            }
            None => wrong.push(format!("row {row_id}: no live row at read-back")),
        }
    }
    log::info!(
        "shop-prices: rolled rune price written to {n} row(s) ({missing} configured row(s) had no live ShopLineupParam entry)"
    );
    if wrong.is_empty() {
        log::info!("shop-prices: read-back OK -- {ok} row(s) hold the rolled price");
    } else {
        log::warn!(
            "shop-prices: READ-BACK MISMATCH on {} of {} configured row(s). First 10: {}",
            wrong.len(),
            verify.len(),
            wrong[..wrong.len().min(10)].join(" | ")
        );
    }
    DONE.store(true, Ordering::Relaxed);
    true
}
