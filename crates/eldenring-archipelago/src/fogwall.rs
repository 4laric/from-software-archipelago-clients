//! Region-lock fog-wall VISUAL (MVP). Draws a shimmering fog plane at a locked region boundary as a
//! cosmetic marker so the silent play-region KICK ([[region]]) reads as a sealed border instead of an
//! unexplained teleport. **This module never blocks or enforces anything** — enforcement stays with
//! the baked KICK reactor (flag 76970); the fog is decoupled from detection, so it only has to RENDER,
//! not be area-tested (the area-test approach failed ~10 cycles; see SPEC-region-lock-fogwall-visual.md).
//!
//! Mechanism (proven runtime path, dodges the baker's collision-bundle hang): the `eldenring` 0.14
//! crate exposes `CSWorldGeomMan` → `geom_block_data_by_id_mut(&block_id)` →
//! `spawn_geometry(asset, &GeometrySpawnParameters{ position, rot, scale })`, the same live
//! asset-instantiation call the crate's `spawn-asset` example uses to place `AEG099_831`. We spawn an
//! `AEG099_052` fog plane (the boss-fog VISUAL model — its collision lives in a *separate* hkx hull that
//! we deliberately do NOT bring, so the plane is walk-through: exactly right for a visual-only marker).
//!
//! Placement is DATA, never hardcoded blind: a wall needs a `BlockId` (map tile) + local
//! `BlockPosition`, which must be captured in-game at the real boundary (the spec is explicit —
//! "verify in-game, don't hardcode"). Turn on capture logging (`fogWallDebug` slot_data, or flip
//! `FORCE_DEBUG_CAPTURE`) and ride to the boundary; the log prints the exact `block_id` + `x,y,z` to
//! drop into a wall entry. MVP first target: the Limgrave↔Caelid seam in front of the Smoldering
//! Church (where Anastasia, Tarnished-Eater invades).
//!
//! Walls come from two sources, merged: compile-time `BUILTIN_WALLS` (for the fastest client-only
//! iteration loop — no apworld/gen round-trip) and slot_data `fogWalls` (the shippable path once a
//! transform is known). A wall shows while its `open_flag` is OFF; once the region's lock item is
//! received (flag set by [[region]]::open_on_received_name) we stop respawning it.

use eldenring::cs::{CSWorldGeomMan, GeometrySpawnParameters, WorldChrMan};
use eldenring::position::BlockPosition;
use fromsoftware_shared::FromStatic;
use serde_json::Value;

use crate::flags;

/// Default fog model: the boss-fog visual plane. Collision is a paired hkx hull we don't spawn, so
/// this renders as a translucent, *passable* wall — correct for a marker (the KICK does the blocking).
const DEFAULT_ASSET: &str = "AEG099_052";

/// Flip to `true` for a capture ride when you have no way to set `fogWallDebug` in slot_data. Logs the
/// player's current block id + local position every time the block changes, so you can read off the
/// transform to place a wall. Leave `false` for shipped builds (it's chatty).
const FORCE_DEBUG_CAPTURE: bool = false;

/// Compile-time walls, merged ahead of slot_data `fogWalls`. Empty by default: fill an entry AFTER you
/// capture a real transform (debug capture prints it). Template for the MVP Limgrave↔Caelid seam —
/// replace the `TODO` numbers with the captured `block_id` / `x,y,z`, then set `placed: true`:
///
/// ```ignore
/// FogWall::builtin("Caelid seam (Smoldering Church)", /*open_flag*/ 0, DEFAULT_ASSET,
///     /*block_id*/ 0x00, /*x*/ 0.0, /*y*/ 0.0, /*z*/ 0.0, /*rot_y*/ 0.0, /*scale*/ 4.0),
/// ```
///
/// `open_flag` = the Caelid region's physical open flag (from slot_data `regionOpenFlags["Caelid Lock"]`
/// — read it off the connect log's region config, do not guess).
const BUILTIN_WALLS: &[FogWallStatic] = &[
    // (none yet — capture the transform first)
    // FogWall::builtin("Caelid seam (Smoldering Church)", 0, DEFAULT_ASSET, 0x00, 0.0, 0.0, 0.0, 0.0, 4.0),
];

