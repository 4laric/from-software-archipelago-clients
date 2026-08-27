//! Pure Bloodborne auto-upgrade policy.
//!
//! Runtime code is responsible for reading and applying reinforcement state.
//! This module only decides the level, keeping game-memory details out of the
//! replayable client policy.

pub const MAX_WEAPON_LEVEL: u8 = 10;
pub const REINFORCEMENT_ROW_STEP: u32 = 100;

/// Raise a received weapon to the player's target level without ever lowering
/// it or exceeding Bloodborne's +10 reinforcement cap.
pub fn auto_upgrade_level(enabled: bool, received_level: u8, target_level: Option<u8>) -> u8 {
    let received = received_level.min(MAX_WEAPON_LEVEL);
    if !enabled {
        return received;
    }
    let Some(target) = target_level else {
        return received;
    };
    received.max(target.min(MAX_WEAPON_LEVEL))
}

/// Move the descriptor pair from its received reinforcement row to the
/// selected row. Bloodborne's CUSA03173 01.09 EquipParamWeapon families use a
/// stride of 100 between +0..+10 rows (live-observed with Ludwig's Holy Blade
/// +1: 8,100,000 -> 8,100,100).
pub fn reinforced_descriptor_pair(
    raw: u32,
    normalized: u32,
    received_level: u8,
    delivered_level: u8,
) -> Option<(u32, u32)> {
    let levels = delivered_level.checked_sub(received_level)?;
    let delta = u32::from(levels).checked_mul(REINFORCEMENT_ROW_STEP)?;
    Some((raw.checked_add(delta)?, normalized.checked_add(delta)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raises_to_target() {
        assert_eq!(auto_upgrade_level(true, 0, Some(6)), 6);
    }

    #[test]
    fn never_lowers_an_upgraded_receive() {
        assert_eq!(auto_upgrade_level(true, 8, Some(4)), 8);
    }

    #[test]
    fn clamps_to_plus_ten() {
        assert_eq!(auto_upgrade_level(true, 0, Some(99)), 10);
    }

    #[test]
    fn disabled_or_unknown_target_is_identity() {
        assert_eq!(auto_upgrade_level(false, 3, Some(8)), 3);
        assert_eq!(auto_upgrade_level(true, 3, None), 3);
    }

    #[test]
    fn descriptor_pair_moves_to_the_selected_reinforcement_row() {
        assert_eq!(
            reinforced_descriptor_pair(0x807B_98A0, 8_100_000, 0, 1),
            Some((0x807B_9904, 8_100_100))
        );
        assert_eq!(
            reinforced_descriptor_pair(0x807B_9904, 8_100_100, 1, 1),
            Some((0x807B_9904, 8_100_100))
        );
        assert_eq!(reinforced_descriptor_pair(1, 1, 2, 1), None);
    }
}
