//! Pure save-file round-trip, extracted from `grant.rs` `write_save` / `configure`.
//!
//! The serialized shape mirrors `write_save` exactly. `BTree*` collections are used only so the
//! struct has a deterministic `PartialEq` / JSON ordering for tests; the live code may keep
//! `HashSet`/`HashMap` and scatter/gather through this type.

use std::collections::{BTreeMap, BTreeSet};

/// Everything persisted per save, round-tripped through `apconfig`-adjacent JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct SaveState {
    pub last_received_index: i64,
    pub start_items_granted: bool,
    pub notify_granted: BTreeSet<i32>,
    /// Fresh-save flag-poll baseline (gf-flagpoll-baseline-persist): the guarding acquisition
    /// flags that already read SET on the FIRST in-world poll of a genuinely fresh save. Persisted
    /// once so a mid-playthrough RECONNECT reloads this ORIGINAL set instead of re-snapshotting the
    /// progressed flag state (which would exclude every prior pickup's check forever -- the
    /// check-eater bug). Empty on a save that has not captured one yet.
    pub flag_poll_baseline: BTreeSet<u32>,
    pub progressive_counter: BTreeMap<String, i32>,
    pub progressive_high_index: i64,
}

impl SaveState {
    /// Exactly the object shape written by `write_save`.
    pub fn to_json(&self) -> String {
        let notify: Vec<i32> = self.notify_granted.iter().copied().collect();
        let flag_poll_baseline: Vec<u32> = self.flag_poll_baseline.iter().copied().collect();
        let counter: serde_json::Map<String, serde_json::Value> = self
            .progressive_counter
            .iter()
            .map(|(k, &v)| (k.clone(), serde_json::Value::from(v)))
            .collect();
        serde_json::json!({
            "last_received_index":    self.last_received_index,
            "start_items_granted":    self.start_items_granted,
            "notify_granted":         notify,
            "flag_poll_baseline":     flag_poll_baseline,
            "progressive_counter":    serde_json::Value::Object(counter),
            "progressive_high_index": self.progressive_high_index,
        })
        .to_string()
    }

    /// Tolerant load mirroring `configure` / `load_last_index` / `progressive::restore` defaults.
    /// A malformed or partial save never panics — it falls back to documented defaults.
    pub fn from_json(text: &str) -> Self {
        let v: serde_json::Value = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
        let notify = v
            .get("notify_granted")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|n| n.as_i64().map(|n| n as i32))
                    .collect()
            })
            .unwrap_or_default();
        let flag_poll_baseline = v
            .get("flag_poll_baseline")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|n| n.as_u64().map(|n| n as u32))
                    .collect()
            })
            .unwrap_or_default();
        let counter = v
            .get("progressive_counter")
            .and_then(|x| x.as_object())
            .map(|o| {
                o.iter()
                    .filter_map(|(k, n)| n.as_i64().map(|n| (k.clone(), n as i32)))
                    .collect()
            })
            .unwrap_or_default();
        SaveState {
            last_received_index: v
                .get("last_received_index")
                .and_then(|x| x.as_i64())
                .unwrap_or(0),
            start_items_granted: v
                .get("start_items_granted")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
            notify_granted: notify,
            flag_poll_baseline,
            progressive_counter: counter,
            progressive_high_index: v
                .get("progressive_high_index")
                .and_then(|x| x.as_i64())
                .unwrap_or(-1),
        }
    }
}

impl Default for SaveState {
    /// Fresh save: nothing granted, high-index sentinel -1 (matches `from_json`'s absent-key default).
    fn default() -> Self {
        SaveState {
            last_received_index: 0,
            start_items_granted: false,
            notify_granted: std::collections::BTreeSet::new(),
            flag_poll_baseline: std::collections::BTreeSet::new(),
            progressive_counter: std::collections::BTreeMap::new(),
            progressive_high_index: -1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_state_round_trips() {
        let mut counter = BTreeMap::new();
        counter.insert("progressive_physick".to_string(), 3);
        counter.insert("progressive_stone_bell".to_string(), 1);
        let mut notify = BTreeSet::new();
        notify.insert(0x4000_0B5B);
        notify.insert(-42); // signed FullIDs are legal; must survive serde
        let mut flag_poll_baseline = BTreeSet::new();
        flag_poll_baseline.insert(60000u32); // Flask of Crimson Tears -- a fresh-save default
        flag_poll_baseline.insert(60020u32); // Wondrous Physick
        flag_poll_baseline.insert(400357u32); // Black Knifeprint -- high id, must survive serde

        let before = SaveState {
            last_received_index: 17,
            start_items_granted: true,
            notify_granted: notify,
            flag_poll_baseline,
            progressive_counter: counter,
            progressive_high_index: 16,
        };
        let after = SaveState::from_json(&before.to_json());
        assert_eq!(
            before, after,
            "save -> JSON -> load must preserve every field"
        );
    }

    #[test]
    fn absent_keys_get_documented_defaults() {
        // A Phase-4 (single-field) save predates the Phase-5 keys; load must not panic.
        let legacy = r#"{"last_received_index": 5}"#;
        let s = SaveState::from_json(legacy);
        assert_eq!(s.last_received_index, 5);
        assert!(!s.start_items_granted);
        assert!(s.notify_granted.is_empty());
        assert!(s.flag_poll_baseline.is_empty(), "absent flag_poll_baseline -> empty default");
        assert!(s.progressive_counter.is_empty());
        assert_eq!(s.progressive_high_index, -1, "default high-index is -1");
    }

    #[test]
    fn malformed_json_loads_as_defaults_not_panic() {
        let s = SaveState::from_json("{ this is not json");
        assert_eq!(s.last_received_index, 0);
        assert_eq!(s.progressive_high_index, -1);
    }
}
