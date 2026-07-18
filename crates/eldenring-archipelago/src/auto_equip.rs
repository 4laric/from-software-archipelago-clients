//! auto_equip — when `options.auto_equip` is on, equip a received WEAPON into a primary hand slot.
//!
//! STATUS: the option parsing, weapon detection + hand routing (`er_logic::auto_equip`), and the
//! receive -> queue -> presence-gated `tick` pipeline are all COMPLETE and correct. The one missing
//! piece is the game's real WEAPON-EQUIP FUNCTION, and the module ships INERT (`EQUIP_FUNC_RVA == 0`)
//! until it's resolved -- `tick` logs once and no-ops, never wrong.
//!
//! WHY NOT ReplaceTool: the Hexinton table's `ReplaceTool` (AOB `?? 0f b6 f1 ?? 8b d8 ...` - 0x19) is
//! GOODS-ONLY. It masks the id to 28 bits and `bts`-sets the goods category bit, reconstructing a
//! GOODS full id -- which is why the table uses it solely for "Set flask level" (passing raw goods
//! rows 1000/1001/1050/1051). Feeding it a weapon row would look up the same-numbered GOODS row. So
//! it is NOT the weapon-equip fn; the real one must be located (find-what-writes on `equipment_entries.
//! weapon_primary_right` while equipping in the menu -> the writer is/leads to the equip fn; capture
//! its entry + call signature). A raw ItemId write to the slot won't do either: the game keeps three
//! coupled reps -- the `equipment_entries` ItemId, `chr_asm.gaitem_handles[slot]`, and
//! `chr_asm.equipment_param_ids[slot]` -- plus an arm-style/model refresh, and only the equip fn
//! updates all of them.
//!
//! Hand routing (pure, host-tested): shields -> LEFT primary hand, every other weapon class -> RIGHT
//! (main hand), from `EQUIP_PARAM_WEAPON_ST.wep_type`.
//!
//! RESOLUTION: like `warp.rs`, the fn will be pinned by RVA + a prologue signature verified against
//! the running image. The call shape below (equipData, current_id, replace_id, flag) is a PLACEHOLDER
//! copied from ReplaceTool; the real fn's signature is TBD and this call is rewritten once the probe
//! lands. The RVA-0 guard keeps the placeholder unreachable, so it can never fire with a wrong sig.

use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{EquipParamWeapon, GameDataMan, SoloParamRepository};
use er_logic::auto_equip::Hand;
use fromsoftware_shared::FromStatic;

/// Weapon-equip fn entry RVA on 2.6.2.0. **PROBE-PENDING** — 0 means unresolved; `tick` stays inert
/// until this (and the prologue below) are filled. The function itself is TBD (ReplaceTool ruled out;
/// see module docs) -- locate it via find-what-writes on the weapon-right equip slot.
const EQUIP_FUNC_RVA: usize = 0x0;
/// First 16 bytes at the entry, read from the pinned exe. A mismatch = stale RVA for the running
/// build -> refuse to call. **PROBE-PENDING** — empty until the scan reports it.
const EQUIP_FUNC_SIG: &[u8] = &[];

/// PLACEHOLDER call shape (from ReplaceTool): rcx=equipGameData, rdx=current_id, r8=replace_id,
/// r9=flag. The real weapon-equip fn's signature is TBD; rewritten when the probe lands.
type EquipFn = unsafe extern "C" fn(*mut c_void, i32, i32, i32) -> u64;
/// Placeholder flag arg.
const EQUIP_FLAG: i32 = 1;

static ENABLED: AtomicBool = AtomicBool::new(false);
static PENDING: Mutex<Vec<i32>> = Mutex::new(Vec::new());
/// One-time "not yet probed" log guard so the inert path logs exactly once.
static UNRESOLVED_LOGGED: AtomicBool = AtomicBool::new(false);

/// Set from slot_data `options.auto_equip` at connect.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if on {
        log::info!("auto_equip: enabled (received weapons -> primary hand)");
    }
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Queue a received weapon FullID for equipping. Called from the received-item loop for weapon-
/// category items when `auto_equip` is on. No-op if disabled (belt-and-braces; the caller gates too).
pub fn enqueue(full_id: i32) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if let Ok(mut q) = PENDING.lock() {
        q.push(full_id);
    }
}

fn current_module_base() -> Option<usize> {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    let hmodule = unsafe { GetModuleHandleW(None) }.ok()?;
    Some(hmodule.0 as usize)
}

