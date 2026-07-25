//! shop_repoint.rs — write the preview good ONTO the shop check row, so the FMG/icon override the
//! client already performs is something the player can actually see.
//!
//! This is the production caller for [`er_logic::shop_repoint`]; the decision, the skip taxonomy and
//! the load-edge timeline all live there and are host-tested. Read that module header first — it
//! carries the full account of the bug. In one line: `shopPreviewGoods` moved off the slot's vanilla
//! ware and onto a dedicated spare goods row (locks 2026-07-20, every foreign slot 2026-07-22), the
//! client kept rewriting the FMG + icon of whatever id it was handed, and nobody ever wrote the row —
//! so since 07-20 those overrides have been landing on a row no menu reads. Alaric, 2026-07-25:
//! foreign shop slots show the VANILLA ware.
//!
//! ## What this pass does NOT do
//!
//! It does not suppress the bag-add of the spare. Buying a repointed slot hands the player the spare
//! good (dressed with the AP name + flower, so it reads as a receipt for the purchase) and the AP
//! echo delivers the real reward. Nulling a shop bag-add is the retired crash-adjacent path
//! (`shop_sell`'s SOLD_SUPPRESS, dead since ECHO-DEDUP) and this pass deliberately does not revive
//! it. The spare is NOT the check-lot placeholder 8852, so `check_lots::is_placeholder` — which nulls
//! unconditionally — does not match it either. That separation is the whole reason the shop side
//! needs its own rows.
//!
//! ## Ordering — three constraints, all load-bearing
//!
//! 1. **After `shop_sell::run()` has finished**, not merely after it in the tick: until it latches,
//!    the set of rows it owns is incomplete, and repointing a row it is about to rewrite would poison
//!    its `derived_preview` fallback (which reads each row's ware off the LIVE row — a repointed row
//!    would preview the spare onto itself). Gated on [`crate::shop_sell::is_done`].
//! 2. **Never touch a row `shop_sell` rewrote.** Those rows natively sell the real own-world reward
//!    and the world leaves their preview at the vanilla ware on purpose, so a repoint would drag them
//!    back to vanilla and break ECHO-DEDUP's param-revert guard.
//! 3. **Re-armed on the in-world edge** (`reset()`, called from core.rs beside `check_lots`/
//!    `shop_sell`). A map load streams `ShopLineupParam` back in and reverts the write; without the
//!    re-arm the repoint holds until the player's first load and is vanilla for the rest of the run.
//!    That is the 2026-07-24 `shop_sell` bug shape, and it is pinned by the replay timeline.

#![allow(dead_code)]

use eldenring::cs::{ShopLineupParam, SoloParamRepository};
use er_logic::shop_repoint::{Repoint, SkipReason, decide};
use fromsoftware_shared::FromStatic;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// slot_data `locationFlags`: AP location id -> its guarding event flag. Inverted at run to map a
/// live row's `eventFlag_forStock` back to its AP location, exactly as `shop_sell` does.
static LOC_FLAGS: Mutex<Option<HashMap<i64, u32>>> = Mutex::new(None);
static DONE: AtomicBool = AtomicBool::new(false);

pub fn configure(location_flags: HashMap<i64, u32>) {
    log::info!(
        "shop-repoint: configured {} location flag(s)",
        location_flags.len()
    );
    *LOC_FLAGS.lock().unwrap() = Some(location_flags);
    DONE.store(false, Ordering::Relaxed);
}

/// Re-arm after a load edge / reconnect. See ordering constraint 3.
pub fn reset() {
    DONE.store(false, Ordering::Relaxed);
}

