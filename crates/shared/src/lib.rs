use std::sync::{Arc, Mutex};
use std::{fs, panic, path::Path};

use anyhow::Result;
use backtrace::Backtrace;
use chrono::prelude::*;
use hudhook::Hudhook;
use log::*;
use simplelog::{ColorChoice, CombinedLogger, SharedLogger, TermLogger, TerminalMode, WriteLogger};
use windows::Win32::UI::WindowsAndMessaging::MessageBoxW;
use windows::core::*;

mod clipboard;
mod config;
mod connect_error;
mod core;
mod crash_handler;
mod error_display;
pub mod foreign_blocks;
mod game;
mod input_blocker;
pub mod log_collapse;
pub mod mod_stack;
mod overlay;
pub mod probes;
mod section_profiler;
pub mod utils;

pub use core::*;
use error_display::*;
pub use game::*;
pub use input_blocker::*;
pub(crate) use section_profiler::*;

/// Handle panics by both logging and popping up a message box, which is the
/// most reliable way to make something visible to the end user.
pub fn handle_panics<G: Game>() {
    panic::set_hook(Box::new(|panic_info| {
        let mut message = String::new();
        if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            message.push_str(&format!("Rust panic: {s}"));
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            message.push_str(&format!("Rust panic: {s}"));
        } else {
            message.push_str(&format!("Rust panic: {:?}", panic_info.payload()));
        }

        message.push_str(&format!("\n{:?}", Backtrace::new()));

        error!("{}", message);
        message_box::<G>(message);
    }));
}

/// Displays a message box with the given message.
fn message_box<G: Game>(message: impl Into<String>) {
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(message.into()),
            &HSTRING::from(format!("{} Archipelago Client", G::TYPE.short_name())),
            Default::default(),
        );
    }
}

/// Starts the logger which logs to both stdout and a file which users can send
/// to the devs for debugging.
pub fn start_logger() {
    // R14 (SWEEP): a swallowed init error here meant file logging was silently absent for the
    // whole session -- making every OTHER runtime problem undetectable. No logger exists yet at
    // this point, so eprintln! is the only available channel; on failure, also try the current
    // directory as a fallback before giving up.
    match utils::mod_directory() {
        Ok(dir) => {
            if let Err(err) = start_logger_for_dir(dir) {
                eprintln!(
                    "AP client: logger init FAILED for mod directory ({err}); trying current directory"
                );
                if let Err(err2) = start_logger_for_dir(".") {
                    eprintln!(
                        "AP client: fallback logger init FAILED too ({err2}); file logging is OFF this session"
                    );
                    return;
                }
            }
            info!("Logger initialized.");
        }
        Err(dir_err) => {
            if let Err(err) = start_logger_for_dir(".") {
                eprintln!(
                    "AP client: no mod directory ({dir_err}) and logger init FAILED for current directory ({err}); file logging is OFF this session"
                );
                return;
            }
            info!("Failed to determine mod directory, logging to current directory instead.");
        }
    }

    // Provenance, emitted only now because it CANNOT be emitted earlier: resolving the mod
    // directory is what starts the logger, so `load_mod_directory`'s own println! lines land on
    // stdout with no logger to catch them. Every path that reaches here has a live logger.
    mod_stack::log_provenance();
}

