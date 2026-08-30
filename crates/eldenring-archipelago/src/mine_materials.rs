//! Per-seed replacement of repeatable mine-stone asset rewards.
//!
//! The world census proves that the 133 placed deposits share 11 unflagged ItemLotParam_map rows.
//! Each row is a GOODS-only, repeatable, break-on-pickup asset reward. `mineMaterialRoll` carries
//! `{lot_id: replacement_goods_id}` for exactly those rows.
//!
//! We change only `lotItemId01`. Quantity, category, weight, flags, asset presence and respawn fields
//! remain vanilla, so deposits stay ordinary repeatable pickups and never become AP checks.

#![allow(dead_code)]

use eldenring::cs::SoloParamRepository;
use fromsoftware_shared::FromStatic;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static ROLL: Mutex<Option<HashMap<u32, i32>>> = Mutex::new(None);
static DONE: AtomicBool = AtomicBool::new(false);

pub fn configure(roll: HashMap<u32, i32>) {
    let count = roll.len();
    *ROLL.lock().unwrap() = Some(roll);
    DONE.store(false, Ordering::Relaxed);
    log::info!("mine-materials: configured {count} repeatable map lot(s) for reroll");
}

pub fn run() -> bool {
    if DONE.load(Ordering::Relaxed) {
        return true;
    }
    let roll: Vec<(u32, i32)> = match ROLL.lock().unwrap().as_ref() {
        Some(table) if !table.is_empty() => {
            table.iter().map(|(&lot, &goods)| (lot, goods)).collect()
        }
        Some(_) => {
            DONE.store(true, Ordering::Relaxed);
            return true;
        }
        None => return true,
    };

    // SAFETY: game-thread, in-world mutable access to the FD4 param singleton, matching the existing
    // check-lot and enemy-drop passes. param_guard defers through restream windows.
    let repo = match unsafe { SoloParamRepository::instance_mut() } {
        Ok(repo) => repo,
        Err(_) => return false,
    };
    if !crate::param_guard::is_available::<eldenring::cs::ItemLotParam_map>(
        repo,
        "mine-material reroll",
    ) {
        return false;
    }

    let mut changed = 0usize;
    for (lot, goods) in roll {
        let Some(row) = crate::param_guard::get_mut::<eldenring::cs::ItemLotParam_map>(
            repo,
            lot,
            "mine-material reroll",
        ) else {
            continue;
        };
        row.set_lot_item_id01(goods);
        changed += 1;
    }
    log::info!(
        "mine-materials: rerolled {changed} repeatable map lot(s) (quantity/flags/respawn untouched)"
    );
    DONE.store(true, Ordering::Relaxed);
    true
}

pub fn reset() {
    DONE.store(false, Ordering::Relaxed);
}
