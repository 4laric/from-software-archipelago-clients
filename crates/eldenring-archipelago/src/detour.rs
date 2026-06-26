//! `AddItemFunc` detour — ECHO model (`own_world:true`). Supersedes the stage-2 local-grant detour.
//!
//! Self-found synthetic pickup → report the check + SUPPRESS the world pickup, and let the server
//! ECHO the item back as a received item (the `update_live` received-item path grants it, running
//! progressive / region-open / notify by name). The detour does NOT grant locally — that's what kept
//! self-found progressive/region/notify from working under the old `own_world:false` local-grant.
//! `grant_full_id` stays (used by the received-item path). RVA + signature pinned to 2.6.2.0.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use eldenring::cs::GameDataMan;
use fromsoftware_shared::FromStatic;
use retour::GenericDetour;

use er_codec::{decode_synthetic, is_synthetic_goods, row_id_of};

use crate::params;

type AddItemFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, u64) -> u64;

static HOOK: OnceLock<GenericDetour<AddItemFn>> = OnceLock::new();
/// Live inventory pointer captured on every pickup; reused to grant items that never pass a pickup.
static LAST_INVENTORY: AtomicUsize = AtomicUsize::new(0);
/// AP location ids the detour suppressed, drained by `update_live` -> `mark_checked`.
static PENDING_CHECKS: Mutex<Vec<i64>> = Mutex::new(Vec::new());

/// EXPERIMENTAL — resolve the inventory pointer from a game static so server/start grants don't have
/// to wait for the player's first in-game pickup to capture it (the long-standing UX wart, inherited
/// from the standalone). DEFAULT OFF: a WRONG pointer would crash on grant. The detour logs a
/// one-time `inventory-ptr CONFIRM/MISMATCH` line comparing the static-resolved address to the
/// pointer the game actually hands it; once a run shows CONFIRM, flip this to `true` and rebuild.
const USE_STATIC_INVENTORY_PRIME: bool = false;
/// One-time guard for the static-vs-game inventory-pointer confirmation log.
static INV_PTR_CHECKED: AtomicBool = AtomicBool::new(false);

const ADD_ITEM_FUNC_RVA: usize = 0x0056_05B0;
const ADD_ITEM_FUNC_SIG: &[u8] = &[
    0x40, 0x55, 0x56, 0x57, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57, 0x48, 0x8D, 0xAC, 0x24,
];
const ITEMBUF_ENTRY_ID_OFF: usize = 0x04;
const ITEMBUF_ENTRY_OFF: usize = 0x20; // a constructed itembuf's entry sits at buf+0x20

pub fn install() -> Result<(), Box<dyn std::error::Error>> {
    if HOOK.get().is_some() {
        return Ok(());
    }
    let target_addr = current_module_base().ok_or("no module base for eldenring.exe")? + ADD_ITEM_FUNC_RVA;
    if !signature_matches(target_addr) {
        return Err(format!(
            "AddItemFunc signature mismatch @ {target_addr:#x} — pinned 2.6.2.0 RVA stale for this build"
        )
        .into());
    }
    let target: AddItemFn = unsafe { std::mem::transmute::<usize, AddItemFn>(target_addr) };
    let detour = unsafe { GenericDetour::<AddItemFn>::new(target, add_item_detour)? };
    unsafe {
        detour.enable()?;
    }
    let _ = HOOK.set(detour);
    log::info!("AddItemFunc detour installed @ {target_addr:#x}");
    Ok(())
}

pub fn take_pending_checks() -> Vec<i64> {
    std::mem::take(&mut *PENDING_CHECKS.lock().unwrap())
}

/// Whether the detour has captured a live inventory pointer yet (set on the player's first pickup).
/// `update_live` gates server-pushed grants on this so the receive watermark advances atomically.
pub fn has_inventory() -> bool {
    LAST_INVENTORY.load(Ordering::Relaxed) >= 0x10000
}

/// Address of `PlayerGameData.equipment.equip_inventory_data` — the structure AddItemFunc takes as
/// its inventory arg — resolved from the GameDataMan singleton (the SAME typed path `upgrades.rs`
/// walks in-world). `None` until the player is placed. SAFE to compute; whether it is the pointer the
/// game hands the detour is exactly what the confirmation log verifies.
fn static_inventory_ptr() -> Option<usize> {
    if !crate::flags::in_world() {
        return None;
    }
    let gdm = unsafe { GameDataMan::instance() }.ok()?;
    let pgd = gdm.main_player_game_data.as_ref();
    let inv = &pgd.equipment.equip_inventory_data as *const _ as usize;
    (inv >= 0x10000).then_some(inv)
}

/// Tick helper (game thread): if no inventory pointer is captured yet, seed `LAST_INVENTORY` from the
/// static path so grants flush WITHOUT waiting for a pickup. No-op unless `USE_STATIC_INVENTORY_PRIME`
/// is enabled (and confirmed safe). Once a real pickup captures the game's own pointer it takes over.
pub fn prime_inventory_if_needed() {
    if !USE_STATIC_INVENTORY_PRIME || LAST_INVENTORY.load(Ordering::Relaxed) >= 0x10000 {
        return;
    }
    if let Some(inv) = static_inventory_ptr() {
        LAST_INVENTORY.store(inv, Ordering::Relaxed);
        log::info!("primed inventory pointer from static @ {inv:#x} (no pickup needed)");
    }
}

