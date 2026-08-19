//! Resolve a synthetic placeholder's `EquipParamGoods` row into the pure `er_codec::GoodsRowFields`.
//!
//! Re-homed from the standalone `eldenring-ap/game/params.rs` (typed eldenring-0.14 path; the
//! Phase-1 spike proved it reaches the goods param in-game, rowCount 3795). The manual ParamBase
//! walk is gone. [`crate::param_guard`] performs the same typed lookup without panicking when a
//! game callback lands after the holder has been emptied during world teardown, and the five
//! carrier fields come off `EQUIP_PARAM_GOODS_ST` snake_case getters.

use eldenring::cs::{EquipParamGoods, SoloParamRepository};
use eldenring::param::EQUIP_PARAM_GOODS_ST;
use er_codec::GoodsRowFields;
use fromsoftware_shared::FromStatic;

/// Look up a goods row by its (category-stripped) row id and project the AP carrier fields.
/// `None` if the param repo isn't ready (pre-world) or the id is absent.
pub fn goods_row_fields(row_id: i32) -> Option<GoodsRowFields> {
    // SAFETY: FD4 singleton accessor, read-only on the game thread. The AddItem detour can still
    // fire while the world is tearing down, so singleton availability is not enough; param_guard
    // checks the requested holder's res-cap before touching the row.
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    let row: &EQUIP_PARAM_GOODS_ST =
        crate::param_guard::get::<EquipParamGoods>(repo, row_id as u32, "add-item goods decode")?;
    Some(GoodsRowFields {
        vagrant_item_lot_id: row.vagrant_item_lot_id(),
        vagrant_bonus_ene_drop_item_lot_id: row.vagrant_bonus_ene_drop_item_lot_id(),
        basic_price: row.basic_price(),
        sell_value: row.sell_value(),
        disable_use_at_out_of_coliseum: row.disable_use_at_out_of_coliseum(),
    })
}

/// Sorted list of the SYNTHETIC goods row ids (injected AP placeholders, id > SYNTHETIC_GOODS_MIN_ID).
/// Used by `fmg_inject` to add a GoodsName entry per synthetic id. Empty if the repo isn't up yet.
/// MUST be called in-world (FD4 singleton populated). Iterates the goods table once.
pub fn synthetic_goods_ids() -> Vec<u32> {
    let repo = match unsafe { SoloParamRepository::instance() } {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let Some(rows) = crate::param_guard::rows::<EquipParamGoods>(repo, "synthetic-goods scan")
    else {
        return Vec::new();
    };
    let mut v: Vec<u32> = Vec::new();
    for (id, _row) in rows {
        if er_codec::is_synthetic_goods(er_codec::CATEGORY_GOODS | id) {
            v.push(id);
        }
    }
    v.sort_unstable();
    v
}
