//! auto_equip -- when `options.auto_equip` is on, equip a received WEAPON or PROTECTOR immediately.
//!
//! ## How equipping actually works on 2.6.2.0
//!
//! ER has no `equip(equipData, slot, item)` primitive. An equipped piece is FOUR coupled
//! representations, all of which must agree. Not three -- diffing the whole `EquipGameData`
//! header across a real menu equip on 2.6.2.0 showed the game moving exactly four dwords:
//!
//!   1. `chr_asm.gaitem_handles[slot]`      -- the refcounted handle of the inventory instance
//!   2. `chr_asm.equipment_param_ids[slot]` -- the param row (`item_id & 0x0FFFFFFF`)
//!   3. `equipment_entries[slot]`           -- the FullID, category nibble included
//!   4. `EquipGameData + 0x08 + slot * 4`   -- the INVENTORY INDEX of the equipped entry
//!
//! (4) is what the equipment MENU reads. Write only 1-3 and the weapon appears in the player's
//! hand and behaves correctly, but every menu slot still renders as empty -- confirmed in-game
//! before this rep was found. It is `unk8: [u32; 22]` in `fromsoftware-rs`: unnamed, exactly as
//! wide as the 22 `ChrAsmSlot`s, sitting immediately before `chr_asm`. Its value is
//! `key_items_capacity + <index into the normal-items list>`; verified against every occupied
//! slot of a live character, armour included.
//!
//! The handle is NOT derived from an item id by any function -- it is read straight off the
//! inventory entry (`EquipInventoryDataListEntry.gaitem_handle`), which is the same list this
//! module already walks. Handles are refcounted, so (1) goes through the game's own copy-assign,
//! `ChrAsm::operator=` at [`CHR_ASM_COMMIT_RVA`], which acquires the new handle and releases the
//! old one across all 22 slots. Writing the handle field directly would leak a reference.
//!
//! (3) is a plain `ItemId` array with no refcounting, so it is written directly.
//!
//! There is no model-refresh call to make: the renderer polls `EquipGameData.chr_asm`. Verified
//! in-game on 2.6.2.0 -- writing the three reps put a weapon in the player's hand immediately,
//! with the menu closed and no load boundary, including for a weapon whose gaitem handle had
//! never been in `chr_asm` before. The menu renders from the same array
//! (`EquipItemData::equip_entries` reads back as exactly `&equipment_entries`), so there is no
//! second representation to maintain.
//!
//! ## Scope
//!
//! Equips unconditionally, including mid-boss-fight, and clobbers whatever occupies the slot.
//! See `er_logic::auto_equip` for why that is the intended behaviour and not an oversight.
//! `arm_style` is deliberately not written: the live probe equipped correctly without touching it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{
    ChrAsm, ChrAsmEquipEntries, EquipGameData, EquipParamProtector, EquipParamWeapon, GaitemHandle,
    GameDataMan, SoloParamRepository,
};
use er_logic::auto_equip::Equipable;
use fromsoftware_shared::FromStatic;

/// `ChrAsm::operator=` on 2.6.2.0 WW -- the refcounted commit. `(rcx = dest_chr_asm,
/// rdx = src_chr_asm)`. Copies both weapon ids, the xmm block, then loops 22 times calling the
/// handle-assign helper at `+0x682580` (acquire `+0x671A80` / release `+0x671B00`) for
/// `gaitem_handles`, then copies `equipment_param_ids[22]`.
const CHR_ASM_COMMIT_RVA: usize = 0x245C00;

/// First 16 bytes at [`CHR_ASM_COMMIT_RVA`], read from the pinned exe. A mismatch means the RVA is
/// stale for the running build -> refuse to call rather than jump into the middle of something else.
const CHR_ASM_COMMIT_SIG: &[u8] = &[
    0x48, 0x89, 0x5C, 0x24, 0x08, // mov [rsp+8], rbx
    0x48, 0x89, 0x6C, 0x24, 0x10, // mov [rsp+0x10], rbp
    0x48, 0x89, 0x74, 0x24, 0x18, // mov [rsp+0x18], rsi
    0x57, // push rdi
];

/// `ChrAsm::operator=(dest, src)`. Returns dest; we ignore it.
type ChrAsmCommit = unsafe extern "C" fn(*mut ChrAsm, *const ChrAsm) -> *mut ChrAsm;

/// Byte offset of the per-slot inventory-index array inside `EquipGameData` -- rep (4). The crate
/// keeps it private (`unk8`), so it has to be reached by raw offset. `chr_asm` IS public and sits
/// directly after it, so pinning `chr_asm`'s offset pins this one: if the crate ever reshapes the
/// struct the assert below fails the build instead of letting us write into the wrong field.
const EQUIP_INDEX_OFF: usize = 0x08;
const _: () = assert!(std::mem::offset_of!(EquipGameData, chr_asm) == 0x6C);

