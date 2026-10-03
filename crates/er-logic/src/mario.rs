//! Strict Mario capability contract and reconnect-safe received-history fold.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const ABI_VERSION: u32 = 1;
pub const FEATURE: &str = "mario_capabilities_v1";
pub const REGRESSION_FEATURE: &str = "mario_regression_v1";
pub const REGRESSION_INTERACT_FLAG: u32 = 8;
pub const STATS_FEATURE: &str = "mario_stats_v1";
pub const SUPPORTS_STATS_FLAG: u32 = 16;
pub const FLUDD_FEATURE: &str = "mario_fludd_v1";
pub const SUPPORTS_FLUDD_FLAG: u32 = 32;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Addon {
    Cappy,
    Sonic,
}
impl Addon {
    pub const ALL: [Self; 2] = [Self::Cappy, Self::Sonic];
    pub fn index(self) -> usize {
        match self {
            Self::Cappy => 0,
            Self::Sonic => 1,
        }
    }
    pub fn option(self) -> &'static str {
        match self {
            Self::Cappy => "mario_cappy",
            Self::Sonic => "mario_sonic_movement",
        }
    }
    pub fn feature(self) -> &'static str {
        match self {
            Self::Cappy => "mario_cappy_v1",
            Self::Sonic => "mario_sonic_movement_v1",
        }
    }
    pub fn support_flag(self) -> u32 {
        match self {
            Self::Cappy => 64,
            Self::Sonic => 128,
        }
    }
    pub fn keys(self) -> &'static [&'static str] {
        match self {
            Self::Cappy => &["cap_throw", "cap_bounce"],
            Self::Sonic => &["spin_dash", "drop_dash", "air_dash"],
        }
    }
}
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddonConfig {
    pub enabled: bool,
    pub unlocked: u32,
}
#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
pub struct AddonState {
    pub abi_version: u32,
    pub flags: u32,
    pub unlocked: u32,
    pub runtime_state: u32,
}
impl AddonState {
    pub fn matches(&self, expected: AddonConfig) -> bool {
        self.abi_version == ABI_VERSION
            && self.flags & 4 != 0
            && (self.flags & 2 != 0) == expected.enabled
            && self.unlocked == expected.unlocked
    }
    pub fn acknowledged(&self, expected: AddonConfig) -> bool {
        self.matches(expected) && self.flags & 1 != 0
    }
}
const _: () = assert!(size_of::<AddonState>() == 16);
pub const FLUDD_KEYS: &[&str] = &[
    "fludd_hover",
    "fludd_rocket",
    "fludd_turbo",
    "progressive_fludd_tank",
];

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fludd {
    pub enabled: bool,
    pub nozzles: u32,
    pub tank_level: u32,
}
#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
pub struct FluddState {
    pub abi_version: u32,
    pub flags: u32,
    pub unlocked_nozzles: u32,
    pub tank_level: u32,
    pub selected_nozzle: u32,
    pub water_units: u32,
    pub capacity_units: u32,
}
impl FluddState {
    pub fn matches(&self, expected: Fludd) -> bool {
        // Older ER-Mario workers use 60+20/tier; the larger-tank workers use
        // 300+100/tier. Both retain the same ABI and exact receipt identity.
        let legacy_capacity = expected
            .tank_level
            .checked_mul(20)
            .and_then(|n| n.checked_add(60));
        let larger_capacity = expected
            .tank_level
            .checked_mul(100)
            .and_then(|n| n.checked_add(300));
        self.abi_version == ABI_VERSION
            && self.flags & 4 != 0
            && (self.flags & 2 != 0) == expected.enabled
            && self.unlocked_nozzles == expected.nozzles
            && self.tank_level == expected.tank_level
            && (Some(self.capacity_units) == legacy_capacity
                || Some(self.capacity_units) == larger_capacity)
    }
    pub fn acknowledged(&self, expected: Fludd) -> bool {
        self.matches(expected) && self.flags & 1 != 0
    }
}
const _: () = assert!(size_of::<FluddState>() == 28);
pub const CAPABILITIES: &[(&str, u32)] = &[
    ("progressive_jump", 129),
    ("backflip", 256),
    ("side_flip", 512),
    ("long_jump", 2),
    ("wall_kick", 4),
    ("dive", 8),
    ("ground_pound", 16),
    ("enemy_grab", 32),
    ("boss_swing", 64),
];

/// Read-back from the worker, shared with the versioned C ABI.
#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
pub struct BridgeState {
    pub abi_version: u32,
    pub flags: u32,
    pub managed: u32,
    pub unlocked: u32,
}
impl BridgeState {
    pub fn acknowledged(&self, managed: u32, unlocked: u32) -> bool {
        self.abi_version == ABI_VERSION
            && self.flags & 7 == 7
            && self.managed == managed
            && self.unlocked == unlocked
    }
}
const _: () = assert!(size_of::<BridgeState>() == 16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub max_wedges: u32,
    pub power_basis_points: u32,
}
impl Default for Stats {
    fn default() -> Self {
        Self {
            max_wedges: 8,
            power_basis_points: 10_000,
        }
    }
}

