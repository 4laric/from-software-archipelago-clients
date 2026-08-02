//! Auto-equip decision logic (pure).
//!
//! When the `auto_equip` option is on, a received WEAPON or PROTECTOR is equipped immediately --
//! including mid-boss-fight. That is deliberate: the motivating case is the French Challenge
//! (Wretch start + randomizer + Use What You Get + permadeath + all bosses + region lock), whose
//! whole premise is that you do not choose your build. Clobbering the weapon in your hand at the
//! worst possible moment IS the feature, not a bug to guard against.
//!
//! Everything here is host-testable and holds no game state. The caller supplies the two param
//! fields we cannot read without the game: `EQUIP_PARAM_WEAPON_ST.wep_type` and
//! `EQUIP_PARAM_PROTECTOR_ST.protectorCategory`.

/// Category nibble of a FullID (`(category << 28) | row`), matching the encoding used across the
/// client. `ItemCategory::Weapon = 0`, `Protector = 1`.
const CATEGORY_MASK: u32 = 0xF000_0000;
const CATEGORY_WEAPON: u32 = 0x0000_0000;
const CATEGORY_PROTECTOR: u32 = 0x1000_0000;

/// `ChrAsmSlot` indices we can target. The full enum is 0..=21; these are the six that auto-equip
/// ever writes. Confirmed against a live `chr_asm` dump on 2.6.2.0: slots 0/2/3/4/5 held unarmed
/// (`110000`) for the five idle hand slots, and 12..=15 held protector entries `0x10002710`,
/// `0x10002774`, `0x100027D8`, `0x1000283C` -- head, chest, hands, legs, in that order.
pub const SLOT_WEAPON_LEFT_1: u32 = 0;
pub const SLOT_WEAPON_RIGHT_1: u32 = 1;
pub const SLOT_PROTECTOR_HEAD: u32 = 12;
pub const SLOT_PROTECTOR_CHEST: u32 = 13;
pub const SLOT_PROTECTOR_HANDS: u32 = 14;
pub const SLOT_PROTECTOR_LEGS: u32 = 15;

/// Which primary hand a received weapon should occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hand {
    Left,
    Right,
}

/// What kind of thing a received FullID is, and therefore which param table the caller must read
/// to resolve the target slot. Anything else (goods, gems, accessories) is not auto-equipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Equipable {
    /// Read `EQUIP_PARAM_WEAPON_ST.wep_type`, then [`slot_for_wep_type`].
    Weapon,
    /// Read `EQUIP_PARAM_PROTECTOR_ST.protectorCategory`, then [`slot_for_protector_category`].
    Protector,
}

/// Is this received FullID something auto-equip handles at all?
pub fn equipable(full_id: i32) -> Option<Equipable> {
    match (full_id as u32) & CATEGORY_MASK {
        CATEGORY_WEAPON => Some(Equipable::Weapon),
        CATEGORY_PROTECTOR => Some(Equipable::Protector),
        _ => None,
    }
}

/// Is this received FullID a weapon? Retained for callers that only care about the weapon case.
pub fn is_weapon(full_id: i32) -> bool {
    (full_id as u32) & CATEGORY_MASK == CATEGORY_WEAPON
}

/// `EQUIP_PARAM_WEAPON_ST.wep_type` values that are LEFT-hand only (shields). ER weaponCategory:
/// 65 = Small Shield, 67 = Medium/Standard Shield, 69 = Greatshield. RUNTIME-UNCONFIRMED against a
/// live equip -- if a shield lands in the right hand the fix is this list, and the failure is benign
/// (the player re-hands it manually; no crash). Catalysts (staff 57 / seal 59) and torches (87) are
/// deliberately NOT here: they're routinely main-handed, so they default RIGHT like any weapon.
const LEFT_HAND_WEP_TYPES: &[u16] = &[65, 67, 69];

/// The primary hand a weapon of this `wep_type` should occupy. Shields -> LEFT; everything else ->
/// RIGHT (the main-hand slot).
pub fn hand_for_wep_type(wep_type: u16) -> Hand {
    if LEFT_HAND_WEP_TYPES.contains(&wep_type) {
        Hand::Left
    } else {
        Hand::Right
    }
}

/// The `ChrAsmSlot` index a weapon of this `wep_type` should occupy.
pub fn slot_for_wep_type(wep_type: u16) -> u32 {
    match hand_for_wep_type(wep_type) {
        Hand::Left => SLOT_WEAPON_LEFT_1,
        Hand::Right => SLOT_WEAPON_RIGHT_1,
    }
}

