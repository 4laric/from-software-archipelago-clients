//! `merchant_bells` -- the I/O half of "talk to a merchant, their bell is already handed in"
//! (er-archipelago#325).
//!
//! Every rule about WHICH merchant, WHICH flag and WHETHER to write lives in
//! `er_logic::merchant_bells`, pure and unit-tested on any host. This module only reads the live
//! event flag, writes it, and queues the player-facing notice.
//!
//! ## 🛑 It rides the ESD detour, so it does the least possible work there
//!
//! [`on_shop_open`] runs INSIDE the game's ESD dispatch, on the game thread, in the same frame as
//! `shop_hints::on_shop_open`. When the option is off it is one relaxed atomic load and a return --
//! the table is not consulted, no flag is read. When it is on, the cost is a binary search over 38
//! rows plus one flag read, and (once per merchant, ever) one flag write.
//!
//! ## Why the notice is queued rather than drawn here
//!
//! The toast deck lives on `Client` and is drawn from the overlay; the detour has no `&mut Client`.
//! So the decision leaves a string behind and `core`'s tick drains it -- the same take/put-back
//! shape `shop_hints` uses for its hint batches.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use er_logic::merchant_bells::{
    Outcome, plan_hand_in, toast_text, vanilla_only_bells, vanilla_stock_toast,
};

/// `options.merchant_bells_on_talk`. Off until slot_data says otherwise, so a seed that never
/// declares the key -- including every foreign apworld -- behaves exactly as it does today.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Notices raised by the detour, drained by [`take_notice`] on the tick that owns the toast deck.
static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// How many bells this session has handed in. Logged on reset so a playtest can say "I visited
/// nine merchants and nine bells landed" without reading every line.
static HANDED_IN: AtomicUsize = AtomicUsize::new(0);

/// Hand-in flags whose complete shop blocks contain zero AP rows in this seed. Unlike ENABLED,
/// this drives a fallback for vanilla hand-ins too, so it is configured for every greenfield seed.
static VANILLA_ONLY: Mutex<Vec<(u32, &'static str)>> = Mutex::new(Vec::new());

/// Set flags already observed this connection. The first in-world pass is a silent baseline, so
/// reconnecting never re-announces every bell handed in earlier in the save.
static OBSERVED_HAND_INS: Mutex<HashSet<u32>> = Mutex::new(HashSet::new());
static HAND_INS_PRIMED: AtomicBool = AtomicBool::new(false);

/// Called at slot_data parse. Also clears any notice left over from a previous connection.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if let Ok(mut q) = PENDING.lock() {
        q.clear();
    }
    if on {
        log::info!(
            "merchant bells: ON -- opening a merchant's buy menu hands their Bell Bearing to the \
             Twin Maiden Husks ({} merchant(s) covered; buy menu only, and the four peddler / DLC \
             seller bells that release stock instead of adding a menu entry are not covered)",
            er_logic::merchant_bell_table::MERCHANT_BELLS.len()
        );
    }
}

/// Is the detour live? A READ-BACK of the state the detour itself consults -- not a memo of
/// whether `set_enabled` was called. `feature_handshake` subtracts this from the seed's
/// `requiresClientFeatures` declaration, which is the only thing that would have caught #536.
pub fn is_armed() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Configure the active AP shop rows from this seed's `shopRowFlags` wire table.
///
/// A bell absent from all of those rows still opens its vanilla ShopLineupParam block when handed
/// in. Polling its hand-in flag lets the client explain that unavoidable non-pool path (#555).
pub fn configure_shop_rows(rows: impl IntoIterator<Item = u32>) {
    let active: HashSet<u32> = rows.into_iter().collect();
    let vanilla = vanilla_only_bells(&active);
    log::info!(
        "merchant bells: {} bell(s) have no AP stock in this seed ({} active shop row(s))",
        vanilla.len(),
        active.len()
    );
    *VANILLA_ONLY.lock().unwrap() = vanilla;
    OBSERVED_HAND_INS.lock().unwrap().clear();
    HAND_INS_PRIMED.store(false, Ordering::Relaxed);
}

fn notice_for(flag: u32, name: &str) -> String {
    if VANILLA_ONLY
        .lock()
        .map(|bells| bells.iter().any(|&(candidate, _)| candidate == flag))
        .unwrap_or(false)
    {
        vanilla_stock_toast(name)
    } else {
        toast_text(name)
    }
}

