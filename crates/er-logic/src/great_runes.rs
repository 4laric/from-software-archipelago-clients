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

/// The vanilla Divine-Tower altar for one shardbearer rune: common_func event 90005110's two
/// flag gates, pinned to the six `InitializeCommonEvent` sites in `m34_10/12/13/14/15`.
///
/// The event is `EndIf(EventFlag(restore_flag)); EndIf(!EventFlag(boss_flag)); ...` -- so a rune
/// whose restore flag is already set (the client sets all six every session, #731) or whose
/// shardbearer is still alive shows NO altar prompt. That is client issue #316's "no prompt at the
/// Divine Tower of Caelid", by construction, and the `[reattach] great rune` line prints both
/// halves so a report carries them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TowerGate {
    /// `eventFlagId`: the restore flag the altar sets and exits on (191..=196).
    pub restore_flag: u32,
    /// `eventFlagId2`: the shardbearer's defeat flag the altar requires.
    pub boss_flag: u32,
    /// The lot the altar awards (goods 191..=196 with `AwardItemsIncludingClients`).
    pub restore_lot: u32,
}

/// The altar gates for a boss-drop OR restored row; `None` for anything else (the Great Rune of
/// the Unborn has no altar).
pub fn tower_gate_for(row: i32) -> Option<TowerGate> {
    let restored = canonical_restored_row(row)?;
    let (boss_flag, restore_lot) = match restored {
        191 => (9101, 34100500), // Godrick, m34_10
        192 => (9130, 34130050), // Radahn, m34_13
        193 => (9104, 34140700), // Morgott, m34_14
        194 => (9122, 34120500), // Rykard, m34_12
        195 => (9112, 34140710), // Mohg, m34_14
        196 => (9120, 34150000), // Malenia, m34_15
        _ => return None,
    };
    Some(TowerGate {
        restore_flag: restored as u32,
        boss_flag,
        restore_lot,
    })
}

/// Whether an observed goods row satisfies the desired Great Rune row.
///
/// Great Runes have two equivalent row families: boss-drop (`8148..=8153`) and restored/usable
/// (`191..=196`), and EITHER family satisfies a desire for the other, in both directions
/// (clients#392). Delivery desires the boss-drop row exactly as the seed sends it. Conversely, a
/// restored row observed in the wild (a vanilla Divine-Tower visit on a hybrid save, or a pre-AP
/// save) means the player already has the rune and no boss-row grant is owed. Non-rune goods
/// retain exact-row identity.
///
/// ON "THE RESTORED ROW CANNOT BE GRANTED" (2026-09-06). #392 concluded from Corni's probe
/// (`!give 0x400000c4` / row 196 INERT on every load; `!give 0x40001fd9` / row 8153 works) that
/// AddItem accepts 191..=196 and materialises them nowhere. That conclusion was read through the
/// client's own LEN-BOUNDED key-list walk, which Tako's 2026-09-05/06 logs show cannot see the
/// newest key items after an NPC hand-in (`crate::key_list_window`). Earlier in-play notes (world
/// #682, 2026-08-14) saw client-delivered 191/192 list and equip. So whether a goodsType-15 row
/// lands is UNSETTLED, not refuted; it is the probe gating any delivery change (client #316).
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

    /// The six `InitializeCommonEvent(0, 90005110, ...)` sites in the m34 EMEVD, pinned: restore
    /// flag, boss flag (`eventFlagId2`), restore lot -- and the same answer for either row family.
    #[test]
    fn tower_gates_match_the_six_m34_initializers() {
        let expect = [
            (8148, 191, 9101, 34100500),
            (8149, 192, 9130, 34130050),
            (8150, 193, 9104, 34140700),
            (8151, 194, 9122, 34120500),
            (8152, 195, 9112, 34140710),
            (8153, 196, 9120, 34150000),
        ];
        for (boss_row, restored, boss_flag, lot) in expect {
            let want = TowerGate {
                restore_flag: restored,
                boss_flag,
                restore_lot: lot,
            };
            assert_eq!(tower_gate_for(boss_row), Some(want), "boss row {boss_row}");
            assert_eq!(
                tower_gate_for(restored as i32),
                Some(want),
                "restored row {restored}"
            );
        }
        assert_eq!(tower_gate_for(10080), None, "the Unborn rune has no altar");
        assert_eq!(tower_gate_for(8147), None);
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