/// `equipment_entries` is indexed by raw `ChrAsmSlot`, so its field order must match the enum and
/// its size must be exactly 22 + 10 quick + 6 pouch = 38 `u32`s. If the crate ever reshapes it,
/// this fails the build instead of silently writing the wrong field.
const _: () = assert!(size_of::<ChrAsmEquipEntries>() == 38 * 4);

static ENABLED: AtomicBool = AtomicBool::new(false);
static PENDING: Mutex<Vec<i32>> = Mutex::new(Vec::new());

/// Set from slot_data `options.auto_equip` at connect.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if on {
        log::info!("auto_equip: enabled (received weapons and armour equip on arrival)");
    }
}

/// Queue a received FullID for equipping. Called from the received-item loop. Self-gating: no-op
/// if the option is off, or if the category is not something we equip. The caller deliberately
/// does NOT pre-filter -- it used to gate on `is_weapon`, which silently excluded armour.
///
/// 🛑 #296 / #302 / #303 -- QUEUE THE ID THAT ACTUALLY ENTERS THE BAG.
///
/// `detour::grant_full_id_outcome` runs `upgrades::apply_auto_upgrade` on every grant, so with
/// `auto_upgrade` ON an upgradeable weapon lands in the inventory as `base + N` while the receive
/// loop handed us `base + 0`. `tick()` below looks the queued id up in `owned` by EXACT FullID, so
/// that lookup missed, the id went back on `still_pending`, and it retried forever. Protectors are
/// unaffected -- `apply_auto_upgrade` is identity for them -- which is precisely the asymmetry
/// boblerrr reported: "armor still auto-equips fine -- it's specifically weapons that fail."
/// His 2026-08-03 log has 8 successful equips: 7 protectors, and one weapon (param 52080000,
/// Lordsworn's Bolt) which is AMMUNITION and therefore has no reinforce run for auto_upgrade to
/// raise. Zero upgradeable weapons ever equipped.
///
/// The upgrade is applied HERE rather than at the call site so there is ONE enqueue path and a
/// future caller cannot reintroduce the mismatch. `apply_auto_upgrade` is raise-only and therefore
/// idempotent, so the second application inside the grant is a no-op, and both calls share the
/// 1500ms `UPGRADE_TARGETS` cache -- within a tick they cannot disagree.
///
/// ⚠️ NOT total: if the grant is deferred (`GrantOutcome::NotReady`) across a target refresh, the
/// bagged id can still differ from the queued one. That window is bounded by the retry, and closing
/// it properly means matching on the base row in `tick()` -- deliberately left out of this fix so
/// the change stays one line of behaviour.
pub fn enqueue(full_id: i32) {
    if !ENABLED.load(Ordering::Relaxed) || er_logic::auto_equip::equipable(full_id).is_none() {
        return;
    }
    let full_id = crate::upgrades::apply_auto_upgrade(full_id);
    if let Ok(mut q) = PENDING.lock()
        && !q.contains(&full_id)
    {
        q.push(full_id);
    }
}

fn current_module_base() -> Option<usize> {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    let hmodule = unsafe { GetModuleHandleW(None) }.ok()?;
    Some(hmodule.0 as usize)
}

/// Resolve the commit entry, verifying the prologue. `None` (logged once) if the pin is stale.
fn commit_fn(base: usize) -> Option<ChrAsmCommit> {
    static WARNED: AtomicBool = AtomicBool::new(false);
    let addr = base + CHR_ASM_COMMIT_RVA;
    // SAFETY: reading bytes inside the mapped image at a pinned RVA.
    let actual = unsafe { std::slice::from_raw_parts(addr as *const u8, CHR_ASM_COMMIT_SIG.len()) };
    if actual != CHR_ASM_COMMIT_SIG {
        if !WARNED.swap(true, Ordering::Relaxed) {
            log::warn!(
                "auto_equip: ChrAsm commit signature mismatch @ {addr:#x} -- pinned 2.6.2.0 RVA \
                 stale for this build; auto_equip inert"
            );
        }
        return None;
    }
    // SAFETY: verified prologue at the pinned RVA.
    Some(unsafe { std::mem::transmute::<usize, ChrAsmCommit>(addr) })
}

