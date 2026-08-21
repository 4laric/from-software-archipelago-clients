//! Apply the whetblade check-flag split to the live params (model: `er_logic::whetblade`).
//!
//! Each whetblade's vanilla `ItemLotParam_map.getItemFlagId` is BOTH the menu's first-affinity
//! unlock and this world's check flag for that location, so the client repoints the LOT to the
//! client-owned check flag (65611/65641/65661/65681/65721). After this write: a pool receive can
//! set the affinity flag (keyitems.rs) without firing the check or despawning the treasure, and a
//! real pickup sets the NEW flag, which is what the poll map now watches (core.rs repoints it with
//! the same `er_logic::whetblade::repoint_poll_flags` call that produced our write list).
//!
//! House param-writer rules, learned the hard way by shop_sell/shop_stock/shop_icon:
//!   * DONE latch + `reset()` on the in_world false->true edge — a map load streams ItemLotParam
//!     back in and silently reverts the write (the 2026-07-21 DLC leak class). core.rs calls it in
//!     the same block as `check_lots::reset()`.
//!   * `run()` retries until the param repo is up; one clean pass re-latches.
//!   * Tolerance requires telemetry: a lot missing from the table is warned, not skipped silently.

#![allow(dead_code)]

use fromsoftware_shared::FromStatic;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// `(ItemLotParam_map row, new getItemFlagId)` — from `er_logic::whetblade::repoint_poll_flags`,
/// i.e. exactly the whetblade locations THIS seed uses as checks. Empty = nothing to do.
static REWRITES: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
static DONE: AtomicBool = AtomicBool::new(false);

/// Called from core.rs at slot_data parse, with the rewrite list the poll repoint emitted.
pub fn configure(rewrites: Vec<(u32, u32)>) {
    let n = rewrites.len();
    *REWRITES.lock().unwrap() = rewrites;
    DONE.store(false, Ordering::Relaxed);
    if n > 0 {
        log::info!(
            "whetblade-lots: configured {n} getItemFlagId repoint(s) (check split off the affinity flag)"
        );
    }
}

/// Apply. Returns false if the param repo isn't up yet (caller retries next tick).
pub fn run() -> bool {
    if DONE.load(Ordering::Relaxed) {
        return true;
    }
    let rewrites: Vec<(u32, u32)> = REWRITES.lock().unwrap().clone();
    if rewrites.is_empty() {
        DONE.store(true, Ordering::Relaxed);
        return true;
    }
    // SAFETY: FD4 singleton; game thread, in-world (caller gates). Same sanctioned mutable param
    // access check_lots / enemy_drops use on the live RW table.
    let repo = match unsafe { eldenring::cs::SoloParamRepository::instance_mut() } {
        Ok(r) => r,
        Err(_) => return false,
    };
    // clients#351: defer while the holder is mid-restream -- otherwise every lot lands in `missed`
    // ("stale table?") and DONE latches on a pass that wrote nothing.
    if !crate::param_guard::is_available::<eldenring::cs::ItemLotParam_map>(
        repo,
        "whetblade-lots repoint",
    ) {
        return false;
    }
    let mut n = 0usize;
    let mut missed: Vec<u32> = Vec::new();
    for (lot, flag) in &rewrites {
        if let Some(row) = crate::param_guard::get_mut::<eldenring::cs::ItemLotParam_map>(
            repo,
            *lot,
            "whetblade-lots repoint",
        ) {
            row.set_get_item_flag_id(*flag);
            n += 1;
        } else {
            missed.push(*lot);
        }
    }
    if !missed.is_empty() {
        log::warn!(
            "whetblade-lots: {} lot(s) not found in ItemLotParam_map {:?} -- their checks still \
             poll the REPOINTED flag, which nothing will now set: those locations cannot fire \
             until this resolves (stale table?)",
            missed.len(),
            missed
        );
    }
    log::info!(
        "whetblade-lots: repointed getItemFlagId on {n}/{} whetblade check lot(s)",
        rewrites.len()
    );
    DONE.store(true, Ordering::Relaxed);
    true
}

/// Re-arm: reconnect (configure) handles seed changes; THIS is for the map-load param stream-in
/// revert (core.rs world-edge block, next to check_lots::reset).
pub fn reset() {
    DONE.store(false, Ordering::Relaxed);
}
