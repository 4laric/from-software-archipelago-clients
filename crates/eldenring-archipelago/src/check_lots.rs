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
//! ## The popup — why the placeholder is NAMED
//!
//! Alaric, playtest 2026-07-12: a check gave `Erdtree Greatshield x1` (the real AP item, correct) and,
//! beside it, **`[ERROR] x1`**. That is row 8852's acquisition popup: the row was chosen *because* it has
//! no `GoodsName` FMG entry (that is what proves nothing else references it), and ER renders a nameless
//! goods row as the literal string `[ERROR]`.
//!
//! Nothing was broken — the ware was suppressed, the flag fired, AP granted the item. But `[ERROR]` in a
//! randomizer reads as a crash, so we name it. `shop_preview.rs` already rewrites GoodsName at runtime via
//! `fmg_inject::extend_swap_overrides`; the placeholder is one more entry in that same override map.
//!
//! We name it rather than ZEROING the lot slot: an empty slot would show no popup at all, but it changes
//! what the lot *does*, and the acquisition flag firing on an empty pickup is unverified. The popup is
//! cosmetic; check registration is not. Don't trade a known-good mechanism for a nicer toast.
//!
//! Idempotent; re-armed on tick like the other param passes.

#![allow(dead_code)]

use eldenring::cs::SoloParamRepository;
use fromsoftware_shared::FromStatic; // brings SoloParamRepository::instance_mut into scope
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// lot id -> goods slot indices (1..=8) to repoint at the placeholder.
static BLANK: Mutex<Option<HashMap<u32, Vec<u8>>>> = Mutex::new(None);
/// The one goods id we hand out at checks and then unconditionally suppress. 0 = feature off.
static PLACEHOLDER: AtomicI32 = AtomicI32::new(0);
static DONE: AtomicBool = AtomicBool::new(false);
/// FMG naming is a separate latch: it needs the msg repo up, which lands later than the param repo.
static NAMED: AtomicBool = AtomicBool::new(false);

/// GoodsName FMG category (same constant shop_preview overrides).
const GOODS_NAME_CAT: u32 = 10;
/// What the check's placeholder popup says instead of `[ERROR]`.
const PLACEHOLDER_NAME: &str = "Archipelago Item";

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
    NAMED.store(false, Ordering::Relaxed);
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
        // Param naming, settled by the Windows build 2026-07-11 (the crate is not vendored in the
        // sandbox, so these were guesses and every one was wrong):
        //   table type : eldenring::cs::ItemLotParam_map / ItemLotParam_enemy   (snake, not CamelCase)
        //   row struct : eldenring::param::ITEMLOT_PARAM_ST  -- ONE struct shared by BOTH tables
        //   setters    : set_lot_item_id01..08               (NO underscore before the digits)
        // Check lots live in both tables (map treasure + enemy one-time drops); same row struct, so one
        // setter serves both. Try map, fall back to enemy.
        // Check lots live in BOTH tables (map treasure + enemy one-time drops). Same row struct, so
        // the same setter serves both; try map, fall back to enemy.
        let mut wrote = false;
        if let Some(row) = repo.get_mut::<eldenring::cs::ItemLotParam_map>(lot) {
            for &sl in &slots {
                set_slot(row, sl, ph);
                n += 1;
            }
            wrote = true;
        }
        if !wrote {
            if let Some(row) = repo.get_mut::<eldenring::cs::ItemLotParam_enemy>(lot) {
                for &sl in &slots {
                    set_slot(row, sl, ph);
                    n += 1;
                }
            }
        }
    }
    log::info!(
        "check-lots: blanked {n} check goods slot(s) -> placeholder {ph} (vanilla ware never handed out at a check)"
    );
    DONE.store(true, Ordering::Relaxed);
    true
}

// ItemLotParam_map and ItemLotParam_enemy are two different TABLES that share ONE row struct
// (`ITEMLOT_PARAM_ST`) -- confirmed by the Windows build 2026-07-11. So one setter serves both, and the
// row_as_map shim I'd written for "two layouts" was solving a problem that doesn't exist.
#[inline]
fn set_slot(row: &mut eldenring::param::ITEMLOT_PARAM_ST, slot: u8, id: i32) {
    match slot {
        1 => row.set_lot_item_id01(id),
        2 => row.set_lot_item_id02(id),
        3 => row.set_lot_item_id03(id),
        4 => row.set_lot_item_id04(id),
        5 => row.set_lot_item_id05(id),
        6 => row.set_lot_item_id06(id),
        7 => row.set_lot_item_id07(id),
        8 => row.set_lot_item_id08(id),
        _ => {}
    }
}

/// Give the placeholder a name so its pickup toast reads "Archipelago Item" and not `[ERROR]`.
///
/// Separate from `run()` because it depends on the MSG repo (later than the param repo) and must not
/// hold the lot rewrite hostage — the rewrite is what makes checks work; this is only the toast.
/// Returns false while the msg repo is still coming up, so the caller retries next tick.
pub fn name_placeholder() -> bool {
    if NAMED.load(Ordering::Relaxed) {
        return true;
    }
    let ph = PLACEHOLDER.load(Ordering::Relaxed);
    if ph == 0 {
        NAMED.store(true, Ordering::Relaxed);
        return true; // feature off — nothing hands out the placeholder
    }
    let name: Vec<u16> = PLACEHOLDER_NAME.encode_utf16().collect();
    if crate::fmg_inject::extend_swap_overrides(GOODS_NAME_CAT, &[(ph as u32, name)]) == 0 {
        return false; // msg repo / category not up yet
    }
    log::info!(
        "check-lots: placeholder goods {ph} named \"{PLACEHOLDER_NAME}\" (was the [ERROR] toast)"
    );
    NAMED.store(true, Ordering::Relaxed);
    true
}

/// Re-arm after a reconnect / new seed.
pub fn reset() {
    DONE.store(false, Ordering::Relaxed);
    NAMED.store(false, Ordering::Relaxed);
}
