//! `AddItemFunc` detour — ECHO model (`own_world:true`). Supersedes the stage-2 local-grant detour.
//!
//! Self-found synthetic pickup → report the check + SUPPRESS the world pickup, and let the server
//! ECHO the item back as a received item (the `update_live` received-item path grants it, running
//! progressive / region-open / notify by name). The detour does NOT grant locally — that's what kept
//! self-found progressive/region/notify from working under the old `own_world:false` local-grant.
//! `grant_full_id` stays (used by the received-item path). RVA + signature pinned to 2.6.2.0.

use std::collections::{HashMap, HashSet};
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
/// checkItemFlags from slot_data: full AddItemFunc-space item id -> acquisition flags of the check
/// locations that vanilla-hold it. LIVE vanilla-suppressor since 2026-07-01. The re-pickup
/// discriminator is now the COLLECTED set (KNOWN_COLLECTED_FLAGS), not the live game flag —
/// see that static for why the old live-flag heuristic leaked.
static CHECK_ITEM_FLAGS: Mutex<Option<HashMap<u32, Vec<u32>>>> = Mutex::new(None);

/// Acquisition flags of check locations the client has ALREADY reported as collected (the server
/// checked-set, bridged loc->flag through `locationFlags`; rebuilt each flag-poll tick by
/// `core::update_live`). This REPLACES the old "is the live game flag set at AddItem time?"
/// re-pickup test, which leaked: for ~13% of lots (the probe's "25 true"), and systematically for
/// the 224 shared-flag multi-item lots (605 locations — armor sets, NPC-corpse bundles, boss
/// remembrance drops), the game sets the acquisition flag AT or BEFORE the bag-add, so the live
/// flag already reads set at AddItem and the vanilla item passed through as a bogus "re-pickup"
/// (e.g. Traveler's Clothes 0x100f90c4 / flag 15007980, 2026-07-03 playtest).
///
/// Collected-set logic is race-safe in the correct direction: a location enters this set only on a
/// flag-poll tick STRICTLY AFTER its check was sent, so a first-time pickup (flag not yet in the
/// set) always SUPPRESSES; a genuine re-pickup of a farmable/respawning source (flag collected on a
/// prior, separate event) PASSES. `None` until the first poll → suppress-by-default (never leaks).
static KNOWN_COLLECTED_FLAGS: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

pub fn configure_check_item_flags(map: HashMap<u32, Vec<u32>>) {
    // Armed-or-inert (house rule): one line at configure time says which state the suppressor
    // is in, so a missing/empty checkItemFlags in slot_data is visible instead of silent.
    if map.is_empty() {
        log::info!("vanilla suppressor INERT: checkItemFlags empty/absent in slot_data");
    } else {
        log::info!("vanilla suppressor ARMED for {} check item ids", map.len());
    }
    *CHECK_ITEM_FLAGS.lock().unwrap() = Some(map);
}

/// Replace the collected-flag set. Called by the flag-poll each tick with the acquisition flags of
/// every location currently in the server checked-set (loc->flag via `locationFlags`).
pub fn set_known_collected_flags(flags: HashSet<u32>) {
    *KNOWN_COLLECTED_FLAGS.lock().unwrap() = Some(flags);
}

fn check_item_flags_lookup(raw_id: u32) -> Option<Vec<u32>> {
    CHECK_ITEM_FLAGS.lock().unwrap().as_ref()?.get(&raw_id).cloned()
}

/// AP location ids the detour suppressed, drained by `update_live` -> `mark_checked`.
static PENDING_CHECKS: Mutex<Vec<i64>> = Mutex::new(Vec::new());

/// Resolve the inventory pointer from a game static so server/start grants don't wait for the
/// player's first in-game pickup (the long-standing UX wart). ENABLED: a 2026-06-30 run confirmed the
/// C++ pointer-slot resolver (`static_inventory_ptr_rva`, RVA 0x03D67A50) equals the pointer the game
/// hands the detour, while the typed-field `static_inventory_ptr` MISMATCHED (wrong field). The
/// one-time `inventory-ptr` confirm log in `add_item_detour` keeps verifying both each run.
const USE_STATIC_INVENTORY_PRIME: bool = true;
/// One-time guard for the static-vs-game inventory-pointer confirmation log.
static INV_PTR_CHECKED: AtomicBool = AtomicBool::new(false);

