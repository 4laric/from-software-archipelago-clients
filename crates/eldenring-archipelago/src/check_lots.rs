//! check_lots.rs — blank the vanilla ware AT ITS SOURCE, so nothing has to be suppressed by item id.
//!
//! `detour.rs` only ever sees `raw_id` off the AddItemFunc buffer. It cannot answer "where did this
//! item come from?" — which is why `checkItemFlags` armed suppression by ITEM ID, and why any ware that
//! merely happened to back some check was eaten from EVERY source. Golden Rune [1] backs 46 checks, so
//! every Golden Rune [1] picked up anywhere was eaten until all 46 were collected. Mine an ore node,
//! get a Smithing Stone, stone is some check's ware, stone is eaten. (Alaric, playtest 2026-07-11.)
//!
//! Answer the question at the SOURCE instead: rewrite the CHECK's own item lot so it never hands out
//! the vanilla ware. We can write ItemLotParam at runtime — `enemy_drops.rs` proves it.
//!
//! ⭐ THE UNLOCK: we do NOT need a synthetic goods id per check. That requirement is what killed the
//! original spec (3069 colliding checks vs only 332 spare goods rows). **Checks are detected by the FLAG
//! POLL** — `core.rs` pushes the location the moment its acquisition flag fires — *not* by the item id.
//! The synthetic-id-per-location scheme was a baker-era relic of a client that identified a check from
//! the pickup itself. Ours doesn't. So ONE placeholder row is enough:
//!
//!   * point every check lot's GOODS slot at `apPlaceholderGoods` (row 8852: exists so the game can
//!     grant it, no FMG name, referenced by no lot/shop/recipe),
//!   * suppress that ONE id unconditionally in the detour — it is never a real item, so it can never eat
//!     anything legitimate,
//!   * the flag poll reports the check and AP grants what the seed placed.
//!
//! No vanilla ware is ever handed out at a check (killing the double-dip the REPEATABLE_GOODS stopgap
//! had to accept), and nothing else is watched by id — mined ore, farmed drops, bought and crafted goods
//! all just work.
//!
//! GOODS slots only. Weapon/armor check wares stay on the id-keyed suppressor, which is already sound
//! for them: a weapon is essentially never farmable, so it lives in the check-only set and cannot eat a
//! legitimate source.
//!
//! Idempotent; re-armed on tick like the other param passes.

#![allow(dead_code)]

use eldenring::cs::SoloParamRepository;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

/// lot id -> goods slot indices (1..=8) to repoint at the placeholder.
static BLANK: Mutex<Option<HashMap<u32, Vec<u8>>>> = Mutex::new(None);
/// The one goods id we hand out at checks and then unconditionally suppress. 0 = feature off.
static PLACEHOLDER: AtomicI32 = AtomicI32::new(0);
static DONE: AtomicBool = AtomicBool::new(false);

/// The placeholder id, or 0 when the feature is off. Read by detour.rs.
pub fn placeholder() -> i32 {
    PLACEHOLDER.load(Ordering::Relaxed)
}

/// True iff `raw_id` is the AP placeholder — the detour suppresses these UNCONDITIONALLY. Safe because
/// the row is referenced by no lot, shop or recipe in vanilla, so the ONLY way to receive it is from a
/// check lot we ourselves rewrote.
pub fn is_placeholder(raw_id: i32) -> bool {
    let p = PLACEHOLDER.load(Ordering::Relaxed);
    p != 0 && (raw_id & 0x0FFF_FFFF) == p
}

/// Called from net.rs at connect.
pub fn configure(blank: HashMap<u32, Vec<u8>>, placeholder_goods: i32) {
    let lots = blank.len();
    *BLANK.lock().unwrap() = Some(blank);
    PLACEHOLDER.store(placeholder_goods, Ordering::Relaxed);
    DONE.store(false, Ordering::Relaxed);
    log::info!("check-lots: configured {lots} check lot(s); placeholder goods {placeholder_goods}");
}