/// A merchant opened its buy menu over `ShopLineupParam` rows `[begin, end]`.
///
/// Runs on the game thread inside the ESD dispatch. The caller wraps this in `catch_unwind`; every
/// lock here degrades to silence rather than panicking across the game's own call frame.
pub fn on_shop_open(begin: i32, end: i32) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    match plan_hand_in(begin, end, true, crate::flags::get_event_flag) {
        // Unreachable while `enabled` is hardcoded true above; kept so a future caller that passes
        // the flag through cannot fall into a silent arm.
        Outcome::Disabled => {}
        Outcome::NoBell => {
            log::debug!("merchant bells: shop {begin}..{end} belongs to no bell-bearing merchant");
        }
        Outcome::AlreadyHandedIn { flag, name } => {
            log::debug!("merchant bells: {name} (flag {flag}) is already handed in");
        }
        Outcome::HandIn { flag, name } => {
            // 🛑 `try_set_event_flag` rather than `set_event_flag`: this runs inside a talk frame,
            // and a refused write must SAY so. A silent failure here is the exact shape of "the
            // feature is on and nothing happened" that the option exists to avoid.
            if crate::flags::try_set_event_flag(flag, true) {
                let n = HANDED_IN.fetch_add(1, Ordering::Relaxed) + 1;
                log::info!(
                    "merchant bells: {name} handed to the Twin Maiden Husks (flag {flag} set on \
                     shop {begin}..{end}); {n} this session"
                );
                // The poller below owns vanilla hand-ins. Mark this feature-path write observed so
                // its next pass cannot announce the same flag a second time.
                if let Ok(mut observed) = OBSERVED_HAND_INS.lock() {
                    observed.insert(flag);
                }
                if let Ok(mut q) = PENDING.lock() {
                    q.push(notice_for(flag, name));
                }
            } else {
                log::warn!(
                    "merchant bells: could not set flag {flag} for {name} -- the shop is open but \
                     the Twin Maidens will not stock it"
                );
            }
        }
    }
}

/// Detect vanilla Bell Bearing hand-ins and explain wholly vanilla stock.
///
/// Only bells already classified as having zero AP rows are read. The first pass silently records
/// the save's existing flags; later clear->set edges enqueue one notice. This stays live even when
/// `merchant_bells_on_talk` is off, because the player can kill a merchant and hand the item to the
/// Twin Maidens through the base game's menu.
pub fn poll_hand_ins() {
    let bells = match VANILLA_ONLY.lock() {
        Ok(bells) => bells.clone(),
        Err(_) => return,
    };
    if bells.is_empty() {
        HAND_INS_PRIMED.store(true, Ordering::Relaxed);
        return;
    }

    let set_now: Vec<(u32, &'static str)> = bells
        .into_iter()
        .filter(|&(flag, _)| crate::flags::get_event_flag(flag))
        .collect();
    let Ok(mut observed) = OBSERVED_HAND_INS.lock() else {
        return;
    };
    if !HAND_INS_PRIMED.swap(true, Ordering::Relaxed) {
        observed.extend(set_now.iter().map(|&(flag, _)| flag));
        log::info!(
            "merchant bells: vanilla-stock baseline primed with {} prior hand-in(s)",
            set_now.len()
        );
        return;
    }

    let notices: Vec<String> = set_now
        .into_iter()
        .filter_map(|(flag, name)| observed.insert(flag).then(|| vanilla_stock_toast(name)))
        .collect();
    drop(observed);
    if let Ok(mut pending) = PENDING.lock() {
        pending.extend(notices);
    }
}

/// Drain one queued notice. Called from the tick that owns the toast deck.
pub fn take_notice() -> Option<String> {
    let mut q = PENDING.lock().ok()?;
    if q.is_empty() {
        None
    } else {
        Some(q.remove(0))
    }
}

/// Clear per-session state. Called on disconnect, alongside the other feature resets.
pub fn reset() {
    let n = HANDED_IN.swap(0, Ordering::Relaxed);
    if n > 0 {
        log::info!("merchant bells: {n} bell(s) handed in this session");
    }
    if let Ok(mut q) = PENDING.lock() {
        q.clear();
    }
    if let Ok(mut bells) = VANILLA_ONLY.lock() {
        bells.clear();
    }
    if let Ok(mut observed) = OBSERVED_HAND_INS.lock() {
        observed.clear();
    }
    HAND_INS_PRIMED.store(false, Ordering::Relaxed);
}
