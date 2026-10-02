//! Strict Mario capability contract and reconnect-safe received-history fold.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const ABI_VERSION: u32 = 1;
pub const FEATURE: &str = "mario_capabilities_v1";
pub const REGRESSION_FEATURE: &str = "mario_regression_v1";
pub const REGRESSION_INTERACT_FLAG: u32 = 8;
pub const STATS_FEATURE: &str = "mario_stats_v1";
pub const SUPPORTS_STATS_FLAG: u32 = 16;
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
}
impl Config {
    pub fn validate_bridge_features(&self, state: &BridgeState) -> Result<(), String> {
        if self.requires_regression && state.flags & REGRESSION_INTERACT_FLAG == 0 {
            return Err("Mario seed refused: update er_mario.dll for the Law of Regression statue interaction".into());
        }
        if self.stat_items.is_some() && state.flags & SUPPORTS_STATS_FLAG == 0 {
            return Err("Mario stat seed refused: update er_mario.dll for mario_stats_v1".into());
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
        return if required || requires_regression || stats_enabled {
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
    Ok(Some(Config {
        managed,
        unlock_items,
        requires_regression,
        stat_items,
    }))
}

#[derive(Default, Debug)]
pub struct Session {
    pub config: Option<Config>,
    pub identity: Option<String>,
    pub unlocked: u32,
    pub health_receipts: u32,
    pub power_receipts: u32,
}
impl Session {
    pub fn configure(&mut self, identity: String, config: Option<Config>) {
        if self.identity.as_ref() != Some(&identity) || self.config != config {
            self.unlocked = 0;
            self.health_receipts = 0;
            self.power_receipts = 0;
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
            for (index, id) in items {
                if !seen.insert(index) {
                    continue;
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
        }
    }
    pub fn is_unlock(&self, id: i64) -> bool {
        self.config.as_ref().is_some_and(|config| {
            config.unlock_items.contains_key(&id)
                || config
                    .stat_items
                    .as_ref()
                    .is_some_and(|stats| id == stats.health || id == stats.power)
        })
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