/// The `ChrAsmSlot` index for a protector, from `EQUIP_PARAM_PROTECTOR_ST.protectorCategory`.
///
/// Read the game's own field; do NOT infer the slot from the id. The folklore convention
/// `(id / 100) % 10` disagrees with `protectorCategory` on **44 of the 820 vanilla rows**, so it
/// would mis-slot 44 pieces. Category `4` is dummy data -- 41 rows, ids 1000..2100, no names, all
/// four of `headEquip`/`bodyEquip`/`armEquip`/`legEquip` clear -- and returns `None` so nothing
/// is equipped.
///
/// Known wrinkle, deliberately left alone: 11 rows in the 193xxxx..200xxxx range carry a category
/// of 1 or 3 while setting `headEquip`. `protectorCategory` wins here because it is the field the
/// game itself slots by; if one of those ever lands in the wrong slot the failure is benign and
/// the fix is a small override list.
pub fn slot_for_protector_category(protector_category: u8) -> Option<u32> {
    match protector_category {
        0 => Some(SLOT_PROTECTOR_HEAD),
        1 => Some(SLOT_PROTECTOR_CHEST),
        2 => Some(SLOT_PROTECTOR_HANDS),
        3 => Some(SLOT_PROTECTOR_LEGS),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEAPON_DAGGER: i32 = 0x0000_2710; // category 0, row 10000
    const GOODS_FLASK: i32 = 0x4000_03E9; // category 4 (goods), row 1001
    const PROTECTOR_HELM: i32 = 0x1000_2710; // category 1 (armor)
    const GEM_ASH: i32 = 0x8000_0064u32 as i32; // category 8 (ash of war)

    #[test]
    fn only_weapons_are_weapons() {
        assert!(is_weapon(WEAPON_DAGGER));
        assert!(!is_weapon(GOODS_FLASK));
        assert!(!is_weapon(PROTECTOR_HELM));
        assert!(!is_weapon(GEM_ASH));
    }

    #[test]
    fn weapons_and_protectors_are_equipable_nothing_else_is() {
        assert_eq!(equipable(WEAPON_DAGGER), Some(Equipable::Weapon));
        assert_eq!(equipable(PROTECTOR_HELM), Some(Equipable::Protector));
        assert_eq!(equipable(GOODS_FLASK), None);
        assert_eq!(equipable(GEM_ASH), None);
    }

    #[test]
    fn shields_go_left_weapons_go_right() {
        assert_eq!(hand_for_wep_type(65), Hand::Left); // small shield
        assert_eq!(hand_for_wep_type(67), Hand::Left); // medium shield
        assert_eq!(hand_for_wep_type(69), Hand::Left); // greatshield
        assert_eq!(hand_for_wep_type(1), Hand::Right); // straight sword
        assert_eq!(hand_for_wep_type(57), Hand::Right); // glintstone staff
        assert_eq!(hand_for_wep_type(59), Hand::Right); // sacred seal
        assert_eq!(hand_for_wep_type(87), Hand::Right); // torch (main-hand default)
    }

    #[test]
    fn weapon_slots_follow_the_hand() {
        assert_eq!(slot_for_wep_type(67), SLOT_WEAPON_LEFT_1);
        assert_eq!(slot_for_wep_type(1), SLOT_WEAPON_RIGHT_1);
    }

    #[test]
    fn protector_categories_map_to_the_four_armour_slots() {
        assert_eq!(slot_for_protector_category(0), Some(SLOT_PROTECTOR_HEAD));
        assert_eq!(slot_for_protector_category(1), Some(SLOT_PROTECTOR_CHEST));
        assert_eq!(slot_for_protector_category(2), Some(SLOT_PROTECTOR_HANDS));
        assert_eq!(slot_for_protector_category(3), Some(SLOT_PROTECTOR_LEGS));
    }

    /// Category 4 is the 41-row dummy block (ids 1000..2100, no name, no equip flags). Equipping
    /// one would put a nameless placeholder on the player.
    #[test]
    fn dummy_protector_category_is_refused() {
        assert_eq!(slot_for_protector_category(4), None);
        assert_eq!(slot_for_protector_category(255), None);
    }

    /// The id convention `(id / 100) % 10` is NOT a substitute for the param field: it disagrees
    /// on 44 vanilla rows. 1000 is one of them -- convention says head, the game says dummy.
    #[test]
    fn id_convention_is_not_the_slot_source() {
        let conventional = (1000i32 / 100) % 10; // == 0, i.e. "head"
        assert_eq!(conventional, 0);
        assert_eq!(slot_for_protector_category(4), None); // the game's answer for row 1000
    }
}
