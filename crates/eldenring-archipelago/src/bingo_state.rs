//! Native E1 observations. Typed fields from locked fromsoftware-rs af8f38c;
//! flask goods identities from GoodsName.fmg, decoded in er_logic::bingo.
use std::collections::BTreeMap;

pub fn observe() -> Option<BTreeMap<String, u32>> {
    use eldenring::cs::{GameDataMan, ItemCategory, WorldChrMan};
    use fromsoftware_shared::{FromStatic, NonEmptyIteratorExt};
    if !crate::flags::in_world() {
        return None;
    }
    let world = unsafe { WorldChrMan::instance() }.ok()?;
    let player = world.main_player.as_ref()?;
    if er_logic::death_guard::lists_unsafe_to_touch(player.chr_ins.modules.data.hp) {
        return None;
    }
    let game = unsafe { GameDataMan::instance() }.ok()?;
    let pgd = game.main_player_game_data.as_ref();
    // Visiting another player's world cannot earn this slot's objectives.
    if !pgd.is_my_world {
        return None;
    }
    let inv = &pgd.equipment.equip_inventory_data.items_data;
    let potency = inv
        .normal_entries()
        .iter()
        .chain(inv.multiplay_key_entries().iter())
        .non_empty()
        .filter(|e| e.item_id.category() == ItemCategory::Goods)
        .filter_map(|e| er_logic::bingo::flask_potency(e.item_id.param_id()))
        .max()
        .unwrap_or(0);
    Some(BTreeMap::from([
        ("level".into(), pgd.level),
        ("faith".into(), pgd.faith),
        ("arcane".into(), pgd.arcane),
        ("intelligence".into(), pgd.intelligence),
        ("scadutree".into(), pgd.scadutree_blessing as u32),
        ("spirit_ash".into(), pgd.reversed_spirit_ash as u32),
        (
            "flask_charges".into(),
            pgd.max_hp_flask as u32 + pgd.max_fp_flask as u32,
        ),
        ("flask_potency".into(), potency),
    ]))
}

// All AP flag writers pass through flags.rs. The game's own boss events do not.
static PROTECTED: std::sync::RwLock<Vec<u32>> = std::sync::RwLock::new(Vec::new());
pub fn protect(board: Option<&er_logic::bingo::Board>) {
    *PROTECTED.write().unwrap() = board
        .map(|b| {
            b.cells
                .iter()
                .filter_map(|c| (c.flag != 0).then_some(c.flag))
                .collect()
        })
        .unwrap_or_default();
}
pub fn protected(flag: u32) -> bool {
    PROTECTED.read().unwrap().contains(&flag)
}
