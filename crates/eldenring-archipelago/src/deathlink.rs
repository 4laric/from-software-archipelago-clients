//! DeathLink (Milestone B Stage 5).
//!
//! INCOMING (works): a foreign DeathLink event latches a kill; the game tick sets the baked flag
//! 76996, whose `common.emevd` reactor performs the keep-runes kill (same pattern as the region-lock
//! KICK at 76970). Clears the latch only on a successful flag set, so a kill latched on a menu/load
//! screen retries until in-world.
//!
//! OUTGOING (RE-hole): detecting a LOCAL death needs the player HP / death-state cell, which the
//! standalone never resolved in the `eldenring` crate. `read_local_death` returns false for now, so
//! outgoing is inert; INCOMING is unaffected. Fill `read_local_death` (WorldChrMan.main_player → HP)
//! to enable sending, and add a post-incoming-kill grace window so our own baked kill doesn't echo.

use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::WorldChrMan;
use fromsoftware_shared::FromStatic;

/// Baked common.emevd reactor flag for the DeathLink kill (keep-runes).
const DEATHLINK_KILL_FLAG: u32 = 76996;

static ENABLED: AtomicBool = AtomicBool::new(false);
static KILL_PENDING: AtomicBool = AtomicBool::new(false);
static WAS_DEAD: AtomicBool = AtomicBool::new(false);

/// Set from slot_data `options.death_link` at connect.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    log::info!("DeathLink: {}", if on { "ENABLED" } else { "off" });
}
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// A foreign DeathLink arrived (caller already applied the self-source guard): latch a kill.
pub fn latch_incoming_kill() {
    KILL_PENDING.store(true, Ordering::Relaxed);
}

/// Per-tick: if a kill is pending and we're in-world, set the baked kill flag. The bake kills the
/// player (keeping runes). Latch clears only on a successful set (retries across load screens).
pub fn drive_kill() {
    if KILL_PENDING.load(Ordering::Relaxed)
        && crate::flags::in_world()
        && crate::flags::try_set_event_flag(DEATHLINK_KILL_FLAG, true)
    {
        KILL_PENDING.store(false, Ordering::Relaxed);
        // Pre-arm the outgoing edge so OUR baked kill (HP->0 next frames) isn't re-read as a fresh
        // local death and echoed back to teammates. Cleared again when we revive (HP>0).
        WAS_DEAD.store(true, Ordering::Relaxed);
        log::info!("DeathLink: incoming kill applied (flag {DEATHLINK_KILL_FLAG})");
    }
}

/// Rising-edge local-death detector for OUTGOING DeathLink — true exactly once per death.
/// Inert until `read_local_death` is filled (see module doc).
pub fn poll_local_death() -> bool {
    let dead = read_local_death();
    let was = WAS_DEAD.swap(dead, Ordering::Relaxed);
    dead && !was
}

/// True when local player current HP <= 0. Reads HP via the Hexinton all-in-one v6.0 CE table chain
/// (`elden_ring_artifacts`), anchored on the crate's typed `WorldChrMan.main_player` (so we avoid a
/// CE session). The trailing offsets are pinned to `eldenring.exe` 2.6.2.0, like the AddItemFunc RVA:
///   main_player +0x10 -> [+0x190] -> [+0x0] -> +0x138 (current HP, i32).
/// Each hop is range-guarded so a stale offset returns "not dead" rather than dereferencing garbage.
/// (If the typed `ChrIns`→`CSChrDataModule.hp` field is exposed on `main`, prefer that — version-robust.)
fn read_local_death() -> bool {
    let wcm = match unsafe { WorldChrMan::instance() } {
        Ok(w) => w,
        Err(_) => return false,
    };
    let Some(player) = wcm.main_player.as_ref() else {
        return false; // not in-world / no local player
    };
    let base = player as *const _ as usize;
    let Some(p1) = read_ptr(base, 0x10) else {
        return false;
    };
    let Some(p2) = read_ptr(p1, 0x190) else {
        return false;
    };
    let Some(p3) = read_ptr(p2, 0x0) else {
        return false;
    };
    let Some(hp_addr) = plausible(p3.wrapping_add(0x138)) else {
        return false;
    };
    let hp = unsafe { *(hp_addr as *const i32) };
    // Guard against a wild value from a wrong offset (treat absurd HP as "not dead").
    (0..=0x000F_FFFF).contains(&hp) && hp <= 0
}

/// Plausible user-space pointer range (x64). Cheap guard so a wrong CE offset can't deref garbage.
fn plausible(addr: usize) -> Option<usize> {
    if (0x1_0000..0x7FFF_FFFF_FFFF).contains(&addr) {
        Some(addr)
    } else {
        None
    }
}

/// Read a pointer at `base + off`, returning it only if both the source and the value are plausible.
fn read_ptr(base: usize, off: usize) -> Option<usize> {
    let src = plausible(base.checked_add(off)?)?;
    let val = unsafe { *(src as *const usize) };
    plausible(val)
}
