//! shop_preview.rs — name/description override for FOREIGN (and gem/custom) shop slots only.
//!
//! Own-world rewards are rewritten by `shop_sell` to natively sell the real item (correct name, lore
//! and icon), so this only handles the slots shop_sell can't: FOREIGN items (no ER counterpart) and
//! gem/custom rewards. For those the vanilla good's FMG name + info + caption are overwritten with
//! the AP routing block ("AP: <item> / For: <owner> (<game>) / <kind>").
//!
//! Mechanism: EXTEND-SWAP via fmg_inject::extend_swap_overrides (rebuilds the category block from the
//! LIVE pointer so any length fits; validated before the atomic swap). Runs AFTER fmg_inject. The
//! override is GLOBAL per good id, so we dedup by good id (the shared FMG entry shows one reward).
//!
//! TWO bugs fixed here (2026-07-12):
//!
//!  1. WRONG KEY -- it keyed the FMG override by the ER FullID (`good as u32`, category nibble and
//!     all: 0x40000000|row ~= 1.07e9) instead of the EquipParamGoods ROW id the FMG is actually keyed
//!     by. So every override landed at an id no menu ever reads: the AP name/caption has NEVER been
//!     displayed, and extend_swap merely grew the block with dead entries. `shop_icon` strips the
//!     nibble (er_codec::row_id_of) and says so in a comment; this module was never given the same fix.
//!
//!  2. GLOBAL IDENTITY THEFT -- and fixing (1) alone would have made things WORSE, by finally
//!     activating a global write. The FMG entry is shared: renaming the good behind a shop slot renames
//!     EVERY copy of that good the player will ever hold. 11 vanilla shop rows sell smithing stones, so
//!     one foreign/custom reward landing on one of them would rename the player's whole stone economy
//!     to "AP: <something>". `shop_icon` had exactly this bug on the ICON side and it is what Alaric saw
//!     in the 2026-07-12 playtest (telescope icons on every smithing stone, in the world AND in the
//!     inventory). So the same REAL_GOODS guard applies here: never repaint a good the seed can grant.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

const GOODS_NAME_CAT: u32 = 10;
const GOODS_INFO_CAT: u32 = 20; // the "Item Effect" line the buy menu renders
const GOODS_CAPTION_CAT: u32 = 24;

/// ER GOODS row ids the seed can actually GRANT (from apIdsToItemIds). Never repaint one of these:
/// the FMG entry is shared, so renaming the good behind a shop slot renames every copy the player holds.
static REAL_GOODS: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

static CONFIGURED: Mutex<Vec<(i64, i32)>> = Mutex::new(Vec::new());
static CONFIGURED_SET: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);

pub fn set_real_goods(rows: HashSet<u32>) {
    log::info!(
        "shop-preview: {} real goods row(s) protected from the global FMG override",
        rows.len()
    );
    *REAL_GOODS.lock().unwrap() = Some(rows);
}

/// Has slot_data (or the shop_sell runtime fallback) supplied the (loc -> vanilla good) pairs yet?
/// `run()` waits on this: an apworld that emits no `shopPreviewGoods` must NOT latch DONE on an empty
/// set, or the fallback derived from the live params arrives too late to be used.
pub fn is_configured() -> bool {
    CONFIGURED_SET.load(Ordering::Relaxed)
}

pub fn configure(pairs: Vec<(i64, i32)>) {
    log::info!("shop-preview: configured {} shop slot(s)", pairs.len());
    *CONFIGURED.lock().unwrap() = pairs;
    CONFIGURED_SET.store(true, Ordering::Relaxed);
}

pub fn run() -> bool {
    if DONE.load(Ordering::Relaxed) {
        return true;
    }
    if !CONFIGURED_SET.load(Ordering::Relaxed) {
        return false; // wait for slot_data parse (net.rs)
    }
    if !crate::scout_proof::cache_ready() {
        return false; // wait for the scout reply
    }
    let pairs = CONFIGURED.lock().unwrap().clone();
    if pairs.is_empty() {
        DONE.store(true, Ordering::Relaxed);
        return true;
    }
    // Fail CLOSED: without the protected set we cannot tell a shop-only curio from a Smithing Stone,
    // and guessing wrong renames a real item globally for the whole run. Wait instead.
    let real: HashSet<u32> = match REAL_GOODS.lock().unwrap().clone() {
        Some(r) => r,
        None => return false,
    };

    // FOREIGN / gem slots only — own-world slots are sold natively by shop_sell. Per-category override
    // maps (name 10, info 20, caption 24) deduped by good id (the FMG entry is global).
    let mut nmap: HashMap<u32, Vec<u16>> = HashMap::new();
    let mut imap: HashMap<u32, Vec<u16>> = HashMap::new();
    let mut cmap: HashMap<u32, Vec<u16>> = HashMap::new();
    let (mut overridden, mut native, mut protected) = (0u32, 0u32, 0u32);
    for (loc, good) in &pairs {
        let Some(s) = crate::scout_proof::lookup(*loc) else {
            continue;
        };
        if s.er_sell_id.is_some() {
            native += 1;
            continue; // own-world: shop_sell sells it natively
        }
        // The FMG is keyed by the EquipParamGoods ROW id, not the ER FullID. Strip the category
        // nibble exactly as shop_sell / shop_icon do; a non-GOODS ware has its name in a different FMG
        // category, and reusing a weapon row id as a goods row id would rename the WRONG good.
        let full = *good as u32;
        if er_codec::item_category_of(full) != er_codec::CATEGORY_GOODS {
            continue;
        }
        let gid = er_codec::row_id_of(full);
        // THE GUARD (see the module header): the FMG entry is shared, so renaming the good behind this
        // slot renames every copy the player can hold. If the seed can grant this good, leave the slot
        // showing its vanilla name -- one slot lying about one reward beats a renamed stone economy.
        if real.contains(&gid) {
            protected += 1;
            continue;
        }
        overridden += 1;
        // Pure, host-tested formatter (er-logic name_override::shop_label) so the exact GoodsName +
        // routing caption a lock/foreign slot shows is pinned by unit test, not inlined here.
        let lbl = er_logic::name_override::shop_label(&s.name, &s.owner, &s.game, s.kind);
        nmap.insert(gid, lbl.name.encode_utf16().collect());
        let u: Vec<u16> = lbl.caption.encode_utf16().collect();
        imap.insert(gid, u.clone());
        cmap.insert(gid, u);
    }
    let names: Vec<(u32, Vec<u16>)> = nmap.into_iter().collect();
    let infos: Vec<(u32, Vec<u16>)> = imap.into_iter().collect();
    let caps: Vec<(u32, Vec<u16>)> = cmap.into_iter().collect();
    let n = crate::fmg_inject::extend_swap_overrides(GOODS_NAME_CAT, &names);
    let i = crate::fmg_inject::extend_swap_overrides(GOODS_INFO_CAT, &infos);
    let c = crate::fmg_inject::extend_swap_overrides(GOODS_CAPTION_CAT, &caps);
    log::info!(
        "shop-preview: {overridden} foreign/gem slot(s) ({} distinct, {native} own-world via shop_sell, \
         {protected} left vanilla to protect a real good's shared FMG entry) -> extend-swap names={n} infos={i} captions={c}",
        names.len()
    );
    DONE.store(true, Ordering::Relaxed);
    true
}
