//! Resolve a synthetic placeholder's `EquipParamGoods` row into the pure `er_codec::GoodsRowFields`.
//!
//! Re-homed from the standalone `eldenring-ap/game/params.rs` (typed eldenring-0.14 path; the
//! Phase-1 spike proved it reaches the goods param in-game, rowCount 3795). The manual ParamBase
//! walk is gone — `SoloParamRepository::get::<EquipParamGoods>(id)` does the typed lookup, and the
//! five carrier fields come off `EQUIP_PARAM_GOODS_ST` snake_case getters.

use er_codec::GoodsRowFields;
use eldenring::cs::{EquipParamGoods, SoloParamRepository};
use eldenring::param::EQUIP_PARAM_GOODS_ST;
use fromsoftware_shared::FromStatic;

/// Look up a goods row by its (category-stripped) row id and project the AP carrier fields.
/// `None` if the param repo isn't ready (pre-world) or the id is absent.
pub fn goods_row_fields(row_id: i32) -> Option<GoodsRowFields> {
    // SAFETY: FD4 singleton accessor; only reached from the AddItemFunc detour, which by
    // construction fires during in-world pickups (so the param tables are populated).
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    let row: &EQUIP_PARAM_GOODS_ST = repo.get::<EquipParamGoods>(row_id as u32)?;
    Some(GoodsRowFields {
        vagrant_item_lot_id: row.vagrant_item_lot_id(),
        vagrant_bonus_ene_drop_item_lot_id: row.vagrant_bonus_ene_drop_item_lot_id(),
        basic_price: row.basic_price(),
        sell_value: row.sell_value(),
        disable_use_at_out_of_coliseum: row.disable_use_at_out_of_coliseum(),
    })
}