/// Additive ABI: the original capability state remains exactly 16 bytes.
#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
pub struct StatsState {
    pub abi_version: u32,
    pub flags: u32,
    pub max_wedges: u32,
    pub power_basis_points: u32,
}
impl StatsState {
    pub fn matches(&self, expected: Stats) -> bool {
        self.abi_version == ABI_VERSION
            && self.flags & 4 != 0
            && self.max_wedges == expected.max_wedges
            && self.power_basis_points == expected.power_basis_points
    }
    pub fn acknowledged(&self, expected: Stats) -> bool {
        self.matches(expected) && self.flags & 3 == 3
    }
}
const _: () = assert!(size_of::<StatsState>() == 16);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatItems {
    pub health: i64,
    pub power: i64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub managed: u32,
    pub unlock_items: BTreeMap<i64, u32>,
    pub requires_regression: bool,
    pub stat_items: Option<StatItems>,
    pub fludd_items: Option<[i64; 4]>,
    pub addon_items: BTreeMap<i64, (Addon, u32)>,
}
impl Config {
    pub fn addon_enabled(&self, addon: Addon) -> bool {
        self.addon_items.values().any(|(a, _)| *a == addon)
    }
    pub fn validate_addon_exports(&self, addon: Addon, available: bool) -> Result<(), String> {
        if self.addon_enabled(addon) && !available {
            return Err(format!(
                "Mario seed refused: er_mario.dll is missing {} ABI exports",
                addon.feature()
            ));
        }
        Ok(())
    }
    pub fn validate_bridge_features(&self, state: &BridgeState) -> Result<(), String> {
        if self.requires_regression && state.flags & REGRESSION_INTERACT_FLAG == 0 {
            return Err("Mario seed refused: update er_mario.dll for the Law of Regression statue interaction".into());
        }
        if self.stat_items.is_some() && state.flags & SUPPORTS_STATS_FLAG == 0 {
            return Err("Mario stat seed refused: update er_mario.dll for mario_stats_v1".into());
        }
        if self.fludd_items.is_some() && state.flags & SUPPORTS_FLUDD_FLAG == 0 {
            return Err("Mario FLUDD seed refused: update er_mario.dll for mario_fludd_v1".into());
        }
        for addon in Addon::ALL {
            if self.addon_enabled(addon) && state.flags & addon.support_flag() == 0 {
                return Err(format!(
                    "Mario seed refused: update er_mario.dll for {}",
                    addon.feature()
                ));
            }
        }
        Ok(())
    }
    pub fn validate_fludd_exports(&self, available: bool) -> Result<(), String> {
        if self.fludd_items.is_some() && !available {
            return Err(
                "Mario FLUDD seed refused: er_mario.dll is missing FLUDD ABI exports".into(),
            );
        }
        Ok(())
    }
    pub fn validate_stats_exports(&self, available: bool) -> Result<(), String> {
        if self.stat_items.is_some() && !available {
            return Err(
                "Mario stat seed refused: er_mario.dll is missing the stat ABI exports".into(),
            );
        }
        Ok(())
    }
    pub fn acknowledged(
        &self,
        base: &BridgeState,
        stats: Option<&StatsState>,
        unlocked: u32,
        expected: Stats,
    ) -> bool {
        self.validate_bridge_features(base).is_ok()
            && base.acknowledged(self.managed, unlocked)
            && match stats {
                Some(stats) => stats.acknowledged(expected),
                None => self.stat_items.is_none(),
            }
    }
}

fn option(sd: &Value, key: &str) -> Result<bool, String> {
    match sd.get("options").and_then(|o| o.get(key)) {
        None | Some(Value::Bool(false)) => Ok(false),
        Some(Value::Bool(true)) => Ok(true),
        Some(Value::Number(n)) if n.as_u64() == Some(0) => Ok(false),
        Some(Value::Number(n)) if n.as_u64() == Some(1) => Ok(true),
        _ => Err(format!("options.{key} must be boolean or 0/1")),
    }
}

fn bit(key: &str) -> Result<u32, String> {
    CAPABILITIES
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, bit)| *bit)
        .ok_or_else(|| format!("Unknown Mario capability {key:?}"))
}

