//! Watch apconfig.json and live-apply what changes: connection info (reconnect) and probe toggles
//! (no reconnect, no restart).
//!
//! Once Elden Ring has focus, editing connection info in the in-game overlay is miserable: clicking
//! closes the ER menu, and Escape (which opens it) closes the client's input window. The cause is
//! that ER's `InputBlocker` is `shared::NoOpInputBlocker` -- it blocks NOTHING -- so input reaches the
//! overlay AND the game. DS3 has a real one (nex3/fromsoftware-extra hooks the engine's
//! `dluid_*_device_should_block_input`); no such crate exists for ER.
//!
//! So sidestep the input problem entirely: edit apconfig.json in any text editor, alt-tab back, and
//! the client reconnects. The same argument covers the probe toggles (client#166): a probe is a
//! debug aid you reach for WHEN something looks wrong mid-session -- restarting the client (and the
//! session it was watching) to turn one on defeats the purpose. Edit the `probes` object, alt-tab
//! back, the toggle is live.
//!
//! The DECISIONS (is this a real change? is the file half-written? is this just the echo of our own
//! save?) are er_logic::config_reload::reload_action / probe_reload_action -- host-tested over a
//! timeline in config_reload_replay.rs, because both failure modes only exist across ticks.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use er_logic::config_reload::{
    ConnInfo, ProbeAction, ProbeMap, ReloadAction, probe_reload_action, reload_action,
};

/// What we last applied. `None` until the first successful read (which seeds it, so a fresh boot does
/// not immediately "reconnect" to the file it just loaded from).
static APPLIED: Mutex<Option<ConnInfo>> = Mutex::new(None);
/// The probe map we last installed. Seeded by `prime_probes` from the boot config, for the same
/// reason as `APPLIED`: the first watcher tick must not re-apply (and re-announce) the boot file.
static APPLIED_PROBES: Mutex<Option<ProbeMap>> = Mutex::new(None);
static LAST_TICK: Mutex<Option<Instant>> = Mutex::new(None);
const THROTTLE: Duration = Duration::from_millis(1000);

fn config_path() -> Option<PathBuf> {
    shared::utils::current_module_directory()
        .ok()
        .map(|d| d.join("apconfig.json"))
}

/// Read BOTH watched key families from the one file. A torn write fails the JSON parse up front, so
/// neither predicate ever sees a half-written file; individual missing keys degrade to empty, which
/// each predicate treats safely (an incomplete ConnInfo is ignored; an empty probes object is a
/// meaningful "all off" ONLY when it differs from what is applied -- at boot it equals it).
fn read_on_disk() -> Option<(ConnInfo, ProbeMap)> {
    let raw = std::fs::read_to_string(config_path()?).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?; // torn write -> parse fails -> ignore
    let conn = ConnInfo {
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
    };
    let probes = v
        .get("probes")
        .and_then(|x| x.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, val)| val.as_bool().map(|b| (k.clone(), b)))
                .collect()
        })
        .unwrap_or_default();
    Some((conn, probes))
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

/// Seed `APPLIED_PROBES` from the boot config's probes map, so the first watcher tick cannot
/// re-apply -- and therefore re-ANNOUNCE -- the very file the client just started from.
pub fn prime_probes(map: ProbeMap) {
    if let Ok(mut g) = APPLIED_PROBES.lock() {
        *g = Some(map);
    }
}

/// What one watcher tick decided. The two halves are independent: a tick can reconnect, apply
/// probes, both, or neither. (`reconnect` saving the file writes the probes map too, which is why
/// the probe half shares the own-save-echo guard rather than trusting "we only touched url".)
#[derive(Default)]
pub struct WatchOutcome {
    pub reconnect: Option<ConnInfo>,
    pub probes: Option<ProbeMap>,
}

/// Per-tick watcher. Reports a reconnect when the connection info changed and a probe map when the
/// probe toggles changed; the caller performs both applies (the probe apply also re-arms the
/// once-per-change announcement -- see shared::probes::rearm_announcement).
pub fn poll() -> WatchOutcome {
    {
        let Ok(mut last) = LAST_TICK.lock() else {
            return WatchOutcome::default();
        };
        if let Some(t) = *last
            && t.elapsed() < THROTTLE
        {
            return WatchOutcome::default();
        }
        *last = Some(Instant::now());
    }

    let Some((on_disk, on_disk_probes)) = read_on_disk() else {
        return WatchOutcome::default();
    };

    let reconnect = {
        let Ok(mut applied) = APPLIED.lock() else {
            return WatchOutcome::default();
        };
        // First read after boot: adopt it silently. Never reconnect to the file we just started from.
        match applied.as_ref() {
            None => {
                *applied = Some(on_disk);
                None
            }
            Some(cur) => match reload_action(cur, &on_disk) {
                ReloadAction::Ignore => None,
                ReloadAction::Reconnect(next) => {
                    log::info!(
                        "config hot-reload: apconfig.json changed (url={} slot={}) -- reconnecting",
                        next.url,
                        next.slot
                    );
                    *applied = Some(next.clone()); // update BEFORE reconnecting: update_connection_info
                    Some(next) //                     SAVES the file; this stops that echoing into a storm
                }
            },
        }
    };

    let probes = {
        let Ok(mut applied) = APPLIED_PROBES.lock() else {
            return WatchOutcome {
                reconnect,
                probes: None,
            };
        };
        match applied.as_ref() {
            None => {
                *applied = Some(on_disk_probes);
                None
            }
            Some(cur) => match probe_reload_action(cur, &on_disk_probes) {
                ProbeAction::Ignore => None,
                ProbeAction::Apply(next) => {
                    log::info!(
                        "config hot-reload: probes changed ({} keys) -- applying live",
                        next.len()
                    );
                    *applied = Some(next.clone());
                    Some(next)
                }
            },
        }
    };

    WatchOutcome { reconnect, probes }
}
