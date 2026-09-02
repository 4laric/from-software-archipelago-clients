//! `impl shared::Game for EldenRing` — the binding/lifecycle adapter (mirrors `ds3/game.rs`).
//!
//! Every type here is one the existing ER client already resolves, so this is grounded, not
//! invented. Lines marked `// VERIFY` are the spots most likely to need a tweak on the first
//! Windows build (the Phase 1-5 builds each had one or two of these).

use std::thread;
use std::time::Instant;

use anyhow::{Result, bail};
use eldenring::cs::{CSMenuManImp, CSMouseMan, CSTaskGroupIndex, CSTaskImp, WorldChrMan};
use eldenring::fd4::FD4TaskData;
use er_logic::startup_retry::{DEFAULT_ATTEMPT_TIMEOUT, Next, RetryPolicy};
use fromsoftware_shared::{FromStatic, SharedTaskImpExt};
use log::{info, warn};

/// One-line build identity for the connect banner: `<pkg-version> (<sha> @ <build-time>)`.
/// SHA + build time are stamped into the env by `build.rs`.
pub const CLIENT_BUILD: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("ER_GIT_SHA"),
    " @ ",
    env!("ER_BUILD_TIME"),
    ")"
);

/// Short build identity for the OVERLAY WINDOW TITLE: `<version> (<sha>)`. Deliberately omits the
/// build timestamp so the title stays short enough to survive a narrow window + a player
/// screenshot -- the title is the one surface a player photographs without being asked. The
/// long form ([`CLIENT_BUILD`], with timestamp) stays on the connect banner in the log.
pub const CLIENT_BUILD_TITLE: &str =
    concat!(env!("CARGO_PKG_VERSION"), " (", env!("ER_GIT_SHA"), ")");

pub struct EldenRing;

impl shared::Game for EldenRing {
    type Core = crate::core::Core;
    /// ER renders on DX12 (DS3 is DX11).
    type GraphicsHooks = hudhook::hooks::dx12::ImguiDx12Hooks; // VERIFY: dx12 hook name in workspace hudhook
    /// Real ER input blocker: hooks the standard input APIs ER uses (XInput / DirectInput8 /
    /// GetKeyboardState) so overlay input stops leaking to the game. See `crate::input`.
    type InputBlocker = crate::input::EldenRingInputBlocker;
    const TYPE: shared::GameType = shared::GameType::EldenRing; // requires the shared change (below)
    const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
    /// Overlay title shows version + git SHA so a screenshot identifies the exact build (the
    /// version alone cannot: the lockstep-bump commit and the guard-fix commit both report the
    /// same CLIENT_VERSION).
    const CLIENT_BUILD: &str = CLIENT_BUILD_TITLE;
    const OVERLAY_THEME: Option<shared::OverlayTheme> = Some(shared::OverlayTheme {
        background: [
            0x14 as f32 / 255.0,
            0x11 as f32 / 255.0,
            0x0c as f32 / 255.0,
        ],
        title_background: [
            0x1e as f32 / 255.0,
            0x18 as f32 / 255.0,
            0x12 as f32 / 255.0,
        ],
        border: [
            0x4a as f32 / 255.0,
            0x3c as f32 / 255.0,
            0x24 as f32 / 255.0,
        ],
        text: [
            0xe7 as f32 / 255.0,
            0xdc as f32 / 255.0,
            0xc3 as f32 / 255.0,
        ],
        muted_text: [
            0x97 as f32 / 255.0,
            0x90 as f32 / 255.0,
            0x6f as f32 / 255.0,
        ],
        accent: [
            0xc8 as f32 / 255.0,
            0xa9 as f32 / 255.0,
            0x6e as f32 / 255.0,
        ],
        selection: [
            0x4a as f32 / 255.0,
            0x3c as f32 / 255.0,
            0x24 as f32 / 255.0,
        ],
    });
    /// ER uses the ECHO model: the server sends our own checks back as received items, so self-found
    /// items run the same name-based logic (progressive / region-open / notify) as remote items.
    /// (DS3/Sekiro keep the default `false` inventory-scan-convert model.) Requires the shared change.
    const OWN_WORLD: bool = true;

    /// Schedule per-frame work on CSTaskImp / FrameBegin -- the same idiom the existing client uses.
    fn run_recurring_task(mut task: impl FnMut() + 'static + Send) -> Result<()> {
        wait_for_task_scheduler()?.run_recurring(
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

    unsafe fn force_cursor_visible() {
        if let Ok(menu) = unsafe { CSMenuManImp::instance_mut() } {
            menu.disable_mouse_cursor = false;
        }
        if let Ok(mouse) = unsafe { CSMouseMan::instance_mut() } {
            mouse.show_cursor = true;
        }
    }
}

/// Ask the game for `CSTaskImp`, retrying while its singleton map is still being built.
///
/// `CSTaskImp::wait_for_instance` loops on `InstanceError::Null` but *returns* on
/// `InstanceError::NotFound`, and `NotFound` is precisely the transient case: `from-singleton`
/// documents `map()` as "may not contain all singletons if it is called before Dantelion2
/// reflection is initialized by the process". `wait_for_system_init` only waits on the CSWindow
/// `hInstance`, which is set earlier than that, so it can hand us a window in which `CSTask` is not
/// in the map yet. That surfaced to players as a fatal `Could not translate RVA to VA` modal over a
/// perfectly healthy game (4laric/er-archipelago#475), on an install that had launched fine minutes
/// before. Retrying is the whole fix; see [`er_logic::startup_retry`] for the policy and the
/// evidence.
///
/// Note the finite per-attempt timeout. `Duration::MAX` made `SystemInitError::Timeout`
/// unconstructible, so a game that genuinely never came up reported the same misleading text.
fn wait_for_task_scheduler() -> Result<&'static CSTaskImp> {
    let policy = RetryPolicy::default();
    let started = Instant::now();
    let mut attempts: u32 = 0;

    loop {
        attempts += 1;

        match CSTaskImp::wait_for_instance(DEFAULT_ATTEMPT_TIMEOUT) {
            Ok(scheduler) => {
                if attempts > 1 {
                    info!(
                        "CSTask was not registered yet on startup; got it on attempt {attempts} after {:?}.",
                        started.elapsed(),
                    );
                }
                return Ok(scheduler);
            }
            Err(err) => {
                if attempts == 1 {
                    warn!("CSTask lookup failed on the first attempt ({err}); retrying.");
                }

                match policy.after_failure(started.elapsed()) {
                    Next::RetryAfter(delay) => thread::sleep(delay),
                    Next::GiveUp => {
                        bail!(policy.give_up_message(attempts, started.elapsed(), &err.to_string()))
                    }
                }
            }
        }
    }
}
