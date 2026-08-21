//! Runtime port of `no_weapon_requirements` (2026-07-01). The option's game half was BAKED
//! (regulation.bin stat-requirement zeroing by the retired baker), so on a vanilla-regulation
//! pure-runtime install it silently did nothing. This zeroes the requirements in LIVE param
//! memory instead, once per session: `EQUIP_PARAM_WEAPON_ST proper_{strength,agility,magic,
//! faith,luck}` (weapons and ammo share the table) and `MAGIC_PARAM_ST
//! requirement_{intellect,faith,luck}` (spells). Writes are process-local and re-applied each
//! launch; zeroing only ever LOWERS requirements, so a re-run or a mid-session reconnect is safe.

use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{EquipParamWeapon, Magic, SoloParamRepository};
use fromsoftware_shared::FromStatic;

static ENABLED: AtomicBool = AtomicBool::new(false);
static APPLIED: AtomicBool = AtomicBool::new(false);

/// Set from slot_data `options.no_weapon_requirements` at connect.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if on {
        log::info!("no_weapon_requirements: enabled (runtime param zeroing)");
    }
}

/// Re-arm the once-per-session zeroing. Called from core.rs's `in_world` false->true edge: a map
/// load streams EquipParamWeapon / Magic back in and restores the vanilla requirements, and APPLIED
/// would otherwise stop `tick()` ever re-zeroing them -- the option would work until the player's
/// first load and then quietly stop, which reads as "the option does nothing". Clears the LATCH
/// ONLY: `ENABLED` comes from slot_data at connect and must survive the edge. Safe to re-run --
/// zeroing only ever LOWERS a requirement, which the module doc already relies on for reconnects.
pub fn reset() {
    APPLIED.store(false, Ordering::Relaxed);
}

/// Per-tick until applied: zero all weapon/spell stat requirements. Needs the param repo
/// populated (in-world); retries until rows are visible, then latches for the session.
pub fn tick() {
    if !ENABLED.load(Ordering::Relaxed) || APPLIED.load(Ordering::Relaxed) {
        return;
    }
    // MENU/BOOT GATE (2026-07-02): the param repo's holders exist but are NOT settled during
    // early boot / at the main menu -- the crate's rows_mut PANICS there ("Expected param
    // holder to have exactly one res cap"; seen live: auto-connect at T+1s -> instant crash on
    // every launch of a shop seed). in_world() is the same signal every other param writer
    // gates on; the params only matter in-world anyway.
    if !crate::flags::in_world() {
        return;
    }
    // SAFETY: FD4 singleton; only mutated on the single-threaded FrameBegin tick.
    let Ok(repo) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    // #351: mid-restream holder -> panics upstream. Defer instead: APPLIED is not latched, so
    // the pass re-runs next tick (the world-edge reset() re-arms it after a restream anyway).
    let Some(rows) =
        crate::param_guard::rows_mut::<EquipParamWeapon>(repo, "no_weapon_requirements")
    else {
        return;
    };
    let mut weapons = 0u32;
    for (_id, row) in rows {
        row.set_proper_strength(0);
        row.set_proper_agility(0);
        row.set_proper_magic(0);
        row.set_proper_faith(0);
        row.set_proper_luck(0);
        weapons += 1;
    }
    if weapons == 0 {
        return; // param file not populated yet -- retry next tick
    }
    let Some(rows) = crate::param_guard::rows_mut::<Magic>(repo, "no_weapon_requirements") else {
        return;
    };
    let mut spells = 0u32;
    for (_id, row) in rows {
        row.set_requirement_intellect(0);
        row.set_requirement_faith(0);
        row.set_requirement_luck(0);
        spells += 1;
    }
    APPLIED.store(true, Ordering::Relaxed);
    log::info!("no_weapon_requirements: zeroed {weapons} weapon rows + {spells} spell rows");
}
