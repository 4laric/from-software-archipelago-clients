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

use eldenring::cs::{EquipParamGoods, SoloParamRepository};
use fromsoftware_shared::FromStatic; // brings SoloParamRepository::instance_mut into scope
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Can this pass write a lot slot's CATEGORY field alongside its item id? **`true` since the
/// `ITEMLOT_PARAM_ST` per-slot category setter was wired** (`set_lot_item_category01..08`, `s32`).
///
/// The setter name was not guessed. `ItemLotParam.xml` in `fromsoftware-rs` at the SHA this crate
/// pins declares `s32 lotItemCategory01`; the param generator emits `set_{normalize_name(field)}`
/// for every standard field; and `normalize_name` maps `lotItemId01 -> lot_item_id01`, which is the
/// control -- that setter is already called eight times in this file.
const CAN_WRITE_SLOT_CATEGORY: bool = true;

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
    let tele_icon = match crate::param_guard::get::<EquipParamGoods>(
        repo,
        TELESCOPE_GOOD_ID,
        "check-lots dress placeholder",
    ) {
        Some(row) => row.icon_id(),
        None => return false, // telescope row not up yet -- retry next tick
    };
    if let Some(row) = crate::param_guard::get_mut::<EquipParamGoods>(
        repo,
        ph as u32,
        "check-lots dress placeholder",
    ) {
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
    if crate::fmg_inject::extend_swap_overrides(GOODS_NAME_CAT, &[(ph as u32, name)]) == 0 {
        return false; // msg repo / category not up yet -- retry next tick (icon write is idempotent)
    }
    crate::fmg_inject::extend_swap_overrides(GOODS_INFO_CAT, &[(ph as u32, caption.clone())]);
    crate::fmg_inject::extend_swap_overrides(GOODS_CAPTION_CAT, &[(ph as u32, caption)]);
    log::info!("check-lots: placeholder {ph} dressed (AP flower iconId {tele_icon} + GoodsName)");
    DRESSED.store(true, Ordering::Relaxed);
    true
}