/// Starts a logger for the given directory.
fn start_logger_for_dir(dir: impl AsRef<Path>) -> Result<()> {
    let mut loggers: Vec<Box<dyn SharedLogger>> = vec![TermLogger::new(
        LevelFilter::Warn,
        simplelog::Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )];
    if let Ok(logger) = create_write_logger(&dir) {
        loggers.push(logger);
    }
    // Same wiring `CombinedLogger::init` does (max level from the sub-loggers, then install), with
    // the duplicate collapser spliced in front of the sinks.
    //
    // WHY: hudhook's DX12 present hook logs TWO ERROR lines every single frame for as long as the
    // game window is minimised -- `renderer/pipeline.rs:150` bails on a 0x0 swapchain and
    // `hooks/dx12.rs:235` reports the HRESULT that bail returns. Neither has a rate limit and
    // hudhook offers no way to quiet them. One player's five logs held 612,842 of those lines,
    // which buried every real line and turned "please upload your log" into a 200 MB request. We
    // cannot fix the dependency's logging, but this is the seam where its records reach our file.
    // See `log_collapse` -- it collapses ANY repeated record, not those two strings.
    let combined = CombinedLogger::new(loggers);
    log::set_max_level(combined.level());
    log::set_boxed_logger(Box::new(log_collapse::CollapseDuplicates::new(combined)))?;
    log_session_separator();
    // Native crash telemetry, armed as soon as a logger exists so its "armed" line is captured.
    // A native fault (the 2026-07-24 warp CTD) previously ended the log with NO record at all;
    // this names the faulting module+offset in crash-<pid>.txt beside the log + the log itself.
    crash_handler::install(dir.as_ref().join("log"));
    Ok(())
}

/// The first line of every launch: a separator a reader can segment the file on.
///
/// `create_write_logger` opens `archipelago-<date>.log` in APPEND mode, one file per DAY, so a
/// player who launches the game four times uploads one file holding four sessions -- and if they
/// regenerated in between, several different seeds. Nothing in the file said where one ended and
/// the next began; you had to know that `Logger initialized.` implies a fresh process. Triage has
/// been answered off the wrong session more than once for exactly that reason.
///
/// 🛑 DELIBERATELY NOT A COUNTER. "SESSION 3 of 4" would mean re-reading the existing log at
/// startup to count previous separators, on the game's load path -- and these files reach hundreds
/// of megabytes (one report arrived with 612,842 lines of hudhook render spam in it). The wall
/// clock plus the pid identify a launch uniquely and cost nothing; the ordinal is whatever `grep -n`
/// prints beside it.
fn log_session_separator() {
    info!(
        "=== SESSION START {} | pid {} | this file is APPENDED across launches: everything above \
         belongs to an earlier run ===",
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        std::process::id()
    );
}

/// Creates a write logger that writes to files in [dir].
fn create_write_logger(dir: impl AsRef<Path>) -> Result<Box<WriteLogger<fs::File>>> {
    let dir = dir.as_ref().join("log");
    fs::create_dir_all(&dir)?;
    let filename = dir.join(Local::now().format("archipelago-%Y-%m-%d.log").to_string());
    Ok(WriteLogger::new(
        LevelFilter::Info,
        simplelog::Config::default(),
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(filename)?,
    ))
}

/// Initializes the basic hooks into the underlying rendering system for the
/// current mod.
pub fn initialize<G: Game>(blocker: G::InputBlocker) {
    std::thread::spawn(move || {
        info!("Worker thread initialized.");

        // This mutex isn't strictly necessary since in practice we're only
        // ever touching this on DS3's main thread. But Rust doesn't have
        // any way of knowing that and using a Mutex is simpler than
        // creating a newtype that implements Sync, so we do it anyway.
        // Because there won't be any contention, it should be very
        // inexpensive.
        let mut core = G::Core::new().map(|core| Arc::new(Mutex::new(core)));

        if let Ok(core2) = core.as_ref() {
            let core2 = core2.clone();
            // Safety: We're playing a little fast and loose here, not
            // scheduling the task on the main thread. It seems to work, but
            // really we should probably handle it in the error display.
            if let Err(err) = unsafe {
                G::run_recurring_task(move || {
                    let mut core2 = core2.lock().unwrap();
                    prof!(core2.base_mut().profiler(), "AP mod logic", {
                        core2.update(G::is_main_menu());
                    });
                })
            } {
                core = Err(err);
            }
        }

        info!("Game system initialized.");

        if let Err(e) = Hudhook::builder()
            .with::<G::GraphicsHooks>(ErrorDisplay::<G>::new(core, blocker))
            .build()
            .apply()
        {
            panic!("Couldn't apply hooks: {e:?}");
        }
    });
}
