//! Event-flag get/set + player region accessor, re-homed from the standalone `eldenring-ap/game/
//! flags.rs` (the report-channel half is gone — checks now go straight to `mark_checked`). Symbols
//! resolved against the `eldenring` crate (0.14): flags live on `CSEventFlagMan.virtual_memory_flag`;
//! the current region is `WorldChrMan.main_player.play_region_id`.

use eldenring::cs::{CSEventFlagMan, WorldChrMan};
use fromsoftware_shared::FromStatic;

/// Read an event flag (true = set). Returns false before `CSEventFlagMan` initializes.
pub fn get_event_flag(flag_id: u32) -> bool {
    match unsafe { CSEventFlagMan::instance() } {
        Ok(m) => m.virtual_memory_flag.get_flag(flag_id),
        Err(_) => false,
    }
}

/// Set an event flag. Idempotent + save-persisted, so replaying on reconnect is harmless.
pub fn set_event_flag(flag_id: u32, enabled: bool) {
    let _ = try_set_event_flag(flag_id, enabled);
}

/// Set an event flag, returning whether the holder was ready (false = retry later).
pub fn try_set_event_flag(flag_id: u32, enabled: bool) -> bool {
    match unsafe { CSEventFlagMan::instance_mut() } {
        Ok(m) => {
            m.virtual_memory_flag.set_flag(flag_id, enabled);
            true
        }
        Err(_) => false,
    }
}

/// Player's current `PlayRegionId`, or `None` if not in-world.
pub fn play_region_id() -> Option<i32> {
    let wcm = unsafe { WorldChrMan::instance() }.ok()?;
    Some(wcm.main_player.as_ref()?.play_region_id as i32)
}

/// True once the player is loaded into the world.
pub fn in_world() -> bool {
    play_region_id().is_some()
}