/// lot id -> goods slot indices, PER TABLE. The table travels with the lot now: ItemLotParam_map and
/// ItemLotParam_enemy are two different tables that can hold the SAME row id, so a merged map loses the
/// table and forces a guess. The old code guessed map-first and fell back to enemy -- which meant every
/// enemy lot colliding with a map id was NEVER blanked, and a boss that is "just an enemy" handed out
/// its vanilla drop and fired no check. (Alaric, playtest 2026-07-12: the Unsightly Catacombs duo,
/// enemy lot 30120, paid the vanilla Perfumer Tricia ash while all five of that map's treasure checks
/// randomised correctly.) The apworld knows which CSV each lot came from; it just used to throw it away.
static BLANK_MAP: Mutex<Option<HashMap<u32, Vec<u8>>>> = Mutex::new(None);
static BLANK_ENEMY: Mutex<Option<HashMap<u32, Vec<u8>>>> = Mutex::new(None);
/// lot id -> NON-GOODS slot indices (weapon / armor / talisman / gem-ash). These are REPOINTED at the
/// placeholder like goods slots, writing the slot's category alongside its id so a GOODS row is legal
/// where a weapon used to sit. They were once EMPTIED (id = 0, num = 0) -- that removed the pickup, and
/// the pickup IS the check, so it silently killed every gear chest, scarab drop and boss drop.
///
/// Scoped by the apworld to FLAGGED one-time lots, so a farmable source is never eaten.
///
/// This comment used to end: "the check's own acquisition flag still fires on the emptied pickup --
/// registration is by flag poll, not by item id." That sentence was FALSE, and believing it is why the
/// bug shipped: an emptied slot spawns nothing, so there is no pickup and the flag never fires. Left
/// as a headstone -- a comment asserting an invariant is a claim, and this claim had no test.
static NON_GOODS_MAP: Mutex<Option<HashMap<u32, Vec<u8>>>> = Mutex::new(None);
static NON_GOODS_ENEMY: Mutex<Option<HashMap<u32, Vec<u8>>>> = Mutex::new(None);
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
pub fn configure(
    blank_map: HashMap<u32, Vec<u8>>,
    blank_enemy: HashMap<u32, Vec<u8>>,
    non_goods_map: HashMap<u32, Vec<u8>>,
    non_goods_enemy: HashMap<u32, Vec<u8>>,
    placeholder_goods: i32,
) {
    let (nm, ne) = (blank_map.len(), blank_enemy.len());
    let (zm, ze) = (non_goods_map.len(), non_goods_enemy.len());
    *BLANK_MAP.lock().unwrap() = Some(blank_map);
    *BLANK_ENEMY.lock().unwrap() = Some(blank_enemy);
    *NON_GOODS_MAP.lock().unwrap() = Some(non_goods_map);
    *NON_GOODS_ENEMY.lock().unwrap() = Some(non_goods_enemy);
    PLACEHOLDER.store(placeholder_goods, Ordering::Relaxed);
    DONE.store(false, Ordering::Relaxed);
    DRESSED.store(false, Ordering::Relaxed);
    log::info!(
        "check-lots: configured {nm} MAP + {ne} ENEMY goods + {zm} MAP + {ze} ENEMY non-goods check lot(s); placeholder goods {placeholder_goods}"
    );
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
    let grab = |m: &Mutex<Option<HashMap<u32, Vec<u8>>>>| -> Option<Vec<(u32, Vec<u8>)>> {
        m.lock()
            .unwrap()
            .as_ref()
            .map(|h| h.iter().map(|(k, v)| (*k, v.clone())).collect())
    };
    let (blank_map, blank_enemy) = match (grab(&BLANK_MAP), grab(&BLANK_ENEMY)) {
        (None, None) => return true, // not configured (non-greenfield seed)
        (a, b) => (a.unwrap_or_default(), b.unwrap_or_default()),
    };
    let non_goods_map = grab(&NON_GOODS_MAP).unwrap_or_default();
    let non_goods_enemy = grab(&NON_GOODS_ENEMY).unwrap_or_default();
    if blank_map.is_empty()
        && blank_enemy.is_empty()
        && non_goods_map.is_empty()
        && non_goods_enemy.is_empty()
    {
        DONE.store(true, Ordering::Relaxed);
        return true;
    }

    // SAFETY: FD4 singleton; game thread, in-world (caller gates). Same sanctioned mutable param access
    // shop_sell / shop_flags / enemy_drops use on the live RW table.
    let repo = match unsafe { SoloParamRepository::instance_mut() } {
        Ok(r) => r,
        Err(_) => return false,
    };
    // clients#351: a mid-restream ItemLotParam holder would make EVERY lot read as absent, land
    // them all in `missed` ("stale gen data?"), and latch DONE on a pass that wrote nothing.
    // Defer instead -- the world edge's reset() re-arms us and the next tick retries.
    if !crate::param_guard::is_available::<eldenring::cs::ItemLotParam_map>(
        repo,
        "check-lots neutralise pass",
    ) || !crate::param_guard::is_available::<eldenring::cs::ItemLotParam_enemy>(
        repo,
        "check-lots neutralise pass",
    ) {
        return false;
    }

    let mut n = 0usize;
    let mut changed = 0usize;
    let mut already_correct = 0usize;
    let mut missed: Vec<u32> = Vec::new();

    // NO FALLBACK, NO GUESSING. Each lot is written to the table the apworld SAID it came from.
    //
    // The old code did `if map.get_mut(lot) { ...; wrote = true } if !wrote { enemy.get_mut(lot) }`.
    // But map and enemy are two DIFFERENT tables that can hold the same row id -- so on a collision the
    // map lookup won, `wrote` latched, and the ENEMY row was never blanked. A boss that is "just an
    // enemy" (its drop is an ItemLotParam_enemy row) then handed out its vanilla drop and fired no
    // check. Alaric, playtest 2026-07-12: the Unsightly Catacombs duo -- enemy lot 30120, present in
    // this very table -- paid the vanilla Perfumer Tricia ash, while all five of that map's TREASURE
    // checks randomised correctly. That contrast is the whole diagnosis.
    //
    // Note we deliberately do NOT blank both tables "to be safe": that would gut an unrelated map lot's
    // goods slot at the same id. The table is a FACT the apworld already has; it just used to throw it
    // away. Now it travels with the lot.
    // GOODS slots go through the same predicate as non-goods. This arm used to call set_slot()
    // directly, which is the very decision/write split the non-goods arm below was fixed to remove --
    // half an invariant is not an invariant. Behaviour is unchanged: slot_write(goods) yields the
    // placeholder id with `category: None`, i.e. exactly the old id-only write.
    //
    // `category: None` is deliberate and load-bearing. The goods bucket is lotItemCategory {0, 1, 6}
    // (greenfield/gen_data.py `_LOT_CAT_GOODS`), NOT just 1 -- 0 is Golden Rune / Gravel Stone, 6 is
    // sorceries, and all three carry the goods FullID nibble. That path is in-game-proven, so we leave
    // those categories exactly as the game shipped them and normalise only the non-goods slots, which
    // are the ones that genuinely need telling. Do not "tidy" this into an unconditional category
    // write: it would churn a proven path on a symmetry argument.
    let goods_write = er_logic::check_neutralise::slot_write(true, CAN_WRITE_SLOT_CATEGORY, ph);
    if let Some(w) = goods_write {
        for (lot, slots) in &blank_map {
            if let Some(row) = crate::param_guard::get_mut::<eldenring::cs::ItemLotParam_map>(
                repo,
                *lot,
                "check-lots neutralise pass",
            ) {
                for &sl in slots {
                    if slot_matches(row, sl, w.item_id, w.category) {
                        already_correct += 1;
                    } else {
                        changed += 1;
                    }
                    set_slot_full(row, sl, w.item_id, w.category);
                    n += 1;
                }
            } else {
                missed.push(*lot);
            }
        }
        for (lot, slots) in &blank_enemy {
            if let Some(row) = crate::param_guard::get_mut::<eldenring::cs::ItemLotParam_enemy>(
                repo,
                *lot,
                "check-lots neutralise pass",
            ) {
                for &sl in slots {
                    if slot_matches(row, sl, w.item_id, w.category) {
                        already_correct += 1;
                    } else {
                        changed += 1;
                    }
                    set_slot_full(row, sl, w.item_id, w.category);
                    n += 1;
                }
            } else {
                missed.push(*lot);
            }
        }
    }

    // NON-GOODS check slots. The comment that used to sit here claimed "the acquisition flag still
    // fires on the emptied pickup and the flag poll registers the check". IT DOES NOT (Alaric,
    // 2026-07-24): a zeroed slot spawns nothing, so there is no pickup, so the flag never sets and
    // the check never registers. Weapons, armour, talismans and Ashes of War are ALL non-goods, so
    // this quietly killed every gear chest, every scarab Ash-of-War drop and every boss drop --
    // Leonine Misbegotten's check (flag 510800) never fired in a four-hour session while carrying
    // progression. Note this file's own module doc had the right rule for goods all along ("the
    // popup is cosmetic; check registration is not"); the zero pass contradicted it.
    //
    // Suppression loses to detection until the slot's CATEGORY can travel with the id: a lot slot's
    // item id is only meaningful alongside its category, so a non-goods slot cannot hold the goods
    // placeholder without one. Flip CAN_WRITE_SLOT_CATEGORY once that setter is wired and the same
    // predicate starts repointing these too -- no other change needed here.
    // The WRITE comes from er_logic, not from a branch here. This block used to ask er_logic for a
    // Plan and then call zero_slot() inside the RepointToPlaceholder arm -- so flipping the flag
    // above would have EMPTIED every non-goods check slot and reintroduced the dead-check bug
    // eee9b1b fixed, while the predicate next to it said "repoint". The decision and the write are
    // one value now (`slot_write`), so they cannot disagree again.
    let write = er_logic::check_neutralise::slot_write(false, CAN_WRITE_SLOT_CATEGORY, ph);
    if let Some(w) = write {
        for (lot, slots) in &non_goods_map {
            if let Some(row) = crate::param_guard::get_mut::<eldenring::cs::ItemLotParam_map>(
                repo,
                *lot,
                "check-lots neutralise pass",
            ) {
                for &sl in slots {
                    if slot_matches(row, sl, w.item_id, w.category) {
                        already_correct += 1;
                    } else {
                        changed += 1;
                    }
                    set_slot_full(row, sl, w.item_id, w.category);
                    n += 1;
                }
            } else {
                missed.push(*lot);
            }
        }
        for (lot, slots) in &non_goods_enemy {
            if let Some(row) = crate::param_guard::get_mut::<eldenring::cs::ItemLotParam_enemy>(
                repo,
                *lot,
                "check-lots neutralise pass",
            ) {
                for &sl in slots {
                    if slot_matches(row, sl, w.item_id, w.category) {
                        already_correct += 1;
                    } else {
                        changed += 1;
                    }
                    set_slot_full(row, sl, w.item_id, w.category);
                    n += 1;
                }
            } else {
                missed.push(*lot);
            }
        }
    } else if !non_goods_map.is_empty() || !non_goods_enemy.is_empty() {
        // Tolerance requires telemetry: say what is inert and why, once per arm.
        log::warn!(
            "check-lots: non-goods REPOINT pass INERT for {} MAP + {} ENEMY lot(s) -- \
             CAN_WRITE_SLOT_CATEGORY is off, so the vanilla ware LEAKS at these checks (you may \
             receive a duplicate). The checks still register: this arm deliberately does not empty \
             the slot, because emptying removes the pickup and the pickup IS the check.",
            non_goods_map.len(),
            non_goods_enemy.len()
        );
    }
    if !missed.is_empty() {
        log::warn!(
            "check-lots: {} lot(s) were not found in the table the apworld named (stale gen data?): {:?}",
            missed.len(),
            &missed[..missed.len().min(20)]
        );
    }
    log::info!(
        "check-lots: wrote {} MAP + {} ENEMY goods-blank, {} MAP + {} ENEMY non-goods repoint lot(s) ({} missing from the named table)",
        blank_map.len(),
        blank_enemy.len(),
        non_goods_map.len(),
        non_goods_enemy.len(),
        missed.len()
    );
    log::info!(
        "check-lots: neutralised {n} check slot(s) -> goods placeholder {ph} \
         (changed {changed}, already-correct {already_correct}, missing rows {}); non-goods slots are \
         REPOINTED (id + category) now that the category travels with the id -- never emptied, \
         because the pickup IS the check",
        missed.len()
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

// Repoint a NON-GOODS check slot at the goods placeholder: the id AND the category, in one write.
// A lot slot's item id is only meaningful alongside its category -- the placeholder is an
// EquipParamGoods row, so a weapon/armour/talisman/Ash slot must be told it now holds goods or the
// id is a garbage reference into the wrong table. `category: None` leaves the slot's category alone
// (a slot that is already goods).
//
// This REPLACES `zero_slot`, which wrote (id 0, num 0). That function is deliberately gone rather
// than left unused: emptying a slot removes the pickup, the pickup IS the check, and the way this
// bug shipped was an emptying helper sitting in reach of a branch that meant to repoint. Do not
// reintroduce one -- `er_logic::check_neutralise` has no "empty it" variant for the same reason.
#[inline]
fn set_slot_full(
    row: &mut eldenring::param::ITEMLOT_PARAM_ST,
    slot: u8,
    id: i32,
    category: Option<i32>,
) {
    set_slot(row, slot, id);
    let Some(cat) = category else { return };
    match slot {
        1 => row.set_lot_item_category01(cat),
        2 => row.set_lot_item_category02(cat),
        3 => row.set_lot_item_category03(cat),
        4 => row.set_lot_item_category04(cat),
        5 => row.set_lot_item_category05(cat),
        6 => row.set_lot_item_category06(cat),
        7 => row.set_lot_item_category07(cat),
        8 => row.set_lot_item_category08(cat),
        _ => {}
    }
}

#[inline]
fn slot_matches(
    row: &eldenring::param::ITEMLOT_PARAM_ST,
    slot: u8,
    id: i32,
    category: Option<i32>,
) -> bool {
    let (current_id, current_category) = match slot {
        1 => (row.lot_item_id01(), row.lot_item_category01()),
        2 => (row.lot_item_id02(), row.lot_item_category02()),
        3 => (row.lot_item_id03(), row.lot_item_category03()),
        4 => (row.lot_item_id04(), row.lot_item_category04()),
        5 => (row.lot_item_id05(), row.lot_item_category05()),
        6 => (row.lot_item_id06(), row.lot_item_category06()),
        7 => (row.lot_item_id07(), row.lot_item_category07()),
        8 => (row.lot_item_id08(), row.lot_item_category08()),
        _ => return false,
    };
    current_id == id && category.is_none_or(|category| current_category == category)
}

/// Re-arm after a reconnect / new seed.
pub fn reset() {
    DONE.store(false, Ordering::Relaxed);
    DRESSED.store(false, Ordering::Relaxed);
}