/// Set the first time the GAME itself calls AddItemFunc (a real pickup / the post-load
/// inventory being populated). Distinguishes a genuinely-live inventory from the static prime,
/// so start-item grants can wait until AFTER the save/new-game load replace (which clobbers a
/// grant made during the load screen). See patch_greenfield_start_item_clobber.py.
static REAL_PICKUP_SEEN: AtomicBool = AtomicBool::new(false);

/// True once the game has driven AddItemFunc at least once this session (inventory is live).
pub fn real_pickup_seen() -> bool {
    REAL_PICKUP_SEEN.load(Ordering::Relaxed)
}

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
    let target_addr =
        current_module_base().ok_or("no module base for eldenring.exe")? + ADD_ITEM_FUNC_RVA;
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

/// SECOND inventory-resolver candidate: the pointer stored at the pinned static slot
/// `Inventory_PtrLoc_RVA` — the value the C++ client (`Inventory_PtrLoc_RVA = 0x03D67A50`) read and
/// granted through successfully on 2.6.2.0. This reads a POINTER from a static location, vs
/// `static_inventory_ptr`'s ADDRESS-of-embedded-field. The confirm log reports both so one pickup
/// identifies which (if either) equals the pointer the game hands the detour.
const INVENTORY_PTRLOC_RVA: usize = 0x03D6_7A50;
fn static_inventory_ptr_rva() -> Option<usize> {
    let slot = current_module_base()? + INVENTORY_PTRLOC_RVA;
    // SAFETY: pinned data RVA inside the loaded eldenring.exe image; reads one pointer-sized word.
    // Only called inside the one-time, in-world confirm block (mapped memory). Diagnostic only.
    let inst = unsafe { (slot as *const usize).read_unaligned() };
    (inst >= 0x10000).then_some(inst)
}

/// Tick helper (game thread): if no inventory pointer is captured yet, seed `LAST_INVENTORY` from the
/// static path so grants flush WITHOUT waiting for a pickup. No-op unless `USE_STATIC_INVENTORY_PRIME`
/// is enabled (and confirmed safe). Once a real pickup captures the game's own pointer it takes over.
pub fn prime_inventory_if_needed() {
    if !USE_STATIC_INVENTORY_PRIME || LAST_INVENTORY.load(Ordering::Relaxed) >= 0x10000 {
        return;
    }
    if !crate::flags::in_world() {
        return; // the slot only holds a valid inventory instance once the player is loaded
    }
    // Use the RVA pointer-slot resolver (CONFIRMED 2026-06-30); the typed-field resolver MISMATCHED.
    if let Some(inv) = static_inventory_ptr_rva() {
        LAST_INVENTORY.store(inv, Ordering::Relaxed);
        log::info!("primed inventory pointer from rva-slot @ {inv:#x} (no pickup needed)");
    }
}

/// Cracked Pot FullID (GOODS | goods 9500) — the item the Chapel pot-relief guard watches.
const CRACKED_POT_FULL_ID: i32 = 0x4000_0000 | 9500;
/// Vanilla latch flag of m10_01 event 10010792 ("shop lineup: empty-pot pre-consumption"): the
/// event sets it when it completes (both branches), and `EndIf(EventFlag(10019200))` makes every
/// later run of the event inert once it is on.
const CHAPEL_POT_RELIEF_LATCH: u32 = 10_019_200;
/// Chapel of Anticipation play_region sub-id (m10_01; the fresh-character spawn map).
const CHAPEL_SUB_REGION: i32 = 10010;

