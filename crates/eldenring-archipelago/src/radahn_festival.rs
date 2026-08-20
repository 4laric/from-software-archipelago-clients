//! Game-side Radahn festival companion-flag reconciler.
//!
//! The decision lives in `er_logic::radahn_festival`; this binding supplies live flag reads and
//! set-with-readback retry. It never writes festival-over flag 9413, leaving vanilla common event
//! 3040 to preserve the Jerren conversation and out-of-area timing.

use std::sync::atomic::{AtomicU32, Ordering};

use er_logic::radahn_festival::{
    FestivalState, RADAHN_AFTERGLOW_FLAG, RADAHN_DEFEAT_FLAG, RADAHN_GLOBAL_DEFEAT_FLAG,
};

use crate::flags;

const GLOBAL_DEFEAT_BIT: u32 = 1;
const AFTERGLOW_BIT: u32 = 2;

/// One warning bit per companion write. A lost/rejected write retries every in-world tick, but is
/// logged once until readback confirms it.
static WARNED: AtomicU32 = AtomicU32::new(0);

fn apply(flag: u32, bit: u32, applied: &mut Vec<u32>) {
    let accepted = flags::try_set_event_flag(flag, true);
    if flags::get_event_flag(flag) {
        WARNED.fetch_and(!bit, Ordering::Relaxed);
        applied.push(flag);
    } else if WARNED.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
        log::warn!(
            "Radahn festival: companion flag {flag} did not stick (write accepted={accepted}); \
             retrying every in-world tick"
        );
    }
}

/// Repair the two state flags that vanilla would have set after the original Radahn character died.
///
/// The arena defeat flag is the authority: AP item receipt and Great Rune possession do not set it.
/// Once both companion flags read back set this becomes a cheap, write-free poll.
pub fn tick() {
    if !flags::in_world() {
        return;
    }

    let state = FestivalState {
        defeated: flags::get_event_flag(RADAHN_DEFEAT_FLAG),
        global_defeat: flags::get_event_flag(RADAHN_GLOBAL_DEFEAT_FLAG),
        afterglow: flags::get_event_flag(RADAHN_AFTERGLOW_FLAG),
    };
    let writes = er_logic::radahn_festival::reconcile(state);
    let mut applied = Vec::new();
    if writes.global_defeat {
        apply(RADAHN_GLOBAL_DEFEAT_FLAG, GLOBAL_DEFEAT_BIT, &mut applied);
    }
    if writes.afterglow {
        apply(RADAHN_AFTERGLOW_FLAG, AFTERGLOW_BIT, &mut applied);
    }
    if !applied.is_empty() {
        log::info!(
            "Radahn festival: arena defeat flag {RADAHN_DEFEAT_FLAG} set; repaired companion \
             flag(s) {applied:?}. Vanilla event 3040 still owns the safe transition to 9413"
        );
    }
}
