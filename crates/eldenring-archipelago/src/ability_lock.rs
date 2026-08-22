//! ability_lock.rs -- enforcement of individual ability locks (er-archipelago#945,
//! SPEC-ability-lock-mode). The decision is `er_logic::ability_lock` (host-tested); this file arms
//! it against the running game and owns the runtime lock/unlock state.
//!
//! ## The mechanism: the game's own logical-action layer
//!
//! ER resolves every input -- gamepad, keyboard, mouse -- into `ChrActions` on the player's
//! `CSChrActionRequestModule` (`chr_ins.modules.action_request`), one bit per LOGICAL action
//! (r1, jump, rolling, ...), AFTER the keybind map. Each frame [`enforce`] makes that module's
//! `disabled_action_inputs` -- the game's OWN "these actions are off" field -- agree with our
//! state: locked abilities' bits are SET, and abilities we govern but have UNLOCKED are CLEARED
//! (so an unlock actually restores the action, not just "stop re-locking it"). It also clears the
//! locked bits from `action_requests`/`new_action_presses` the same frame. This is:
//!   * KEYBIND-AGNOSTIC -- rebind roll to any key/button, it still sets the `rolling` bit;
//!   * device-agnostic -- one path covers pad AND keyboard/mouse;
//!   * menu-safe with no predicate -- menu navigation never flows through the character's action
//!     requests, so a persistent disable does not lock the player out of menus.
//!
//! ## Runtime state: MANAGED and LIVE
//!
//! [`MANAGED`] is every ability this feature has governed this session (its domain -- the only
//! bits it will ever clear, so it never touches a disable the GAME set for its own reasons).
//! [`LIVE`] is the currently-locked subset (`LIVE ⊆ MANAGED`). Unlock clears a LIVE bit; the next
//! frame `enforce` restores the action. This is the seam a future find-to-unlock item plugs into:
//! on receiving an "unlock roll" item the client calls [`unlock`], exactly what the `!ability`
//! console lever does by hand today.
//!
//! ## Where the locked set comes from
//!
//! Two sources, slot_data preferred: the apworld's `options.locked_abilities` (parsed by
//! `er_logic::options::parse_ability_lock` and installed via [`set_locked_mask`]), falling back to
//! the `ER_ABILITY_LOCK_TEST="roll,r1,l2"` env var for test builds whose apworld predates the
//! option. `heal` is not here (its mechanism is the flask-charge clamp, spec 4.1). `crouch` -> `l3`
//! is the one unverified action map (see er_logic).
//!
//! ## The read-back rule (spec 4.3)
//!
//! `no_equip_load` spent a month writing a field no code read, logging success. So this logs every
//! locked action the player actually pressed (`new_action_presses`, rate-limited per ability) with
//! a session tally: if the log never says "blocked", the mask is not proven to be doing anything.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use eldenring::cs::WorldChrMan;
use fromsoftware_shared::FromStatic;

use er_logic::ability_lock::{Ability, chr_action_mask, parse_set, requested_locked, set_names};

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

/// The feature's domain: every ability it has governed this session. Only these bits are ever
/// cleared from the game's disable field, so a disable the GAME set is never disturbed.
static MANAGED: AtomicU8 = AtomicU8::new(0);
/// The currently-locked subset (`⊆ MANAGED`). The `u8` is er_logic's Ability set, not ChrActions.
static LIVE: AtomicU8 = AtomicU8::new(0);
/// Whether the locked set has been sourced yet (env or slot_data). The env read is one-shot.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

