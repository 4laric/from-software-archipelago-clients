//! Great Rune identity across the two goods-row families the game exposes.

pub const BOSS_DROP_FIRST: i32 = 8148;
pub const RESTORED_FIRST: i32 = 191;
pub const GREAT_RUNE_COUNT: i32 = 6;

/// The restored/equipped row corresponding to an AP-received boss-drop row.
pub fn restored_row_for_received(received_row: i32) -> Option<i32> {
    let offset = received_row - BOSS_DROP_FIRST;
    (0..GREAT_RUNE_COUNT)
        .contains(&offset)
        .then_some(RESTORED_FIRST + offset)
}

/// Whether an observed goods row satisfies the boss-drop row carried by the AP receive stream.
///
/// Great Runes have two equivalent row families: the boss-drop rows AP sends (8148-8153) and the
/// restored/usable rows the game may expose after acquisition (191-196). The equivalence applies
/// wherever possession is observed -- bag, storage, or the Great Rune equip slot. Non-rune goods
/// retain exact-row identity.
pub fn possession_row_satisfies(received_row: i32, observed_row: i32) -> bool {
    observed_row == received_row || restored_row_for_received(received_row) == Some(observed_row)
}

/// Backwards-compatible spelling for the first consumer of the row equivalence.
pub fn equipped_row_satisfies(received_row: i32, equipped_row: i32) -> bool {
    possession_row_satisfies(received_row, equipped_row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_six_boss_drop_rows_map_to_their_restored_rows() {
        assert_eq!(
            (BOSS_DROP_FIRST..BOSS_DROP_FIRST + GREAT_RUNE_COUNT)
                .map(restored_row_for_received)
                .collect::<Vec<_>>(),
            (RESTORED_FIRST..RESTORED_FIRST + GREAT_RUNE_COUNT)
                .map(Some)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn neighbours_and_unrelated_goods_do_not_alias_a_rune() {
        for row in [190, 197, 8147, 8154, 10020] {
            assert_eq!(restored_row_for_received(row), None);
        }
        assert!(!equipped_row_satisfies(8150, 194));
        assert!(equipped_row_satisfies(8150, 193));
    }

    #[test]
    fn restored_rows_satisfy_possession_in_every_store() {
        for offset in 0..GREAT_RUNE_COUNT {
            let received = BOSS_DROP_FIRST + offset;
            let restored = RESTORED_FIRST + offset;
            assert!(possession_row_satisfies(received, received));
            assert!(possession_row_satisfies(received, restored));
        }

        assert!(possession_row_satisfies(9000, 9000));
        assert!(!possession_row_satisfies(9000, 191));
    }
}
