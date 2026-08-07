//! THROWAWAY probe harness: set / read arbitrary event flags from `apconfig.json`.
//!
//! 🛑 DELETE BEFORE MERGE. This is a debugging affordance for the SPEC-ashen-capital-lock probe
//! (2026-08-06), not a feature. It writes save-persisted event flags on demand with no validation
//! whatsoever -- a typo here corrupts a save's flag state permanently.
//!
//! WHY IT EXISTS. The probe has to answer "does `m11_05` work with a synthetic 9116 and no burn
//! cutscene in the save's history", which means setting 118 and 9116 by hand. Cheat Engine cannot
//! do that reliably: the only decode on record (`id = byteOffset*8 + (7 - bitStart) + 50000`) was
//! calibrated on flag 76101 and covers that band only, and 118 / 9116 sit in different flag
//! groups. `flags::set_event_flag` goes through the game's own `CSEventFlagMan::virtual_memory_flag`
//! resolver, which is correct for any id -- so the client is the right instrument, and driving it
//! from the config file means re-running the probe is a text edit rather than a rebuild.
//!
//! WIRE SHAPE (`apconfig.json`, both keys optional; absent = this module is inert and silent):
//!
//! ```json
//! {
//!   "url": "...", "slot": "...",
//!   "debugReadFlags":  [60100, 73100, 118, 9116, 71100, 71109],
//!   "debugSetFlags":   [[118, true], [9116, true]]
//! }
//! ```
//!
//! * `debugReadFlags` -- logged once per change, one line per flag. ALWAYS include a control that
//!   must read TRUE on any mid-game save (60100 Torrent whistle, 73100 Limgrave anchor grace) or an
//!   all-false result looks like a clean answer while discriminating nothing.
//! * `debugSetFlags` -- `[id, value]` pairs, applied ONCE per change to the list. Edit the file and
//!   alt-tab back to fire again; re-applying the SAME list is deliberately a no-op, so the capital
//!   reconciler (which owns 9116 by position every tick) is never fought in a write war. To force a
//!   replay of an identical list, change any whitespace in the file.
//!
//! Both are gated on `flags::in_world()`: the flag holder is not trustworthy at the menu or
//! mid-load, and `try_set_event_flag` reports that by returning false, so an application that finds
//! the holder unready is retried on the next tick rather than silently dropped.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::flags;

/// Last list we applied, verbatim as it appeared in the file. `None` until the first read, so a
/// client that boots with `debugSetFlags` already present DOES apply it once on the first tick in
/// world (the alternative -- seeding from the boot read like config_watch does -- would mean the
/// probe silently does nothing on the run where you set it up).
static APPLIED: Mutex<Option<String>> = Mutex::new(None);
static REPORTED: Mutex<Option<String>> = Mutex::new(None);
static LAST_TICK: Mutex<Option<Instant>> = Mutex::new(None);
const THROTTLE: Duration = Duration::from_millis(1000);

fn config_path() -> Option<PathBuf> {
    shared::utils::current_module_directory()
        .ok()
        .map(|d| d.join("apconfig.json"))
}

/// Per-tick entry point. Cheap: throttled to 1 Hz, and returns immediately when neither key is
/// present. Call from `core::update_live`.
pub fn tick() {
    {
        let mut last = LAST_TICK.lock().unwrap();
        if let Some(t) = *last
            && t.elapsed() < THROTTLE
        {
            return;
        }
        *last = Some(Instant::now());
    }
    if !flags::in_world() {
        return; // menu / load: the flag holder is not trustworthy
    }
    let Some(path) = config_path() else { return };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    // A torn write fails to parse and is simply ignored -- the next tick reads it again.
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return;
    };

    if let Some(list) = v.get("debugReadFlags") {
        let key = list.to_string();
        let mut reported = REPORTED.lock().unwrap();
        if reported.as_deref() != Some(key.as_str()) {
            for id in list.as_array().into_iter().flatten() {
                match id.as_u64() {
                    Some(id) => log::info!(
                        "debug read: flag {} = {}",
                        id,
                        flags::get_event_flag(id as u32)
                    ),
                    None => log::warn!("debug read: {id} is not a flag id -- skipped"),
                }
            }
            *reported = Some(key);
        }
    }

    if let Some(list) = v.get("debugSetFlags") {
        let key = list.to_string();
        let mut applied = APPLIED.lock().unwrap();
        if applied.as_deref() == Some(key.as_str()) {
            return; // unchanged -- do not fight the reconciler
        }
        let mut all_ready = true;
        for pair in list.as_array().into_iter().flatten() {
            let (Some(id), Some(val)) = (
                pair.get(0).and_then(|x| x.as_u64()),
                pair.get(1).and_then(|x| x.as_bool()),
            ) else {
                log::warn!("debug set: {pair} is not an [id, bool] pair -- skipped");
                continue;
            };
            let id = id as u32;
            // READBACK, not a success flag: try_set_event_flag reports only that the holder was
            // ready. Whether the bit STUCK is a separate question and the one worth logging --
            // the same distinction capital_warp_intercept draws.
            let ready = flags::try_set_event_flag(id, val);
            if !ready {
                all_ready = false;
                log::warn!("debug set: flag {id} -- holder not ready, retrying next tick");
                continue;
            }
            log::info!(
                "debug set: flag {} -> {} ; readback {}",
                id,
                val,
                flags::get_event_flag(id)
            );
        }
        // Latch only once every pair found a ready holder, so a partially-applied list is retried
        // whole rather than being marked done.
        if all_ready {
            *applied = Some(key);
        }
    }
}
