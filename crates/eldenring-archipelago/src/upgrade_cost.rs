//! FLATTEN_REGULAR_UPGRADES — flatten regular (non-somber) weapon reinforcement costs at
//! runtime by editing `EquipMtrlSetParam` in memory. No baker, no RE, no code-cave.
//!
//! The baker used to flatten these counts on-disk (regulation.bin `EquipMtrlSetParam` edit).
//! With the baker retired we do the SAME edit against the live param table, exactly the way
//! `shop_flags.rs` rewrites `ShopLineupParam` rows: `SoloParamRepository::instance_mut()` +
//! `rows_mut::<EquipMtrlSetParam>()` + typed `set_item_num0X()` setters.
//!
//! Model (Paramdex EQUIP_MTRL_SET_PARAM_ST): each reinforcement step points (via
//! `ReinforceParamWeapon.materialSetId`) at one EquipMtrlSetParam row that lists up to 6
//! (materialId0X, itemNum0X) pairs — the material and how many are needed for that step. We
//! scan every row and, for any slot whose material is a REGULAR smithing stone, clamp its
//! required count to 1. Somber material sets are left untouched (their materialIds are somber
//! stones, which are not in REGULAR_STONE_IDS).
//!
//! WIRING (see patch_flatten_regular_upgrades.py):
//!   - lib.rs: `mod upgrade_cost;`
//!   - core.rs update_live() (slot_data parse, by set_auto_upgrade): `set_flatten(sd.pointer(
//!     "/options/flatten_regular_upgrades").and_then(|v| v.as_i64()).unwrap_or(0) != 0);`
//!   - core.rs in-world tick (by shop_flags::run): `crate::upgrade_cost::maybe_apply();`
//!
//! FALLBACK: if an in-game smithing-menu check shows the counts did NOT drop, switch to the
//! grant-time 2x multiplier in detour.rs (see the response notes) — this module then just
//! stays off (option default 0).

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{EquipMtrlSetParam, SoloParamRepository};
use eldenring::param::EQUIP_MTRL_SET_PARAM_ST;
use fromsoftware_shared::FromStatic;

/// Regular (non-somber) upgrade materials: Smithing Stone [1]-[8] (EquipParamGoods 10100-10107)
/// + Ancient Dragon Smithing Stone (10140). Somber stones (10160-10168, 10200) are deliberately
/// EXCLUDED so somber weapons keep their vanilla curve.
const REGULAR_STONE_IDS: &[i32] = &[
    10100, 10101, 10102, 10103, 10104, 10105, 10106, 10107, 10140,
];

/// Flat required count per regular-stone upgrade step.
const FLAT_COUNT: i8 = 1;

static ENABLED: AtomicBool = AtomicBool::new(false);
static APPLIED: AtomicBool = AtomicBool::new(false);

/// Set from slot_data `/options/flatten_regular_upgrades` at connect. Re-arms the one-shot apply
/// so a reconnect re-flattens a freshly (re)loaded param table.
pub fn set_flatten(on: bool) {
    ENABLED.store(on, Ordering::SeqCst);
    APPLIED.store(false, Ordering::SeqCst);
    log::info!("flatten_regular_upgrades: {}", if on { "ON" } else { "off" });
}

fn is_regular_stone(id: i32) -> bool {
    REGULAR_STONE_IDS.contains(&id)
}

/// Idempotent in-world one-shot: clamp every regular-stone material count to FLAT_COUNT. Read-
/// then-act and lower-only; no-op if disabled, already applied, or the repo isn't up yet (retried
/// next tick). Returns the number of material slots changed.
pub fn maybe_apply() -> u32 {
    if !ENABLED.load(Ordering::SeqCst) || APPLIED.load(Ordering::SeqCst) {
        return 0;
    }
    // SAFETY: FD4 singleton; game thread, in-world (caller gates). instance_mut + rows_mut are the
    // crate's sanctioned mutable access on the live RW param table (mirrors shop_flags).
    let repo = match unsafe { SoloParamRepository::instance_mut() } {
        Ok(r) => r,
        Err(_) => return 0, // repo not ready; try again next tick
    };
    let mut changed = 0u32;
    for (id, row) in repo.rows_mut::<EquipMtrlSetParam>() {
        changed += flatten_row(id, row);
    }
    APPLIED.store(true, Ordering::SeqCst);
    log::info!(
        "flatten_regular_upgrades: === clamped {changed} regular-stone material slot(s) to {FLAT_COUNT} ==="
    );
    changed
}

fn flatten_row(id: u32, row: &mut EQUIP_MTRL_SET_PARAM_ST) -> u32 {
    let mut n = 0u32;
    macro_rules! slot {
        ($mid:ident, $num:ident, $set:ident) => {{
            let mid = row.$mid();
            let cur = row.$num();
            // cur == -1 means "slot unused"; only lower real counts > FLAT_COUNT.
            if is_regular_stone(mid) && cur > FLAT_COUNT {
                row.$set(FLAT_COUNT);
                n += 1;
                log::info!(
                    "flatten: mtrlset {id} {} {cur} -> {FLAT_COUNT} (stone {mid})",
                    stringify!($num)
                );
            }
        }};
    }
    slot!(material_id01, item_num01, set_item_num01);
    slot!(material_id02, item_num02, set_item_num02);
    slot!(material_id03, item_num03, set_item_num03);
    slot!(material_id04, item_num04, set_item_num04);
    slot!(material_id05, item_num05, set_item_num05);
    slot!(material_id06, item_num06, set_item_num06);
    n
}