/// One fog-wall placement. `placed == false` (no block/position) means debug-only: it is never spawned,
/// only used to remind you it still needs a transform.
#[derive(Clone)]
pub struct FogWall {
    /// Human label for logs.
    label: String,
    /// Region open flag; the wall is drawn while this is OFF, retired once it's set.
    open_flag: u32,
    /// AEG099 fog asset model name.
    asset: String,
    /// Raw `BlockId` (i32) of the map tile the wall lives in. `None` = unplaced.
    block_id: Option<i32>,
    /// Local block-space position of the wall. `None` = unplaced.
    pos: Option<(f32, f32, f32)>,
    /// Euler rotation (degrees, game convention) about x / y / z.
    rot: (f32, f32, f32),
    /// Per-axis scale.
    scale: (f32, f32, f32),
    /// Runtime latch: true once spawned into the currently-loaded block; re-armed when that block
    /// unloads (so re-approaching a still-locked border re-draws the wall, without stacking dupes
    /// while the block stays loaded).
    spawned: bool,
}

impl FogWall {
    /// `const`-friendly builder for `BUILTIN_WALLS`. Kept `const fn`; `label`/`asset` stay `&'static str`
    /// here and are converted to `String` when the table is cloned into the config.
    #[allow(clippy::too_many_arguments, dead_code)]
    const fn builtin(
        label: &'static str,
        open_flag: u32,
        asset: &'static str,
        block_id: i32,
        x: f32,
        y: f32,
        z: f32,
        rot_y: f32,
        scale: f32,
    ) -> FogWallStatic {
        FogWallStatic {
            label,
            open_flag,
            asset,
            block_id,
            x,
            y,
            z,
            rot_y,
            scale,
        }
    }

    fn placed(&self) -> bool {
        self.block_id.is_some() && self.pos.is_some()
    }
}

/// `const`-constructible mirror of [`FogWall`] for the compile-time table (String isn't const-buildable).
pub struct FogWallStatic {
    label: &'static str,
    open_flag: u32,
    asset: &'static str,
    block_id: i32,
    x: f32,
    y: f32,
    z: f32,
    rot_y: f32,
    scale: f32,
}

impl From<&FogWallStatic> for FogWall {
    fn from(s: &FogWallStatic) -> Self {
        FogWall {
            label: s.label.to_string(),
            open_flag: s.open_flag,
            asset: s.asset.to_string(),
            block_id: Some(s.block_id),
            pos: Some((s.x, s.y, s.z)),
            rot: (0.0, s.rot_y, 0.0),
            scale: (s.scale, s.scale, s.scale),
            spawned: false,
        }
    }
}

#[derive(Default)]
pub struct FogWallConfig {
    walls: Vec<FogWall>,
    /// Log the player's block id + position on every block change, to capture wall transforms.
    debug: bool,
    /// Last block id we logged in debug mode (dedup so we only print on change).
    last_logged_block: i32,
}

/// Parse the fog-wall config out of slot_data and merge the compile-time `BUILTIN_WALLS` ahead of it.
/// Absent `fogWalls` is fine (the common case until a transform is captured) — debug capture still runs.
pub fn parse(sd: &Value) -> FogWallConfig {
    let mut walls: Vec<FogWall> = BUILTIN_WALLS.iter().map(FogWall::from).collect();
    if let Some(arr) = sd.get("fogWalls").and_then(|v| v.as_array()) {
        for w in arr {
            if let Some(fw) = parse_wall(w) {
                walls.push(fw);
            }
        }
    }
    let debug = FORCE_DEBUG_CAPTURE
        || sd
            .get("fogWallDebug")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    let placed = walls.iter().filter(|w| w.placed()).count();
    log::info!(
        "fogwall: {} wall(s) configured ({placed} placed, {} awaiting transform), debug_capture={debug}",
        walls.len(),
        walls.len() - placed
    );
    FogWallConfig {
        walls,
        debug,
        last_logged_block: i32::MIN,
    }
}

