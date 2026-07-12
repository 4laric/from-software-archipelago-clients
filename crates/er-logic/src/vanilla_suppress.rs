//! Vanilla-pickup suppression decision (pure seam).
//!
//! A vanilla item id that belongs to a check location is the check's ORIGINAL ware; its bag-add is
//! suppressed until the check is COLLECTED (reported to the server), after which a genuine re-pickup
//! of a farmable/respawning source passes through. The re-pickup discriminator is the COLLECTED-flag
//! set (the server checked-set, bridged location -> acquisition flag via slot_data `locationFlags`),
//! NOT the live game acquisition flag.
//!
//! Why not the live flag: for shared-flag multi-item lots (armor sets, NPC-corpse bundles, boss
//! remembrance drops -- 224 flags / 605 locations in the ER datapackage) the game sets the single
//! shared acquisition flag AT or BEFORE the bag-add, so a "is the flag set now?" test reads true at
//! AddItem time and passed the vanilla item through as a bogus "re-pickup" -- the leak observed on
//! Traveler's Clothes (item 0x100f90c4, flag 15007980) in the 2026-07-03 playtest.
//!
//! The collected-set is race-safe in the correct direction: a location enters it only on a flag-poll
//! tick STRICTLY AFTER its check was reported, so the FIRST pickup (flags not yet collected) always
//! suppresses, and a genuine re-pickup (flags collected on a prior, separate event) passes.

use std::collections::HashSet;

/// `true` = SUPPRESS the vanilla bag-add (this pickup IS the check itself, not yet collected).
///
/// `mapped_flags` = the picked item id's check acquisition flags (`checkItemFlags[id]`).
/// `collected` = acquisition flags of every location already in the server checked-set.
///
/// Suppress if ANY mapped flag is not yet collected; pass only once EVERY mapped flag is collected.
/// An empty `collected` (no flag-poll yet) therefore suppresses everything -> suppress-by-default
/// never leaks.
pub fn should_suppress(mapped_flags: &[u32], collected: &HashSet<u32>) -> bool {
    mapped_flags.iter().any(|f| !collected.contains(f))
}

#[cfg(test)]
mod tests {
    use super::should_suppress;
    use std::collections::HashSet;

    fn set(flags: &[u32]) -> HashSet<u32> {
        flags.iter().copied().collect()
    }

    #[test]
    fn first_pickup_nothing_collected_suppresses() {
        // No flag-poll has run yet: suppress by default so a first pickup never leaks.
        assert!(should_suppress(&[15007980], &set(&[])));
    }

    #[test]
    fn traveler_clothes_regression() {
        // The exact 2026-07-03 leak: item 0x100f90c4 -> flag 15007980, uncollected at pickup.
        // Old live-flag test PASSED (flag set at/before AddItem); collected-set SUPPRESSES.
        let collected = set(&[]);
        assert!(
            should_suppress(&[15007980], &collected),
            "uncollected check must suppress"
        );

        // After the poll reports it, the same flag is collected -> a re-pickup passes.
        let collected = set(&[15007980]);
        assert!(
            !should_suppress(&[15007980], &collected),
            "collected check must pass on re-pickup"
        );
    }

    #[test]
    fn shared_flag_multi_item_lot_suppresses_before_collection() {
        // Clothes (0x100f90c4) and Manchettes (0x100f9128) are distinct item ids that share one
        // acquisition flag. Each id maps to that same flag; both must suppress on first pickup.
        let shared = 15007980u32;
        let collected = set(&[]);
        assert!(should_suppress(&[shared], &collected)); // clothes id
        assert!(should_suppress(&[shared], &collected)); // manchettes id
    }

    #[test]
    fn all_flags_collected_passes() {
        let collected = set(&[100, 200, 300]);
        assert!(!should_suppress(&[100, 200], &collected));
    }

    #[test]
    fn partial_collection_still_suppresses() {
        // A multi-flag id where only some flags are collected is still an uncollected check.
        let collected = set(&[100]);
        assert!(should_suppress(&[100, 200], &collected));
    }

    #[test]
    fn empty_mapped_flags_passes() {
        // Degenerate: an id with no mapped flags is not a check -> never suppress.
        assert!(!should_suppress(&[], &set(&[])));
    }
}
