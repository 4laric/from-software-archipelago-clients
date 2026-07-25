//! `check_neutralise` — how a check's vanilla lot slot may be suppressed WITHOUT killing the check.
//!
//! ## The invariant, learned the hard way (2026-07-24 playtest)
//!
//! Suppressing the vanilla ware at a check and DETECTING that check are the same act: detection is
//! the pickup's acquisition flag firing. So any neutralisation that removes the pickup also removes
//! the check. There is no popup-vs-registration trade to make here — an emptied slot loses both.
//!
//! `check_lots.rs` got this right for GOODS slots (repointed to the 8852 placeholder: a real,
//! pickable item, flag fires, check registers) and wrong for NON-GOODS, which were zeroed
//! (`id = 0, num = 0`) on the stated belief that "the acquisition flag still fires on the emptied
//! pickup". It does not. Weapons, armour, talismans and Ashes of War are all non-goods, so this
//! silently killed every gear chest, every scarab Ash-of-War drop, and every boss drop —
//! Leonine Misbegotten's check (flag 510800) never fired in a four-hour session, and it was
//! carrying progression.
//!
//! The same file's module doc had already argued the correct position for goods — "the popup is
//! cosmetic; check registration is not" — and the non-goods loop asserted the opposite two hundred
//! lines later. Neither claim had a test. This module is that test.

/// What to do with one check's vanilla lot slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Repoint the slot at the AP goods placeholder: vanilla ware suppressed, pickup preserved.
    RepointToPlaceholder,
    /// Leave the vanilla ware in place. The player may receive a duplicate vanilla item (the
    /// known non-goods leak), but the pickup — and therefore the CHECK — still works.
    LeaveVanilla,
}

/// Choose a neutralisation for a slot.
///
/// `slot_is_goods` — a goods slot can hold the goods placeholder directly.
/// `can_write_slot_category` — whether the caller can also write the slot's CATEGORY field. A lot
/// slot's item id is only meaningful alongside its category, so a non-goods slot can only be
/// repointed at a goods placeholder if the category travels with it.
///
/// Note what is NOT in the return type: there is no "empty it" option, deliberately. Emptying is
/// how the check dies, so it must not be expressible.
pub fn plan(slot_is_goods: bool, can_write_slot_category: bool) -> Plan {
    if slot_is_goods || can_write_slot_category {
        Plan::RepointToPlaceholder
    } else {
        // Suppression loses to detection. A duplicate vanilla item is cosmetic; a check that never
        // registers can block the seed.
        Plan::LeaveVanilla
    }
}

/// `lotItemCategory` value meaning **Goods** — the category the AP placeholder (an
/// `EquipParamGoods` row) needs in whatever slot it lands in.
///
/// Source, not a guess: the apworld derives `lotItemCategory -> FullID top nibble` by VOTING it out
/// of `ItemLotParam` x `ITEM_CATALOG` (`tools/gen_check_lots_table.py`) and records the result in
/// `greenfield/gen_data.py:_LOT_CAT = {"0": 4, "1": 4, "2": 0, "3": 1, "4": 2, "5": 8, "6": 4}`,
/// where nibble 4 is goods. Category 1 is the goods category proper (`gen_data.py`, the enemy-drop
/// reroll: "we reroll only the GOODS slots (lotItemCategory 1 -> FullID nibble 4 ... NOT runes,
/// which was my first guess)"). 0 and 6 also carry the goods nibble (Golden Runes / Gravel Stone;
/// spells) but are not what a fresh placeholder write should claim to be.
pub const LOT_CATEGORY_GOODS: i32 = 1;

/// What to actually write into a lot slot to carry out a [`Plan`].
///
/// `category: None` means "leave the slot's existing category alone" — correct for a slot that is
/// already goods, where the id alone is meaningful.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotWrite {
    pub item_id: i32,
    pub category: Option<i32>,
}

