//! Region locks (Milestone B Stage 3). The pure decision is `er_logic::region_lock::kick_decision`
//! (host-tested); this module is the game-side glue: parse the region config out of slot_data, set
//! the baked KICK flag (76970) when the player is in a locked region, and open regions on receipt of
//! their unlock item.
//!
//! Enforcement is the BAKE's job: the client only SETS flag 76970; the baked `common.emevd` reactor
//! warps the player out to a safe Limgrave grace. We never warp from the client.
//!
//! Random-start warp (ported from the standalone `features.rs`): the actual teleport is ALSO baked
//! (a `WarpPlayer` gated on `randomStartWarpFlag` 76969, dest derived at bake from the rolled
//! region's grace). The client's only job is to SET that trigger flag once, the same way DLC
//! auto-entry works: when the player is in the rolled start area, set `randomStartDoneFlag` (76968,
//! persistent guard) + `randomStartWarpFlag` (76969) a single time. This also unblocks KICK, whose
//! start-window guard waits on `randomStartDoneFlag` (see `kick_decision`). Without this trigger the
//! bake never warps and 76968 never flips, so region enforcement stays suppressed all run.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use crate::flags;

/// Baked common.emevd reactor flag: set while the player is in a locked region with its open-flag off.
const KICK_FLAG: u32 = 76970;

/// Latch so we set the KICK flag once per lock-entry, not every tick.
static KICK_LATCHED: AtomicBool = AtomicBool::new(false);

/// Latch so the random-start warp trigger fires once per session (the persistent `randomStartDoneFlag`
/// is the cross-session guard; this is the in-session dedup, mirroring the standalone `START_LATCHED`).
static START_LATCHED: AtomicBool = AtomicBool::new(false);

/// One disjunctive clause of a natural-key trigger: satisfied when ALL `items` were received AND ALL
/// `flags` are set. Ported from the standalone `features.rs::NkClause`.
#[derive(Default)]
pub struct NkClause {
    pub items: Vec<String>,
    pub flags: Vec<u32>,
}

/// Region-lock config, parsed from slot_data (shapes mirror the standalone `net.rs`).
#[derive(Default)]
pub struct RegionConfig {
    /// `[lo, hi, open_flag]` inclusive 5-digit subregion ranges; locked when the open flag is off.
    pub area_lock_flags: Vec<[i32; 3]>,
    /// `0` = non-random seed (no start guard); else KICK waits until this flag is set.
    pub random_start_done_flag: u32,
    /// `0` = no random start; else the baked warp trigger flag to set once you reach the start area.
    pub random_start_warp_flag: u32,
    /// `0` = no random start; else the play-region id of the rolled start area (where to fire the warp).
    pub random_start_area_id: i32,
    /// lock item name -> the region's physical open flag.
    pub region_open_flags: HashMap<String, u32>,
    /// lock item name -> map-reveal / enforcement-open flags.
    pub lock_reveal_flags: HashMap<String, Vec<u32>>,
    /// lock item name -> grace warp-unlock flags.
    pub region_graces: HashMap<String, Vec<u32>>,
    /// region (lock name) -> disjunction of natural-key clauses. When ANY clause holds, the region's
    /// apparatus blooms WITHOUT an AP lock item being received (vanilla keys / world flags). The
    /// region's open flag doubles as the once-latch. (Ported from the standalone naturalKeyTriggers.)
    pub natural_key_triggers: HashMap<String, Vec<NkClause>>,
}

pub fn parse(sd: &Value) -> RegionConfig {
    // Re-arm the random-start warp latch on each fresh parse (mirrors the standalone `configure`
    // per-connect reset) so a second seed loaded in the same game process can warp again. The
    // persistent `randomStartDoneFlag` still prevents a re-warp within one save.
    START_LATCHED.store(false, Ordering::Relaxed);
    RegionConfig {
        area_lock_flags: parse_triples(sd.get("areaLockFlags")),
        random_start_done_flag: sd
            .get("randomStartDoneFlag")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        random_start_warp_flag: sd
            .get("randomStartWarpFlag")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        random_start_area_id: sd
            .get("randomStartAreaId")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        region_open_flags: str_to_u32(sd.get("regionOpenFlags")),
        lock_reveal_flags: str_to_u32vec(sd.get("lockRevealFlags")),
        region_graces: str_to_u32vec(sd.get("regionGraces")),
        natural_key_triggers: parse_natural_keys(sd.get("naturalKeyTriggers")),
    }
}

