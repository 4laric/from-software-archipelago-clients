//! Auto-equip decision logic (pure).
//!
//! When the `auto_equip` option is on, a received WEAPON is equipped into a primary hand slot. The
//! two decisions here are host-testable and hold no game state:
//!   1. [`is_weapon`] -- is a received FullID a weapon (category 0)? Only weapons auto-equip; armor
//!      (Protector), goods, gems etc. are left alone.
//!   2. [`hand_for_wep_type`] -- given the weapon's `EQUIP_PARAM_WEAPON_ST.wep_type`, does it belong
//!      in the LEFT or RIGHT primary hand? Shields go LEFT (that's their only usable hand); every
//!      other weapon class defaults RIGHT (the main-hand slot).
//!
//! The game-side wrapper (`eldenring-archipelago::auto_equip`) reads `wep_type` from the param table
//! and calls the game's `ReplaceTool` fn to place the item; the ChrAsm sync is the game's job.

/// Category nibble for a weapon FullID (`ItemCategory::Weapon = 0`), matching the `(category<<28)|row`
/// encoding used across the client.
const CATEGORY_MASK: u32 = 0xF000_0000;
const CATEGORY_WEAPON: u32 = 0x0000_0000;

/// Which primary hand a received weapon should occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hand {
    Left,
    Right,
}

/// `EQUIP_PARAM_WEAPON_ST.wep_type` values that are LEFT-hand only (shields). ER weaponCategory:
/// 65 = Small Shield, 67 = Medium/Standard Shield, 69 = Greatshield. RUNTIME-UNCONFIRMED against a
/// live equip -- if a shield lands in the right hand the fix is this list, and the failure is benign
/// (the player re-hands it manually; no crash). Catalysts (staff 57 / seal 59) and torches (87) are
/// deliberately NOT here: they're routinely main-handed, so they default RIGHT like any weapon.
const LEFT_HAND_WEP_TYPES: &[u16] = &[65, 67, 69];

/// Is this received FullID a weapon (the only category we auto-equip)?
pub fn is_weapon(full_id: i32) -> bool {
    (full_id as u32) & CATEGORY_MASK == CATEGORY_WEAPON
}

/// The primary hand a weapon of this `wep_type` should occupy. Shields -> LEFT; everything else ->
/// RIGHT (the main-hand slot).
pub fn hand_for_wep_type(wep_type: u16) -> Hand {
    if LEFT_HAND_WEP_TYPES.contains(&wep_type) {
        Hand::Left
    } else {
        Hand::Right
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
    fn shields_go_left_weapons_go_right() {
        assert_eq!(hand_for_wep_type(65), Hand::Left); // small shield
        assert_eq!(hand_for_wep_type(67), Hand::Left); // medium shield
        assert_eq!(hand_for_wep_type(69), Hand::Left); // greatshield
        assert_eq!(hand_for_wep_type(1), Hand::Right); // straight sword
        assert_eq!(hand_for_wep_type(57), Hand::Right); // glintstone staff
        assert_eq!(hand_for_wep_type(59), Hand::Right); // sacred seal
        assert_eq!(hand_for_wep_type(87), Hand::Right); // torch (main-hand default)
    }
}