/// PHANTOM-CHECK GUARD (flags 66150/66170, found 2026-07-09): vanilla m10_01 event 10010792 is a
/// patch save-migration — on its first run it waits up to 5s and, if the player ALREADY owns a
/// Cracked Pot (goods 9500), assumes a pre-patch save that bought Gostoc's pots and force-sets the
/// relocated pot-instance flags 66150/66170/66180 ("already obtained"). A fresh AP character spawns
/// in the Chapel of Anticipation (m10_01) and the start-item loadout grants 10x Cracked Pot inside
/// that window, so the migration misfires and the flag-poll reports the two Sainted Hero's Grave
/// pot locations (data.py f66150/f66170) as checked at startup, every seed.
///
/// Returns true when granting goods 9500 is SAFE: the latch is set (the armed event already
/// completed / can never re-run), or we are outside m10_01 — in which case we also set the latch
/// ourselves so a later first-load of m10_01 (Four Belfries waygate) can't run the migration
/// against pots we granted. While in the chapel pre-latch, callers get `false` and retry; the
/// event self-latches ≤6s after map start, so the pots are merely deferred a few seconds.
fn chapel_pot_relief_safe() -> bool {
    if crate::flags::get_event_flag(CHAPEL_POT_RELIEF_LATCH) {
        return true;
    }
    let in_chapel = crate::flags::play_region_id()
        .map(|pr| (if pr >= 1_000_000 { pr / 100 } else { pr }) == CHAPEL_SUB_REGION)
        // Unknown region (load screen): hold — conservative, the caller retries next tick.
        .unwrap_or(true);
    if in_chapel {
        return false;
    }
    // Outside the chapel with the latch unset: latch it so any future m10_01 first-load EndIfs
    // the migration instead of reading our granted pots as a pre-patch save.
    let _ = crate::flags::try_set_event_flag(CHAPEL_POT_RELIEF_LATCH, true);
    true
}

/// Pot goods whose HELD count trips a vanilla relief event that force-sets EVERY pot-location flag —
/// a mass phantom-check across the pot flag ranges (66000-66190 / 66400-66490 / 66700-66790).
/// common.emevd counts the bare goods row and fires at an EXACT threshold: event 1460 Goods 9500 == 20
/// -> flag 6902; 1461 Goods 9501 == 10 -> 6903; 1462 Goods 9510 == 10 -> 6904. We cap pot DELIVERIES
/// one below each threshold so the held count can never equal it. Pots are permanent reusable
/// containers (count only rises), and the pool ships ~16 Cracked Pot locations plus 10 in the start
/// loadout, so 20 is otherwise very reachable. Nobody needs 19+ pots, so the cap is invisible in play.
const POT_DELIVERY_CAPS: &[(i32, i32)] = &[
    (0x4000_0000 | 9500, 19),    // Cracked Pot        (event 1460, threshold 20)
    (0x4000_0000 | 9501, 9),     // Ritual Pot         (event 1461, threshold 10)
    (0x4000_0000 | 9510, 9),     // Perfume Bottle     (event 1462, threshold 10)
    (0x4000_0000 | 2_009_500, 9), // Hefty Cracked Pot (DLC; threshold 10, flags 669xx)
];

/// Total held quantity of a bare GOODS row (sums stacks). None if the inventory isn't reachable this
/// tick. Same read-only walk as `upgrades::held_scadu_fragments` / `inventory`.
fn count_held_goods_row(row: i32) -> Option<i32> {
    use eldenring::cs::{GameDataMan, ItemCategory};
    use fromsoftware_shared::FromStatic;
    let gdm = unsafe { GameDataMan::instance() }.ok()?;
    let pgd = gdm.main_player_game_data.as_ref();
    let mut total: i64 = 0;
    for entry in pgd.equipment.equip_inventory_data.items_data.items() {
        if entry.item_id.category() == ItemCategory::Goods && entry.item_id.param_id() as i32 == row {
            total += entry.quantity as i64;
        }
    }
    Some(total.min(i32::MAX as i64) as i32)
}

