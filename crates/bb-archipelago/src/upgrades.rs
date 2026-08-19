//! Pure Bloodborne auto-upgrade policy.
//!
//! Runtime code is responsible for reading and applying reinforcement state.
//! This module only decides the level, keeping game-memory details out of the
//! replayable client policy.

pub const MAX_WEAPON_LEVEL: u8 = 10;

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
}
