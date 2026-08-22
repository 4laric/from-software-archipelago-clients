//! ability_lock.rs — TEST-BUILD enforcement of individual ability locks (er-archipelago#945,
//! SPEC-ability-lock-mode §4.3). The decision is er_logic::ability_lock (host-tested against
//! the probe-measured menu states); this file is the arm.
//!
//! ## Scope, deliberately narrow
//!
//! Env-driven, no world feature, no slot_data: `ER_ABILITY_LOCK_TEST="roll,r1,l2"` masks those
//! abilities' GAMEPAD inputs while the game is in gameplay (`ChrMenuFlags & 0b1100 == 0`, the
//! 2026-08-21 probe finding) and no NPC conversation is live (`esd_probe::inventory_grants_safe`
//! — B is "back" in dialogue). XInput only: ER pads poll `XInputGetState`, which `input.rs`
//! already detours; keyboard/mouse masking needs the player's live binds and ships later or not
//! at all. Not blocked on the SpEffect-9621 field test — that blanket path layers on top if it
//! proves out.
//!
//! ## The read-back rule (spec §4.3)
//!
//! `no_equip_load` spent a month writing a field no code read, logging success. So this module
//! logs every suppressed press (rate-limited per ability) and keeps a session tally — if the
//! log never says "masked", the mask is not proven to be doing anything, whatever this header
//! claims.
//!
//! ## Threading and the #372 lesson
//!
//! `XInputGetState` is called from the game's input path, not our tick, and the hook frame
//! cannot unwind — so every game-object read here sits inside `catch_unwind`, and any failure
//! (menu manager not up, mid-teardown, a panic underneath) degrades to NO MASK for that poll.
//! Fail-open on purpose: a lock that flickers off for a frame is a curiosity; an input system
//! that panics in a nounwind frame is crash-19968's cousin.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use fromsoftware_shared::FromStatic;

use er_logic::ability_lock::{
    Ability, GamepadMask, gamepad_mask, parse_set, set_names, suppressed,
};

static CONFIG: OnceLock<u8> = OnceLock::new();
static CLOCK: OnceLock<Instant> = OnceLock::new();
/// Per-ability last-log timestamp (ms), indexed by bit position; 0 = never.
static LAST_LOG_MS: [AtomicU64; 7] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
static SUPPRESSED_TOTAL: AtomicU32 = AtomicU32::new(0);
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
                log::info!(
                    "ability-lock: ER_ABILITY_LOCK_TEST is set but names nothing -- inactive"
                );
                0
            }
            Ok(s) => {
                log::info!(
                    "ability-lock TEST ACTIVE (#945): [{}] masked on the GAMEPAD while in \
                     gameplay (menu predicate + talk gate; menus and dialogue are never \
                     masked). Keyboard/mouse are NOT masked in this build, and rebinding a \
                     locked action to another button evades it -- both are documented v1 \
                     limits. Suppressed presses are logged; unset ER_ABILITY_LOCK_TEST to \
                     disable.",
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

/// The context read: menu flags + talk gate, behind `catch_unwind`. `None` = could not read =
/// no masking this poll (fail-open; see the header).
fn mask_now(locked: u8) -> Option<GamepadMask> {
    std::panic::catch_unwind(|| {
        let mm = unsafe { eldenring::cs::CSMenuManImp::instance() }.ok()?;
        let flags = mm.player_menu_ctrl.chr_menu_flags.flags;
        // The bitfield macro keeps its tuple field private; the struct IS a plain u32 (see
        // fromsoftware-rs menu_man.rs) and Copy, so transmute is the whole accessor.
        let raw = unsafe { std::mem::transmute::<eldenring::cs::ChrMenuFlags, u32>(flags) };
        let talk_quiet = crate::esd_probe::inventory_grants_safe();
        Some(gamepad_mask(locked, raw, talk_quiet))
    })
    .ok()
    .flatten()
}

/// Called by `input.rs`'s `XInputGetState` detour with the freshly-read gamepad fields, AFTER
/// its whole-device block. Edits in place. Cheap when inactive: one OnceLock read.
pub fn filter_gamepad(buttons: &mut u16, left_trigger: &mut u8, right_trigger: &mut u8) {
    let locked = locked_set();
    if locked == 0 {
        return;
    }
    let Some(mask) = mask_now(locked) else {
        return;
    };
    if mask.is_empty() {
        return;
    }
    // Read-back BEFORE the edit: which locked abilities the player is pressing right now.
    let hit = suppressed(locked, mask, *buttons, *left_trigger, *right_trigger);
    if hit != 0 {
        report(hit);
    }
    *buttons &= !mask.clear_buttons;
    if mask.zero_left_trigger {
        *left_trigger = 0;
    }
    if mask.zero_right_trigger {
        *right_trigger = 0;
    }
}

fn report(hit: u8) {
    let now = now_ms();
    for a in Ability::ALL {
        if hit & a.bit() == 0 {
            continue;
        }
        let total = SUPPRESSED_TOTAL.fetch_add(1, Ordering::Relaxed) + 1;
        let slot = &LAST_LOG_MS[a.bit().trailing_zeros() as usize];
        let last = slot.load(Ordering::Relaxed);
        if now.saturating_sub(last) < LOG_SPACING_MS {
            continue;
        }
        slot.store(now, Ordering::Relaxed);
        log::info!(
            "ability-lock: masked {} (locked; {total} suppressed input poll(s) this session)",
            a.name()
        );
    }
}
