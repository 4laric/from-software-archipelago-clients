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
}
