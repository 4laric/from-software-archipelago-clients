//! Elden Ring Archipelago client, built on the fswap `shared` framework. `DllMain` hands off to the
//! shared lifecycle, which spawns the worker, builds the `Core`, schedules `Core::update` every
//! FrameBegin on the game thread, and applies the Hudhook overlay.

use std::ffi::c_void;

use windows::Win32::{Foundation::HINSTANCE, System::SystemServices::DLL_PROCESS_ATTACH};

mod core;
mod game;
// Added as `update_live` grows (Milestone B): mod hook; mod region; mod grants; mod inventory;
//   + the re-homed funnels: mod flags; mod grant; mod deathlink; mod upgrades; mod params;

/// DLL entry point — standard 3-arg Win32 `DllMain`. ModEngine2 loads externals via `LoadLibrary`,
/// so the OS invokes this with DLL_PROCESS_ATTACH. (3-arg works under me2 AND me3.)
///
/// We never do real work on the loader lock: hand off to `shared::initialize`, which spawns a worker.
#[unsafe(no_mangle)]
extern "system" fn DllMain(_hinst: HINSTANCE, call_reason: u32, _reserved: *mut c_void) -> bool {
    if call_reason != DLL_PROCESS_ATTACH {
        return true;
    }

    shared::handle_panics::<game::EldenRing>();
    shared::start_logger();

    // No AddItemFunc detour: under the inventory-scan item model (own_world:false, DS3-style),
    // self-found placeholders are converted by scanning the inventory in `update_live`.

    shared::initialize::<game::EldenRing>(shared::NoOpInputBlocker);
    true
}
