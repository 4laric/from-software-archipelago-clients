//! Start-of-run grants the standalone applied at connect: start items (Torrent + flasks etc.), start
//! graces (the Limgrave graces unlocked so you can warp), and `reveal_all_maps` (map_option=give).
//! Ported from the standalone `features.rs` (drain_start_items / queue_start_graces / reveal_all_maps).
//!
//! Flags are idempotent + save-persisted, so setting them every connect is harmless. Start ITEMS are
//! gated once-per-save (persisted) since they are NOT replayed through the received-item stream.

use serde_json::Value;

use crate::flags;

/// Base-game world-map reveal flags (standalone `MAP_UNLOCK_FLAGS`, map_id < 2_000_000).
const MAP_REVEAL_FLAGS_BASE: &[u32] = &[
    62010, 62011, 62012, // Limgrave W, Weeping, Limgrave E
    62020, 62021, 62022, // Liurnia E/N/W
    62030, 62031, 62032, // Altus, Leyndell, Gelmir
    62040, 62041, // Caelid, Dragonbarrow
    62050, 62051, 62052, // Mountaintops W/E, Snowfield
    62060, 62061, 62062, 62063, 62064, // Ainsel, Lake of Rot, Mohgwyn, Siofra, Deeproot
];
/// DLC world-map reveal flags (Land of Shadow pieces); only set when DLC is enabled.
const MAP_REVEAL_FLAGS_DLC: &[u32] = &[62080, 62081, 62082, 62083, 62084];
/// Underground (Underworld) map VIEW-unlock flag -- distinct from the per-region map FRAGMENT
/// flags above: without it the underground map layer never displays even when the fragments
/// (62060-62064) are set. (CE [[EventFlagMan]+0x28]+0xFA0 bit6; id via the offset->id formula,
/// confirmed live 2026-07-04. Verify with a set->readback the first time you build.)
const UNDERGROUND_MAP_VIEW_UNLOCK: u32 = 82001;

#[derive(Default)]
pub struct StartConfig {
    pub start_items: Vec<i32>,  // FullIDs (Torrent = 0x40000000 | 130, etc.)
    pub start_graces: Vec<u32>, // grace flags to set at start
    pub reveal_all_maps: bool,
    pub enable_dlc: bool,
}

pub fn parse(sd: &Value) -> StartConfig {
    let arr_i32 = |v: Option<&Value>| {
        v.and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|n| n.as_i64().map(|n| n as i32))
                    .collect()
            })
            .unwrap_or_default()
    };
    let arr_u32 = |v: Option<&Value>| {
        v.and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|n| n.as_u64().map(|n| n as u32))
                    .collect()
            })
            .unwrap_or_default()
    };
    StartConfig {
        start_items: arr_i32(sd.get("startItems")),
        start_graces: arr_u32(sd.get("startGraces")),
        reveal_all_maps: sd
            .get("reveal_all_maps")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        // int-or-bool tolerant: the apworld ships options.enable_dlc as an int (1/0), which
        // .as_bool() read as absent — silently skipping the DLC map-reveal flags. The
        // top-level key stays as a bool fallback for older seeds.
        enable_dlc: er_logic::options::parse_dlc(sd)
            || sd.get("enable_dlc").and_then(|v| v.as_bool()).unwrap_or(false),
    }
}

/// Set start graces + (if requested) all map-reveal flags. Idempotent. Returns false if the flag
/// holder isn't up yet (caller retries next tick), true once everything is set.
pub fn apply_start_flags(cfg: &StartConfig) -> bool {
    for &f in &cfg.start_graces {
        if !flags::try_set_event_flag(f, true) {
            return false;
        }
    }
    // Underground (Underworld) map VIEW unlock. The underground map layer will NOT display
    // even with its fragment flags (62060-62064) set unless this flag is on -- root cause of
    // the underground-map-won't-paint bug (pinned live via CE flag-logger bisection 2026-07-04).
    // Set unconditionally at connect: it only makes the underground map viewable (the fill still
    // gates on the per-region fragment flags), so it covers BOTH reveal_all_maps and the
    // progressive per-region unlock path. See memory er-underground-map-quadrant-flags.
    if !flags::try_set_event_flag(UNDERGROUND_MAP_VIEW_UNLOCK, true) {
        return false;
    }
    if cfg.reveal_all_maps {
        for &f in MAP_REVEAL_FLAGS_BASE {
            if !flags::try_set_event_flag(f, true) {
                return false;
            }
        }
        if cfg.enable_dlc {
            for &f in MAP_REVEAL_FLAGS_DLC {
                if !flags::try_set_event_flag(f, true) {
                    return false;
                }
            }
        }
    }
    true
}
