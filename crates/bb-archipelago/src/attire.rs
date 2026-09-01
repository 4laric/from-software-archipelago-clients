//! Closed-world Bloodborne attire catalog accepted by native delivery.
//!
//! These 68 rows mirror the reviewed four-piece sets published by the world in
//! `worlds/bloodborne/starting_attire_catalog.tsv`. Category 1 reaches a
//! distinct native allocator, so arbitrary protector ids remain forbidden.

use crate::config::FeedEffectBinding;

const SET_BASES: [u32; 17] = [
    10_000, 70_000, 80_000, 100_000, 110_000, 120_000, 130_000, 140_000, 150_000, 180_000, 210_000,
    220_000, 230_000, 350_000, 370_000, 380_000, 400_000,
];

/// Returns the only receive policy allowed for a reviewed protector id.
pub fn receive_policy(protector_id: u32) -> Option<FeedEffectBinding> {
    let base = protector_id - (protector_id % 10_000);
    if !SET_BASES.contains(&base) {
        return None;
    }
    match protector_id - base {
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
    fn reviewed_catalog_is_exactly_seventeen_complete_sets() {
        let accepted = SET_BASES
            .into_iter()
            .flat_map(|base| [base, base + 1_000, base + 2_000, base + 3_000])
            .filter(|id| receive_policy(*id).is_some())
            .count();
        assert_eq!(accepted, 68);
    }

    #[test]
    fn refuses_nearby_and_unreviewed_rows() {
        assert_eq!(receive_policy(11_000), Some(FeedEffectBinding::AttireChest));
        assert_eq!(receive_policy(10_001), None);
        assert_eq!(receive_policy(90_000), None);
        assert_eq!(receive_policy(404_000), None);
    }
}
