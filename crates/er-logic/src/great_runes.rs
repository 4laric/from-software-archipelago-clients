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
/// (`191..=196`), and EITHER family satisfies a desire for the other, in both directions
/// (clients#392). Delivery desires the boss-drop row exactly as the seed sends it: the restored
/// row cannot be granted at all -- AddItem accepts it and materialises it nowhere (Corni probe,
/// 2026-08-22: `!give 0x400000c4` / row 196 INERT on every load; `!give 0x40001fd9` / row 8153
/// works). Conversely, a restored row observed in the wild (a vanilla Divine-Tower visit on a
/// hybrid save, or a pre-AP save) means the player already has the rune and no boss-row grant is
/// owed. Non-rune goods retain exact-row identity.
///
/// HISTORY: this used to be asymmetric -- a restored row satisfied a boss-row desire (client #313)
/// but not the inverse, because delivery rewrote the map to desire the restored row (#316) and a
/// boss-only save was owed one restored-row backfill. That premise died with #392: the rewrite was
/// the bug (it manufactured grants of a row the engine swallows, so the reconciler re-emitted the
/// grant on every load -- Corni's log: 15 INERT grants of row 196 across two loads), and with
/// delivery desiring the boss-drop row the asymmetry has no consumer left.
pub fn possession_row_satisfies(desired_row: i32, observed_row: i32) -> bool {
    desired_row == observed_row
        || match (
            restored_row_for_received(desired_row),
            restored_row_for_received(observed_row),
        ) {
            (Some(a), Some(b)) => a == b,
            (Some(restored), None) => restored == observed_row,
            (None, Some(restored)) => restored == desired_row,
            (None, None) => false,
        }
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
    fn either_family_satisfies_possession_in_every_store() {
        for offset in 0..GREAT_RUNE_COUNT {
            let boss_drop = BOSS_DROP_FIRST + offset;
            let restored = RESTORED_FIRST + offset;
            assert!(possession_row_satisfies(boss_drop, boss_drop));
            assert!(possession_row_satisfies(boss_drop, restored));
            assert!(
                possession_row_satisfies(restored, boss_drop),
                "clients#392: symmetric -- a restored row in the wild means the rune is owned"
            );
            assert!(possession_row_satisfies(restored, restored));
            // Cross-rune aliasing is still forbidden: Radahn's restored row does not satisfy
            // Morgott's boss row.
            let other_restored = RESTORED_FIRST + (offset + 1) % GREAT_RUNE_COUNT;
            assert!(!possession_row_satisfies(boss_drop, other_restored));
        }

        assert!(possession_row_satisfies(9000, 9000));
        assert!(!possession_row_satisfies(9000, 191));
        assert!(!possession_row_satisfies(191, 9000));
    }

    #[test]
    fn neighbours_and_unrelated_goods_do_not_alias_a_rune() {
        for row in [190, 197, 8147, 8154, 10020] {
            assert_eq!(restored_row_for_received(row), None);
        }
        assert!(!equipped_row_satisfies(8150, 194));
        assert!(equipped_row_satisfies(8150, 193));
    }
}
