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

use eldenring::cs::{EquipParamGoods, SoloParamRepository};
use fromsoftware_shared::FromStatic;   // brings SoloParamRepository::instance_mut into scope
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

/// EquipParamGoods row id of the Telescope -- its iconId is the one me3's VFS menu override repaints
/// into the AP flower (see shop_icon.rs / er-ap-icon-override). Read live, never written.
const TELESCOPE_GOOD_ID: u32 = 2040;
/// FMG category ids for GoodsName / GoodsInfo / GoodsCaption (mirrors shop_preview).
const GOODS_NAME_CAT: u32 = 10;
const GOODS_INFO_CAT: u32 = 20;
const GOODS_CAPTION_CAT: u32 = 24;

static DRESSED: AtomicBool = AtomicBool::new(false);

/// Give the placeholder a FACE.
///
/// Every check's goods slot now hands out row 8852, which ships with "no GoodsName entry" and whatever
/// icon it happened to inherit -- so a check pickup read as a nameless telescope. That is not a
/// cosmetic detail: the pickup toast is the ONLY feedback that a check fired, and an anonymous
/// telescope is indistinguishable from a bug. (Alaric, playtest 2026-07-12.)
///
/// So: point it at the Telescope's iconId, which me3's override repaints to the AP flower, and inject
/// a real name. Safe to write GLOBALLY -- unlike a vanilla ware, row 8852 is referenced by no lot, shop
/// or recipe and can never be granted as a real item, so nothing else in the game wears this identity.
/// That asymmetry is exactly what makes the same write UNSAFE in shop_icon/shop_preview.
pub fn dress_placeholder() -> bool {
    if DRESSED.load(Ordering::Relaxed) {
        return true;
    }
    let ph = PLACEHOLDER.load(Ordering::Relaxed);
    if ph == 0 {
        return true; // feature off -- nothing to dress
    }
    // SAFETY: FD4 singleton; game thread, in-world (caller gates).
    let repo = match unsafe { SoloParamRepository::instance_mut() } {
        Ok(r) => r,
        Err(_) => return false,
    };
    let tele_icon = match repo.get::<EquipParamGoods>(TELESCOPE_GOOD_ID) {
        Some(row) => row.icon_id(),
        None => return false, // telescope row not up yet -- retry next tick
    };
    if let Some(row) = repo.get_mut::<EquipParamGoods>(ph as u32) {
        if row.icon_id() != tele_icon {
            row.set_icon_id(tele_icon);
        }
    } else {
        return false;
    }
    let name: Vec<u16> = "Archipelago Item".encode_utf16().collect();
    let caption: Vec<u16> =
        "A check. What it really holds is decided by the multiworld -- it is on its way to you."
            .encode_utf16()
            .collect();
    crate::fmg_inject::extend_swap_overrides(GOODS_NAME_CAT, &[(ph as u32, name)]);
    crate::fmg_inject::extend_swap_overrides(GOODS_INFO_CAT, &[(ph as u32, caption.clone())]);
    crate::fmg_inject::extend_swap_overrides(GOODS_CAPTION_CAT, &[(ph as u32, caption)]);
    log::info!("check-lots: placeholder {ph} dressed (AP flower iconId {tele_icon} + GoodsName)");
    DRESSED.store(true, Ordering::Relaxed);
    true
}

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
    let (mut hit_map, mut hit_enemy) = (0usize, 0usize);
    let (mut suspect, mut missed): (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
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
        // ⚠️ "try map, FALL BACK to enemy" is a suspected bug, and this is the probe that decides it.
        //
        // ItemLotParam_map and ItemLotParam_enemy are two DIFFERENT tables, and nothing stops the same
        // row id existing in both. If it does, the map lookup wins, `wrote` latches, and the ENEMY row
        // -- the boss's one-time drop -- is never blanked, so the boss hands out its vanilla ware and
        // no check fires. 338 of the blanked lots are short-form enemy ids (300, 310, 2020, 30120 ...),
        // exactly the range where a map row can plausibly collide.
        //
        // Alaric, playtest 2026-07-12: killed the Unsightly Catacombs duo (m30_12, enemy lot 30120 --
        // which IS in this table) and was handed the vanilla Perfumer Tricia ash, while all FIVE of
        // that map's treasure checks randomised correctly. That is this bug's exact signature.
        //
        // So: record which table each lot actually resolved to. If a short enemy-form id resolves to
        // MAP, the collision is real and the fix is to carry the table per lot from gen (the apworld
        // knows which CSV each lot came from) instead of guessing here. Measure, then fix -- do not
        // "helpfully" blank both tables, which would gut an unrelated map lot's goods slot.
        let mut wrote = false;
        if let Some(row) = repo.get_mut::<eldenring::cs::ItemLotParam_map>(lot) {
            for &sl in &slots {
                set_slot(row, sl, ph);
                n += 1;
            }
            wrote = true;
            hit_map += 1;
            if lot < 1_000_000 {
                // Short id = enemy-form. Resolving in the MAP table is the collision we are hunting.
                suspect.push(lot);
            }
        }
        if !wrote {
            if let Some(row) = repo.get_mut::<eldenring::cs::ItemLotParam_enemy>(lot) {
                for &sl in &slots {
                    set_slot(row, sl, ph);
                    n += 1;
                }
                hit_enemy += 1;
            } else {
                missed.push(lot);
            }
        }
    }
    log::info!("check-lots: blanked {n} check goods slot(s) -> placeholder {ph} (vanilla ware never handed out at a check)");
    log::info!(
        "check-lots: table resolution -- {hit_map} lot(s) found in ItemLotParam_MAP, {hit_enemy} in \
         ItemLotParam_ENEMY, {} found in NEITHER",
        missed.len()
    );
    if !suspect.is_empty() {
        log::warn!(
            "check-lots: {} SHORT (enemy-form) lot id(s) resolved in the MAP table and so their ENEMY \
             row was NEVER blanked -- these bosses will hand out their vanilla drop and fire no check: \
             {:?}",
            suspect.len(),
            &suspect[..suspect.len().min(40)]
        );
    }
    if !missed.is_empty() {
        log::warn!(
            "check-lots: {} lot(s) exist in NEITHER param table (stale gen data?): {:?}",
            missed.len(),
            &missed[..missed.len().min(20)]
        );
    }
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

/// Re-arm after a reconnect / new seed.
pub fn reset() {
    DONE.store(false, Ordering::Relaxed);
}
