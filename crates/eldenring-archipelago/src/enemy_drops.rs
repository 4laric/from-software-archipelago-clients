//! enemy_drops.rs — reroll the FARMABLE enemy drops to consumables, per seed.
//!
//! Same predicate as shop_stock, on the other param table:
//!
//!     A LOT WITH NO FLAG CANNOT BE A CHECK, SO IT IS FREE TO REROLL.
//!
//! ItemLotParam_enemy splits cleanly: 244 rows carry `getItemFlagId` — those ARE the one-time
//! enemy/boss drop CHECKS — and 4891 carry no flag, i.e. repeatable farmable drops. The apworld only
//! ever sends the unflagged ones (`features/enemy_drops.py` filters on the flag), so a check row cannot
//! reach this module. EMEVD `AwardItemLot` references 15 lot ids and none is unflagged, so no reward
//! hides in the free set either.
//!
//! ⚠ The flag column is `getItemFlagId`, SINGULAR. ER leaves the per-slot `getItemFlagId01..08` columns
//! at zero, so reading those — the obvious guess — reports every row as unflagged. Taking that at face
//! value would have handed the reroll 5047 map checks and 244 enemy-drop checks to overwrite, boss drops
//! among them. Recorded here because the same trap is one lookup away for anyone touching item lots.
//!
//! WHAT CHANGES: only the GOODS slots (`lotItemCategory == 1`). Weapon/armor/talisman drop slots keep
//! their vanilla contents. `lotItemBasePoint` (the drop WEIGHT) is never written, so drop RATES stay
//! exactly vanilla — an enemy that dropped something 5% of the time still does; only the identity of the
//! consumable changes. Nothing new drops, nothing stops dropping.
//!
//! slot_data `enemyDropRoll`: { "<lot id>": [slot, goodsId, slot, goodsId, ...] }  (flat pairs — the
//! contract's LISTVAL_INT_MAP shape is list[int], and validate_slot_data rejects nesting).
//!
//! Idempotent; re-armed on tick like the other param passes.

#![allow(dead_code)]

use eldenring::cs::SoloParamRepository;
use fromsoftware_shared::FromStatic; // brings SoloParamRepository::instance_mut into scope
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// (slot index 1..=8, goods row id) -- one rerolled drop slot.
type DropSlot = (u8, i32);
/// lot id -> the slots we rerolled in it.
type RollTable = HashMap<u32, Vec<DropSlot>>;

static ROLL: Mutex<Option<RollTable>> = Mutex::new(None);
static DONE: AtomicBool = AtomicBool::new(false);

/// Called from net.rs at connect with the parsed slot_data map.
pub fn configure(roll: HashMap<u32, Vec<(u8, i32)>>) {
    let lots = roll.len();
    let slots: usize = roll.values().map(|v| v.len()).sum();
    *ROLL.lock().unwrap() = Some(roll);
    DONE.store(false, Ordering::Relaxed);
    log::info!("enemy-drops: configured {lots} lot(s) / {slots} goods slot(s) for reroll");
}

/// Apply the reroll. Returns false if the param repo isn't up yet (caller retries next tick).
pub fn run() -> bool {
    if DONE.load(Ordering::Relaxed) {
        return true;
    }
    let roll: Vec<(u32, Vec<(u8, i32)>)> = match ROLL.lock().unwrap().as_ref() {
        Some(m) if !m.is_empty() => m.iter().map(|(k, v)| (*k, v.clone())).collect(),
        Some(_) => {
            DONE.store(true, Ordering::Relaxed); // feature off
            return true;
        }
        None => return true, // not configured (non-greenfield seed)
    };

    // SAFETY: FD4 singleton; game thread, in-world (caller gates). Same sanctioned mutable param access
    // shop_sell / shop_flags use on the live RW table.
    let repo = match unsafe { SoloParamRepository::instance_mut() } {
        Ok(r) => r,
        Err(_) => return false, // repo not up yet -- retry
    };

    let mut n = 0usize;
    for (lot, slots) in roll {
        // Param naming, settled by the Windows build 2026-07-11:
        //   table type : eldenring::cs::ItemLotParam_enemy   (snake, not CamelCase)
        //   row struct : eldenring::param::ITEMLOT_PARAM_ST  (shared with ItemLotParam_map)
        //   setters    : set_lot_item_id01..08               (NO underscore before the digits)
        // lotItemBasePoint (the drop WEIGHT) is deliberately NOT written, so drop rates stay vanilla.
        let Some(row) = repo.get_mut::<eldenring::cs::ItemLotParam_enemy>(lot) else {
            continue;
        };
        for (slot, gid) in slots {
            match slot {
                1 => row.set_lot_item_id01(gid),
                2 => row.set_lot_item_id02(gid),
                3 => row.set_lot_item_id03(gid),
                4 => row.set_lot_item_id04(gid),
                5 => row.set_lot_item_id05(gid),
                6 => row.set_lot_item_id06(gid),
                7 => row.set_lot_item_id07(gid),
                8 => row.set_lot_item_id08(gid),
                _ => continue,
            }
            n += 1;
        }
    }
    log::info!("enemy-drops: rerolled {n} farmable goods slot(s) (drop rates untouched)");
    DONE.store(true, Ordering::Relaxed);
    true
}

/// Re-arm after a reconnect / new seed.
pub fn reset() {
    DONE.store(false, Ordering::Relaxed);
}
