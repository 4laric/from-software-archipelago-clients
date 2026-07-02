//! Ticker-only pickup notifications (runtime port, 2026-07-01).
//!
//! Alaric's preferred UX: an AP-granted item shows ONLY the native right-side item-gain ticker
//! ("Golden Rune [4] x1") -- non-blocking, no input -- and NOT the full-screen "NEW ... Y:OK"
//! acquisition modal that halts the game on every receive.
//!
//! Two independent per-item paramdef fields exist on all five grantable equip param types:
//! - `showLogCondType`  (acquisition LOG   = the right-side ticker; vanilla default 1 = ON) -- LEAVE.
//! - `showDialogCondType`(acquisition DIALOG= the "NEW Y:OK" modal; enum GET_DIALOG_CONDITION_TYPE,
//!   vanilla default 2 = "new only") -- set to 0 = None so the modal never fires.
//!
//! This WAS a regulation.bin bake (`SoulsRandomizers MiscSetup.cs EldenCommonPass`, confirmed
//! in-game 2026-06-14: every pickup ticker-only, no modal). The baker retired 2026-07-01, so on a
//! vanilla-regulation pure-runtime install the modal came back. Same fix, done in LIVE param memory
//! once per session -- identical pattern to `no_weapon_reqs::tick()`. Crafting materials (Rowa
//! Fruit etc.) already ship dialog=0 in vanilla, which is exactly the look we replicate game-wide.
//!
//! Process-local, idempotent, re-applied each launch. Editing only the dialog field leaves the
//! ticker untouched. ON by default (matches the retired bake's always-on behavior); a slot_data /
//! option toggle can gate it later if anyone wants the vanilla modal back.

use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{
    EquipParamAccessory, EquipParamGem, EquipParamGoods, EquipParamProtector, EquipParamWeapon,
    SoloParamRepository,
};
use fromsoftware_shared::FromStatic;

/// enum GET_DIALOG_CONDITION_TYPE value "None" -- never show the blocking acquisition modal.
const DIALOG_NONE: u8 = 0;

static ENABLED: AtomicBool = AtomicBool::new(true);
static APPLIED: AtomicBool = AtomicBool::new(false);

/// Opt out (e.g. from a future slot_data toggle). ON by default.
#[allow(dead_code)] // no caller yet by design: the opt-out half of the API waits for the
// slot_data/option toggle promised in the module docs; keeping it so the toggle is a one-line
// wire-up in core.rs configure rather than a re-derivation.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Per-tick until applied: set `showDialogCondType = 0` on every row of the five grantable equip
/// param types, leaving `showLogCondType` (the ticker) alone. Needs the param repo populated
/// (in-world); retries until rows are visible, then latches for the session.
pub fn tick() {
    if !ENABLED.load(Ordering::Relaxed) || APPLIED.load(Ordering::Relaxed) {
        return;
    }
    // MENU/BOOT GATE (2026-07-02): same crash family as no_weapon_reqs -- the crate's rows_mut
    // PANICS ("exactly one res cap") on unsettled param holders during early boot / main menu.
    // Seen live: url in apconfig -> auto-connect at T+1s -> instant crash every launch.
    if !crate::flags::in_world() {
        return;
    }
    // SAFETY: FD4 singleton; only mutated on the single-threaded FrameBegin tick (same contract
    // as no_weapon_reqs::tick()).
    let Ok(repo) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    let mut n = 0u32;
    for (_id, row) in repo.rows_mut::<EquipParamGoods>() {
        row.set_show_dialog_cond_type(DIALOG_NONE);
        n += 1;
    }
    if n == 0 {
        return; // param file not populated yet -- retry next tick
    }
    for (_id, row) in repo.rows_mut::<EquipParamWeapon>() {
        row.set_show_dialog_cond_type(DIALOG_NONE);
        n += 1;
    }
    for (_id, row) in repo.rows_mut::<EquipParamProtector>() {
        row.set_show_dialog_cond_type(DIALOG_NONE);
        n += 1;
    }
    for (_id, row) in repo.rows_mut::<EquipParamAccessory>() {
        row.set_show_dialog_cond_type(DIALOG_NONE);
        n += 1;
    }
    for (_id, row) in repo.rows_mut::<EquipParamGem>() {
        row.set_show_dialog_cond_type(DIALOG_NONE);
        n += 1;
    }
    APPLIED.store(true, Ordering::Relaxed);
    log::info!("notif-ticker: set showDialogCondType=0 on {n} item rows (ticker-only pickups)");
}
