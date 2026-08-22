//! ability_lock.rs -- TEST-BUILD enforcement of individual ability locks (er-archipelago#945,
//! SPEC-ability-lock-mode). The decision is `er_logic::ability_lock` (host-tested); this file is
//! the arm.
//!
//! ## The mechanism: the game's own logical-action layer
//!
//! ER resolves every input -- gamepad, keyboard, mouse -- into `ChrActions` on the player's
//! `CSChrActionRequestModule` (`chr_ins.modules.action_request`), one bit per LOGICAL action
//! (r1, jump, rolling, ...), AFTER the keybind map. Each frame we OR the locked abilities' bits
//! into that module's `disabled_action_inputs` -- the game's OWN "these actions are off" field --
//! and, belt-and-suspenders, clear them from `action_requests`/`new_action_presses` the same
//! frame. This is:
//!   * KEYBIND-AGNOSTIC -- rebind roll to any key/button, it still sets the `rolling` bit;
//!   * device-agnostic -- one path covers pad AND keyboard/mouse;
//!   * menu-safe with no predicate -- menu navigation never flows through the character's action
//!     requests, so a persistent disable does not lock the player out of their own inventory.
//!
//! ## Scope, deliberately narrow
//!
//! Env-driven, no world feature, no slot_data: `ER_ABILITY_LOCK_TEST="roll,r1,l2"`. `heal` is not
//! here (its mechanism is the flask-charge clamp, spec 4.1). `crouch` -> `l3` is the one
//! unverified action map (see er_logic).
//!
//! ## The read-back rule (spec 4.3)
//!
//! `no_equip_load` spent a month writing a field no code read, logging success. So this logs every
//! locked action the player actually pressed (`new_action_presses`, rate-limited per ability) with
//! a session tally: if the log never says "blocked", the mask is not proven to be doing anything.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use eldenring::cs::WorldChrMan;
use fromsoftware_shared::FromStatic;

use er_logic::ability_lock::{Ability, chr_action_mask, parse_set, requested_locked, set_names};

static CONFIG: OnceLock<u8> = OnceLock::new();
static CLOCK: OnceLock<Instant> = OnceLock::new();
/// Per-ability last-log timestamp (ms), indexed by this module's bit position; 0 = never.
static LAST_LOG_MS: [AtomicU64; 7] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
static BLOCKED_TOTAL: AtomicU32 = AtomicU32::new(0);
const LOG_SPACING_MS: u64 = 1500;

fn now_ms() -> u64 {
    CLOCK.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

fn locked_set() -> u8 {
    *CONFIG.get_or_init(|| {
        let Ok(v) = std::env::var("ER_ABILITY_LOCK_TEST") else {
            return 0;
        };
        match parse_set(&v) {
            Ok(0) => {
                log::info!("ability-lock: ER_ABILITY_LOCK_TEST is set but names nothing -- inactive");
                0
            }
            Ok(s) => {
                log::info!(
                    "ability-lock TEST ACTIVE (#945): [{}] disabled at the game's LOGICAL action \
                     layer (CSChrActionRequestModule.disabled_action_inputs). Keybind-agnostic and \
                     device-agnostic -- rebinds and keyboard/mouse are covered; menus are never \
                     affected. Blocked presses are logged; unset ER_ABILITY_LOCK_TEST to disable.",
                    set_names(s)
                );
                s
            }
            Err(e) => {
                log::warn!("ability-lock: ER_ABILITY_LOCK_TEST rejected ({e}) -- INACTIVE");
                0
            }
        }
    })
}

/// Per-frame: disable the locked abilities on the player's action-request module. Call from the
/// overlay frame hook. Cheap and fail-open -- if the player or the module is not up this frame,
/// it does nothing (a lock that flickers off for a frame is a curiosity; a panic in here is not).
pub fn enforce() {
    let locked = locked_set();
    if locked == 0 {
        return;
    }
    let mask = chr_action_mask(locked);
    if mask == 0 {
        return;
    }
    // SAFETY: FD4 singleton, mutated only on the single-threaded tick -- the same contract every
    // other player write in this crate (scaling, traps) relies on.
    let Ok(wcm) = (unsafe { WorldChrMan::instance_mut() }) else {
        return;
    };
    let Some(player) = wcm.main_player.as_mut() else {
        return;
    };
    let module = &mut *player.chr_ins.modules.action_request;

    // Read-back BEFORE we clear anything: which locked actions were newly pressed this frame.
    // ChrActions is a u64-backed bitfield (private tuple); reinterpret as the u64 it is.
    let presses = unsafe { *(&module.new_action_presses as *const _ as *const u64) };
    let hit = requested_locked(locked, presses);

    // 1) the game's own disable field -- persistent, timing-insensitive.
    unsafe { *(&mut module.disabled_action_inputs as *mut _ as *mut u64) |= mask };
    // 2) belt-and-suspenders: clear this frame's requests/presses so nothing acts on them even if
    //    disabled_action_inputs turns out not to gate a given action.
    unsafe { *(&mut module.action_requests as *mut _ as *mut u64) &= !mask };
    unsafe { *(&mut module.new_action_presses as *mut _ as *mut u64) &= !mask };

    if hit != 0 {
        report(hit);
    }
}

/// Rate-limited per-ability read-back log + running session tally.
fn report(hit: u8) {
    let now = now_ms();
    for a in Ability::ALL {
        if hit & a.bit() == 0 {
            continue;
        }
        let total = BLOCKED_TOTAL.fetch_add(1, Ordering::Relaxed) + 1;
        let slot = &LAST_LOG_MS[a.bit().trailing_zeros() as usize];
        let last = slot.load(Ordering::Relaxed);
        if now.saturating_sub(last) < LOG_SPACING_MS && last != 0 {
            continue;
        }
        slot.store(now, Ordering::Relaxed);
        log::info!(
            "ability-lock: blocked {} (locked; {total} blocked action press(es) this session)",
            a.name()
        );
    }
}