/// Per-tick until the pending queue drains. An item not yet in the bag stays queued for a later
/// tick -- the grant and the receive are not ordered with respect to each other.
pub fn tick() {
    if !ENABLED.load(Ordering::Relaxed) || !crate::flags::in_world() {
        return;
    }
    let pending: Vec<i32> = match PENDING.lock() {
        Ok(q) if !q.is_empty() => q.clone(),
        _ => return,
    };

    let Some(base) = current_module_base() else {
        return;
    };
    let Some(commit) = commit_fn(base) else {
        return; // stale pin -- keep the queue, equip nothing
    };
    let (Ok(gdm), Ok(repo)) = (unsafe { GameDataMan::instance_mut() }, unsafe {
        SoloParamRepository::instance()
    }) else {
        return;
    };

    // SAFETY: FD4 singletons, read/written on the single-threaded FrameBegin tick (same contract as
    // inventory.rs / no_equip_load.rs).
    let pgd = &mut *gdm.main_player_game_data;

    // Snapshot id -> (handle, inventory index) first, so the inventory borrow is released before we
    // mutate chr_asm. Both values come off the entry this loop already visits; the index is the
    // entry's position in the normal-items list, biased by `key_items_capacity` the way the game
    // biases it (indices below the capacity address the key-item list instead).
    let inventory = &pgd.equipment.equip_inventory_data.items_data;
    let key_items_capacity = inventory.key_items_capacity;
    let owned: HashMap<u32, (GaitemHandle, u32)> = inventory
        .normal_entries()
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| {
            let e = slot.as_option()?;
            Some((
                e.item_id.into_inner(),
                (e.gaitem_handle, key_items_capacity + i as u32),
            ))
        })
        .collect();

    let mut still_pending: Vec<i32> = Vec::new();
    for fid in pending {
        let full = fid as u32;
        let Some(&(handle, inv_index)) = owned.get(&full) else {
            still_pending.push(fid); // not granted yet -- retry next tick
            continue;
        };
        let param_id = full & 0x0FFF_FFFF;

        let slot = match er_logic::auto_equip::equipable(fid) {
            Some(Equipable::Weapon) => {
                // Weapon rows are upgradeable, so round to the base row the way the game does.
                let wep_type = repo
                    .get::<EquipParamWeapon>((param_id / 100) * 100)
                    .map(|w| w.wep_type());
                match wep_type {
                    Some(t) => {
                        // None = it does not belong in a hand at all (AMMUNITION). Skipping is
                        // deliberate and is better than the old fall-through: boblerrr's
                        // 2026-08-03 log has param 52080000 (wep_type 85, Lordsworn's Bolt)
                        // landing in SLOT_WEAPON_RIGHT_1, i.e. the player's main hand replaced by
                        // a crossbow bolt (#294). The ammo still arrives in the bag; it is only
                        // not auto-equipped.
                        let Some(slot) = er_logic::auto_equip::slot_for_wep_type(t) else {
                            log::info!(
                                "auto_equip: weapon {full:#010x} wepType={t} is ammunition -- \
                                 delivered to the bag, not equipped (no verified quiver slot)"
                            );
                            continue;
                        };
                        slot
                    }
                    // A row the param table does not know: default to the main hand rather than
                    // dropping the item on the floor.
                    None => er_logic::auto_equip::SLOT_WEAPON_RIGHT_1,
                }
            }
            Some(Equipable::Protector) => {
                // Protectors are not upgradeable -- no rounding.
                let Some(cat) = repo
                    .get::<EquipParamProtector>(param_id)
                    .map(|p| p.protector_category())
                else {
                    log::debug!("auto_equip: protector {full:#010x} has no param row -- skipped");
                    continue;
                };
                let Some(slot) = er_logic::auto_equip::slot_for_protector_category(cat) else {
                    log::debug!(
                        "auto_equip: protector {full:#010x} protectorCategory={cat} is not an \
                         equippable slot -- skipped"
                    );
                    continue;
                };
                slot
            }
            None => continue, // queue() gates this, belt and braces
        };
        let idx = slot as usize;

        let equipment = &mut pgd.equipment;
        // Already in that slot -- nothing to do, and re-committing would churn refcounts.
        if equipment.chr_asm.equipment_param_ids[idx] == param_id as i32 {
            continue;
        }

        // (3) the FullID array: plain ItemIds, no refcounting, indexed by ChrAsmSlot.
        // SAFETY: `idx` is one of the six slot constants, all < 22 < 38; size asserted above.
        unsafe {
            (&raw mut equipment.equipment_entries)
                .cast::<u32>()
                .add(idx)
                .write(full);
        }

        // (4) the inventory index the equipment MENU reads. Plain u32, no refcounting.
        // SAFETY: `idx` < 22, and the array is 22 wide at `EQUIP_INDEX_OFF`, pinned by the
        // `offset_of!(EquipGameData, chr_asm)` assert above.
        unsafe {
            (equipment as *mut EquipGameData)
                .cast::<u8>()
                .add(EQUIP_INDEX_OFF)
                .cast::<u32>()
                .add(idx)
                .write(inv_index);
        }

        // (1) + (2) via the game's copy-assign, so the new handle is acquired and the outgoing one
        // released. Build a bitwise copy of the live ChrAsm, edit the one slot, hand it back as the
        // source. ChrAsm is plain data (ids, handles, a bitfield block) with no Drop.
        // SAFETY: `live` is a valid initialised ChrAsm; `src` is a byte-identical copy of it.
        unsafe {
            let live = &raw mut equipment.chr_asm;
            let mut src = std::ptr::read(live);
            src.gaitem_handles[idx] = handle;
            src.equipment_param_ids[idx] = param_id as i32;
            commit(live, &raw const src);
        }

        log::info!(
            "auto_equip: slot {slot} <- {full:#010x} (param {param_id}, handle {handle:?}, \
             inv_index {inv_index})"
        );
    }

    if let Ok(mut q) = PENDING.lock() {
        *q = still_pending;
    }
}