/// Resolve the pinned weapon-equip entry, verifying the prologue. Returns `None` (logging once)
/// while PROBE-PENDING or on a signature mismatch — the feature simply stays inert.
fn equip_fn(base: usize) -> Option<EquipFn> {
    // PROBE-PENDING gate: RVA 0 means unresolved. When the probe fills RVA it fills SIG too, so the
    // prologue check below is meaningful; a lone RVA with an empty SIG can't happen by construction.
    if EQUIP_FUNC_RVA == 0 {
        if !UNRESOLVED_LOGGED.swap(true, Ordering::Relaxed) {
            log::warn!(
                "auto_equip: weapon-equip fn RVA/signature PROBE-PENDING -- feature inert until pinned"
            );
        }
        return None;
    }
    let addr = base + EQUIP_FUNC_RVA;
    // SAFETY: reads EQUIP_FUNC_SIG.len() bytes inside the loaded eldenring.exe image.
    let actual = unsafe { std::slice::from_raw_parts(addr as *const u8, EQUIP_FUNC_SIG.len()) };
    if actual != EQUIP_FUNC_SIG {
        if !UNRESOLVED_LOGGED.swap(true, Ordering::Relaxed) {
            log::warn!(
                "auto_equip: weapon-equip fn signature mismatch @ {addr:#x} -- pinned 2.6.2.0 RVA stale for this build"
            );
        }
        return None;
    }
    // SAFETY: signature verified; entry is the pinned game fn.
    Some(unsafe { std::mem::transmute::<usize, EquipFn>(addr) })
}

/// The base weapon param row for a weapon FullID: category is 0 (weapon), so param_id == row; round
/// to the nearest 100 so an upgraded/affinity id resolves to its base `EQUIP_PARAM_WEAPON_ST` row
/// (same rounding the game's `get_equip_param` does).
fn base_weapon_row(full_id: i32) -> u32 {
    let row = (full_id as u32) & 0x0FFF_FFFF;
    (row / 100) * 100
}

/// Per-tick until the pending queue drains: equip each received weapon that is now in inventory.
/// Gated on `auto_equip` + in-world; a not-yet-owned weapon stays queued for a later tick.
pub fn tick() {
    if !ENABLED.load(Ordering::Relaxed) || !crate::flags::in_world() {
        return;
    }
    let pending: Vec<i32> = match PENDING.lock() {
        Ok(q) if !q.is_empty() => q.clone(),
        _ => return,
    };

    let base = match current_module_base() {
        Some(b) => b,
        None => return,
    };
    let Some(equip) = equip_fn(base) else {
        return; // PROBE-PENDING or stale sig -- keep the queue; nothing granted
    };

    // SAFETY: FD4 singletons; read/called on the single-threaded FrameBegin tick (same as inventory.rs
    // / no_equip_load.rs). We take a raw pointer to the embedded EquipGameData for the game fn.
    let Ok(gdm) = (unsafe { GameDataMan::instance() }) else {
        return;
    };
    let pgd = gdm.main_player_game_data.as_ref();
    let equip_ptr = &pgd.equipment as *const _ as *mut c_void;

    // Presence snapshot (only weapons matter): a queued weapon is equipped only once it's in the bag.
    let mut present: HashSet<u32> = HashSet::new();
    for entry in pgd.equipment.equip_inventory_data.items_data.items() {
        present.insert(((entry.item_id.category() as u32) << 28) | entry.item_id.param_id());
    }

    let Ok(repo) = (unsafe { SoloParamRepository::instance() }) else {
        return;
    };

    let mut still_pending: Vec<i32> = Vec::new();
    for fid in pending {
        let full = fid as u32;
        if !present.contains(&full) {
            still_pending.push(fid); // not owned yet -- retry next tick
            continue;
        }
        // Route by weapon type; a row the param table doesn't know defaults to the right (main) hand.
        let hand = repo
            .get::<EquipParamWeapon>(base_weapon_row(fid))
            .map(|w| er_logic::auto_equip::hand_for_wep_type(w.wep_type()))
            .unwrap_or(Hand::Right);
        let current = match hand {
            Hand::Left => pgd.equipment.equipment_entries.weapon_primary_left,
            Hand::Right => pgd.equipment.equipment_entries.weapon_primary_right,
        };
        let current_id = current.into_inner() as i32;
        if current_id == fid {
            continue; // already in that hand -- done
        }
        // SAFETY: game fn on the game thread; equip_ptr is the live EquipGameData, ids are valid.
        let rc = unsafe { equip(equip_ptr, current_id, fid, EQUIP_FLAG) };
        log::info!(
            "auto_equip: {:?} hand {current_id:#010x} -> {fid:#010x} (equip rc={rc})",
            hand
        );
    }

    if let Ok(mut q) = PENDING.lock() {
        *q = still_pending;
    }
}
