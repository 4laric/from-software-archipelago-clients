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

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use er_logic::merchant_bells::{Outcome, plan_hand_in, toast_text};

/// `options.merchant_bells_on_talk`. Off until slot_data says otherwise, so a seed that never
/// declares the key -- including every foreign apworld -- behaves exactly as it does today.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Notices raised by the detour, drained by [`take_notice`] on the tick that owns the toast deck.
static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// How many bells this session has handed in. Logged on reset so a playtest can say "I visited
/// nine merchants and nine bells landed" without reading every line.
static HANDED_IN: AtomicUsize = AtomicUsize::new(0);

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
                if let Ok(mut q) = PENDING.lock() {
                    q.push(toast_text(name));
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
}
