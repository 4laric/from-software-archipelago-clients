use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const TEST_PEBBLE_EVENT_FLAG: u32 = 52_100_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocationBinding {
    pub ap_location_id: i64,
    pub event_flag: u32,
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
        });
        self
    }
}
