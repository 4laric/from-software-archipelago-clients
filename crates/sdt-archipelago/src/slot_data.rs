use std::{collections::HashMap, hash::Hash, str::FromStr};

use sekiro::sprj::ItemId;
use serde::Deserialize;

/// The slot data supplied by the Archipelago server which provides specific
/// information about how to set up this game.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotData {
    /// A map from Archipelago's item IDs to DS3's.
    pub ap_ids_to_item_ids: HashMap<I64Key, DeserializableItemId>,

    /// A map from Archipelago's item IDs to the number of instances of that
    /// item the given ID should grant.
    pub item_counts: HashMap<I64Key, u32>,
}

#[derive(Debug, Deserialize, Hash, PartialEq, Eq)]
#[serde(try_from = "&str")]
#[repr(transparent)]
pub struct I64Key(pub i64);

impl TryFrom<&str> for I64Key {
    type Error = <i64 as FromStr>::Err;

    fn try_from(value: &str) -> Result<I64Key, Self::Error> {
        value.parse()
    }
}

impl FromStr for I64Key {
    type Err = <i64 as FromStr>::Err;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(I64Key(i64::from_str(value)?))
    }
}

#[cfg(test)]
mod key_tests {
    use super::*;

    #[test]
    fn old_map_keys_and_try_from_keep_the_same_integer_parser() {
        let old: HashMap<I64Key, u32> =
            json::from_str(r#"{"-9223372036854775808":1,"9223372036854775807":2}"#).unwrap();
        assert_eq!(old.get(&I64Key(i64::MIN)), Some(&1));
        assert_eq!(old.get(&I64Key(i64::MAX)), Some(&2));
        for value in [
            "0",
            "-1",
            "+42",
            "9223372036854775807",
            "",
            " 1",
            "9223372036854775808",
        ] {
            let expected = value.parse::<i64>().map(I64Key);
            assert_eq!(I64Key::try_from(value), expected);
            assert_eq!(value.parse::<I64Key>(), expected);
        }
    }
}

/// A deserializable wrapper over [ItemId].
#[derive(Debug, Deserialize)]
#[serde(try_from = "u32")]
#[repr(transparent)]
pub struct DeserializableItemId(pub ItemId);

impl TryFrom<u32> for DeserializableItemId {
    type Error = <ItemId as TryFrom<u32>>::Error;

    fn try_from(value: u32) -> Result<DeserializableItemId, Self::Error> {
        Ok(DeserializableItemId(value.try_into()?))
    }
}