/// Run once in-world, after `shop_sell` has latched. Returns false (retry next tick) until slot_data,
/// the preview pairs, `shop_sell` and the param repo are all up.
pub fn run() -> bool {
    if DONE.load(Ordering::Relaxed) {
        return true;
    }
    let loc_flags = {
        let g = LOC_FLAGS.lock().unwrap();
        match g.as_ref() {
            Some(m) => m.clone(),
            None => return false, // wait for slot_data parse (net.rs / core.rs)
        }
    };
    // The preview pairs are read from shop_preview rather than plumbed separately, so BOTH sources
    // feed this pass: slot_data `shopPreviewGoods`, and the runtime ShopLineupParam fallback
    // shop_sell installs for a foreign apworld that omits the key.
    let Some(pairs) = crate::shop_preview::configured_pairs() else {
        return false; // preview not configured yet
    };
    // shop_sell must have finished, or its row-ownership set is incomplete (ordering constraint 1).
    if !crate::shop_sell::is_done() {
        return false;
    }
    if pairs.is_empty() {
        DONE.store(true, Ordering::Relaxed);
        return true;
    }
    let preview: HashMap<i64, i32> = pairs.into_iter().collect();
    let mut flag_to_loc: HashMap<u32, i64> = HashMap::with_capacity(loc_flags.len());
    for (&loc, &flag) in loc_flags.iter() {
        if flag != 0 {
            flag_to_loc.insert(flag, loc);
        }
    }

    // SAFETY: FD4 singleton; game thread, in-world (caller gates). Same sanctioned mutable access on
    // the live RW param table that shop_sell / shop_stock / check_lots use.
    let repo = match unsafe { SoloParamRepository::instance_mut() } {
        Ok(r) => r,
        Err(_) => return false, // repo not up yet -- retry next tick
    };

    // Scan immutably -> plan, then apply (avoids holding a row borrow across get_mut), exactly as
    // shop_sell does.
    let mut plan: Vec<(u32, i32, u8)> = Vec::new();
    let (mut sold, mut no_preview, mut not_goods, mut already) = (0u32, 0u32, 0u32, 0u32);
    let mut check_rows = 0u32;
    for (id, row) in repo.rows::<ShopLineupParam>() {
        let f = row.event_flag_for_stock();
        if f == 0 {
            continue;
        }
        let Some(&loc) = flag_to_loc.get(&f) else {
            continue;
        };
        check_rows += 1;
        match decide(
            preview.get(&loc).map(|&g| g as i64),
            row.equip_id(),
            row.equip_type(),
            crate::shop_sell::sold_natively(id),
        ) {
            Repoint::Write(eid, etype) => plan.push((id, eid, etype)),
            Repoint::Skip(SkipReason::SoldNatively) => sold += 1,
            Repoint::Skip(SkipReason::NoPreview) => no_preview += 1,
            Repoint::Skip(SkipReason::NotGoods) => not_goods += 1,
            Repoint::Skip(SkipReason::AlreadyPointed) => already += 1,
        }
    }
    let n = plan.len();
    for (id, eid, etype) in &plan {
        if let Some(row) = repo.get_mut::<ShopLineupParam>(*id) {
            row.set_equip_id(*eid);
            row.set_equip_type(*etype);
            // Same reason as shop_sell: a row-level nameMsgId override outlives the ware and the menu
            // prefers it, so a repointed slot would show its old label instead of the AP name we just
            // went to the trouble of writing into the placeholder's FMG entry.
            if row.name_msg_id() != -1 {
                row.set_name_msg_id(-1);
            }
            // `value` (the rune price) is deliberately UNTOUCHED: the slot must still cost what the
            // slot cost. shop_stock rewrites price because it rerolls the WARE of an infinite row;
            // here the ware is a cosmetic stand-in for a reward the player is buying at this slot's
            // own price.
        }
    }
    // TALLY, not a bare count: a pass that writes 0 rows is indistinguishable from a broken one
    // without the reasons (CONTRIBUTING, "Log why things were skipped, not just that they were").
    // `not_goods` is the interesting one -- every spare in greenfield/spare_goods.tsv is a goods row,
    // so a non-zero count means a preview value reached here that is NOT a repoint target.
    log::info!(
        "shop-repoint: repointed {n} shop check row(s) at their preview good \
         ({check_rows} check row(s) seen, {sold} owned by shop_sell, {already} already pointed, \
         {no_preview} no preview entry, {not_goods} preview not a GOODS row)"
    );
    if n == 0 && check_rows > 0 && already == 0 {
        log::warn!(
            "shop-repoint: INERT -- {check_rows} check row(s) matched and none was repointed. \
             Foreign/lock slots will display their VANILLA ware."
        );
    }
    DONE.store(true, Ordering::Relaxed);
    true
}