/// Absence is compatible with older seeds. A declared feature without its payload is an error.
pub fn parse(sd: &Value) -> Result<Option<Config>, String> {
    let required = sd
        .get("requiresClientFeatures")
        .and_then(Value::as_array)
        .is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some(FEATURE)));
    let requires_regression = sd
        .get("requiresClientFeatures")
        .and_then(Value::as_array)
        .is_some_and(|tags| {
            tags.iter()
                .any(|tag| tag.as_str() == Some(REGRESSION_FEATURE))
        });
    let stats_required = sd
        .get("requiresClientFeatures")
        .and_then(Value::as_array)
        .is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some(STATS_FEATURE)));
    let enabled = option(sd, "mario_mode")?;
    let stats_enabled = option(sd, "mario_stat_upgrades")?;
    let fludd_enabled = option(sd, "mario_fludd")?;
    let mut addons_enabled = [false; 2];
    for addon in Addon::ALL {
        let enabled = option(sd, addon.option())?;
        let required = sd
            .get("requiresClientFeatures")
            .and_then(Value::as_array)
            .is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some(addon.feature())));
        if enabled != required {
            return Err(format!(
                "{} and {} handshake must both be enabled",
                addon.option(),
                addon.feature()
            ));
        }
        if !enabled
            && sd
                .get("abilityUnlockItems")
                .and_then(Value::as_object)
                .is_some_and(|map| {
                    map.values()
                        .any(|key| key.as_str().is_some_and(|key| addon.keys().contains(&key)))
                })
        {
            return Err(format!(
                "Mario addon items declared while {} is off",
                addon.option()
            ));
        }
        addons_enabled[addon.index()] = enabled;
    }
    let fludd_required = sd
        .get("requiresClientFeatures")
        .and_then(Value::as_array)
        .is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some(FLUDD_FEATURE)));
    if fludd_enabled != fludd_required {
        return Err("Mario FLUDD and mario_fludd_v1 handshake must both be enabled".into());
    }
    if !fludd_enabled
        && sd
            .get("abilityUnlockItems")
            .and_then(Value::as_object)
            .is_some_and(|map| {
                map.values()
                    .any(|key| key.as_str().is_some_and(|key| FLUDD_KEYS.contains(&key)))
            })
    {
        return Err("Mario FLUDD items declared while mario_fludd is off".into());
    }
    if stats_enabled != stats_required {
        return Err("Mario stat upgrades and mario_stats_v1 handshake must both be enabled".into());
    }
    if !stats_enabled
        && sd
            .get("abilityUnlockItems")
            .and_then(Value::as_object)
            .is_some_and(|map| {
                map.values().any(|key| {
                    matches!(
                        key.as_str(),
                        Some("progressive_health" | "progressive_power")
                    )
                })
            })
    {
        return Err("Mario stat items declared while mario_stat_upgrades is off".into());
    }
    if !enabled {
        return if required
            || requires_regression
            || stats_enabled
            || fludd_enabled
            || addons_enabled.iter().any(|enabled| *enabled)
        {
            Err("Mario capability feature declared but mario_mode is off".into())
        } else {
            Ok(None)
        };
    }
    if !required {
        return Err("Mario mode needs mario_capabilities_v1 handshake".into());
    }
    let managed = 1023;
    let map = sd
        .get("abilityUnlockItems")
        .and_then(Value::as_object)
        .ok_or("Mario abilityUnlockItems must be an object")?;
    let mut unlock_items = BTreeMap::new();
    let mut mapped = BTreeSet::new();
    let mut health = None;
    let mut power = None;
    let mut fludd = [None; 4];
    let mut addon_items = BTreeMap::new();
    let mut addon_mapped = [0; 2];
    for (id, key) in map {
        let ap_id: i64 = id
            .parse()
            .map_err(|_| "Mario AP item id must be an integer")?;
        if ap_id <= 0 || ap_id.to_string() != *id {
            return Err("Mario AP item id must be canonical and positive".into());
        }
        if sd
            .get("apIdsToItemIds")
            .and_then(Value::as_object)
            .is_some_and(|items| items.contains_key(id))
        {
            return Err("Mario unlock item must not also grant an Elden Ring item".into());
        }
        let key = key
            .as_str()
            .ok_or("Mario unlockItems values must be strings")?;
        if let Some((addon, index)) = Addon::ALL.into_iter().find_map(|addon| {
            addon
                .keys()
                .iter()
                .position(|name| *name == key)
                .map(|i| (addon, i))
        }) {
            let mask = 1 << index;
            if !addons_enabled[addon.index()] || addon_mapped[addon.index()] & mask != 0 {
                return Err(
                    "Mario unlockItems must map each enabled addon family exactly once".into(),
                );
            }
            addon_mapped[addon.index()] |= mask;
            addon_items.insert(ap_id, (addon, mask));
            continue;
        }
        if let Some(index) = FLUDD_KEYS
            .iter()
            .position(|name| *name == key)
            .filter(|_| fludd_enabled)
        {
            if fludd[index].replace(ap_id).is_some() {
                return Err("Mario unlockItems must map each FLUDD family exactly once".into());
            }
            continue;
        }
        let stat_id = match key {
            "progressive_health" if stats_enabled => Some(&mut health),
            "progressive_power" if stats_enabled => Some(&mut power),
            _ => None,
        };
        if let Some(stat_id) = stat_id {
            if stat_id.replace(ap_id).is_some() {
                return Err("Mario unlockItems must map each stat family exactly once".into());
            }
            continue;
        }
        let mask = bit(key)?;
        if managed & mask == 0 || !mapped.insert(mask) {
            return Err("Mario unlockItems must map each locked move exactly once".into());
        }
        unlock_items.insert(ap_id, mask);
    }
    let stat_items = if stats_enabled {
        Some(StatItems {
            health: health.ok_or("Mario unlockItems is missing progressive_health")?,
            power: power.ok_or("Mario unlockItems is missing progressive_power")?,
        })
    } else {
        None
    };
    if mapped.iter().copied().fold(0, |a, b| a | b) != managed {
        return Err("Mario unlockItems is missing a locked move".into());
    }
    for addon in Addon::ALL {
        if addons_enabled[addon.index()]
            && addon_mapped[addon.index()] != (1 << addon.keys().len()) - 1
        {
            return Err(format!(
                "Mario unlockItems is missing a {} family",
                addon.feature()
            ));
        }
    }
    Ok(Some(Config {
        managed,
        unlock_items,
        requires_regression,
        stat_items,
        addon_items,
        fludd_items: if fludd_enabled {
            Some(
                fludd
                    .into_iter()
                    .collect::<Option<Vec<_>>>()
                    .ok_or("Mario unlockItems is missing a FLUDD family")?
                    .try_into()
                    .unwrap(),
            )
        } else {
            None
        },
    }))
}

