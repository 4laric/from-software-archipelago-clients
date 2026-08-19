use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::RUNTIME_BUILD;
use crate::feed::{AttireSlot, EquipClass, FeedEffect};

pub const TEST_PEBBLE_EVENT_FLAG: u32 = 52_100_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocationBinding {
    pub ap_location_id: i64,
    pub event_flag: u32,
    #[serde(default)]
    pub vanilla_award_suppressed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GoodsBinding {
    pub normalized_item_id: u32,
    #[serde(default = "one")]
    pub quantity: u32,
    /// `Some(level)` marks an upgradeable weapon and records the level carried
    /// by the received item. `None` keeps existing category-4 goods behavior.
    #[serde(default)]
    pub reinforcement_level: Option<u8>,
    #[serde(default)]
    pub feed_effect: FeedEffectBinding,
}

const fn one() -> u32 {
    1
}

const fn default_location_check_debounce() -> u8 {
    3
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedEffectBinding {
    RightHandWeapon,
    LeftHandWeapon,
    AttireHead,
    AttireChest,
    AttireHands,
    AttireLegs,
    CaryllRune,
    OathRune,
    RuneWorkshopTool,
    #[default]
    NotEquippable,
}

impl FeedEffectBinding {
    pub fn effect(self) -> FeedEffect {
        let class = match self {
            Self::RightHandWeapon => EquipClass::RightHandWeapon,
            Self::LeftHandWeapon => EquipClass::LeftHandWeapon,
            Self::AttireHead => EquipClass::Attire(AttireSlot::Head),
            Self::AttireChest => EquipClass::Attire(AttireSlot::Chest),
            Self::AttireHands => EquipClass::Attire(AttireSlot::Hands),
            Self::AttireLegs => EquipClass::Attire(AttireSlot::Legs),
            Self::CaryllRune => EquipClass::CaryllRune,
            Self::OathRune => EquipClass::OathRune,
            Self::RuneWorkshopTool => return FeedEffect::RuneWorkshopTool,
            Self::NotEquippable => EquipClass::NotEquippable,
        };
        FeedEffect::Item(class)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub bridge_root: PathBuf,
    /// shadPS4's current log, used only to discover the launch-relative eboot base.
    #[serde(default)]
    pub shad_log: Option<PathBuf>,
    #[serde(default)]
    pub locations: Vec<LocationBinding>,
    #[serde(default)]
    pub items: HashMap<i64, GoodsBinding>,
    #[serde(default)]
    pub auto_upgrade: bool,
    #[serde(default)]
    pub auto_equip: bool,
    /// Live location checks remain disarmed until the backend can prove that
    /// it is reading the save the player explicitly bound to this config.
    #[serde(default)]
    pub expected_save_identity: Option<String>,
    #[serde(default = "default_location_check_debounce")]
    pub location_check_debounce: u8,
    #[serde(default)]
    pub mock_set_flags: Vec<u32>,
}

impl RuntimeConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn with_test_pebble_location(mut self, ap_location_id: i64) -> Self {
        self.locations.push(LocationBinding {
            ap_location_id,
            event_flag: TEST_PEBBLE_EVENT_FLAG,
            vanilla_award_suppressed: false,
        });
        self
    }

    /// Replace seed-owned bindings with the tables emitted by the apworld.
    ///
    /// An older seed that has neither key keeps the local config as a migration
    /// fallback. Once either key is present, malformed rows are fatal: silently
    /// mixing two seed contracts is worse than refusing to arm.
    pub fn apply_slot_data(mut self, slot_data: &json::Value) -> Result<Self> {
        if let Some(build) = slot_data.get("runtime_build") {
            anyhow::ensure!(
                build.as_str() == Some(RUNTIME_BUILD),
                "seed runtime build mismatch: client expects {RUNTIME_BUILD}, seed supplied {}",
                build
            );
        }
        if let Some(value) = slot_data.get("runtime_locations") {
            let rows: HashMap<String, SlotLocationBinding> =
                json::from_value(value.clone()).context("parsing slot_data.runtime_locations")?;
            let mut locations = Vec::with_capacity(rows.len());
            for (raw_id, row) in rows {
                locations.push(LocationBinding {
                    ap_location_id: raw_id
                        .parse()
                        .with_context(|| format!("invalid AP location id {raw_id:?}"))?,
                    event_flag: row.event_flag,
                    vanilla_award_suppressed: row.vanilla_award_suppressed,
                });
            }
            locations.sort_by_key(|row| row.ap_location_id);
            self.locations = locations;
        }
        if let Some(value) = slot_data.get("runtime_items") {
            let rows: HashMap<String, GoodsBinding> =
                json::from_value(value.clone()).context("parsing slot_data.runtime_items")?;
            let mut items = HashMap::with_capacity(rows.len());
            for (raw_id, row) in rows {
                items.insert(
                    raw_id
                        .parse()
                        .with_context(|| format!("invalid AP item id {raw_id:?}"))?,
                    row,
                );
            }
            self.items = items;
        }
        if let Some(value) = slot_data.get("auto_upgrade") {
            self.auto_upgrade = value
                .as_bool()
                .context("slot_data.auto_upgrade must be a boolean")?;
        }
        if let Some(value) = slot_data.get("auto_equip") {
            self.auto_equip = value
                .as_bool()
                .context("slot_data.auto_equip must be a boolean")?;
        }
        anyhow::ensure!(
            self.location_check_debounce >= 2,
            "location_check_debounce must be at least 2"
        );
        Ok(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
struct SlotLocationBinding {
    event_flag: u32,
    #[serde(default)]
    vanilla_award_suppressed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use json::json;

    fn local() -> RuntimeConfig {
        RuntimeConfig {
            bridge_root: PathBuf::from("bridge"),
            shad_log: None,
            locations: vec![LocationBinding {
                ap_location_id: 1,
                event_flag: 2,
                vanilla_award_suppressed: false,
            }],
            items: HashMap::new(),
            auto_upgrade: false,
            auto_equip: false,
            expected_save_identity: Some("mock-save".into()),
            location_check_debounce: 3,
            mock_set_flags: vec![],
        }
    }

    #[test]
    fn slot_data_replaces_seed_owned_tables() {
        let config = local()
            .apply_slot_data(&json!({
                "runtime_locations": {
                    "12259363": {"event_flag": 52410800, "vanilla_award_suppressed": false}
                },
                "runtime_items": {
                    "12255488": {
                        "normalized_item_id": 1073742824,
                        "quantity": 1,
                        "reinforcement_level": 0,
                        "feed_effect": "right_hand_weapon"
                    }
                },
                "auto_upgrade": true,
                "auto_equip": true
            }))
            .unwrap();
        assert_eq!(config.locations.len(), 1);
        assert_eq!(config.locations[0].ap_location_id, 12_259_363);
        assert_eq!(config.locations[0].event_flag, 52_410_800);
        assert_eq!(config.items[&12_255_488].normalized_item_id, 0x4000_03E8);
        assert_eq!(config.items[&12_255_488].reinforcement_level, Some(0));
        assert_eq!(
            config.items[&12_255_488].feed_effect,
            FeedEffectBinding::RightHandWeapon
        );
        assert!(config.auto_upgrade);
        assert!(config.auto_equip);
    }

    #[test]
    fn older_slot_data_keeps_the_local_migration_config() {
        assert_eq!(
            local(),
            local().apply_slot_data(&json!({"version": 1})).unwrap()
        );
    }

    #[test]
    fn malformed_present_tables_fail_closed() {
        let error = local()
            .apply_slot_data(&json!({"runtime_locations": {"not-an-id": {"event_flag": 1}}}))
            .unwrap_err();
        assert!(format!("{error:#}").contains("invalid AP location id"));
    }

    #[test]
    fn a_present_runtime_build_must_match_exactly() {
        local()
            .apply_slot_data(&json!({"runtime_build": RUNTIME_BUILD}))
            .unwrap();
        let error = local()
            .apply_slot_data(&json!({"runtime_build": "bb-0.1.0-r2"}))
            .unwrap_err();
        assert!(format!("{error:#}").contains("seed runtime build mismatch"));
    }

    #[test]
    fn slot_options_replace_local_policy_toggles() {
        let config = local()
            .apply_slot_data(&json!({"auto_upgrade": true, "auto_equip": true}))
            .unwrap();
        assert!(config.auto_upgrade);
        assert!(config.auto_equip);
    }

    #[test]
    fn feed_metadata_maps_to_the_pure_policy_types() {
        assert_eq!(
            FeedEffectBinding::AttireChest.effect(),
            FeedEffect::Item(EquipClass::Attire(AttireSlot::Chest))
        );
        assert_eq!(
            FeedEffectBinding::RuneWorkshopTool.effect(),
            FeedEffect::RuneWorkshopTool
        );
    }
}
