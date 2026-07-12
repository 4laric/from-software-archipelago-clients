//! minibaker.rs -- runtime ShopLineupParam injection ("mini-baker"): repurpose one reserved shop row
//! into an always-in-stock, unlimited-quantity vendor. Greenfield uses it to sell Stonesword Keys at
//! the Twin Maiden Husks so imp-statue (fog-seal) checks are never permanently missable. Same live-param
//! primitive as shop_flags.rs (SoloParamRepository row access + field writes), gated on the slot_data
//! `stoneswordVendorRow` id so non-greenfield seeds are untouched.
//!
//! Row layout (confirmed live 2026-07-06, memory er-minibaker-shoplineup):
//!   equipId@+0x00 (i32)  value@+0x04 (i32)  eventFlag_forStock@+0x0C (u32)  sellQuantity@+0x14 (i16)
//! equipType (+0x17) is already 3 (goods) on the reserved row and Stonesword Key is goods, so only
//! equipId / value / eventFlag_forStock / sellQuantity are written.

#![allow(dead_code)]

use eldenring::cs::{ShopLineupParam, SoloParamRepository};
use eldenring::param::SHOP_LINEUP_PARAM;
use fromsoftware_shared::FromStatic;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const OFF_EQUIP: usize = 0x00;
const OFF_VALUE: usize = 0x04;
const OFF_STOCK_FLAG: usize = 0x0C;
const OFF_SELL_QTY: usize = 0x14;

const STONESWORD_KEY: i32 = 8000; // goods id
const PRICE: i32 = 4000; // runes

/// Reserved ShopLineupParam row id from slot_data `stoneswordVendorRow` (0 = feature off).
static ROW: AtomicU32 = AtomicU32::new(0);
static LOGGED: AtomicBool = AtomicBool::new(false);

/// Called from core.rs when slot_data is parsed. `row_id` 0 leaves every shop row vanilla.
pub fn configure(row_id: u32) {
    ROW.store(row_id, Ordering::Relaxed);
    LOGGED.store(false, Ordering::Relaxed);
    if row_id != 0 {
        log::info!("minibaker: Stonesword Key vendor reserved on ShopLineupParam row {row_id}");
    }
}

/// Base address of the reserved row, or None if the param repo isn't up / the row is absent.
fn row_base(row_id: u32) -> Option<usize> {
    // SAFETY: FD4 singleton; game thread, in-world (caller gates). Same access shop_flags.rs uses.
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    let row: &SHOP_LINEUP_PARAM = repo.get::<ShopLineupParam>(row_id)?;
    Some(row as *const SHOP_LINEUP_PARAM as usize)
}

/// Re-applied each in-world tick (cheap: one lookup + four scalar writes) so it survives shop refreshes
/// and param reloads. No-op when no row is reserved. Returns false only while the repo isn't ready yet.
pub fn run() -> bool {
    let row = ROW.load(Ordering::Relaxed);
    if row == 0 {
        return true;
    }
    let Some(base) = row_base(row) else {
        return false;
    };
    // SAFETY: the live RW param table (the game writes it too); writing four scalar fields of one row.
    unsafe {
        ((base + OFF_EQUIP) as *mut i32).write_unaligned(STONESWORD_KEY);
        ((base + OFF_VALUE) as *mut i32).write_unaligned(PRICE);
        ((base + OFF_STOCK_FLAG) as *mut u32).write_unaligned(0); // 0 = always in stock (no flag gate)
        ((base + OFF_SELL_QTY) as *mut i16).write_unaligned(-1); // -1 = unlimited
    }
    if !LOGGED.swap(true, Ordering::Relaxed) {
        log::info!(
            "minibaker: row {row} -> Stonesword Key (id {STONESWORD_KEY}) @ {PRICE} runes, unlimited stock"
        );
    }
    true
}
