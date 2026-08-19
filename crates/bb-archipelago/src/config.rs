use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DescriptorEvidence {
    GoodsFormulaObserved,
    LiveGrantInventoryUi,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeItemBinding {
    pub raw_descriptor: u32,
    pub normalized_item_id: u32,
    pub item_category: u8,
    pub descriptor_evidence: DescriptorEvidence,
    #[serde(default = "one")]
    pub quantity: u32,
    /// `Some(level)` marks an upgradeable weapon and records the level carried
    /// by the received item. `None` keeps existing category-4 goods behavior.
    #[serde(default)]
    pub reinforcement_level: Option<u8>,
    #[serde(default)]
    pub feed_effect: FeedEffectBinding,
}

impl RuntimeItemBinding {
    fn validate(&self, ap_item_id: i64) -> Result<()> {
        anyhow::ensure!(
            (1..=99).contains(&self.quantity),
            "AP item {ap_item_id} has invalid grant quantity {}",
            self.quantity
        );
        anyhow::ensure!(
            self.reinforcement_level.is_none_or(|level| level <= 10),
            "AP item {ap_item_id} has invalid reinforcement level {:?}",
            self.reinforcement_level
        );
        match self.item_category {
            4 => {
                anyhow::ensure!(
                    self.descriptor_evidence == DescriptorEvidence::GoodsFormulaObserved,
                    "AP item {ap_item_id} category-4 descriptor lacks observed-goods evidence"
                );
                anyhow::ensure!(
                    self.normalized_item_id & 0xF000_0000 == 0x4000_0000
                        && self.raw_descriptor & 0xF000_0000 == 0xB000_0000
                        && (self.normalized_item_id & 0x0FFF_FFFF)
                            == (self.raw_descriptor & 0x0FFF_FFFF),
                    "AP item {ap_item_id} has an incompatible category-4 raw/normalized descriptor pair"
                );
                anyhow::ensure!(
                    self.reinforcement_level.is_none(),
                    "AP item {ap_item_id} category-4 goods cannot carry a reinforcement level"
                );
            }
            0 => {
                anyhow::ensure!(
                    self.descriptor_evidence == DescriptorEvidence::LiveGrantInventoryUi,
                    "AP item {ap_item_id} category-0 descriptor is not live-validated"
                );
                anyhow::ensure!(
                    self.normalized_item_id & 0xF000_0000 == 0
                        && self.raw_descriptor & 0xF000_0000 == 0x8000_0000
                        && (self.normalized_item_id & 0x0FFF_FFFF)
                            == (self.raw_descriptor & 0x0FFF_FFFF),
                    "AP item {ap_item_id} has an incompatible category-0 raw/normalized descriptor pair"
                );
                anyhow::ensure!(
                    self.quantity == 1,
                    "AP item {ap_item_id} category-0 equipment quantity must be one"
                );
                anyhow::ensure!(
                    self.reinforcement_level.is_some(),
                    "AP item {ap_item_id} category-0 weapon has no reinforcement level"
                );
                anyhow::ensure!(
                    matches!(
                        self.feed_effect,
                        FeedEffectBinding::RightHandWeapon | FeedEffectBinding::LeftHandWeapon
                    ),
                    "AP item {ap_item_id} category-0 weapon has incompatible receive policy {:?}",
                    self.feed_effect
                );
            }
            category => anyhow::bail!(
                "AP item {ap_item_id} uses unsupported Bloodborne item category {category}"
            ),
        }
        Ok(())
    }
}

const fn one() -> u32 {
    1
}

const fn default_location_check_debounce() -> u8 {
    3
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SuppressionRequirement {
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub manifest_format: String,
    #[serde(default)]
    pub plan_sha256: String,
}

#[derive(Debug, Deserialize)]
struct SuppressionManifest {
    format: String,
    plan_sha256: String,
    output_gameparam_sha256: String,
    output_relative_path: String,
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
    pub items: HashMap<i64, RuntimeItemBinding>,
    #[serde(default)]
    pub auto_upgrade: bool,
    #[serde(default)]
    pub auto_equip: bool,
    /// Both live checks and received-item mutation remain disarmed until the
    /// backend proves that it is operating on this explicitly bound save.
    #[serde(default)]
    pub expected_save_identity: Option<String>,
    /// Local build manifest produced by `build_vanilla_suppression.ps1`.
    #[serde(default)]
    pub suppression_manifest: Option<PathBuf>,
    /// The binder actually loaded by the game, not the separate build output.
    #[serde(default)]
    pub installed_gameparam: Option<PathBuf>,
    /// Seed-owned requirement. Local configuration cannot weaken this value.
    #[serde(default)]
    pub suppression: SuppressionRequirement,
    #[serde(default = "default_location_check_debounce")]
    pub location_check_debounce: u8,
    #[serde(default)]
    pub mock_set_flags: Vec<u32>,
    /// AP location whose debounced check completes this bounded world.
    #[serde(default)]
    pub goal_location: Option<i64>,
}

impl RuntimeConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let config: Self =
            json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
        config.validate_items()?;
        Ok(config)
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
            let rows: HashMap<String, RuntimeItemBinding> =
                json::from_value(value.clone()).context("parsing slot_data.runtime_items")?;
            let mut items = HashMap::with_capacity(rows.len());
            for (raw_id, row) in rows {
                let ap_item_id = raw_id
                    .parse()
                    .with_context(|| format!("invalid AP item id {raw_id:?}"))?;
                row.validate(ap_item_id)?;
                items.insert(ap_item_id, row);
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
        if let Some(value) = slot_data.get("suppression") {
            self.suppression =
                json::from_value(value.clone()).context("parsing slot_data.suppression")?;
        }
        if let Some(value) = slot_data.get("goal_location") {
            self.goal_location = Some(
                value
                    .as_i64()
                    .context("slot_data.goal_location must be a signed 64-bit integer")?,
            );
        }
        if let Some(goal) = self.goal_location {
            anyhow::ensure!(
                self.locations
                    .iter()
                    .any(|location| location.ap_location_id == goal),
                "goal location {goal} is absent from runtime_locations"
            );
        }
        let claims_suppression = self
            .locations
            .iter()
            .any(|location| location.vanilla_award_suppressed);
        anyhow::ensure!(
            !claims_suppression || self.suppression.required,
            "seed marks vanilla awards suppressed without requiring an installed binder witness"
        );
        if self.suppression.required {
            anyhow::ensure!(
                !self.suppression.manifest_format.is_empty(),
                "required suppression manifest format is missing"
            );
            require_sha256("seed suppression plan", &self.suppression.plan_sha256)?;
        }
        anyhow::ensure!(
            self.location_check_debounce >= 2,
            "location_check_debounce must be at least 2"
        );
        self.validate_items()?;
        Ok(self)
    }

    fn validate_items(&self) -> Result<()> {
        for (&ap_item_id, binding) in &self.items {
            binding.validate(ap_item_id)?;
        }
        Ok(())
    }

    /// Prove that the binder the seed requires is the binder on disk at the
    /// configured game path. The build manifest alone is not installation
    /// evidence; the installed file is independently hashed here.
    pub fn verify_suppression_install(&self) -> Result<Option<String>> {
        if !self.suppression.required {
            return Ok(None);
        }
        let manifest_path = self
            .suppression_manifest
            .as_deref()
            .context("seed requires vanilla-award suppression; configure suppression_manifest")?;
        let installed_path = self
            .installed_gameparam
            .as_deref()
            .context("seed requires vanilla-award suppression; configure installed_gameparam")?;
        let manifest_bytes = fs::read(manifest_path)
            .with_context(|| format!("reading suppression manifest {}", manifest_path.display()))?;
        let manifest: SuppressionManifest = json::from_slice(&manifest_bytes)
            .with_context(|| format!("parsing suppression manifest {}", manifest_path.display()))?;
        anyhow::ensure!(
            manifest.format == self.suppression.manifest_format,
            "suppression manifest format mismatch: expected {:?}, found {:?}",
            self.suppression.manifest_format,
            manifest.format
        );
        anyhow::ensure!(
            manifest.plan_sha256 == self.suppression.plan_sha256,
            "suppression plan mismatch: seed expects {}, manifest describes {}",
            self.suppression.plan_sha256,
            manifest.plan_sha256
        );
        anyhow::ensure!(
            manifest.output_relative_path.replace('\\', "/")
                == "param/gameparam/gameparam.parambnd.dcx",
            "suppression manifest names unexpected output path {:?}",
            manifest.output_relative_path
        );
        require_sha256(
            "suppression manifest output",
            &manifest.output_gameparam_sha256,
        )?;
        let installed_suffix = installed_path
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        anyhow::ensure!(
            installed_suffix.ends_with("/param/gameparam/gameparam.parambnd.dcx"),
            "installed_gameparam must name the game installation's param/gameparam/gameparam.parambnd.dcx"
        );
        if let Some(build_root) = manifest_path.parent() {
            let build_output = build_root.join("gameparam.parambnd.dcx");
            if build_output.exists() {
                anyhow::ensure!(
                    fs::canonicalize(&build_output)? != fs::canonicalize(installed_path)?,
                    "installed_gameparam points to the separate build artifact, not the game installation"
                );
            }
        }
        let installed_hash = sha256_file(installed_path)?;
        anyhow::ensure!(
            installed_hash == manifest.output_gameparam_sha256,
            "installed gameparam mismatch: expected {}, found {} at {}",
            manifest.output_gameparam_sha256,
            installed_hash,
            installed_path.display()
        );
        Ok(Some(installed_hash))
    }
}

fn require_sha256(label: &str, value: &str) -> Result<()> {
    anyhow::ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{label} SHA-256 must contain exactly 64 hexadecimal characters"
    );
    anyhow::ensure!(
        value.bytes().all(|byte| !byte.is_ascii_uppercase()),
        "{label} SHA-256 must use lowercase hexadecimal"
    );
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("hashing {}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
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
            suppression_manifest: None,
            installed_gameparam: None,
            suppression: SuppressionRequirement::default(),
            location_check_debounce: 3,
            mock_set_flags: vec![],
            goal_location: None,
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
                        "raw_descriptor": 2154583648_u32,
                        "normalized_item_id": 7100000,
                        "item_category": 0,
                        "descriptor_evidence": "live_grant_inventory_ui",
                        "quantity": 1,
                        "reinforcement_level": 0,
                        "feed_effect": "right_hand_weapon"
                    }
                },
                "auto_upgrade": true,
                "auto_equip": true,
                "goal_location": 12259363
            }))
            .unwrap();
        assert_eq!(config.locations.len(), 1);
        assert_eq!(config.locations[0].ap_location_id, 12_259_363);
        assert_eq!(config.locations[0].event_flag, 52_410_800);
        assert_eq!(config.items[&12_255_488].normalized_item_id, 0x006C_5660);
        assert_eq!(config.items[&12_255_488].raw_descriptor, 0x806C_5660);
        assert_eq!(config.items[&12_255_488].reinforcement_level, Some(0));
        assert_eq!(
            config.items[&12_255_488].feed_effect,
            FeedEffectBinding::RightHandWeapon
        );
        assert!(config.auto_upgrade);
        assert!(config.auto_equip);
        assert_eq!(config.goal_location, Some(12_259_363));
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
    fn equipment_requires_explicit_live_descriptor_evidence() {
        let error = local()
            .apply_slot_data(&json!({
                "runtime_items": {
                    "12255243": {
                        "raw_descriptor": 0x8132B3A0_u32,
                        "normalized_item_id": 0x0132B3A0_u32,
                        "item_category": 0,
                        "descriptor_evidence": "item_lot_inferred",
                        "quantity": 1,
                        "reinforcement_level": 0,
                        "feed_effect": "right_hand_weapon"
                    }
                }
            }))
            .unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("slot_data.runtime_items"));
        assert!(diagnostic.contains("item_lot_inferred"));
    }

    #[test]
    fn equipment_requires_a_compatible_raw_descriptor() {
        let error = local()
            .apply_slot_data(&json!({
                "runtime_items": {
                    "12255243": {
                        "raw_descriptor": 0x80989680_u32,
                        "normalized_item_id": 0x006C5660,
                        "item_category": 0,
                        "descriptor_evidence": "live_grant_inventory_ui",
                        "quantity": 1,
                        "reinforcement_level": 0,
                        "feed_effect": "right_hand_weapon"
                    }
                }
            }))
            .unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("AP item 12255243"));
        assert!(diagnostic.contains("raw/normalized"));
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
    fn goal_location_must_be_one_of_the_seed_runtime_locations() {
        let error = local()
            .apply_slot_data(&json!({
                "runtime_locations": {
                    "10": {"event_flag": 12411800}
                },
                "goal_location": 11
            }))
            .unwrap_err();
        assert!(format!("{error:#}").contains("absent from runtime_locations"));
    }

    #[test]
    fn suppressed_location_requires_a_seed_owned_install_witness() {
        let error = local()
            .apply_slot_data(&json!({
                "runtime_locations": {
                    "1": {"event_flag": 2, "vanilla_award_suppressed": true}
                }
            }))
            .unwrap_err();
        assert!(format!("{error:#}").contains("without requiring"));
    }

    #[test]
    fn installed_binder_must_match_the_seed_plan_and_manifest_output() {
        let root =
            std::env::temp_dir().join(format!("bb-suppression-witness-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let installed = root
            .join("game-install")
            .join("param")
            .join("gameparam")
            .join("gameparam.parambnd.dcx");
        fs::create_dir_all(installed.parent().unwrap()).unwrap();
        fs::write(&installed, b"verified suppressed binder").unwrap();
        let output_hash = sha256_file(&installed).unwrap();
        let plan_hash = "1".repeat(64);
        let manifest = root.join("build-manifest.json");
        fs::write(
            &manifest,
            json::to_vec_pretty(&json!({
                "format": "bb-vanilla-suppression-build-v1",
                "plan_sha256": plan_hash,
                "output_gameparam_sha256": output_hash,
                "output_relative_path": "param/gameparam/gameparam.parambnd.dcx",
                "installed": false
            }))
            .unwrap(),
        )
        .unwrap();
        let mut config = local();
        config.suppression_manifest = Some(manifest);
        config.installed_gameparam = Some(installed.clone());
        config.suppression = SuppressionRequirement {
            required: true,
            manifest_format: "bb-vanilla-suppression-build-v1".into(),
            plan_sha256: "1".repeat(64),
        };
        assert_eq!(
            config.verify_suppression_install().unwrap(),
            Some(sha256_file(&installed).unwrap())
        );
        fs::write(&installed, b"not the built binder").unwrap();
        let error = config.verify_suppression_install().unwrap_err();
        assert!(format!("{error:#}").contains("installed gameparam mismatch"));
        fs::remove_dir_all(root).unwrap();
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
