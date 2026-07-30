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
    // ⭐⭐⭐ A/B KILL SWITCH -- `ER_SHOP_PRICES=off` skips the write entirely.
    //
    // Alaric, 2026-07-30: "we've had runes in the shop before we started trying to fix the price."
    // That is ground truth and it OUTRANKS the 15-for-15 absence pattern I had been reading as "the
    // purchase menu cannot display a rune". If runes rendered before, the menu can display one, and
    // something we added since is what hides it.
    //
    // This module is the only one that writes ShopLineupParam.value, and it writes it ONLY on rune
    // rows -- so "rune" and "we rewrote the price" have been the SAME VARIABLE in every observation
    // to date. A day was spent diffing EquipParamGoods columns that are all merely collinear with
    // rune-ness (sortGroupId 100, canMultiUse 1, goodsUseAnim 9) while the actual manipulated
    // variable sat in this file, unconsidered.
    //
    // 🛑 THE DECISION TABLE THAT WAS HERE WAS WRONG IN THE DANGEROUS DIRECTION. It read: "absent both
    // ways ⇒ this module is exonerated." Fable root-caused the bug while this switch was in flight,
    // and under the real rule this A/B would have SHIPPED A FALSE EXONERATION.
    //
    // THE RULE (Fable, 2026-07-30, 16/16 row-level predictions across two seeds, zero misses):
    // the purchase menu excludes any row whose `value` is BELOW the ware's own `sellValue`. Not
    // `value == 0`, not the write itself -- the RELATION. Corroborated by a non-rune: Veteran's Helm
    // (sellValue 1000) on a 600-value slot is hidden, while protector 201100 (sellValue 100) on a
    // 1000-value slot two rows away renders.
    //
    // Runes are hidden 15/15 BY CONSTRUCTION, not by bad luck: `rune_worth` is `GOODS_PRICE // 10`,
    // which is exactly `sellValue`, and the roll is `randint(0, 1 * worth)` -- so every rolled price
    // is <= sellValue and the whole roll range sits inside the hidden region. It also retro-explains
    // 2026-07-29's "I have never seen a single rune priced below its value": the below-value ones
    // were there and INVISIBLE. That report was a direct observation of this rule.
    //
    // With the switch OFF, some rune rows reappear and others do NOT (a slot whose vanilla price is
    // under the ware's sellValue stays hidden), and the helm stays hidden in BOTH arms. So a
    // both-arms-absent result would have meant nothing. Kept only as a probe; the fix is a clamp at
    // the write choke point, not this flag.
    if std::env::var("ER_SHOP_PRICES")
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "off" || v == "0" || v == "false"
        })
        .unwrap_or(false)
    {
        log::warn!(
            "shop-prices: DISABLED by ER_SHOP_PRICES -- rune rows keep the price of the ware their \
             slot used to sell. A/B probe for the 2026-07-30 shop-visibility hunt, not a shipping \
             configuration."
        );
        DONE.store(true, Ordering::Relaxed);
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
    // CLAMP ABOVE sellValue. A rolled rune price is <= the rune's sellValue BY CONSTRUCTION
    // (`rune_worth` is `GOODS_PRICE // 10`, which is exactly `sellValue`, and the roll is
    // `randint(0, 1 * worth)`), so without this every rune row we price is invisible in the menu --
    // which is the whole 2026-07-30 bug. Runs AFTER the write so it sees the rolled value.
    let clamp_rows: Vec<(u32, u8, i32)> = verify
        .iter()
        .filter_map(|(row_id, _)| {
            repo.get::<ShopLineupParam>(*row_id)
                .map(|r| (*row_id, r.equip_type(), r.equip_id()))
        })
        .collect();
    crate::shop_value::clamp(repo, &clamp_rows, "shop_prices");

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
