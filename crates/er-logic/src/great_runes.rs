//! Great Rune identity across the two goods-row families the game exposes.

use std::collections::HashMap;

pub const BOSS_DROP_FIRST: i32 = 8148;
pub const RESTORED_FIRST: i32 = 191;
pub const GREAT_RUNE_COUNT: i32 = 6;

const CATEGORY_GOODS: i64 = 0x4000_0000;
const CATEGORY_MASK: i64 = 0xF000_0000;
const ROW_MASK: i64 = 0x0FFF_FFFF;

/// The restored/equipped row corresponding to an AP-received boss-drop row.
pub fn restored_row_for_received(received_row: i32) -> Option<i32> {
    let offset = received_row - BOSS_DROP_FIRST;
    (0..GREAT_RUNE_COUNT)
        .contains(&offset)
        .then_some(RESTORED_FIRST + offset)
}

/// Rewrite the seed's AP-item delivery map to the equippable Great Rune rows.
///
/// The apworld deliberately identifies shardbearer rewards by the boss-drop rows (`8148..=8153`).
/// Those are legitimate inventory rows, but setting the restored flag does not convert them into
/// the rows (`191..=196`) consumed by the Great Rune equip menu. Normalising the map once makes
/// every delivery consumer agree: the direct receive path, reconciliation, and native shop
/// repoints all hand out the restored row. Unrelated goods and every non-goods category retain
/// their exact FullID.
pub fn normalize_delivery_item_map(item_map: &mut HashMap<i64, i64>) {
    for full_id in item_map.values_mut() {
        if *full_id & CATEGORY_MASK != CATEGORY_GOODS {
            continue;
        }
        let row = (*full_id & ROW_MASK) as i32;
        if let Some(restored) = restored_row_for_received(row) {
            *full_id = CATEGORY_GOODS | i64::from(restored);
        }
    }
}

/// Canonical restored/equippable identity for either row in a shardbearer Great Rune pair.
pub fn canonical_restored_row(row: i32) -> Option<i32> {
    restored_row_for_received(row).or_else(|| {
        (RESTORED_FIRST..RESTORED_FIRST + GREAT_RUNE_COUNT)
            .contains(&row)
            .then_some(row)
    })
}

/// Whether an observed goods row satisfies the desired Great Rune row.
///
/// Great Runes have two equivalent row families: boss-drop (`8148..=8153`) and restored/usable
/// (`191..=196`). Delivery now targets the restored row, but legacy saves may contain either one;
/// equivalence therefore has to work in both directions wherever possession is observed -- bag,
/// storage, or the Great Rune equip slot. Non-rune goods retain exact-row identity.
pub fn possession_row_satisfies(desired_row: i32, observed_row: i32) -> bool {
    desired_row == observed_row
        || matches!(
            (
                canonical_restored_row(desired_row),
                canonical_restored_row(observed_row)
            ),
            (Some(desired), Some(observed)) if desired == observed
        )
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
    fn delivery_map_rewrites_all_six_runes_and_nothing_else() {
        const GOODS: i64 = 0x4000_0000;
        let mut map = HashMap::from([
            (1, GOODS | 8148),
            (2, GOODS | 8149),
            (3, GOODS | 8150),
            (4, GOODS | 8151),
            (5, GOODS | 8152),
            (6, GOODS | 8153),
            (7, GOODS | 10080),
            (8, 0x2000_0000 | 8148),
        ]);

        normalize_delivery_item_map(&mut map);

        assert_eq!(
            (1..=6).map(|id| map[&id]).collect::<Vec<_>>(),
            (191..=196).map(|row| GOODS | row).collect::<Vec<_>>()
        );
        assert_eq!(map[&7], GOODS | 10080, "Unborn's unique row is unchanged");
        assert_eq!(map[&8], 0x2000_0000 | 8148, "non-goods ids are unchanged");
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
            let boss_drop = BOSS_DROP_FIRST + offset;
            let restored = RESTORED_FIRST + offset;
            assert!(possession_row_satisfies(boss_drop, boss_drop));
            assert!(possession_row_satisfies(boss_drop, restored));
            assert!(possession_row_satisfies(restored, boss_drop));
            assert!(possession_row_satisfies(restored, restored));
        }

        assert!(possession_row_satisfies(9000, 9000));
        assert!(!possession_row_satisfies(9000, 191));
    }
}