/// Clamp a pot grant so the resulting held count stays strictly below the mass-phantom-check
/// threshold. Returns the qty to actually grant (0 = at/over the cap, skip). Non-pot full_ids and an
/// unreadable inventory pass through unchanged (a transient read miss must not drop an item; the cap
/// re-checks on the next pot grant).
fn pot_capped_qty(full_id: i32, qty: i32) -> i32 {
    let Some(&(_, cap)) = POT_DELIVERY_CAPS.iter().find(|&&(id, _)| id == full_id) else {
        return qty;
    };
    match count_held_goods_row(full_id & 0x0FFF_FFFF) {
        Some(held) => qty.min((cap - held).max(0)),
        None => qty,
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
    // Chapel pot-relief guard: defer Cracked Pot grants that would trip m10_01's migration event
    // (phantom checks f66150/f66170). Every caller treats `false` as retry-next-tick, so the
    // stack simply lands a few seconds later, after the event's own latch (10019200) sets.
    if full_id == CRACKED_POT_FULL_ID && !chapel_pot_relief_safe() {
        return false;
    }
    // Pot-delivery cap: never let a pot grant push the held count to a mass-phantom-check threshold.
    // At/over the cap we report success (the AP item is delivered as far as the watermark cares) but
    // add no physical pot, so the count can't reach 20/10/10 and fire relief events 6902/6903/6904.
    let qty = pot_capped_qty(full_id, qty);
    if qty <= 0 {
        return true;
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
    REAL_PICKUP_SEEN.store(true, Ordering::Relaxed);
    // One-time: compare the pointer the game hands us against the static-resolved candidate, so we
    // can safely enable USE_STATIC_INVENTORY_PRIME (a wrong static pointer would crash on grant).
    if !INV_PTR_CHECKED.swap(true, Ordering::Relaxed) {
        let game = inventory as usize;
        match static_inventory_ptr() {
            Some(s) if s == game => {
                log::info!(
                    "inventory-ptr CONFIRM: static == game ({game:#x}) — safe to enable USE_STATIC_INVENTORY_PRIME"
                )
            }
            Some(s) => {
                log::warn!(
                    "inventory-ptr MISMATCH: static {s:#x} != game {game:#x} — keep static prime OFF (wrong field)"
                )
            }
            None => log::warn!("inventory-ptr: static unresolved at first pickup (game {game:#x})"),
        }
        // Second candidate: the C++-client RVA pointer-SLOT (proven on 2.6.2.0). One pickup thus
        // identifies which resolver — the typed embedded-field above, or this pointer-slot — equals
        // the game's pointer. Point `static_inventory_ptr` at whichever CONFIRMs, then enable the prime.
        match static_inventory_ptr_rva() {
            Some(s) if s == game => log::info!(
                "inventory-ptr CONFIRM (rva-slot): *(base+{INVENTORY_PTRLOC_RVA:#x}) == game ({game:#x}) — use the pointer-slot resolver"
            ),
            Some(s) => log::warn!("inventory-ptr rva-slot {s:#x} != game {game:#x}"),
            None => log::warn!("inventory-ptr rva-slot unresolved at first pickup"),
        }
    }
    let raw_id = unsafe { read_i32(entry, ITEMBUF_ENTRY_ID_OFF) } as u32;

    // Shop native-sell (SHOP-SYSTEM-HANDOFF.md §5): a rewritten own-world slot sells the REAL reward
    // (a non-synthetic id). Suppress its bag-add while the stock flag is unset so the single copy is
    // delivered by the AP grant, not the purchase. Checked BEFORE the synthetic/vanilla decision.
    if crate::shop_sell::should_suppress_sold(raw_id as i32, &|f| crate::flags::get_event_flag(f)) {
        return 0;
    }

    if !is_synthetic_goods(raw_id) {
        // Vanilla-suppress (LIVE 2026-07-01): a vanilla id that belongs to a check location is the
        // check's ORIGINAL ware — suppress its bag-add so the AP grant delivers what the seed placed
        // there. The re-pickup discriminator is the COLLECTED set, not the live game flag: any mapped
        // flag NOT yet in KNOWN_COLLECTED_FLAGS means the check has not been reported yet, so this IS
        // the check pickup → suppress. Only pass (farmable/respawning re-pickup) once EVERY mapped
        // flag is collected. This fixes the shared-flag / early-flag-set leak where the game set the
        // acquisition flag at/before AddItem and the old live-flag test mis-read it as a re-pickup.
        if let Some(flags) = check_item_flags_lookup(raw_id) {
            let guard = KNOWN_COLLECTED_FLAGS.lock().unwrap();
            // No poll yet (None) -> treat as "nothing collected" -> suppress by default (never leaks).
            let suppress = match guard.as_ref() {
                Some(collected) => er_logic::vanilla_suppress::should_suppress(&flags, collected),
                None => true,
            };
            if suppress {
                log::info!("vanilla-suppress: pickup {raw_id:#x} suppressed (check not yet collected)");
                return 0;
            }
            log::info!("vanilla-suppress: pickup {raw_id:#x} passed (check already collected — re-pickup)");
        }
        return call_original(inventory, entry, itembuf, r9);
    }

    match params::goods_row_fields(row_id_of(raw_id) as i32) {
        Some(fields) => {
            let item = decode_synthetic(&fields);
            log::info!(
                "AP check: synthetic {raw_id:#x} -> location {}",
                item.ap_location_id
            );
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
