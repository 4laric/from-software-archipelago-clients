use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::RUNTIME_BUILD;

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
}

const fn one() -> u32 {
    1
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
                    "12255488": {"normalized_item_id": 1073742824, "quantity": 1}
                }
            }))
            .unwrap();
        assert_eq!(config.locations.len(), 1);
        assert_eq!(config.locations[0].ap_location_id, 12_259_363);
        assert_eq!(config.locations[0].event_flag, 52_410_800);
        assert_eq!(config.items[&12_255_488].normalized_item_id, 0x4000_03E8);
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
}
