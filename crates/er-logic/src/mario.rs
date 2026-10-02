//! Strict Mario capability contract and reconnect-safe received-history fold.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const ABI_VERSION: u32 = 1;
pub const FEATURE: &str = "mario_capabilities_v1";
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub managed: u32,
    pub unlock_items: BTreeMap<i64, u32>,
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
    let enabled = match sd.get("options").and_then(|o| o.get("mario_mode")) {
        None | Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        Some(Value::Number(n)) if n.as_u64() == Some(0) => false,
        Some(Value::Number(n)) if n.as_u64() == Some(1) => true,
        _ => return Err("options.mario_mode must be boolean or 0/1".into()),
    };
    if !enabled {
        return if required {
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
    for (id, key) in map {
        let ap_id: i64 = id
            .parse()
            .map_err(|_| "Mario AP item id must be an integer")?;
        if ap_id <= 0 || ap_id.to_string() != *id {
            return Err("Mario AP item id must be canonical and positive".into());
        }
        let mask = bit(key
            .as_str()
            .ok_or("Mario unlockItems values must be strings")?)?;
        if managed & mask == 0 || !mapped.insert(mask) {
            return Err("Mario unlockItems must map each locked move exactly once".into());
        }
        if sd
            .get("apIdsToItemIds")
            .and_then(Value::as_object)
            .is_some_and(|items| items.contains_key(id))
        {
            return Err("Mario unlock item must not also grant an Elden Ring item".into());
        }
        unlock_items.insert(ap_id, mask);
    }
    if mapped.iter().copied().fold(0, |a, b| a | b) != managed {
        return Err("Mario unlockItems is missing a locked move".into());
    }
    Ok(Some(Config {
        managed,
        unlock_items,
    }))
}

#[derive(Default, Debug)]
pub struct Session {
    pub config: Option<Config>,
    pub identity: Option<String>,
    pub unlocked: u32,
}
impl Session {
    pub fn configure(&mut self, identity: String, config: Option<Config>) {
        if self.identity.as_ref() != Some(&identity) || self.config != config {
            self.unlocked = 0;
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
            for (index, id) in items {
                if !seen.insert(index) {
                    continue;
                }
                if let Some(&mask) = config.unlock_items.get(&id) {
                    if mask == 129 {
                        jumps += 1;
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
        }
    }
    pub fn is_unlock(&self, id: i64) -> bool {
        self.config
            .as_ref()
            .is_some_and(|config| config.unlock_items.contains_key(&id))
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
    #[test]
    fn old_seed_defaults_off() {
        assert_eq!(parse(&json!({})), Ok(None));
        assert_eq!(parse(&json!({"options":{"mario_mode":0}})), Ok(None));
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
