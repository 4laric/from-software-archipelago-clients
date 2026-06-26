//! Region locks (Milestone B Stage 3). The pure decision is `er_logic::region_lock::kick_decision`
//! (host-tested); this module is the game-side glue: parse the region config out of slot_data, set
//! the baked KICK flag (76970) when the player is in a locked region, and open regions on receipt of
//! their unlock item.
//!
//! Enforcement is the BAKE's job: the client only SETS flag 76970; the baked `common.emevd` reactor
//! warps the player out to a safe Limgrave grace. We never warp from the client.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use crate::flags;

/// Baked common.emevd reactor flag: set while the player is in a locked region with its open-flag off.
const KICK_FLAG: u32 = 76970;

/// Latch so we set the KICK flag once per lock-entry, not every tick.
static KICK_LATCHED: AtomicBool = AtomicBool::new(false);

/// Region-lock config, parsed from slot_data (shapes mirror the standalone `net.rs`).
#[derive(Default)]
pub struct RegionConfig {
    /// `[lo, hi, open_flag]` inclusive 5-digit subregion ranges; locked when the open flag is off.
    pub area_lock_flags: Vec<[i32; 3]>,
    /// `0` = non-random seed (no start guard); else KICK waits until this flag is set.
    pub random_start_done_flag: u32,
    /// lock item name -> the region's physical open flag.
    pub region_open_flags: HashMap<String, u32>,
    /// lock item name -> map-reveal / enforcement-open flags.
    pub lock_reveal_flags: HashMap<String, Vec<u32>>,
    /// lock item name -> grace warp-unlock flags.
    pub region_graces: HashMap<String, Vec<u32>>,
}

pub fn parse(sd: &Value) -> RegionConfig {
    RegionConfig {
        area_lock_flags: parse_triples(sd.get("areaLockFlags")),
        random_start_done_flag: sd.get("randomStartDoneFlag").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        region_open_flags: str_to_u32(sd.get("regionOpenFlags")),
        lock_reveal_flags: str_to_u32vec(sd.get("lockRevealFlags")),
        region_graces: str_to_u32vec(sd.get("regionGraces")),
    }
}

/// Per-tick: if the player is in a locked region, set the KICK flag once (the bake warps them out).
pub fn tick_kick(cfg: &RegionConfig) {
    let pr = match flags::play_region_id() {
        Some(p) => p,
        None => return,
    };
    let kick = er_logic::region_lock::kick_decision(
        pr,
        &cfg.area_lock_flags,
        cfg.random_start_done_flag,
        &|f| flags::get_event_flag(f),
    );
    if kick {
        if !KICK_LATCHED.swap(true, Ordering::Relaxed) {
            flags::set_event_flag(KICK_FLAG, true);
            log::info!("RegionLock: area {pr} LOCKED -> set KICK flag {KICK_FLAG}");
        }
    } else {
        KICK_LATCHED.store(false, Ordering::Relaxed);
    }
}

/// On receiving an unlock item (by name): open its region + reveal/grace flags. Idempotent. Returns
/// true if `name` is a region-lock item (so the caller can surface a console notification).
pub fn open_on_received_name(cfg: &RegionConfig, name: &str) -> bool {
    let mut opened = false;
    if let Some(&f) = cfg.region_open_flags.get(name) {
        flags::set_event_flag(f, true);
        log::info!("RegionLock '{name}' received -> open flag {f}");
        opened = true;
    }
    if let Some(fs) = cfg.lock_reveal_flags.get(name) {
        for &f in fs {
            flags::set_event_flag(f, true);
        }
    }
    if let Some(fs) = cfg.region_graces.get(name) {
        for &f in fs {
            flags::set_event_flag(f, true);
        }
    }
    opened
}

// --- slot_data parse helpers (shapes from the standalone net.rs) ---------------------------------

fn parse_triples(v: Option<&Value>) -> Vec<[i32; 3]> {
    v.and_then(|v| v.as_array())
        .map(|outer| {
            outer
                .iter()
                .filter_map(|row| row.as_array())
                .filter(|r| r.len() >= 3)
                .map(|r| {
                    [
                        r[0].as_i64().unwrap_or(0) as i32,
                        r[1].as_i64().unwrap_or(0) as i32,
                        r[2].as_i64().unwrap_or(0) as i32,
                    ]
                })
                .collect()
        })
        .unwrap_or_default()
}

fn str_to_u32(v: Option<&Value>) -> HashMap<String, u32> {
    let mut m = HashMap::new();
    if let Some(Value::Object(o)) = v {
        for (k, val) in o {
            if let Some(n) = val.as_u64() {
                m.insert(k.clone(), n as u32);
            }
        }
    }
    m
}

fn str_to_u32vec(v: Option<&Value>) -> HashMap<String, Vec<u32>> {
    let mut m = HashMap::new();
    if let Some(Value::Object(o)) = v {
        for (k, val) in o {
            if let Some(arr) = val.as_array() {
                m.insert(
                    k.clone(),
                    arr.iter().filter_map(|x| x.as_u64().map(|n| n as u32)).collect(),
                );
            }
        }
    }
    m
}
