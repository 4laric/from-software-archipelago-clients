//! Inventory-scan discovery for synthetic placeholders that BYPASS the AddItemFunc detour — shop
//! buys, NPC gifts, offline pickups. The detour SUPPRESSES world pickups (they never enter the bag),
//! so any synthetic goods row sitting in the inventory got there some other way and needs reporting
//! + a local grant here. Under shared `own_world:false` there's no server echo, so the synthetic's
//! own `basicPrice` (local item) is what we grant.
//!
//! Enumeration is the exact path `upgrades.rs` walks for auto_upgrade (proven in-game):
//! `GameDataMan -> main_player_game_data -> equipment.equip_inventory_data.items_data.items()`.

// intentional module-doc prose wrapping, not a markdown list
#![allow(clippy::doc_lazy_continuation)]

use eldenring::cs::{GameDataMan, ItemCategory};
use fromsoftware_shared::FromStatic;

use crate::params;

/// One synthetic placeholder found in the bag. Under the echo model the caller only needs
/// `location` (the server echoes the item back to grant); the decoded local fields are kept for
/// debugging / a potential non-echo path.
#[allow(dead_code)]
pub struct Scanned {
    pub location: i64,
    pub local_item_id: i32,
    pub local_qty: i32,
    pub foreign: bool,
}

/// Walk held goods and decode any synthetic placeholders currently in the bag. Pure read; dedup +
/// grant is the caller's job (it holds the AP client). Empty until the player game data is up.
pub fn scan_synthetics() -> Vec<Scanned> {
    let mut out = Vec::new();
    let gdm = match unsafe { GameDataMan::instance() } {
        Ok(g) => g,
        Err(_) => return out,
    };
    let pgd = gdm.main_player_game_data.as_ref();
    for entry in pgd.equipment.equip_inventory_data.items_data.items() {
        if entry.item_id.category() != ItemCategory::Goods {
            continue;
        }
        let row = entry.item_id.param_id() as i32;
        if !er_codec::is_synthetic_goods(er_codec::CATEGORY_GOODS | row as u32) {
            continue;
        }
        if let Some(fields) = params::goods_row_fields(row) {
            let s = er_codec::decode_synthetic(&fields);
            out.push(Scanned {
                location: s.ap_location_id,
                local_item_id: s.local_item_id,
                local_qty: s.local_quantity.max(1),
                foreign: s.foreign_remove,
            });
        }
    }
    out
}
