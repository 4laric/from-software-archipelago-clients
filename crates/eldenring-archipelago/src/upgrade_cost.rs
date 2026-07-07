//! FLATTEN_REGULAR_UPGRADES — cap regular (non-somber) weapon reinforcement costs at runtime by
//! editing `EquipMtrlSetParam` in memory. No baker, no RE, no code-cave.
//!
//! GRADUATED (2026-07-07): the slot_data option is an INT `stones/level` (0 = off; 1..4 = cap each
//! regular-stone upgrade step at N). Was a bool (cap at 1). Lower-only: a step already costing <= N
//! is untouched, so vanilla 2/4/6 with N=3 becomes 2/3/3 (never RAISED — reconnect-safe). N=1 is the
//! old flatten. The apworld's per-sphere auto-ramp sizes stone supply against THIS same ladder, so
//! keep the two in sync (tools/analyze_upgrade_curve.py models the identical cap-at-N cost).
//!
//! Model (Paramdex EQUIP_MTRL_SET_PARAM_ST): each reinforcement step points (via
//! `ReinforceParamWeapon.materialSetId`) at one EquipMtrlSetParam row listing up to 6
//! (materialId0X, itemNum0X) pairs. We scan every row and, for any slot whose material is a REGULAR
//! smithing stone, clamp its required count DOWN to N. Somber material sets are left untouched.
//!
//! WIRING (see patch_flatten_regular_upgrades.py):
//!   - lib.rs: `mod upgrade_cost;`
//!   - core.rs update_live() (slot_data parse): `set_flatten(sd.pointer(
//!     "/options/flatten_regular_upgrades").and_then(|v| v.as_i64()).unwrap_or(0));`
//!   - core.rs in-world tick (by shop_flags::run): `crate::upgrade_cost::maybe_apply();`
//!
//! FALLBACK: if an in-game smithing-menu check shows the counts did NOT drop, switch to the
//! grant-time multiplier in detour.rs — this module then just stays off (option default 0).

#![allow(dead_code)]

use std::sync::atomic::{AtomicI32, Ordering};

use eldenring::cs::{EquipMtrlSetParam, SoloParamRepository};
use eldenring::param::EQUIP_MTRL_SET_PARAM_ST;
use fromsoftware_shared::FromStatic;

// The per-slot clamp decision + cap bound + re-arm latch semantics are pure and live in er-logic
// (`upgrade_cost`), where they are replay-tested (`upgrade_cost_replay`) against the reconnect param
// reload. This module supplies the live-param seam; it must use the SAME decision so the test guards
// the shipped path.
use er_logic::upgrade_cost::{clamp_count, MAX_CAP};

/// Resolved stones-per-level cap: 0 = off, 1..MAX_CAP = cap each regular step at N. Set from
/// slot_data at connect.
static FLAT_N: AtomicI32 = AtomicI32::new(0);
static APPLIED: AtomicI32 = AtomicI32::new(-1); // last-applied cap; -1 = not yet applied this arm

/// Set from slot_data `/options/flatten_regular_upgrades` at connect. Re-arms the one-shot apply so a
/// reconnect re-caps a freshly (re)loaded param table. 0 disables; values are clamped to [0, MAX_CAP].
pub fn set_flatten(n: i64) {
    let cap = n.clamp(0, MAX_CAP as i64) as i32;
    FLAT_N.store(cap, Ordering::SeqCst);
    APPLIED.store(-1, Ordering::SeqCst);
    if cap > 0 {
        log::info!("flatten_regular_upgrades: ON (cap regular steps at {cap} stone(s)/level)");
    } else {
        log::info!("flatten_regular_upgrades: off");
    }
}

/// Idempotent in-world one-shot: clamp every regular-stone material count DOWN to the resolved cap N.
/// Read-then-act and lower-only; no-op if disabled, already applied at this cap, or the repo isn't up
/// yet (retried next tick). Returns the number of material slots changed.
pub fn maybe_apply() -> u32 {
    let cap = FLAT_N.load(Ordering::SeqCst);
    if cap <= 0 || APPLIED.load(Ordering::SeqCst) == cap {
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
        changed += flatten_row(id, row, cap);
    }
    APPLIED.store(cap, Ordering::SeqCst);
    log::info!(
        "flatten_regular_upgrades: === clamped {changed} regular-stone material slot(s) to {cap} ==="
    );
    changed
}

fn flatten_row(id: u32, row: &mut EQUIP_MTRL_SET_PARAM_ST, cap: i32) -> u32 {
    let mut n = 0u32;
    macro_rules! slot {
        ($mid:ident, $num:ident, $set:ident) => {{
            let mid = row.$mid();
            let cur = row.$num();
            // Shared lower-only decision (er_logic::upgrade_cost): Some(new) only for a regular stone
            // above the cap; None for unused (-1) / non-regular / already-low slots.
            if let Some(nv) = clamp_count(mid, cur, cap) {
                row.$set(nv);
                n += 1;
                log::info!(
                    "flatten: mtrlset {id} {} {cur} -> {nv} (stone {mid})",
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
