//! Closed-world Bloodborne attire catalog accepted by native delivery.
//!
//! Category 1 reaches a distinct native allocator, so arbitrary protector ids
//! remain forbidden: only ids on this list are delivered. The list is the
//! world's reviewed attire catalog (`worlds/bloodborne/starting_attire_catalog.tsv`
//! plus `attire_additions.tsv`), and every id on it was checked against
//! `EquipParamProtector` in CUSA03173 01.09 on 2026-09-03: the row exists and
//! the slot the game records for it matches the offset rule below (head +0,
//! chest +1000, arms +2000, legs +3000). One catalog row failed that check,
//! protector 292000 ("Surgical Long Gloves (White)"), which has no row in the
//! game at all; it is not on this list and the world drops it too.

use crate::config::FeedEffectBinding;

/// Every reviewed protector id, ascending. 17 four-piece sets (68) from the
/// original catalog plus 58 pieces from the expanded one.
const REVIEWED_PROTECTORS: [u32; 126] = [
    10_000, 11_000, 12_000, 13_000, 20_000, 21_000, 22_000, 23_000, 30_000, 31_000, 32_000, 33_000,
    40_000, 41_000, 42_000, 43_000, 50_000, 51_000, 52_000, 53_000, 60_000, 61_000, 62_000, 63_000,
    70_000, 71_000, 72_000, 73_000, 80_000, 81_000, 82_000, 83_000, 100_000, 101_000, 102_000,
    103_000, 110_000, 111_000, 112_000, 113_000, 120_000, 121_000, 122_000, 123_000, 130_000,
    131_000, 132_000, 133_000, 140_000, 141_000, 142_000, 143_000, 150_000, 151_000, 152_000,
    153_000, 180_000, 181_000, 182_000, 183_000, 190_000, 191_000, 193_000, 200_000, 201_000,
    203_000, 210_000, 211_000, 212_000, 213_000, 220_000, 221_000, 222_000, 223_000, 230_000,
    231_000, 232_000, 233_000, 241_000, 242_000, 243_000, 250_000, 260_000, 270_000, 280_000,
    281_000, 290_000, 291_000, 293_000, 301_000, 311_000, 313_000, 320_000, 321_000, 330_000,
    331_000, 332_000, 333_000, 340_000, 341_000, 342_000, 343_000, 350_000, 351_000, 352_000,
    353_000, 360_000, 361_000, 363_000, 370_000, 371_000, 372_000, 373_000, 380_000, 381_000,
    382_000, 383_000, 390_000, 391_000, 392_000, 393_000, 400_000, 401_000, 402_000, 403_000,
    430_000,
];

/// Returns the only receive policy allowed for a reviewed protector id.
pub fn receive_policy(protector_id: u32) -> Option<FeedEffectBinding> {
    if REVIEWED_PROTECTORS.binary_search(&protector_id).is_err() {
        return None;
    }
    match protector_id % 10_000 {
        0 => Some(FeedEffectBinding::AttireHead),
        1_000 => Some(FeedEffectBinding::AttireChest),
        2_000 => Some(FeedEffectBinding::AttireHands),
        3_000 => Some(FeedEffectBinding::AttireLegs),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_catalog_is_sorted_unique_and_slot_shaped() {
        assert!(REVIEWED_PROTECTORS.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(REVIEWED_PROTECTORS.len(), 126);
        for id in REVIEWED_PROTECTORS {
            assert!(receive_policy(id).is_some(), "{id} has no slot policy");
        }
        // The original 17 complete sets are all still accepted.
        for base in [
            10_000, 70_000, 80_000, 100_000, 110_000, 120_000, 130_000, 140_000, 150_000, 180_000,
            210_000, 220_000, 230_000, 350_000, 370_000, 380_000, 400_000,
        ] {
            for offset in [0, 1_000, 2_000, 3_000] {
                assert!(receive_policy(base + offset).is_some(), "{}", base + offset);
            }
        }
    }

    #[test]
    fn refuses_nearby_unreviewed_and_phantom_rows() {
        assert_eq!(receive_policy(11_000), Some(FeedEffectBinding::AttireChest));
        assert_eq!(receive_policy(22_000), Some(FeedEffectBinding::AttireHands));
        assert_eq!(receive_policy(10_001), None);
        assert_eq!(receive_policy(90_000), None);
        assert_eq!(receive_policy(404_000), None);
        // No row in EquipParamProtector; the catalog row was a phantom.
        assert_eq!(receive_policy(292_000), None);
        // Partial sets stay partial: the White Church set has no arms row.
        assert_eq!(
            receive_policy(291_000),
            Some(FeedEffectBinding::AttireChest)
        );
        assert_eq!(receive_policy(293_000), Some(FeedEffectBinding::AttireLegs));
    }
}
