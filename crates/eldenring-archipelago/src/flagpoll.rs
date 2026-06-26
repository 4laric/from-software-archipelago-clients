//! Flag-polling for checks whose acquisition BYPASSES the AddItemFunc detour AND doesn't leave a
//! synthetic in the bag — NPC gifts, NPC death drops, quest-step rewards, offline pickups. Ported
//! from the standalone `features.rs::poll_location_flags`.
//!
//! Each AP location has a guarding vanilla event flag; when that flag fires (you killed Alexander,
//! Boc's quest advanced), the location is checked. The map is emitted BY THE BAKE into `apconfig.json`
//! (`location_flags`, `sweep_flags`) — we read it from the same file shared reads, via a separate
//! tolerant parse so we don't disturb shared's typed `Config`. Boss/dungeon SWEEPS ride along.

use std::collections::HashMap;

use serde_json::Value;

/// Bake-emitted flag maps from `apconfig.json`.
#[derive(Default)]
pub struct FlagPollConfig {
    /// AP location id -> guarding event flag.
    pub location_flags: HashMap<i64, u32>,
    /// Boss-attribution sweep: event flag -> AP location ids it clears.
    pub sweep_flags: HashMap<u32, Vec<i64>>,
}

/// Read `location_flags` + `sweep_flags` out of `apconfig.json` (the file shared loads for url/slot).
/// Tolerant: missing file/keys -> empty config (flag-polling simply does nothing).
pub fn load() -> FlagPollConfig {
    let mut cfg = FlagPollConfig::default();
    let Ok(dir) = shared::utils::mod_directory() else {
        return cfg;
    };
    let Ok(text) = std::fs::read_to_string(dir.join("apconfig.json")) else {
        return cfg;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return cfg;
    };
    if let Some(obj) = v.get("location_flags").and_then(|x| x.as_object()) {
        for (k, val) in obj {
            if let (Ok(loc), Some(flag)) = (k.parse::<i64>(), val.as_u64()) {
                cfg.location_flags.insert(loc, flag as u32);
            }
        }
    }
    if let Some(obj) = v.get("sweep_flags").and_then(|x| x.as_object()) {
        for (k, val) in obj {
            if let (Ok(flag), Some(arr)) = (k.parse::<u32>(), val.as_array()) {
                cfg.sweep_flags
                    .insert(flag, arr.iter().filter_map(|x| x.as_i64()).collect());
            }
        }
    }
    log::info!(
        "flag-poll config: {} location flags, {} sweep flags",
        cfg.location_flags.len(),
        cfg.sweep_flags.len()
    );
    cfg
}

/// Parse `dungeonSweeps` out of slot_data: trigger AP location -> member AP location ids. When the
/// trigger's guarding flag fires, every member is also checked (clears a dungeon in one boss kill).
pub fn parse_dungeon_sweeps(sd: &Value) -> HashMap<i64, Vec<i64>> {
    let mut m = HashMap::new();
    if let Some(obj) = sd.get("dungeonSweeps").and_then(|x| x.as_object()) {
        for (k, val) in obj {
            if let (Ok(trigger), Some(arr)) = (k.parse::<i64>(), val.as_array()) {
                m.insert(trigger, arr.iter().filter_map(|x| x.as_i64()).collect());
            }
        }
    }
    m
}
