//! Pure, replayable decisions derived from the Archipelago received-item feed.
//!
//! Slot selection must not depend on the live loadout. The server replays the
//! complete received stream on reconnect, so a live "first empty slot" rule can
//! move equipment every time the client starts. Stream ordinals and progression
//! items are stable inputs and therefore produce a fixed point.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AttireSlot {
    Head,
    Chest,
    Hands,
    Legs,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EquipClass {
    RightHandWeapon,
    LeftHandWeapon,
    Attire(AttireSlot),
    CaryllRune,
    OathRune,
    NotEquippable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedEffect {
    Item(EquipClass),
    RuneWorkshopTool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceivedFact {
    pub index: u64,
    pub effect: FeedEffect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EquipTarget {
    RightHand(usize),
    LeftHand(usize),
    Attire(AttireSlot),
    CaryllRune(usize),
    OathRune,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EquipDecision {
    pub received_index: u64,
    pub ordinal: u64,
    pub target: EquipTarget,
}

const HAND_SLOTS: u64 = 2;
const CARYLL_RUNE_SLOTS: u64 = 3;

/// Reduces a complete received stream to deterministic auto-equip decisions.
///
/// The caller supplies Bloodborne item classification from slot data. Facts
/// must be in AP receive order; rejecting disorder keeps the result independent
/// of map iteration or reconnect timing.
pub fn equip_decisions(
    facts: impl IntoIterator<Item = ReceivedFact>,
) -> Result<Vec<EquipDecision>, FeedOrderError> {
    let mut decisions = Vec::new();
    let mut ordinals: HashMap<EquipClass, u64> = HashMap::new();
    let mut rune_workshop_unlocked = false;
    let mut previous_index = None;

    for fact in facts {
        if previous_index.is_some_and(|previous| fact.index <= previous) {
            return Err(FeedOrderError {
                previous: previous_index.unwrap(),
                current: fact.index,
            });
        }
        previous_index = Some(fact.index);

        let FeedEffect::Item(class) = fact.effect else {
            rune_workshop_unlocked = true;
            continue;
        };
        let ordinal = ordinals.entry(class).or_default();
        let current = *ordinal;
        *ordinal += 1;

        let target = match class {
            EquipClass::RightHandWeapon => EquipTarget::RightHand((current % HAND_SLOTS) as usize),
            EquipClass::LeftHandWeapon => EquipTarget::LeftHand((current % HAND_SLOTS) as usize),
            EquipClass::Attire(slot) => EquipTarget::Attire(slot),
            EquipClass::CaryllRune if rune_workshop_unlocked => {
                EquipTarget::CaryllRune((current % CARYLL_RUNE_SLOTS) as usize)
            }
            EquipClass::CaryllRune | EquipClass::NotEquippable => continue,
            EquipClass::OathRune => EquipTarget::OathRune,
        };
        decisions.push(EquipDecision {
            received_index: fact.index,
            ordinal: current,
            target,
        });
    }
    Ok(decisions)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeedOrderError {
    pub previous: u64,
    pub current: u64,
}

impl std::fmt::Display for FeedOrderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "received feed is not strictly ordered: {} then {}",
            self.previous, self.current
        )
    }
}

impl std::error::Error for FeedOrderError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(index: u64, class: EquipClass) -> ReceivedFact {
        ReceivedFact {
            index,
            effect: FeedEffect::Item(class),
        }
    }

    #[test]
    fn hand_weapons_rotate_from_the_feed_not_live_occupancy() {
        let facts = [
            item(0, EquipClass::RightHandWeapon),
            item(1, EquipClass::RightHandWeapon),
            item(2, EquipClass::RightHandWeapon),
            item(3, EquipClass::LeftHandWeapon),
            item(4, EquipClass::LeftHandWeapon),
        ];
        let decisions = equip_decisions(facts).unwrap();
        assert_eq!(
            decisions.iter().map(|d| d.target).collect::<Vec<_>>(),
            [
                EquipTarget::RightHand(0),
                EquipTarget::RightHand(1),
                EquipTarget::RightHand(0),
                EquipTarget::LeftHand(0),
                EquipTarget::LeftHand(1),
            ]
        );
    }

    #[test]
    fn rune_slots_are_unlocked_by_the_received_feed() {
        let facts = [
            item(0, EquipClass::CaryllRune),
            ReceivedFact {
                index: 1,
                effect: FeedEffect::RuneWorkshopTool,
            },
            item(2, EquipClass::CaryllRune),
            item(3, EquipClass::CaryllRune),
            item(4, EquipClass::CaryllRune),
            item(5, EquipClass::CaryllRune),
        ];
        let decisions = equip_decisions(facts).unwrap();
        assert_eq!(
            decisions.iter().map(|d| d.target).collect::<Vec<_>>(),
            [
                // The pre-tool rune still consumed ordinal zero. Once unlocked,
                // the next stable stream ordinal starts in slot two.
                EquipTarget::CaryllRune(1),
                EquipTarget::CaryllRune(2),
                EquipTarget::CaryllRune(0),
                EquipTarget::CaryllRune(1),
            ]
        );
    }

    #[test]
    fn replay_is_a_fixed_point() {
        let facts = vec![
            item(10, EquipClass::RightHandWeapon),
            ReceivedFact {
                index: 11,
                effect: FeedEffect::RuneWorkshopTool,
            },
            item(12, EquipClass::CaryllRune),
            item(13, EquipClass::CaryllRune),
            item(14, EquipClass::RightHandWeapon),
        ];
        assert_eq!(
            equip_decisions(facts.clone()).unwrap(),
            equip_decisions(facts).unwrap()
        );
    }

    #[test]
    fn rejects_out_of_order_or_duplicate_indices() {
        let error = equip_decisions([
            item(5, EquipClass::RightHandWeapon),
            item(5, EquipClass::LeftHandWeapon),
        ])
        .unwrap_err();
        assert_eq!(
            error,
            FeedOrderError {
                previous: 5,
                current: 5
            }
        );
    }
}
