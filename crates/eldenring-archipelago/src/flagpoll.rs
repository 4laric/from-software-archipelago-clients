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
    // Legacy baker-written apconfig.json (location_flags/sweep_flags) -- usually absent now.
    merge_table_file(&mut cfg, &dir.join("apconfig.json"));
    // PURE-RUNTIME BRIDGE (2026-07-01): the SEED-INDEPENDENT static detection table, if the user
    // drops it next to the DLL, supplies the sweep groups the retired baker used to write into
    // apconfig (overworld/castle tiers -- e.g. Castle Morne, flag 1044320800). slot_data
    // locationFlags still wins for per-location flags (merged over this in core.rs); members not
    // in this seed are filtered by valid_locations at poll time. Durable fix = emit sweepFlags
    // in slot_data (contract work).
    let static_path = dir.join("er_static_detection_table.json");
    if static_path.exists() {
        merge_table_file(&mut cfg, &static_path);
    } else {
        // R9 (SWEEP): this table is env-dependent -- say so once instead of only hinting via
        // the count line below. (apconfig.json absence above stays silent: absent by design.)
        log::info!(
            "flag-poll: static detection table absent at {} -- sweep groups limited to slot_data",
            static_path.display()
        );
    }
    log::info!(
        "flag-poll config: {} location flags, {} sweep flags",
        cfg.location_flags.len(),
        cfg.sweep_flags.len()
    );
    cfg
}

/// Merge `location_flags` + `sweep_flags` from a JSON file into [cfg]. Tolerant: missing
/// file/keys -> no-op. Later files win per key.
fn merge_table_file(cfg: &mut FlagPollConfig, path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        log::warn!("flag-poll: {} exists but is not valid JSON -- ignored", path.display());
        return;
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
    log::info!("flag-poll: merged table {}", path.display());
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

/// Parse `dungeonSweepFlags` out of slot_data: boss-defeat event flag -> member AP location ids.
/// Greenfield's flag-keyed dungeon sweeps: when the flag fires (boss killed) every member is checked.
/// Rides the same `sweep_flags` poll path the legacy apconfig table used.
pub fn parse_sweep_flags(sd: &Value) -> HashMap<u32, Vec<i64>> {
    let mut m = HashMap::new();
    if let Some(obj) = sd.get("dungeonSweepFlags").and_then(|x| x.as_object()) {
        for (k, val) in obj {
            if let (Ok(flag), Some(arr)) = (k.parse::<u32>(), val.as_array()) {
                m.insert(flag, arr.iter().filter_map(|x| x.as_i64()).collect());
            }
        }
    }
    m
}

/// Parse `sweepLockGates` out of slot_data: sweep-trigger EVENT FLAG (the SAME key space as
/// `dungeonSweepFlags` — the boss-defeat flag that triggers the sweep) -> boss-lock item NAME that
/// must be in the cumulative received set before that flag's dungeon sweep fires (BOSS_LOCKS_PATCH,
/// SPEC-region-capstone-model §7). Absent key / empty map = every sweep ungated.
///
/// KEY-SPACE FIX (2026-07-08): was parsed as an i64 "AP location" and only ever consumed in the
/// location-keyed `dungeon_sweeps` loop, which the apworld emits EMPTY (`dungeonSweeps = {}`), so
/// the defer silently never fired. It is keyed by the sweep-trigger FLAG and must gate the
/// flag-keyed `sweep_flags` loop (the live path fed by `dungeonSweepFlags`). See core.rs §5b.
pub fn parse_sweep_lock_gates(sd: &Value) -> HashMap<u32, String> {
    let mut m = HashMap::new();
    if let Some(obj) = sd.get("sweepLockGates").and_then(|x| x.as_object()) {
        for (k, val) in obj {
            if let (Ok(flag), Some(name)) = (k.parse::<u32>(), val.as_str()) {
                m.insert(flag, name.to_string());
            }
        }
    }
    m
}