/// Grant an item (full_id = real item id | category nibble) by constructing an itembuf and calling
/// the original AddItemFunc with the captured inventory pointer. Returns false if the hook isn't
/// installed or no inventory pointer has been captured yet (no pickup this session) — caller retries.
/// MUST run on the game thread (the FrameBegin tick / update_live).
pub fn grant_full_id(full_id: i32, qty: i32) -> bool {
    if HOOK.get().is_none() {
        return false;
    }
    // Stage 6a: raise granted weapons to the player's current max reinforce tier (inert if off).
    let full_id = crate::upgrades::apply_auto_upgrade(full_id);
    let inv = LAST_INVENTORY.load(Ordering::Relaxed);
    if inv < 0x10000 {
        return false; // no inventory instance captured yet; retry after the player's first pickup
    }
    grant_item(inv as *mut c_void, full_id, qty);
    true
}

fn call_original(inventory: *mut c_void, entry: *mut c_void, itembuf: *mut c_void, r9: u64) -> u64 {
    match HOOK.get() {
        Some(h) => unsafe { h.call(inventory, entry, itembuf, r9) },
        None => 0,
    }
}

unsafe extern "C" fn add_item_detour(
    inventory: *mut c_void,
    entry: *mut c_void,
    itembuf: *mut c_void,
    r9: u64,
) -> u64 {
    LAST_INVENTORY.store(inventory as usize, Ordering::Relaxed);
    // One-time: compare the pointer the game hands us against the static-resolved candidate, so we
    // can safely enable USE_STATIC_INVENTORY_PRIME (a wrong static pointer would crash on grant).
    if !INV_PTR_CHECKED.swap(true, Ordering::Relaxed) {
        let game = inventory as usize;
        match static_inventory_ptr() {
            Some(s) if s == game => {
                log::info!("inventory-ptr CONFIRM: static == game ({game:#x}) — safe to enable USE_STATIC_INVENTORY_PRIME")
            }
            Some(s) => {
                log::warn!("inventory-ptr MISMATCH: static {s:#x} != game {game:#x} — keep static prime OFF (wrong field)")
            }
            None => log::warn!("inventory-ptr: static unresolved at first pickup (game {game:#x})"),
        }
    }
    let raw_id = unsafe { read_i32(entry, ITEMBUF_ENTRY_ID_OFF) } as u32;

    if !is_synthetic_goods(raw_id) {
        return call_original(inventory, entry, itembuf, r9);
    }

    match params::goods_row_fields(row_id_of(raw_id) as i32) {
        Some(fields) => {
            let item = decode_synthetic(&fields);
            log::info!("AP check: synthetic {raw_id:#x} -> location {}", item.ap_location_id);
            PENDING_CHECKS.lock().unwrap().push(item.ap_location_id);
            // own_world:true: report the check + suppress; the server echoes the item back and the
            // received-item path grants it (running progressive / region-open / notify by name).
            0 // suppress the world pickup
        }
        None => {
            log::warn!("synthetic id {raw_id:#x} but goods row unresolved; passing through");
            call_original(inventory, entry, itembuf, r9)
        }
    }
}

/// Port of the standalone `GrantItem`: 0x50-byte descriptor, entry at buf+0x20.
fn grant_item(inventory: *mut c_void, id_with_category: i32, quantity: i32) {
    if id_with_category == 0 || inventory.is_null() {
        return;
    }
    let mut buf = [0u64; 0x50 / 8];
    let base = buf.as_mut_ptr() as *mut u8;
    unsafe {
        (base.add(0x20) as *mut i32).write_unaligned(1);
        (base.add(0x24) as *mut i32).write_unaligned(id_with_category);
        (base.add(0x28) as *mut i32).write_unaligned(quantity);
        (base.add(0x30) as *mut i32).write_unaligned(-1);
        (base.add(0x34) as *mut i32).write_unaligned(-1);
        (base.add(0x40) as *mut i64).write_unaligned(-1);
        (base.add(0x4C) as *mut i32).write_unaligned(-1);
        let entry = base.add(ITEMBUF_ENTRY_OFF) as *mut c_void;
        let itembuf = base as *mut c_void;
        if let Some(h) = HOOK.get() {
            h.call(inventory, entry, itembuf, 0);
        }
    }
}

fn current_module_base() -> Option<usize> {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    let hmodule = unsafe { GetModuleHandleW(None) }.ok()?;
    Some(hmodule.0 as usize)
}
fn signature_matches(addr: usize) -> bool {
    let actual = unsafe { std::slice::from_raw_parts(addr as *const u8, ADD_ITEM_FUNC_SIG.len()) };
    actual == ADD_ITEM_FUNC_SIG
}
unsafe fn read_i32(base: *const c_void, off: usize) -> i32 {
    unsafe { ((base as *const u8).add(off) as *const i32).read_unaligned() }
}