/// The WRITE a caller should perform for one slot. `None` = touch nothing (`Plan::LeaveVanilla`).
///
/// This exists because the decision and the write had drifted apart in production: the client
/// branched on `Plan::RepointToPlaceholder` and then called `zero_slot()` inside that branch, so
/// flipping `CAN_WRITE_SLOT_CATEGORY` to `true` would have re-emptied every non-goods check slot —
/// reintroducing the exact dead-check bug `eee9b1b` fixed, with a green predicate sitting right
/// next to it saying otherwise. A predicate whose production caller does something else is not a
/// fix (CONTRIBUTING: "a green predicate with no production caller is not a fix — it is a spec").
/// So the write itself is now the predicate's return value and there is nothing left to disagree
/// with.
pub fn slot_write(
    slot_is_goods: bool,
    can_write_slot_category: bool,
    placeholder_id: i32,
) -> Option<SlotWrite> {
    match plan(slot_is_goods, can_write_slot_category) {
        Plan::LeaveVanilla => None,
        Plan::RepointToPlaceholder => Some(SlotWrite {
            item_id: placeholder_id,
            category: if slot_is_goods {
                None
            } else {
                Some(LOT_CATEGORY_GOODS)
            },
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_goods_slot_takes_the_placeholder() {
        assert_eq!(plan(true, false), Plan::RepointToPlaceholder);
        assert_eq!(plan(true, true), Plan::RepointToPlaceholder);
    }

    #[test]
    fn a_non_goods_slot_keeps_its_pickup_until_the_category_can_travel_with_it() {
        // THE BUG: this case used to empty the slot, which killed the check outright.
        assert_eq!(
            plan(false, false),
            Plan::LeaveVanilla,
            "no category write => suppression must yield to detection, never empty the slot"
        );
        assert_eq!(
            plan(false, true),
            Plan::RepointToPlaceholder,
            "once the category can be written, non-goods suppress like goods"
        );
    }

    #[test]
    fn no_input_ever_empties_a_slot() {
        // The invariant, stated as a test: whatever we decide, the pickup survives -- because the
        // pickup IS the check. (Enforced by the type today; asserted so a future variant cannot
        // quietly reintroduce the 2026-07-24 dead-check class.)
        for goods in [false, true] {
            for cat in [false, true] {
                let p = plan(goods, cat);
                assert!(
                    matches!(p, Plan::RepointToPlaceholder | Plan::LeaveVanilla),
                    "every plan must leave something to pick up"
                );
            }
        }
    }
    #[test]
    fn repointing_never_writes_an_empty_slot() {
        // THE REGRESSION THIS FILE EXISTS FOR, now stated over the WRITE and not just the plan.
        // check_lots.rs branched on RepointToPlaceholder and called zero_slot() inside it, so the
        // flag flip would have emptied every non-goods check slot -- no pickup, no acquisition
        // flag, no check. An id of 0 must be unreachable from any input.
        for goods in [false, true] {
            for cat in [false, true] {
                if let Some(w) = slot_write(goods, cat, 8852) {
                    assert_ne!(w.item_id, 0, "a repoint must never write an empty item id");
                    assert_eq!(w.item_id, 8852);
                }
            }
        }
    }

    #[test]
    fn a_non_goods_repoint_carries_the_goods_category_with_the_id() {
        // A lot slot's item id is only meaningful alongside its category: the placeholder is an
        // EquipParamGoods row, so a non-goods slot must be told so in the same write.
        let w = slot_write(false, true, 8852).expect("category write available => repoint");
        assert_eq!(w.category, Some(LOT_CATEGORY_GOODS));
        // A goods slot is already goods; rewriting its category would be a no-op at best.
        let g = slot_write(true, true, 8852).expect("goods always repoints");
        assert_eq!(g.category, None);
    }

    #[test]
    fn no_category_write_means_no_write_at_all_for_non_goods() {
        assert_eq!(slot_write(false, false, 8852), None,
                   "without a category write the vanilla ware STAYS -- a duplicate beats a dead check");
    }
}