fn now_ms() -> u64 {
    CLOCK.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

/// One-shot env bootstrap: the fallback source for test builds whose apworld has no
/// `locked_abilities` option. `set_locked_mask` (slot_data) overrides this whenever it carries a
/// non-empty set. Idempotent -- the env is read at most once.
fn ensure_initialized() {
    if INITIALIZED.swap(true, Ordering::AcqRel) {
        return;
    }
    let Ok(v) = std::env::var("ER_ABILITY_LOCK_TEST") else {
        return;
    };
    match parse_set(&v) {
        Ok(0) => log::info!("ability-lock: ER_ABILITY_LOCK_TEST names nothing -- inactive"),
        Ok(s) => {
            MANAGED.fetch_or(s, Ordering::Relaxed);
            LIVE.store(s, Ordering::Relaxed);
            log::info!(
                "ability-lock ACTIVE from env (#945): [{}] disabled at the game's LOGICAL action \
                 layer -- keybind- and device-agnostic, menus unaffected. `!ability` toggles at \
                 runtime; blocked presses are logged.",
                set_names(s)
            );
        }
        Err(e) => log::warn!("ability-lock: ER_ABILITY_LOCK_TEST rejected ({e}) -- INACTIVE"),
    }
}

/// Install the locked set from slot_data (`options.locked_abilities`). Wins over the env fallback.
/// A zero mask is ignored so an absent/empty option cannot wipe an env-armed test set.
pub fn set_locked_mask(mask: u8) {
    INITIALIZED.store(true, Ordering::Release);
    if mask == 0 {
        return;
    }
    MANAGED.fetch_or(mask, Ordering::Relaxed);
    LIVE.store(mask, Ordering::Relaxed);
    log::info!(
        "ability-lock ACTIVE from slot_data (#945): [{}] locked at the logical-action layer.",
        set_names(mask)
    );
}

/// The currently-locked set (for the console readout).
pub fn live_set() -> u8 {
    LIVE.load(Ordering::Relaxed)
}
/// The feature's domain (every ability it governs).
pub fn managed_set() -> u8 {
    MANAGED.load(Ordering::Relaxed)
}

/// Unlock one ability at runtime: clear its LIVE bit. `enforce` restores the action next frame.
/// The ability stays MANAGED, so it can be re-locked. This is the future item-unlock entry point.
pub fn unlock(a: Ability) {
    LIVE.fetch_and(!a.bit(), Ordering::Relaxed);
}
/// Lock one ability at runtime: set its LIVE bit and admit it to the managed domain.
pub fn lock(a: Ability) {
    MANAGED.fetch_or(a.bit(), Ordering::Relaxed);
    LIVE.fetch_or(a.bit(), Ordering::Relaxed);
    INITIALIZED.store(true, Ordering::Release);
}
/// Unlock every managed ability (leaves the domain intact so they can be re-locked).
pub fn unlock_all() {
    LIVE.store(0, Ordering::Relaxed);
}
/// Re-lock every managed ability.
pub fn lock_all() {
    LIVE.store(MANAGED.load(Ordering::Relaxed), Ordering::Relaxed);
}

/// Per-frame: make the player's `disabled_action_inputs` agree with (MANAGED, LIVE). Call from the
/// overlay frame hook. Fail-open -- if the player/module is not up this frame it does nothing.
pub fn enforce() {
    ensure_initialized();
    let managed = MANAGED.load(Ordering::Relaxed);
    if managed == 0 {
        return; // feature never armed -- touch nothing
    }
    let live = LIVE.load(Ordering::Relaxed);
    let mask_live = chr_action_mask(live);
    let mask_managed = chr_action_mask(managed);
    // Managed bits that are currently UNLOCKED -- to be restored (cleared from the disable field).
    let restore = mask_managed & !mask_live;

    // SAFETY: FD4 singleton, mutated only on the single-threaded tick -- the same contract every
    // other player write in this crate (scaling, traps) relies on.
    let Ok(wcm) = (unsafe { WorldChrMan::instance_mut() }) else {
        return;
    };
    let Some(player) = wcm.main_player.as_mut() else {
        return;
    };
    let module = &mut *player.chr_ins.modules.action_request;

    // ChrActions is a u64-backed bitfield (private tuple); reinterpret as the u64 it is.
    let disabled = &mut module.disabled_action_inputs as *mut _ as *mut u64;
    let requests = &mut module.action_requests as *mut _ as *mut u64;
    let presses = &mut module.new_action_presses as *mut _ as *mut u64;

    // Read-back BEFORE clearing: which locked actions were newly pressed this frame.
    let hit = requested_locked(live, unsafe { *presses });

    unsafe {
        *disabled |= mask_live; //   lock:   set the disable bit
        *disabled &= !restore; //    unlock: restore the managed-but-unlocked bit
        *requests &= !mask_live; //  belt-and-suspenders: drop this frame's locked requests
        *presses &= !mask_live;
    }

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