/// Apply. Returns false if the param repo isn't up yet (caller retries next tick).
pub fn run() -> bool {
    if DONE.load(Ordering::Relaxed) {
        return true;
    }
    let ph = PLACEHOLDER.load(Ordering::Relaxed);
    if ph == 0 {
        DONE.store(true, Ordering::Relaxed);
        return true; // feature off
    }
    let blank: Vec<(u32, Vec<u8>)> = match BLANK.lock().unwrap().as_ref() {
        Some(m) if !m.is_empty() => m.iter().map(|(k, v)| (*k, v.clone())).collect(),
        Some(_) => {
            DONE.store(true, Ordering::Relaxed);
            return true;
        }
        None => return true, // not configured (non-greenfield seed)
    };

    // SAFETY: FD4 singleton; game thread, in-world (caller gates). Same sanctioned mutable param access
    // shop_sell / shop_flags / enemy_drops use on the live RW table.
    let repo = match unsafe { SoloParamRepository::instance_mut() } {
        Ok(r) => r,
        Err(_) => return false,
    };

    let mut n = 0usize;
    for (lot, slots) in blank {
        // NOTE (Windows build): `ItemLotParamMap` / `ItemLotParamEnemy` and their `set_lot_item_id_0N`
        // setters are the symbols I could not verify -- the eldenring crate is not vendored in the
        // sandbox. Fix the names here if they differ; the logic is unaffected. Check lots live in BOTH
        // tables (map treasure + enemy one-time drops), so we try map first and fall back to enemy.
        let mut wrote = false;
        if let Some(row) = repo.get_mut::<eldenring::cs::ItemLotParamMap>(lot) {
            for &s in &slots {
                set_slot(row_as_map(row), s, ph);
                n += 1;
            }
            wrote = true;
        }
        if !wrote {
            if let Some(row) = repo.get_mut::<eldenring::cs::ItemLotParamEnemy>(lot) {
                for &s in &slots {
                    set_slot_enemy(row, s, ph);
                    n += 1;
                }
            }
        }
    }
    log::info!("check-lots: blanked {n} check goods slot(s) -> placeholder {ph} (vanilla ware never handed out at a check)");
    DONE.store(true, Ordering::Relaxed);
    true
}

// The two tables share a layout; these shims exist only so the setter names live in ONE place when the
// Windows build corrects them.
#[inline]
fn row_as_map(r: &mut eldenring::param::ITEM_LOT_PARAM_MAP) -> &mut eldenring::param::ITEM_LOT_PARAM_MAP {
    r
}

#[inline]
fn set_slot(row: &mut eldenring::param::ITEM_LOT_PARAM_MAP, slot: u8, id: i32) {
    match slot {
        1 => row.set_lot_item_id_01(id),
        2 => row.set_lot_item_id_02(id),
        3 => row.set_lot_item_id_03(id),
        4 => row.set_lot_item_id_04(id),
        5 => row.set_lot_item_id_05(id),
        6 => row.set_lot_item_id_06(id),
        7 => row.set_lot_item_id_07(id),
        8 => row.set_lot_item_id_08(id),
        _ => {}
    }
}

#[inline]
fn set_slot_enemy(row: &mut eldenring::param::ITEM_LOT_PARAM_ENEMY, slot: u8, id: i32) {
    match slot {
        1 => row.set_lot_item_id_01(id),
        2 => row.set_lot_item_id_02(id),
        3 => row.set_lot_item_id_03(id),
        4 => row.set_lot_item_id_04(id),
        5 => row.set_lot_item_id_05(id),
        6 => row.set_lot_item_id_06(id),
        7 => row.set_lot_item_id_07(id),
        8 => row.set_lot_item_id_08(id),
        _ => {}
    }
}

/// Re-arm after a reconnect / new seed.
pub fn reset() {
    DONE.store(false, Ordering::Relaxed);
}