#[derive(Default, Debug)]
pub struct Session {
    pub config: Option<Config>,
    pub identity: Option<String>,
    pub unlocked: u32,
    pub health_receipts: u32,
    pub power_receipts: u32,
    pub fludd_nozzles: u32,
    pub tank_receipts: u32,
    pub addon_unlocks: [u32; 2],
}
impl Session {
    pub fn configure(&mut self, identity: String, config: Option<Config>) {
        if self.identity.as_ref() != Some(&identity) || self.config != config {
            self.unlocked = 0;
            self.health_receipts = 0;
            self.power_receipts = 0;
            self.fludd_nozzles = 0;
            self.tank_receipts = 0;
            self.addon_unlocks = [0; 2];
        }
        self.identity = Some(identity);
        self.config = config;
    }
    /// Scan the complete stream. A disconnect or temporarily incomplete reconnect stream cannot
    /// revoke an earned move; a different seed/slot always starts a new fold.
    pub fn receive_history(&mut self, items: impl IntoIterator<Item = (i64, i64)>) {
        if let Some(config) = &self.config {
            let mut seen = BTreeSet::new();
            let mut bits = 0;
            let mut jumps = 0;
            let mut health = 0_u32;
            let mut power = 0_u32;
            let mut nozzles = 0;
            let mut tanks = 0_u32;
            for (index, id) in items {
                if !seen.insert(index) {
                    continue;
                }
                if let Some(&(addon, mask)) = config.addon_items.get(&id) {
                    self.addon_unlocks[addon.index()] |= mask;
                }
                if let Some(ids) = config.fludd_items {
                    for (i, nozzle_id) in ids[..3].iter().enumerate() {
                        if id == *nozzle_id {
                            nozzles |= 1 << i;
                        }
                    }
                    if id == ids[3] {
                        tanks = (tanks + 1).min(3);
                    }
                }
                if let Some(stats) = &config.stat_items {
                    if id == stats.health {
                        health = (health + 1).min(4);
                    }
                    if id == stats.power {
                        power = (power + 1).min(3);
                    }
                }
                if let Some(&mask) = config.unlock_items.get(&id) {
                    if mask == 129 {
                        jumps = (jumps + 1).min(2);
                    } else {
                        bits |= mask;
                    }
                }
            }
            if jumps >= 1 {
                bits |= 1;
            }
            if jumps >= 2 {
                bits |= 128;
            }
            self.unlocked |= bits;
            self.health_receipts = self.health_receipts.max(health);
            self.power_receipts = self.power_receipts.max(power);
            self.fludd_nozzles |= nozzles;
            self.tank_receipts = self.tank_receipts.max(tanks);
        }
    }
    pub fn is_unlock(&self, id: i64) -> bool {
        self.config.as_ref().is_some_and(|config| {
            config.unlock_items.contains_key(&id)
                || config.addon_items.contains_key(&id)
                || config.fludd_items.is_some_and(|ids| ids.contains(&id))
                || config
                    .stat_items
                    .as_ref()
                    .is_some_and(|stats| id == stats.health || id == stats.power)
        })
    }
    pub fn expected_fludd(&self) -> Fludd {
        if self
            .config
            .as_ref()
            .is_some_and(|c| c.fludd_items.is_some())
        {
            Fludd {
                enabled: true,
                nozzles: self.fludd_nozzles,
                tank_level: self.tank_receipts.min(3),
            }
        } else {
            Fludd::default()
        }
    }
    pub fn expected_addon(&self, addon: Addon) -> AddonConfig {
        if self.config.as_ref().is_some_and(|c| c.addon_enabled(addon)) {
            AddonConfig {
                enabled: true,
                unlocked: self.addon_unlocks[addon.index()],
            }
        } else {
            AddonConfig::default()
        }
    }
    pub fn expected_stats(&self) -> Stats {
        if self.config.as_ref().is_some_and(|c| c.stat_items.is_some()) {
            Stats {
                max_wedges: 4 + self.health_receipts.min(4),
                power_basis_points: 7500 + 2500 * self.power_receipts.min(3),
            }
        } else {
            Stats::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn seed() -> Value {
        json!({"requiresClientFeatures":[FEATURE],"options":{"mario_mode":1},
            "abilityUnlockItems":CAPABILITIES.iter().enumerate().map(|(i,(key,_))|
                ((100+i).to_string(),json!(key))).collect::<serde_json::Map<String,Value>>()})
    }
    fn stat_seed() -> Value {
        let mut sd = seed();
        sd["requiresClientFeatures"] = json!([FEATURE, STATS_FEATURE]);
        sd["options"]["mario_stat_upgrades"] = json!(1);
        sd["abilityUnlockItems"]["200"] = json!("progressive_health");
        sd["abilityUnlockItems"]["201"] = json!("progressive_power");
        sd
    }
    fn fludd_seed() -> Value {
        let mut sd = stat_seed();
        sd["requiresClientFeatures"] = json!([FEATURE, STATS_FEATURE, FLUDD_FEATURE]);
        sd["options"]["mario_fludd"] = json!(1);
        for (i, key) in FLUDD_KEYS.iter().enumerate() {
            sd["abilityUnlockItems"][(300 + i).to_string()] = json!(key);
        }
        sd
    }
    fn addon_seed() -> Value {
        let mut sd = fludd_seed();
        for addon in Addon::ALL {
            sd["options"][addon.option()] = json!(1);
            sd["requiresClientFeatures"]
                .as_array_mut()
                .unwrap()
                .push(json!(addon.feature()));
            for (i, key) in addon.keys().iter().enumerate() {
                sd["abilityUnlockItems"][(400 + addon.index() * 10 + i).to_string()] = json!(key);
            }
        }
        sd
    }
    #[test]
    fn addon_contract_is_strict_complete_optional_and_independent() {
        let config = parse(&addon_seed()).unwrap().unwrap();
        assert_eq!(config.managed, 1023);
        assert_eq!(config.addon_items.len(), 5);
        for addon in Addon::ALL {
            assert!(config.addon_enabled(addon));
            assert!(config.validate_addon_exports(addon, false).is_err());
            for bad in [
                json!(2),
                json!(-1),
                json!(1.0),
                json!("true"),
                Value::Null,
                json!(false),
            ] {
                let mut sd = addon_seed();
                sd["options"][addon.option()] = bad;
                assert!(parse(&sd).is_err());
            }
            for key in addon.keys() {
                let mut sd = addon_seed();
                sd["abilityUnlockItems"]
                    .as_object_mut()
                    .unwrap()
                    .retain(|_, v| v != key);
                assert!(parse(&sd).is_err());
            }
            let mut sd = addon_seed();
            sd["requiresClientFeatures"]
                .as_array_mut()
                .unwrap()
                .retain(|t| t != addon.feature());
            assert!(parse(&sd).is_err());
            let mut sd = addon_seed();
            sd["options"]["mario_mode"] = json!(0);
            assert!(parse(&sd).is_err());
            let mut sd = seed();
            sd["abilityUnlockItems"]["999"] = json!(addon.keys()[0]);
            assert!(parse(&sd).is_err());
            let mut sd = addon_seed();
            sd["abilityUnlockItems"]["999"] = json!(addon.keys()[0]);
            assert!(parse(&sd).is_err());
            let mut sd = addon_seed();
            sd["options"][addon.option()] = json!(true);
            assert_eq!(parse(&sd).unwrap().unwrap(), config);
            // Disable only this addon, preserving every other configured family.
            sd["options"][addon.option()] = json!(0);
            sd["requiresClientFeatures"]
                .as_array_mut()
                .unwrap()
                .retain(|t| t != addon.feature());
            sd["abilityUnlockItems"]
                .as_object_mut()
                .unwrap()
                .retain(|_, v| !addon.keys().contains(&v.as_str().unwrap()));
            let other = parse(&sd).unwrap().unwrap();
            assert!(!other.addon_enabled(addon));
            assert!(other.validate_addon_exports(addon, false).is_ok());
        }
        assert!(config
            .validate_bridge_features(&BridgeState {
                flags: 48,
                ..Default::default()
            })
            .is_err());
        assert!(config
            .validate_bridge_features(&BridgeState {
                flags: 240,
                ..Default::default()
            })
            .is_ok());
        for sd in [seed(), stat_seed(), fludd_seed()] {
            assert!(parse(&sd).unwrap().unwrap().addon_items.is_empty());
        }
        let mut sd = addon_seed();
        sd["apIdsToItemIds"] = json!({"400":123});
        assert!(parse(&sd).is_err());
        let mut sd = addon_seed();
        sd["abilityUnlockItems"]["400"] = json!("unknown_cap");
        assert!(parse(&sd).is_err());
    }
    #[test]
    fn all_previous_mario_family_counts_keep_addons_off() {
        let mut fludd_only = fludd_seed();
        fludd_only["options"]["mario_stat_upgrades"] = json!(0);
        fludd_only["requiresClientFeatures"]
            .as_array_mut()
            .unwrap()
            .retain(|tag| tag != STATS_FEATURE);
        fludd_only["abilityUnlockItems"]
            .as_object_mut()
            .unwrap()
            .retain(|_, v| v != "progressive_health" && v != "progressive_power");
        for (count, sd) in [
            (9, seed()),
            (11, stat_seed()),
            (13, fludd_only),
            (15, fludd_seed()),
        ] {
            assert_eq!(sd["abilityUnlockItems"].as_object().unwrap().len(), count);
            let config = parse(&sd).unwrap().unwrap();
            let mut session = Session::default();
            session.configure("legacy".into(), Some(config.clone()));
            for addon in Addon::ALL {
                assert!(!config.addon_enabled(addon));
                assert!(config.validate_addon_exports(addon, false).is_ok());
                assert_eq!(session.expected_addon(addon), AddonConfig::default());
            }
        }
    }
    #[test]
    fn addon_receipts_are_synthetic_indexed_reconnect_safe_and_identity_scoped() {
        let config = parse(&addon_seed()).unwrap();
        let mut session = Session::default();
        session.configure("a".into(), config.clone());
        let history = [
            (0, 400),
            (1, 401),
            (2, 410),
            (3, 411),
            (4, 412),
            (4, 412),
            (5, 412),
            (6, 100),
            (7, 100),
            (8, 303),
            (9, 200),
        ];
        session.receive_history(history);
        assert_eq!(
            session.expected_addon(Addon::Cappy),
            AddonConfig {
                enabled: true,
                unlocked: 3
            }
        );
        assert_eq!(
            session.expected_addon(Addon::Sonic),
            AddonConfig {
                enabled: true,
                unlocked: 7
            }
        );
        assert_eq!(session.unlocked, 129);
        assert_eq!(session.tank_receipts, 1);
        assert_eq!(session.health_receipts, 1);
        for id in [400, 401, 410, 411, 412] {
            assert!(session.is_unlock(id));
        }
        session.configure("a".into(), config.clone());
        session.receive_history([]);
        session.receive_history(history);
        assert_eq!(session.expected_addon(Addon::Sonic).unlocked, 7);
        session.configure("b".into(), config);
        assert_eq!(session.expected_addon(Addon::Sonic).unlocked, 0);
        session.receive_history([(0, 410), (0, 411)]);
        assert_eq!(session.expected_addon(Addon::Sonic).unlocked, 1);
        session.configure("old".into(), parse(&seed()).unwrap());
        for addon in Addon::ALL {
            assert_eq!(session.expected_addon(addon), AddonConfig::default());
        }
    }
    #[test]
    fn addon_acknowledgment_requires_live_exact_configuration_not_runtime_action() {
        let expected = AddonConfig {
            enabled: true,
            unlocked: 3,
        };
        let state = AddonState {
            abi_version: 1,
            flags: 7,
            unlocked: 3,
            runtime_state: 999,
        };
        assert!(state.acknowledged(expected));
        for flags in [0, 3, 5, 6] {
            assert!(!AddonState { flags, ..state }.acknowledged(expected));
        }
        assert!(!AddonState {
            unlocked: 1,
            ..state
        }
        .acknowledged(expected));
        assert!(!AddonState {
            abi_version: 2,
            ..state
        }
        .acknowledged(expected));
        assert!(AddonState {
            abi_version: 1,
            flags: 5,
            runtime_state: 999,
            ..Default::default()
        }
        .acknowledged(AddonConfig::default()));
        assert!(!AddonState {
            abi_version: 1,
            flags: 7,
            ..Default::default()
        }
        .acknowledged(AddonConfig::default()));
    }
    #[test]
    fn fludd_contract_requires_complete_families_feature_and_mario_mode() {
        let config = parse(&fludd_seed()).unwrap().unwrap();
        assert_eq!(config.fludd_items, Some([300, 301, 302, 303]));
        assert!(config.validate_fludd_exports(false).is_err());
        assert!(config
            .validate_bridge_features(&BridgeState {
                flags: 16,
                ..Default::default()
            })
            .is_err());
        assert!(config
            .validate_bridge_features(&BridgeState {
                flags: 48,
                ..Default::default()
            })
            .is_ok());
        assert!(parse(&seed())
            .unwrap()
            .unwrap()
            .validate_fludd_exports(false)
            .is_ok());
        for option in [
            json!(2),
            json!(-1),
            json!(1.0),
            json!("true"),
            Value::Null,
            json!(false),
        ] {
            let mut sd = fludd_seed();
            sd["options"]["mario_fludd"] = option;
            assert!(parse(&sd).is_err());
        }
        for key in FLUDD_KEYS {
            let mut sd = fludd_seed();
            sd["abilityUnlockItems"]
                .as_object_mut()
                .unwrap()
                .retain(|_, v| v != key);
            assert!(parse(&sd).is_err());
        }
        let mut sd = fludd_seed();
        sd["requiresClientFeatures"] = json!([FEATURE, STATS_FEATURE]);
        assert!(parse(&sd).is_err());
        let mut sd = fludd_seed();
        sd["options"]["mario_mode"] = json!(0);
        assert!(parse(&sd).is_err());
        let mut sd = seed();
        sd["abilityUnlockItems"]["303"] = json!("progressive_fludd_tank");
        assert!(parse(&sd).is_err());
        let mut sd = fludd_seed();
        sd["abilityUnlockItems"]["304"] = json!("fludd_hover");
        assert!(parse(&sd).is_err());
        let mut sd = fludd_seed();
        sd["apIdsToItemIds"] = json!({"300":123});
        assert!(parse(&sd).is_err());
    }
    #[test]
    fn fludd_fold_deduplicates_indices_preserves_reconnect_and_resets_new_identity() {
        let config = parse(&fludd_seed()).unwrap();
        let mut session = Session::default();
        session.configure("a".into(), config.clone());
        let history = vec![
            (0, 300),
            (1, 301),
            (2, 302),
            (3, 303),
            (3, 303),
            (4, 303),
            (5, 303),
            (6, 303),
            (7, 200),
        ];
        session.receive_history(history.clone());
        assert_eq!(
            session.expected_fludd(),
            Fludd {
                enabled: true,
                nozzles: 7,
                tank_level: 3
            }
        );
        assert_eq!(session.expected_stats().max_wedges, 5);
        assert!(session.is_unlock(303));
        session.configure("a".into(), config.clone());
        session.receive_history(vec![(0, 300)]);
        assert_eq!(session.expected_fludd().tank_level, 3);
        session.receive_history(history);
        assert_eq!(session.expected_fludd().tank_level, 3);
        session.configure("b".into(), config);
        assert_eq!(
            session.expected_fludd(),
            Fludd {
                enabled: true,
                ..Default::default()
            }
        );
        session.receive_history(vec![(0, 303), (0, 303)]);
        assert_eq!(session.expected_fludd().tank_level, 1);
        session.configure("old".into(), parse(&seed()).unwrap());
        assert_eq!(session.expected_fludd(), Fludd::default());
    }
    #[test]
    fn fludd_ack_requires_exact_configuration_and_live_worker_but_ignores_consumables() {
        let expected = Fludd {
            enabled: true,
            nozzles: 3,
            tank_level: 2,
        };
        let state = FluddState {
            abi_version: 1,
            flags: 7,
            unlocked_nozzles: 3,
            tank_level: 2,
            selected_nozzle: 2,
            water_units: 1,
            capacity_units: 100,
        };
        assert!(state.acknowledged(expected));
        for flags in [0, 3, 5, 6] {
            assert!(!FluddState { flags, ..state }.acknowledged(expected));
        }
        assert!(!FluddState {
            unlocked_nozzles: 1,
            ..state
        }
        .acknowledged(expected));
        assert!(!FluddState {
            tank_level: 1,
            ..state
        }
        .acknowledged(expected));
        assert!(!FluddState {
            capacity_units: 80,
            ..state
        }
        .acknowledged(expected));
        assert!(FluddState {
            abi_version: 1,
            flags: 5,
            capacity_units: 60,
            ..Default::default()
        }
        .acknowledged(Fludd::default()));
        assert!(!FluddState {
            abi_version: 1,
            flags: 7,
            capacity_units: 60,
            ..Default::default()
        }
        .acknowledged(Fludd::default()));
    }
    #[test]
    fn fludd_ack_accepts_both_capacity_schemes_at_every_received_tier() {
        for tank_level in 0..=3 {
            let expected = Fludd {
                enabled: true,
                nozzles: 7,
                tank_level,
            };
            for capacity_units in [60 + 20 * tank_level, 300 + 100 * tank_level] {
                let state = FluddState {
                    abi_version: ABI_VERSION,
                    flags: 7,
                    unlocked_nozzles: 7,
                    tank_level,
                    selected_nozzle: 1,
                    water_units: capacity_units,
                    capacity_units,
                };
                assert!(state.acknowledged(expected));
                // Ack records unlock receipts, not water consumption or nozzle selection.
                for water_units in [0, 1, capacity_units / 2, capacity_units, capacity_units + 1] {
                    assert!(FluddState {
                        water_units,
                        ..state
                    }
                    .acknowledged(expected));
                }
                assert!(!FluddState {
                    capacity_units: capacity_units + 1,
                    ..state
                }
                .acknowledged(expected));
                for flags in [0, 3, 5, 6] {
                    assert!(!FluddState { flags, ..state }.acknowledged(expected));
                }
                assert!(!FluddState {
                    abi_version: 2,
                    ..state
                }
                .acknowledged(expected));
                assert!(!FluddState {
                    unlocked_nozzles: 1,
                    ..state
                }
                .acknowledged(expected));
                assert!(!FluddState {
                    tank_level: (tank_level + 1) % 4,
                    ..state
                }
                .acknowledged(expected));
            }
            // An otherwise valid capacity from a different tier cannot acknowledge this one.
            let other_tier = (tank_level + 1) % 4;
            for capacity_units in [60 + 20 * other_tier, 300 + 100 * other_tier] {
                assert!(!FluddState {
                    abi_version: ABI_VERSION,
                    flags: 7,
                    unlocked_nozzles: 7,
                    tank_level,
                    capacity_units,
                    ..Default::default()
                }
                .acknowledged(expected));
            }
        }
        for capacity_units in [60, 300] {
            assert!(FluddState {
                abi_version: ABI_VERSION,
                flags: 5,
                capacity_units,
                ..Default::default()
            }
            .acknowledged(Fludd::default()));
        }
    }
    #[test]
    fn fludd_capacity_overflow_is_a_refusal_instead_of_a_panic() {
        let expected = Fludd {
            enabled: true,
            nozzles: 7,
            tank_level: u32::MAX,
        };
        assert!(!FluddState {
            abi_version: ABI_VERSION,
            flags: 7,
            unlocked_nozzles: 7,
            tank_level: u32::MAX,
            capacity_units: 40,
            ..Default::default()
        }
        .acknowledged(expected));
    }
    #[test]
    fn stats_option_and_handshake_are_strict_and_default_off() {
        assert_eq!(parse(&seed()).unwrap().unwrap().stat_items, None);
        let mut sd = stat_seed();
        let config = parse(&sd).unwrap().unwrap();
        assert_eq!(
            config.stat_items,
            Some(StatItems {
                health: 200,
                power: 201
            })
        );
        for enabled in [json!(true), json!(1)] {
            sd["options"]["mario_stat_upgrades"] = enabled;
            assert_eq!(parse(&sd).unwrap().unwrap(), config);
        }
        for bad in [json!(2), json!(-1), json!(1.0), json!("true"), Value::Null] {
            let mut sd = stat_seed();
            sd["options"]["mario_stat_upgrades"] = bad;
            assert!(parse(&sd).is_err(), "{sd}");
        }
        for remove in ["progressive_health", "progressive_power", "long_jump"] {
            let mut sd = stat_seed();
            sd["abilityUnlockItems"]
                .as_object_mut()
                .unwrap()
                .retain(|_, key| key != remove);
            assert!(parse(&sd).is_err(), "{sd}");
        }
        for key in [json!("unknown_stat"), json!("progressive_health"), json!(7)] {
            let mut sd = stat_seed();
            sd["abilityUnlockItems"]["202"] = key;
            assert!(parse(&sd).is_err(), "{sd}");
        }
        let mut sd = stat_seed();
        sd["requiresClientFeatures"] = json!([FEATURE]);
        assert!(parse(&sd).is_err());
        let mut sd = stat_seed();
        sd["options"]["mario_stat_upgrades"] = json!(false);
        assert!(parse(&sd).is_err());
        sd["requiresClientFeatures"] = json!([FEATURE]);
        assert!(parse(&sd).is_err()); // stray stat family with option off
        let mut sd = stat_seed();
        sd["options"]["mario_mode"] = json!(0);
        assert!(parse(&sd).is_err());
        assert!(parse(&json!({"options":{"mario_stat_upgrades":1}})).is_err());
        for id in ["200", "201"] {
            let mut sd = stat_seed();
            sd["apIdsToItemIds"] = json!({id:123});
            assert!(parse(&sd).is_err());
        }
        let mut sd = stat_seed();
        sd["abilityUnlockItems"]["200"] = json!("progressive_power");
        assert!(parse(&sd).is_err()); // colliding IDs cannot encode both families
    }
    #[test]
    fn stat_receipts_are_indexed_replayed_saturated_and_identity_scoped() {
        let config = parse(&stat_seed()).unwrap();
        let mut session = Session::default();
        assert_eq!(session.expected_stats(), Stats::default());
        session.configure("seed:1".into(), config.clone());
        assert_eq!(
            session.expected_stats(),
            Stats {
                max_wedges: 4,
                power_basis_points: 7500
            }
        );
        assert!(session.is_unlock(200));
        assert!(session.is_unlock(201));
        session.receive_history([(0, 200), (0, 200), (1, 201), (1, 201), (2, 100)]);
        assert_eq!(
            session.expected_stats(),
            Stats {
                max_wedges: 5,
                power_basis_points: 10_000
            }
        );
        assert_eq!(session.unlocked, 1);
        session.receive_history([(0, 200), (1, 201), (2, 100)]);
        assert_eq!((session.health_receipts, session.power_receipts), (1, 1));
        session.receive_history([]);
        session.configure("seed:1".into(), config.clone());
        assert_eq!((session.health_receipts, session.power_receipts), (1, 1));
        session.receive_history([(0, 200), (1, 201), (2, 100), (3, 200), (4, 201), (5, 100)]);
        assert_eq!(
            session.expected_stats(),
            Stats {
                max_wedges: 6,
                power_basis_points: 12_500
            }
        );
        assert_eq!(session.unlocked, 129);
        session.receive_history((0..20).map(|i| (i, if i % 2 == 0 { 200 } else { 201 })));
        assert_eq!((session.health_receipts, session.power_receipts), (4, 3));
        assert_eq!(
            session.expected_stats(),
            Stats {
                max_wedges: 8,
                power_basis_points: 15_000
            }
        );
        session.configure("seed:2".into(), config.clone());
        assert_eq!(
            (
                session.health_receipts,
                session.power_receipts,
                session.unlocked
            ),
            (0, 0, 0)
        );
        session.receive_history([(0, 201)]);
        assert_eq!(
            session.expected_stats(),
            Stats {
                max_wedges: 4,
                power_basis_points: 10_000
            }
        );
        session.configure("other:2".into(), config);
        assert_eq!((session.health_receipts, session.power_receipts), (0, 0));
        session.configure("legacy:2".into(), parse(&seed()).unwrap());
        session.receive_history([(0, 200), (1, 201)]);
        assert_eq!(session.expected_stats(), Stats::default());
        assert!(!session.is_unlock(200));
        session.configure("vanilla:2".into(), None);
        assert_eq!(session.expected_stats(), Stats::default());
    }
    #[test]
    fn stats_require_extension_and_both_exact_live_acknowledgments() {
        let config = parse(&stat_seed()).unwrap().unwrap();
        let base = BridgeState {
            abi_version: 1,
            flags: 7,
            managed: 1023,
            unlocked: 129,
        };
        assert!(config.validate_bridge_features(&base).is_err());
        let base = BridgeState {
            flags: 7 | SUPPORTS_STATS_FLAG,
            ..base
        };
        assert!(config.validate_bridge_features(&base).is_ok());
        assert!(config.validate_stats_exports(false).is_err());
        assert!(config.validate_stats_exports(true).is_ok());
        let target = Stats {
            max_wedges: 5,
            power_basis_points: 12_500,
        };
        let good = StatsState {
            abi_version: 1,
            flags: 7,
            max_wedges: 5,
            power_basis_points: 12_500,
        };
        assert!(config.acknowledged(&base, Some(&good), 129, target));
        assert!(!config.acknowledged(&base, None, 129, target));
        for bad in [
            StatsState { flags: 3, ..good },
            StatsState { flags: 6, ..good },
            StatsState { flags: 5, ..good },
            StatsState {
                max_wedges: 4,
                ..good
            },
            StatsState {
                power_basis_points: 10_000,
                ..good
            },
            StatsState {
                abi_version: 2,
                ..good
            },
        ] {
            assert!(!config.acknowledged(&base, Some(&bad), 129, target));
        }
        // Independent queued setters must not arm when either old snapshot remains.
        assert!(!config.acknowledged(&base, Some(&good), 1, target));
        assert!(!config.acknowledged(&BridgeState { flags: 6, ..base }, Some(&good), 129, target));
        let old = parse(&seed()).unwrap().unwrap();
        assert!(old
            .validate_bridge_features(&BridgeState { flags: 7, ..base })
            .is_ok());
        assert!(old.validate_stats_exports(false).is_ok());
        assert!(old.acknowledged(&base, None, 129, Stats::default()));
        assert!(!old.acknowledged(&base, Some(&good), 129, Stats::default()));
        let normal = StatsState {
            max_wedges: 8,
            power_basis_points: 10_000,
            ..good
        };
        assert!(old.acknowledged(&base, Some(&normal), 129, Stats::default()));
    }
    #[test]
    fn old_seed_defaults_off() {
        assert_eq!(parse(&json!({})), Ok(None));
        assert_eq!(parse(&json!({"options":{"mario_mode":0}})), Ok(None));
    }
    #[test]
    fn regression_feature_refuses_old_bridge_without_breaking_legacy_seeds() {
        let legacy = parse(&seed()).unwrap().unwrap();
        let old_bridge = BridgeState {
            abi_version: ABI_VERSION,
            flags: 7,
            managed: 1023,
            unlocked: 0,
        };
        assert!(!legacy.requires_regression);
        assert!(legacy.validate_bridge_features(&old_bridge).is_ok());
        let mut sd = seed();
        sd["requiresClientFeatures"] = json!([FEATURE, REGRESSION_FEATURE]);
        let current = parse(&sd).unwrap().unwrap();
        assert!(current.requires_regression);
        assert!(current.validate_bridge_features(&old_bridge).is_err());
        assert!(current
            .validate_bridge_features(&BridgeState {
                flags: 15,
                ..old_bridge
            })
            .is_ok());
        // Capability discovery is allowed before a live Mario exists; readiness remains a separate gate.
        let initializing = BridgeState {
            flags: REGRESSION_INTERACT_FLAG,
            ..old_bridge
        };
        assert!(current.validate_bridge_features(&initializing).is_ok());
        assert!(!initializing.acknowledged(1023, 0));
        sd["options"]["mario_mode"] = json!(0);
        assert!(parse(&sd).is_err());
        sd["options"]["mario_mode"] = json!(1);
        sd["requiresClientFeatures"] = json!([REGRESSION_FEATURE]);
        assert!(parse(&sd).is_err());
    }
    #[test]
    fn replay_disconnect_reconnect_and_slot_change() {
        let config = parse(&seed()).unwrap();
        let mut s = Session::default();
        s.configure("seed:1".into(), config.clone());
        s.receive_history([(0, 103), (0, 103), (1, 999)]);
        assert_eq!(s.unlocked, 2);
        s.receive_history([]);
        assert_eq!(s.unlocked, 2);
        s.configure("seed:1".into(), config.clone());
        s.receive_history([(0, 103), (1, 105)]);
        assert_eq!(s.unlocked, 10);
        s.configure("seed:2".into(), config.clone());
        assert_eq!(s.unlocked, 0);
        s.receive_history([(0, 105)]);
        assert_eq!(s.unlocked, 8);
        s.configure("other:2".into(), config);
        assert_eq!(s.unlocked, 0);
        s.configure("vanilla:2".into(), None);
        assert!(!s.is_unlock(103));
    }
    #[test]
    fn malformed_active_payloads_refuse() {
        for payload in [
            Value::Null,
            json!({}),
            json!({"101":"nope"}),
            json!({"101":"long_jump"}),
            json!({"01":"long_jump"}),
            json!({"101":3}),
            json!({"101":"long_jump","102":"long_jump"}),
        ] {
            let mut sd = seed();
            sd["abilityUnlockItems"] = payload;
            assert!(parse(&sd).is_err(), "{sd}");
        }
        assert!(parse(&json!({"requiresClientFeatures":[FEATURE]})).is_err());
        let mut sd = seed();
        sd["requiresClientFeatures"] = json!([]);
        assert!(parse(&sd).is_err());
        let mut sd = seed();
        sd["apIdsToItemIds"] = json!({"101":123});
        assert!(parse(&sd).is_err());
        let mut sd = seed();
        sd["options"]["mario_mode"] = json!(2);
        assert!(parse(&sd).is_err());
        let mut sd = seed();
        sd["abilityUnlockItems"]["107"] = json!("roll");
        assert!(parse(&sd).is_err());
    }
    #[test]
    fn progressive_jumps_count_distinct_history_indices_and_saturate() {
        let mut s = Session::default();
        s.configure("seed:1".into(), parse(&seed()).unwrap());
        s.receive_history([(0, 100), (0, 100)]);
        assert_eq!(s.unlocked, 1);
        s.receive_history([(0, 100)]);
        assert_eq!(s.unlocked, 1);
        s.receive_history([(0, 100), (1, 100)]);
        assert_eq!(s.unlocked, 129);
        s.receive_history([(0, 100), (1, 100), (2, 100)]);
        assert_eq!(s.unlocked, 129);
        s.configure("seed:2".into(), parse(&seed()).unwrap());
        s.receive_history([(0, 100)]);
        assert_eq!(s.unlocked, 1);
    }
    #[test]
    fn bridge_requires_ready_enabled_applied_and_exact_snapshot() {
        let good = BridgeState {
            abi_version: 1,
            flags: 7,
            managed: 1023,
            unlocked: 129,
        };
        assert!(good.acknowledged(1023, 129));
        for flags in 0..7 {
            assert!(!BridgeState { flags, ..good }.acknowledged(1023, 129));
        }
        assert!(!BridgeState {
            abi_version: 2,
            ..good
        }
        .acknowledged(1023, 129));
        assert!(!BridgeState {
            managed: 127,
            ..good
        }
        .acknowledged(1023, 129));
        assert!(!BridgeState {
            unlocked: 1,
            ..good
        }
        .acknowledged(1023, 129));
    }
    #[test]
    fn capability_mask_is_wire_stable() {
        assert_eq!(parse(&seed()).unwrap().unwrap().managed, 1023);
    }
}