/// Per-tick (settled / in-world): bloom regions whose natural-key trigger disjunction is now
/// satisfied. A clause fires when ALL its items are in `received` AND ALL its flags are set; ANY
/// clause fires the region. The region's open flag doubles as the once-latch, so this is idempotent
/// and cheap after the first bloom. Sets graces + open flag + reveal flags directly (the converged
/// client sets flags directly, unlike the standalone's queue). Mirrors `EvaluateNaturalKeyTriggers`.
pub fn tick_natural_key_triggers(cfg: &RegionConfig, received: &HashSet<String>) {
    if cfg.natural_key_triggers.is_empty() {
        return;
    }
    for (name, clauses) in &cfg.natural_key_triggers {
        let open_flag = match cfg.region_open_flags.get(name) {
            Some(&f) => f,
            None => continue, // no apparatus to bloom
        };
        if flags::get_event_flag(open_flag) {
            continue; // already bloomed (latch)
        }
        let fired = clauses.iter().any(|cl| {
            cl.items.iter().all(|nm| received.contains(nm))
                && cl.flags.iter().all(|&fl| flags::get_event_flag(fl))
        });
        if !fired {
            continue;
        }
        let mut n = 0u32;
        if let Some(fs) = cfg.region_graces.get(name) {
            for &f in fs {
                flags::set_event_flag(f, true);
                n += 1;
            }
        }
        flags::set_event_flag(open_flag, true);
        n += 1;
        if let Some(fs) = cfg.lock_reveal_flags.get(name) {
            for &f in fs {
                flags::set_event_flag(f, true);
                n += 1;
            }
        }
        log::info!("Natural-key '{name}' satisfied -> bloomed region ({n} flag(s) set)");
    }
}

/// `{ "LockName": { "anyOf": [ {"items":[..],"flags":[..]}, ... ] } }` -> region -> clause disjunction.
/// Ported from the standalone `net.rs::parse_natural_keys`.
fn parse_natural_keys(v: Option<&Value>) -> HashMap<String, Vec<NkClause>> {
    let mut m = HashMap::new();
    if let Some(Value::Object(o)) = v {
        for (region, body) in o {
            let mut clauses = Vec::new();
            if let Some(any_of) = body.get("anyOf").and_then(|x| x.as_array()) {
                for c in any_of {
                    let items = c
                        .get("items")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    let flags = c
                        .get("flags")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_u64().map(|n| n as u32))
                                .collect()
                        })
                        .unwrap_or_default();
                    clauses.push(NkClause { items, flags });
                }
            }
            m.insert(region.clone(), clauses);
        }
    }
    m
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

/// Per-tick: on a random-start seed, set the baked warp trigger ONCE when the player reaches the
/// rolled start area. Sets `randomStartDoneFlag` (persistent guard, also unblocks KICK) +
/// `randomStartWarpFlag` (the bake's `WarpPlayer` reactor keys on this). No-op on non-random seeds
/// (all three values are 0) or after the warp has fired. Mirrors the standalone `features.rs` latch.
pub fn tick_random_start_warp(cfg: &RegionConfig) {
    if cfg.random_start_warp_flag == 0
        || cfg.random_start_area_id == 0
        || cfg.random_start_done_flag == 0
    {
        return; // not a random-start seed
    }
    if flags::get_event_flag(cfg.random_start_done_flag) {
        return; // already warped (persisted across sessions)
    }
    let pr = match flags::play_region_id() {
        Some(p) => p,
        None => return,
    };
    if pr == cfg.random_start_area_id && !START_LATCHED.swap(true, Ordering::Relaxed) {
        flags::set_event_flag(cfg.random_start_done_flag, true);
        flags::set_event_flag(cfg.random_start_warp_flag, true);
        log::info!(
            "RandomStart: in start area {pr} -> set warp flag {} (done {})",
            cfg.random_start_warp_flag,
            cfg.random_start_done_flag
        );
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
                    arr.iter()
                        .filter_map(|x| x.as_u64().map(|n| n as u32))
                        .collect(),
                );
            }
        }
    }
    m
}
