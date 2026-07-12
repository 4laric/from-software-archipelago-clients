//! Watch apconfig.json and reconnect when the connection info changes.
//!
//! Once Elden Ring has focus, editing connection info in the in-game overlay is miserable: clicking
//! closes the ER menu, and Escape (which opens it) closes the client's input window. The cause is
//! that ER's `InputBlocker` is `shared::NoOpInputBlocker` -- it blocks NOTHING -- so input reaches the
//! overlay AND the game. DS3 has a real one (nex3/fromsoftware-extra hooks the engine's
//! `dluid_*_device_should_block_input`); no such crate exists for ER.
//!
//! So sidestep the input problem entirely: edit apconfig.json in any text editor, alt-tab back, and
//! the client reconnects. The DECISION (is this a real change? is the file half-written? is this just
//! the echo of our own save?) is er_logic::config_reload::reload_action -- host-tested over a timeline
//! in config_reload_replay.rs, because both failure modes only exist across ticks.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use er_logic::config_reload::{ConnInfo, ReloadAction, reload_action};

/// What we last applied. `None` until the first successful read (which seeds it, so a fresh boot does
/// not immediately "reconnect" to the file it just loaded from).
static APPLIED: Mutex<Option<ConnInfo>> = Mutex::new(None);
static LAST_TICK: Mutex<Option<Instant>> = Mutex::new(None);
const THROTTLE: Duration = Duration::from_millis(1000);

fn config_path() -> Option<PathBuf> {
    shared::utils::current_module_directory()
        .ok()
        .map(|d| d.join("apconfig.json"))
}

fn read_on_disk() -> Option<ConnInfo> {
    let raw = std::fs::read_to_string(config_path()?).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?; // torn write -> parse fails -> ignore
    Some(ConnInfo {
        url: v
            .get("url")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        slot: v
            .get("slot")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        password: v
            .get("password")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
    })
}

/// Seed `APPLIED` from whatever the client actually connected with, so the first watcher tick is a
/// no-op rather than a spurious reconnect.
pub fn prime(url: &str, slot: &str, password: Option<String>) {
    if let Ok(mut g) = APPLIED.lock() {
        *g = Some(ConnInfo {
            url: url.to_string(),
            slot: slot.to_string(),
            password,
        });
    }
}

/// Per-tick watcher. Returns the new connection info when the caller should reconnect.
pub fn poll() -> Option<ConnInfo> {
    {
        let mut last = LAST_TICK.lock().ok()?;
        if let Some(t) = *last
            && t.elapsed() < THROTTLE
        {
            return None;
        }
        *last = Some(Instant::now());
    }

    let on_disk = read_on_disk()?;
    let mut applied = APPLIED.lock().ok()?;

    // First read after boot: adopt it silently. Never reconnect to the file we just started from.
    let Some(cur) = applied.as_ref() else {
        *applied = Some(on_disk);
        return None;
    };

    match reload_action(cur, &on_disk) {
        ReloadAction::Ignore => None,
        ReloadAction::Reconnect(next) => {
            log::info!(
                "config hot-reload: apconfig.json changed (url={} slot={}) -- reconnecting",
                next.url,
                next.slot
            );
            *applied = Some(next.clone()); // update BEFORE reconnecting: update_connection_info SAVES
            Some(next) //                     the file, and this is what stops that echoing into a storm
        }
    }
}
