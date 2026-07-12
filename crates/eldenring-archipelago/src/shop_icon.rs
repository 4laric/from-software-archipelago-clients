//! shop_icon.rs — AP "flower" icon for FOREIGN (and gem/custom) shop slots only.
//!
//! Own-world rewards are rewritten by `shop_sell` to natively sell the real item (correct icon for any
//! type), so this only handles the slots shop_sell can't: FOREIGN items (no ER counterpart) and gem/
//! custom rewards. For those the displayed vanilla good keeps the AP flower — the TELESCOPE's iconId,
//! which me3's VFS menu override repaints to the flower (see er-ap-icon-override).
//!
//! Writes via the crate's mutable param API (instance_mut + get_mut + typed set_icon_id). GLOBAL per
//! good id. Driven by the shopPreviewGoods (loc, good) pairs; idempotent; latches once scout-ready.
//!
//! Ported from the standalone `eldenring-ap/game/shop_icon.rs` (see SHOP-SYSTEM-HANDOFF.md):
//! `super::` -> `crate::`, `tracing::` -> `log::`. Param API unchanged (eldenring main == 0.14 here).

#![allow(dead_code)]

use eldenring::cs::{EquipParamGoods, SoloParamRepository};
use fromsoftware_shared::FromStatic;
use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// EquipParamGoods row id of the Telescope — the iconId me3's flower texture overrides. Read live.
const TELESCOPE_GOOD_ID: u32 = 2040;

/// ER GOODS row ids the seed can actually GRANT the player (derived from apIdsToItemIds).
/// Repainting one of these is identity theft: `set_icon_id` writes the SHARED EquipParamGoods row, so
/// flowering a shop slot whose vanilla ware is a Smithing Stone [1] re-icons EVERY Smithing Stone [1]
/// in the game -- inventory, world pickups, other shops. 11 vanilla shop rows sell smithing stones.
/// (Alaric, playtest 2026-07-12: "the injected smithing stones are using telescope icon" -- both in
/// the world AND in the inventory. Both halves were this write.)
static REAL_GOODS: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

static CONFIGURED: Mutex<Vec<(i64, i32)>> = Mutex::new(Vec::new());
static CONFIGURED_SET: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);

/// The goods the seed can grant. Set from core once apIdsToItemIds is parsed; until it is, we refuse
/// to flower anything (see `run`) rather than risk the global write.
pub fn set_real_goods(rows: HashSet<u32>) {
    log::info!(
        "shop-icon: {} real goods row(s) protected from the global icon write",
        rows.len()
    );
    *REAL_GOODS.lock().unwrap() = Some(rows);
}

pub fn configure(pairs: Vec<(i64, i32)>) {
    log::info!("shop-icon: configured {} shop slot(s)", pairs.len());
    *CONFIGURED.lock().unwrap() = pairs;
    CONFIGURED_SET.store(true, Ordering::Relaxed);
}

pub fn run() -> bool {
    if DONE.load(Ordering::Relaxed) {
        return true;
    }
    if !CONFIGURED_SET.load(Ordering::Relaxed) {
        return false; // wait for slot_data parse
    }
    if !crate::scout_proof::cache_ready() {
        return false; // need the scout cache to tell own-world from foreign
    }
    // Fail CLOSED. Without the protected set we cannot tell a shop-only curio from a Smithing Stone,
    // and guessing wrong corrupts a real item globally and permanently for the run. Wait instead.
    let real: HashSet<u32> = match REAL_GOODS.lock().unwrap().clone() {
        Some(r) => r,
        None => return false,
    };
    let pairs = CONFIGURED.lock().unwrap().clone();
    if pairs.is_empty() {
        DONE.store(true, Ordering::Relaxed);
        return true;
    }
    // SAFETY: FD4 singleton; game thread, in-world (caller gates).
    let repo = match unsafe { SoloParamRepository::instance_mut() } {
        Ok(r) => r,
        Err(_) => return false, // repo not up yet — retry next tick
    };
    let tele_icon = match repo.get::<EquipParamGoods>(TELESCOPE_GOOD_ID) {
        Some(row) => row.icon_id(),
        None => return false, // telescope row absent — retry
    };
    let (mut flower, mut native, mut protected) = (0u32, 0u32, 0u32);
    let mut seen: HashSet<u32> = HashSet::new();
    for (loc, good) in pairs {
        // Own-world sellable rewards display natively (shop_sell rewrote the slot) -> nothing to flower.
        if crate::scout_proof::lookup(loc)
            .map(|s| s.er_sell_id.is_some())
            .unwrap_or(false)
        {
            native += 1;
            continue;
        }
        // shopPreviewGoods carries ER FullIDs (gen_data ORs the category nibble into the
        // equipId so the client previews the good in the right param table). The flower repaints
        // an EquipParamGoods.iconId, so it only applies to GOODS wares: strip the nibble to the
        // real goods row id (as shop_sell does), and skip non-goods wares (their icon lives in a
        // different param table; reusing a weapon/armor id as a goods row id would flower the
        // WRONG good). Without this, a GOODS FullID (0x40000000|row, ~1.07e9) never matches a
        // real EquipParamGoods row -> get_mut misses -> the icon is never set and the slot keeps
        // the vanilla good's icon (name/icon desync, playtest 2026-07-07).
        let full = good as u32;
        if er_codec::item_category_of(full) != er_codec::CATEGORY_GOODS {
            continue;
        }
        let gid = er_codec::row_id_of(full);
        if !seen.insert(gid) {
            continue; // dedup
        }
        // THE GUARD. set_icon_id writes the shared EquipParamGoods row, so this is global and
        // permanent for the run. If the player can be granted this good, flowering it repaints every
        // copy they will ever hold. Leave the slot showing its vanilla ware instead: a shop slot that
        // lies about ONE reward is a local, reversible annoyance; a smithing-stone economy that has
        // been renamed and re-iconed is not. (Restoring an honest preview for these slots needs the
        // row itself repointed at a placeholder good -- see the shop-placeholder follow-up.)
        if real.contains(&gid) {
            protected += 1;
            continue;
        }
        if let Some(row) = repo.get_mut::<EquipParamGoods>(gid) {
            if row.icon_id() != tele_icon {
                row.set_icon_id(tele_icon);
            }
        }
        flower += 1;
    }
    log::info!(
        "shop-icon: {flower} foreign/gem slot(s) flowered, {native} own-world handled by shop_sell, \
         {protected} slot(s) LEFT VANILLA because their ware is a real item this seed can grant \
         (flowering it would re-icon every copy globally) (telescope iconId {tele_icon})"
    );
    DONE.store(true, Ordering::Relaxed);
    true
}
