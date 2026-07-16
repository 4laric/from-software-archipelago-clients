//! `impl shared::Game for EldenRing` — the binding/lifecycle adapter (mirrors `ds3/game.rs`).
//!
//! Every type here is one the existing ER client already resolves, so this is grounded, not
//! invented. Lines marked `// VERIFY` are the spots most likely to need a tweak on the first
//! Windows build (the Phase 1-5 builds each had one or two of these).

use std::time::Duration;

use anyhow::Result;
use eldenring::cs::{CSTaskGroupIndex, CSTaskImp, WorldChrMan};
use eldenring::fd4::FD4TaskData;
use fromsoftware_shared::{FromStatic, SharedTaskImpExt};

/// One-line build identity for the connect banner:
/// `<pkg-version> (<sha>[-dirty], <branch> @ <build-time UTC>)`.
/// SHA, branch and build time are stamped into the env by `build.rs`.
pub const CLIENT_BUILD: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("ER_GIT_SHA"),
    ", ",
    env!("ER_GIT_BRANCH"),
    " @ ",
    env!("ER_BUILD_TIME"),
    ")"
);

/// THE build-stamp line (logged once at startup, echoed by `!version`). Settles "is my running DLL
/// current?" in one glance: compare the sha to `git log --oneline -1` of the tree you think you
/// built (a `-dirty` suffix means uncommitted changes were compiled in), and the UTC timestamp to
/// when you built it. Carries the data-contract hash this binary was COMPILED against, so the one
/// line ties code version AND contract version to the log that follows.
pub fn build_line() -> String {
    format!(
        "eldenring-archipelago build: {} ({}, {}) | contract {}",
        env!("ER_GIT_SHA"),
        env!("ER_GIT_BRANCH"),
        env!("ER_BUILD_TIME"),
        crate::contract_gen::CONTRACT_HASH
    )
}

pub struct EldenRing;

impl shared::Game for EldenRing {
    type Core = crate::core::Core;
    /// ER renders on DX12 (DS3 is DX11).
    type GraphicsHooks = hudhook::hooks::dx12::ImguiDx12Hooks; // VERIFY: dx12 hook name in workspace hudhook
    /// No ER input blocker exists in `fromsoftware-extra` yet; start with the no-op (overlay still
    /// captures input via Hudhook). Swap in a real blocker when one lands.
    type InputBlocker = shared::NoOpInputBlocker;
    const TYPE: shared::GameType = shared::GameType::EldenRing; // requires the shared change (below)
    const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
    /// ER uses the ECHO model: the server sends our own checks back as received items, so self-found
    /// items run the same name-based logic (progressive / region-open / notify) as remote items.
    /// (DS3/Sekiro keep the default `false` inventory-scan-convert model.) Requires the shared change.
    const OWN_WORLD: bool = true;

    /// Schedule per-frame work on CSTaskImp / FrameBegin — the same idiom the existing client uses.
    fn run_recurring_task(mut task: impl FnMut() + 'static + Send) -> Result<()> {
        CSTaskImp::wait_for_instance(Duration::MAX)?.run_recurring(
            move |_: &'_ FD4TaskData| task(),
            CSTaskGroupIndex::FrameBegin,
        ); // VERIFY closure arg type
        Ok(())
    }

    /// Main menu / pre-load == no live player. `WorldChrMan.main_player` present == in-world (the
    /// exact signal the current client's `flags::in_world()` uses).
    unsafe fn is_main_menu() -> bool {
        match unsafe { WorldChrMan::instance() } {
            Ok(wcm) => wcm.main_player.as_ref().is_none(),
            Err(_) => true,
        }
    }
}