fn parse_wall(w: &Value) -> Option<FogWall> {
    let open_flag = w.get("openFlag").and_then(|v| v.as_u64())? as u32;
    let asset = w
        .get("asset")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_ASSET)
        .to_string();
    let label = w
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("fogwall")
        .to_string();

    // Block id: accept a raw i32 `blockId`, or a 4-part `block` = [area, block, region, index].
    let block_id = w
        .get("blockId")
        .and_then(|v| v.as_i64())
        .map(|n| n as i32)
        .or_else(|| {
            w.get("block")
                .and_then(|v| v.as_array())
                .filter(|a| a.len() == 4)
                .map(|a| {
                    let g = |i: usize| a[i].as_i64().unwrap_or(0) as i32 & 0xFF;
                    (g(0) << 24) | (g(1) << 16) | (g(2) << 8) | g(3)
                })
        });

    let pos = match (
        w.get("x").and_then(Value::as_f64),
        w.get("y").and_then(Value::as_f64),
        w.get("z").and_then(Value::as_f64),
    ) {
        (Some(x), Some(y), Some(z)) => Some((x as f32, y as f32, z as f32)),
        _ => None,
    };

    let f = |k: &str, d: f32| {
        w.get(k)
            .and_then(Value::as_f64)
            .map(|v| v as f32)
            .unwrap_or(d)
    };
    let uni = f("scale", 1.0);
    let scale = (f("scaleX", uni), f("scaleY", uni), f("scaleZ", uni));
    let rot = (f("rotX", 0.0), f("rotY", 0.0), f("rotZ", 0.0));

    Some(FogWall {
        label,
        open_flag,
        asset,
        block_id,
        pos,
        rot,
        scale,
        spawned: false,
    })
}

/// Read the local player's `(current_block_id, block_position)`, or `None` if not in-world.
fn player_block_pos() -> Option<(i32, BlockPosition)> {
    let wcm = unsafe { WorldChrMan::instance() }.ok()?;
    let p = wcm.main_player.as_ref()?;
    Some((i32::from(p.current_block_id), p.block_position))
}

/// Per-tick (call in-world, on the game thread). For each placed wall whose region is still locked and
/// whose tile is currently loaded, spawn the fog asset once; re-arm when the tile unloads. Also drives
/// the debug transform-capture log. Cheap: no game calls once every wall is either spawned or unplaced.
pub fn tick(cfg: &mut FogWallConfig) {
    let Some((cur_block, cur_pos)) = player_block_pos() else {
        return;
    };

    if cfg.debug && cur_block != cfg.last_logged_block {
        cfg.last_logged_block = cur_block;
        let b = eldenring::cs::BlockId::from(cur_block);
        log::info!(
            "fogwall/capture: block={b} (0x{cur_block:08X}) pos=({:.2}, {:.2}, {:.2})",
            cur_pos.x,
            cur_pos.y,
            cur_pos.z
        );
    }

    for wall in cfg.walls.iter_mut() {
        let (Some(block_raw), Some((x, y, z))) = (wall.block_id, wall.pos) else {
            continue; // unplaced: debug-only
        };

        // Retired: the lock item was received (open flag set). Don't respawn. (An already-spawned
        // instance is left as-is; it vanishes when the tile next reloads.)
        if flags::get_event_flag(wall.open_flag) {
            wall.spawned = false; // so a fresh lock in a new seed re-arms cleanly
            continue;
        }

        let block_id = eldenring::cs::BlockId::from(block_raw);

        // Grab the block's geometry container. `None` = that tile isn't loaded (player not near the
        // border) → nothing to draw, and re-arm the latch so we respawn on re-approach.
        let Some(block_data) = (unsafe { CSWorldGeomMan::instance_mut() })
            .ok()
            .and_then(|wgm| wgm.geom_block_data_by_id_mut(&block_id))
        else {
            wall.spawned = false;
            continue;
        };

        if wall.spawned {
            continue; // already drawn into the currently-loaded tile
        }

        let ins = block_data.spawn_geometry(
            &wall.asset,
            &GeometrySpawnParameters {
                position: BlockPosition::from_xyz(x, y, z),
                rot_x: wall.rot.0,
                rot_y: wall.rot.1,
                rot_z: wall.rot.2,
                scale_x: wall.scale.0,
                scale_y: wall.scale.1,
                scale_z: wall.scale.2,
            },
        );
        wall.spawned = true;
        if let Some(mut nn) = ins {
            // Boss-fog assets are flagged `disable_on_singleplay` ("hides the object whenever the
            // player is alone") — which would make our marker INVISIBLE in normal solo play. Clear
            // it on the fresh instance so the fog renders offline. This is the single biggest render
            // unknown from the spec; if the wall still doesn't show, that's the next thing to probe.
            unsafe { nn.as_mut().info.disable_on_singleplay = 0 };
            log::info!(
                "fogwall: drew '{}' ({} @ block {block_id}) — region locked (flag {} off)",
                wall.label,
                wall.asset,
                wall.open_flag
            );
        } else {
            log::warn!(
                "fogwall: spawn_geometry returned None for '{}' ({} @ block {block_id}) — asset/transform?",
                wall.label,
                wall.asset
            );
        }
    }
}
