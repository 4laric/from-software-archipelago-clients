//! auto_equip -- when `options.auto_equip` is on, equip a received WEAPON, PROTECTOR or TALISMAN
//! immediately.
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
//!
//! TALISMANS (#295) ride the SAME four reps -- they are `ChrAsmSlot` 17..=20 like any other
//! equipment, so rep (1)'s refcount is acquired by the same `ChrAsm::operator=` and rep (4) is the
//! same inventory index. Nothing new was reverse-engineered for them. What IS new is that the
//! target slot is not a function of the item alone: it depends on the talisman's POSITION IN THE
//! AP RECEIVED STREAM and on what is already worn.
//!
//! 🛑 #342 -- THE SLOT COUNT IS NOT READ OFF THE LIVE CHARACTER ANY MORE. It used to be
//! `pgd.unlocked_talisman_slots`, and that made the slot decision a function of live state, so the
//! reconciler's replay of the received set evaluated the same talisman against a different count
//! and silently rearranged the player's loadout. The count now comes from
//! `er_logic::auto_equip::TalismanStream`, which counts Talisman Pouches in the received stream --
//! legitimate for this one item because the pouch is itself an AP item (all three copies are
//! randomized checks), so the stream is upstream of the game's field rather than a second tally of
//! it. The live field is still READ and LOGGED on every accessory equip so a disagreement between
//! the two shows up in a log instead of being argued about.
//!
//! PHYSICK TEARS (#334) ride NONE of the four reps above, and that is the point. Tears are
//! `EQUIP_PARAM_GOODS` and never enter `chr_asm` -- the Flask of Wondrous Physick is a separate
//! two-slot mixture, so the tear branch in `tick()` returns before any of the machinery above.
//!
//! What the mixture is, MEASURED by `physick_probe` on 2.6.2.0 (2026-08-03, two runs):
//!
//!   * `[OptionalItemId; 2]` at [`PHYSICK_MIXTURE_OFF`], the 16 bytes straight after
//!     `equipment_entries`. The crate types the region as `unk3e0: usize, unk3e8: usize`.
//!   * PLAIN, NOT REFCOUNTED. Both slots hold bare GOODS FullIDs (`0x40002AFA`), the same
//!     representation as `equipment_entries`, which the crate documents as unrefcounted and
//!     directly written. So there is no `ChrAsm::operator=` equivalent to go through and no
//!     reference to leak -- a plain dword write is the whole operation.
//!   * An UNOCCUPIED slot is `0xFFFFFFFF`, which is exactly `OptionalItemId::NONE`. That is the
//!     structural confirmation that these are ids and not two dwords that happen to hold ids.
//!   * ONE rep, not four. A whole-`PlayerGameData` dword diff across three mix/unmix events found
//!     ZERO other words moving, with zero suppressed by the noise filter -- including across
//!     `EquipGameData.unk60/unk68`, the only plausible home for a menu-index rep by analogy with
//!     `unk8`.
//!
//! 🛑 What that measurement CANNOT rule out, and what the in-game acceptance test is for: a rep
//! living OUTSIDE `PlayerGameData`. That is the [flask potency] shape, where ER mirrored flask
//! state into the global GaItem table and a half-updated state CTD'd on death. The question
//! collapses to "does a WRITTEN mixture actually fire when you drink?" -- which is a flask, not a
//! probe.
//!
//! [flask potency]: crate::flask

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use eldenring::cs::{
    ChrAsm, ChrAsmEquipEntries, EquipGameData, EquipParamAccessory, EquipParamGoods,
    EquipParamProtector, EquipParamWeapon, GaitemHandle, GameDataMan, SoloParamRepository,
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

/// Byte offset of the Flask of Wondrous Physick's two-tear mixture inside `EquipGameData`.
///
/// MEASURED, not guessed (see the module docs). The crate keeps the region private and mistyped
/// (`unk3e0: usize, unk3e8: usize`), so it has to be reached by raw offset. The assert below pins
/// it to the END of `equipment_entries` -- which IS public -- so a crate reshape fails the build
/// instead of letting us write into whatever moved there. Same guard shape as `EQUIP_INDEX_OFF`.
const PHYSICK_MIXTURE_OFF: usize = 0x3E4;
const _: () = assert!(
    std::mem::offset_of!(EquipGameData, equipment_entries) + size_of::<ChrAsmEquipEntries>() + 4
        == PHYSICK_MIXTURE_OFF
);
const _: () = assert!(
    PHYSICK_MIXTURE_OFF + 4 * er_logic::physick::MIXTURE_SLOTS <= size_of::<EquipGameData>()
);

/// The two mixture slots as the game holds them: [`er_logic::physick::EMPTY_SLOT`] for empty,
/// otherwise a GOODS FullID.
///
/// SAFETY: `equipment` is a live `EquipGameData` and the pair sits wholly inside it (asserted
/// above), so both reads stay in the object.
fn physick_mixture(equipment: &EquipGameData) -> [u32; er_logic::physick::MIXTURE_SLOTS] {
    let base = equipment as *const EquipGameData as usize + PHYSICK_MIXTURE_OFF;
    std::array::from_fn(|i| unsafe { ((base + i * 4) as *const u32).read_unaligned() })
}

/// Write one mixture slot. A plain dword store IS the whole operation: the slots are unrefcounted
/// (module docs), so unlike `chr_asm.gaitem_handles` there is no handle to acquire or release.
///
/// SAFETY: as [`physick_mixture`]; `slot` is bounded by the caller
/// ([`er_logic::physick::slot_for_tear`] only ever returns a valid index).
fn write_physick_slot(equipment: &mut EquipGameData, slot: usize, value: u32) {
    debug_assert!(slot < er_logic::physick::MIXTURE_SLOTS);
    let base = equipment as *mut EquipGameData as usize + PHYSICK_MIXTURE_OFF;
    unsafe { ((base + slot * 4) as *mut u32).write_unaligned(value) };
}

/// Is this received FullID a physick tear? Reads the game's OWN `EquipParamGoods` row, so no
/// hardcoded id list can go stale -- the same instrument `wep_type` and `protectorCategory` use.
fn is_physick_tear(full_id: i32) -> bool {
    let Some(row) = er_logic::physick::goods_row(full_id) else {
        return false;
    };
    // SAFETY: FD4 singleton, read on the game thread like every other param read in this module.
    let Ok(repo) = (unsafe { SoloParamRepository::instance() }) else {
        return false;
    };
    repo.get::<EquipParamGoods>(row)
        .is_some_and(|g| er_logic::physick::is_tear(g.goods_type(), g.sort_id()))
}

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Hold WEAPON equips only, without touching `ENABLED`.
///
/// 🛑🛑 THIS EXISTS BECAUSE `set_enabled` IS NOT A PAUSE LEVER. `ENABLED` gates `enqueue` as well
/// as `tick`, so flipping it off would drop every item received during the fight out of the queue
/// permanently -- AP replays only on connect, so they would never be equipped at all. It also
/// resets `TEAR_SEQ`, and physick tear ordinals have to stay reproducible (#342). This flag is
/// read in ONE place: the drain loop, per entry.
static WEAPONS_PAUSED: AtomicBool = AtomicBool::new(false);

/// Hold or release WEAPON equips. Driven per tick from `er_logic::boss_grants`.
pub fn set_weapons_paused(on: bool) {
    WEAPONS_PAUSED.store(on, Ordering::Relaxed);
}

/// Current hold state, fed back into the decision so a closed gate stays closed.
pub fn weapons_paused() -> bool {
    WEAPONS_PAUSED.load(Ordering::Relaxed)
}

/// Is the equip queue drained? A poisoned lock answers `false` -- "not empty" defers the pause,
/// which is the safe direction (the alternative pauses before the granted weapon has equipped).
pub fn queue_is_empty() -> bool {
    PENDING.lock().map(|q| q.is_empty()).unwrap_or(false)
}
/// Queued FullIDs, each with the stream position the resolver needs: `(full_id, ordinal, pouches)`.
///
/// * TEARS use `ordinal` (their position among tears since connect, from [`TEAR_SEQ`]) and ignore
///   `pouches`.
/// * TALISMANS use both, and take them from `er_logic::auto_equip::TalismanPos` -- the ordinal
///   among talismans in the WHOLE received stream, and the Talisman Pouches that preceded them.
/// * Weapons and protectors read neither; they get `(0, 0)`.
///
/// The position rides the queue rather than being recomputed in `tick()` because `tick()` runs
/// whenever the item happens to reach the bag, which is not the order it was received in.
static PENDING: Mutex<Vec<(i32, u64, u8)>> = Mutex::new(Vec::new());

/// How many physick tears have been enqueued since CONNECT. Reset in [`set_enabled`], which is the
/// whole trick: AP replays the entire received set on every connect, so re-counting the same stream
/// from zero reproduces the same ordinals -- and therefore the same mixture. See
/// `er_logic::physick::slot_for_tear` for why a "which slot did I clobber last" flag does not.
static TEAR_SEQ: AtomicU64 = AtomicU64::new(0);

/// Set from slot_data `options.auto_equip` at connect.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    // Called from the slot_data handler, i.e. once per CONNECT and before the received set replays.
    // Resetting both here is what makes the tear ordinals reproducible: the queue cannot carry an
    // entry from the previous connection into the new numbering, and the counter starts where the
    // replay starts. Nothing is lost by clearing -- AP replays everything we are about to drop.
    TEAR_SEQ.store(0, Ordering::Relaxed);
    if let Ok(mut q) = PENDING.lock()
        && !q.is_empty()
    {
        log::info!(
            "auto_equip: dropping {} queued item(s) at connect -- the received set replays them",
            q.len()
        );
        q.clear();
    }
    if on {
        log::info!(
            "auto_equip: enabled (received weapons, armour, talismans and physick tears are \
             equipped/mixed on arrival; spells are NOT covered)"
        );
    }
}

/// Queue a received FullID for equipping. Called from the received-item loop. Self-gating: no-op
/// if the option is off, or if the category is not something we equip. The caller deliberately
/// does NOT pre-filter -- it used to gate on `is_weapon`, which silently excluded armour, and
/// widening `equipable` to talismans (#295) was the whole of that fix precisely because this is
/// the only gate.
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
/// future caller cannot reintroduce the mismatch -- and it is applied through the host-tested
/// er-logic seam (`crate::upgrades::enqueue_upgrade_id` -> `er_logic::auto_equip::enqueue_id`)
/// rather than inline, because the inline call was the one line of the #296/#302/#303 fix no test
/// could reach (2026-08-04 inert-test audit, F1); `upgrades_replay::auto_equip_queue_matches_bag`
/// now pins the seam against the grant path. The predicate is raise-only and therefore
/// idempotent, so the second application inside the grant is a no-op, and both calls share the
/// 1500ms `UPGRADE_TARGETS` cache -- within a tick they cannot disagree.
///
/// ⚠️ NOT total: if the grant is deferred (`GrantOutcome::NotReady`) across a target refresh, the
/// bagged id can still differ from the queued one. That window is bounded by the retry, and closing
/// it properly means matching on the base row in `tick()` -- deliberately left out of this fix so
/// the change stays one line of behaviour.
///
/// 🛑 THIS RAISE IS FOR RECEIVES ONLY. A caller queueing something ALREADY IN THE BAG must use
/// [`enqueue_held`] instead: there is no grant coming to deposit `base + target`, so raising the
/// id names a row that will never exist and the drain misses it forever. That is #413 (boblerrr
/// 2026-08-07 18:31:38), where #101's fight-equip came through here and the spear never landed.
pub fn enqueue(full_id: i32, talisman: Option<er_logic::auto_equip::TalismanPos>) {
    enqueue_inner(full_id, talisman, Raise::ToAutoUpgradeTarget);
}

/// Should `enqueue_inner` raise the id to the player's auto_upgrade target before queueing it?
#[derive(Clone, Copy, PartialEq, Eq)]
enum Raise {
    /// A RECEIVE. The grant is about to deposit `base + target`, so the queue must say the same.
    ToAutoUpgradeTarget,
    /// ALREADY IN THE BAG. The caller read the real FullID off the inventory; there is no grant
    /// coming, so raising it would name a row that will never exist.
    Never,
}

/// Queue a weapon the player ALREADY OWNS, at the exact FullID the bag reports.
///
/// #413 / boblerrr 2026-08-07 18:31:38. #101's fight-equip called [`enqueue`], which raised
/// `17030000` to `17030003` (`auto_upgrade: 0x103db70 -> 0x103db73 (enqueue)` in his log) for a
/// spear that had been banked at `+0` hours earlier. `tick()` matches by EXACT FullID, so the
/// entry missed, went back on `still_pending`, and retried silently for the rest of the session --
/// the banner printed and no `auto_equip: slot ... <-` line ever followed.
///
/// The raise is right for a receive and wrong here, and the difference is the PREMISE: a receive
/// is queued against an item a grant is about to put in the bag, this is queued against an item
/// that is already there. See `er_logic::auto_equip::held_row_to_equip`, which is what the caller
/// uses to find the id to pass.
///
/// COLLATERAL THIS ALSO CLOSES: `should_pause_weapon_equips` only ARMS on an empty queue, so the
/// permanently-stuck entry left `PENDING` non-empty forever and the #98 mid-fight weapon hold
/// could never arm again for the rest of the session. Unwitnessed in bobler's log (it ends 77s
/// after the banner, with nothing arriving to be held) -- it follows from the code, and it stops
/// following once the entry can drain.
pub fn enqueue_held(full_id: i32) {
    enqueue_inner(full_id, None, Raise::Never);
}

fn enqueue_inner(full_id: i32, talisman: Option<er_logic::auto_equip::TalismanPos>, raise: Raise) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    // Tears are classified HERE and not in `er_logic::auto_equip::equipable`, which is pure and
    // cannot read a param row (its doc comment says so, and that stays true).
    //
    // 🛑 The classification has to happen at ENQUEUE, not in `tick()`. Widening the queue to every
    // GOODS id would park each non-tear good in `still_pending` forever: a Golden Rune the player
    // drinks on pickup never appears in the bag, so the "not granted yet -- retry next tick" arm
    // would retry it every tick for the rest of the session. That is the #308 shape -- a bag walk
    // cannot tell "never arrived" from "already consumed".
    let tear = is_physick_tear(full_id);
    if er_logic::auto_equip::equipable(full_id).is_none() && !tear {
        return;
    }
    // The ordinal is consumed per RECEIVE, before the dedup below, so it is the tear's position in
    // the stream and not its position among the pushes. A duplicate that gets deduped here is
    // deduped identically on replay, so the numbering stays reproducible either way.
    let (ordinal, pouches) = if tear {
        (TEAR_SEQ.fetch_add(1, Ordering::Relaxed), 0)
    } else {
        // #342: a TALISMAN's position comes from the caller's `TalismanStream`, which walks the
        // whole received stream. It cannot be counted here: `enqueue` only sees the tail past the
        // persisted `received_through` watermark, so a session-local counter would report zero
        // pouches to a player who found three last session.
        match talisman {
            Some(pos) => (pos.ordinal, pos.pouches),
            None => {
                if matches!(
                    er_logic::auto_equip::equipable(full_id),
                    Some(er_logic::auto_equip::Equipable::Accessory)
                ) {
                    // Not reachable from the receive loop (it resolves the FullID through the same
                    // item map the stream walk uses), so shout rather than guess quietly: the
                    // fallback is one unlocked slot, i.e. every talisman on slot 1.
                    log::warn!(
                        "auto_equip: talisman {full_id:#010x} enqueued with no stream position -- \
                         falling back to slot 1 (#342)"
                    );
                }
                (0, 0)
            }
        }
    };
    let full_id = match raise {
        Raise::ToAutoUpgradeTarget => crate::upgrades::enqueue_upgrade_id(full_id),
        Raise::Never => full_id,
    };
    if let Ok(mut q) = PENDING.lock()
        && !q.iter().any(|&(id, _, _)| id == full_id)
    {
        q.push((full_id, ordinal, pouches));
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
    let pending: Vec<(i32, u64, u8)> = match PENDING.lock() {
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

    // Read ONCE per tick, before anything borrows `pgd.equipment` mutably. DIAGNOSTIC ONLY since
    // #342 -- the game's own Talisman Pouch count, logged beside the stream-derived one on every
    // accessory equip so the two can be compared in a log. It decides nothing: the slot resolver
    // takes `TalismanPos` and there is no parameter left to feed this into.
    //
    // 🛑 DO NOT put it back. It is live state, and a live modulus is exactly what made the replay
    // of the received set rearrange the loadout (`er_logic::auto_equip::slot_for_accessory`).
    let unlocked_talisman_slots = pgd.unlocked_talisman_slots;

    // Snapshot id -> (handle, inventory index) first, so the inventory borrow is released before we
    // mutate chr_asm. Both values come off the entry this loop already visits; the index is the
    // entry's position in the normal-items list, biased by `key_items_capacity` the way the game
    // biases it (indices below the capacity address the key-item list instead).
    //
    // 🛑 THE KEY-ITEM LIST IS NOT OPTIONAL. This walked `normal_entries()` alone until #334, which
    // was correct for as long as auto_equip only handled weapons, protectors and talismans -- none
    // of which are key items. PHYSICK TEARS ARE: the pinned crate documents
    // `multiplay_key_items` as holding the `REGENERATIVE_MATERIAL` and `WONDROUS_PHYSICK_TEAR`
    // copies of the KEY items list. A delivered tear would therefore never resolve here, would go
    // back on `still_pending`, and would retry forever -- and in a log that is indistinguishable
    // from #296.
    //
    // The index for a key entry is its raw position (the game addresses key items with indices
    // BELOW `key_items_capacity`); it is carried for symmetry only, because the one category that
    // can appear in this list -- goods -- takes the physick branch below and never reaches the
    // four-rep commit that consumes the index.
    let inventory = &pgd.equipment.equip_inventory_data.items_data;
    let key_items_capacity = inventory.key_items_capacity;
    let owned: HashMap<u32, (GaitemHandle, u32)> = inventory
        .current_key_entries()
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| {
            let e = slot.as_option()?;
            Some((e.item_id.into_inner(), (e.gaitem_handle, i as u32)))
        })
        .chain(
            inventory
                .normal_entries()
                .iter()
                .enumerate()
                .filter_map(|(i, slot)| {
                    let e = slot.as_option()?;
                    Some((
                        e.item_id.into_inner(),
                        (e.gaitem_handle, key_items_capacity + i as u32),
                    ))
                }),
        )
        .collect();

    let mut still_pending: Vec<(i32, u64, u8)> = Vec::new();
    for (fid, ordinal, pouches) in pending {
        // WEAPONS HELD (#413): while a boss we armed for is on screen, a weapon arriving from
        // another world must not tear that tool out of the player's hands. The entry is HELD, not
        // dropped -- pushed straight back the same way an ungranted item is -- so it equips the
        // moment the fight ends and nothing in the stream is lost.
        //
        // Weapons only: armour and talismans mid-fight cannot cost the player the fight, and the
        // narrower the carve-out the less of the French Challenge premise it contradicts.
        if WEAPONS_PAUSED.load(Ordering::Relaxed)
            && matches!(
                er_logic::auto_equip::equipable(fid),
                Some(er_logic::auto_equip::Equipable::Weapon)
            )
        {
            still_pending.push((fid, ordinal, pouches));
            continue;
        }
        let full = fid as u32;
        let Some(&(handle, inv_index)) = owned.get(&full) else {
            still_pending.push((fid, ordinal, pouches)); // not granted yet -- retry next tick
            continue;
        };
        let param_id = full & 0x0FFF_FFFF;

        // PHYSICK TEARS branch out here, before any of the four-rep machinery. They are not a
        // `ChrAsmSlot`, there is no handle to acquire and no commit to call: the mixture is a plain
        // `[OptionalItemId; 2]` and a dword store is the entire write.
        if er_logic::auto_equip::equipable(fid).is_none() {
            if !is_physick_tear(fid) {
                continue; // `enqueue` gates this; belt and braces
            }
            let equipment = &mut pgd.equipment;
            let mixture = physick_mixture(equipment);
            let Some(slot) = er_logic::physick::slot_for_tear(mixture, fid, ordinal) else {
                // Already mixed. Idempotent on purpose -- the reconciler replays the whole received
                // set on reconnect, and a slot rotation per replay would be a feature that eats
                // itself.
                continue;
            };
            let before = mixture[slot];
            write_physick_slot(equipment, slot, full);
            log::info!(
                "auto_equip: physick tear {full:#010x} (stream #{ordinal}) -> mixture slot \
                 {slot} (was {before:#010x}, mixture now {:#010x?})",
                physick_mixture(equipment)
            );
            continue;
        }

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
            Some(Equipable::Accessory) => {
                // Talismans are not upgradeable -- no `(param_id / 100) * 100` rounding, same as
                // protectors. A row the param table does not know is skipped rather than defaulted
                // into a slot: unlike the weapon arm there is no "main hand" to fall back to, and
                // an unknown accessory row is far more likely to be a bad id than a real talisman.
                if repo.get::<EquipParamAccessory>(param_id).is_none() {
                    log::debug!(
                        "auto_equip: accessory {full:#010x} has no EquipParamAccessory row -- \
                         skipped"
                    );
                    continue;
                }

                // What is in the four talisman slots RIGHT NOW. A slot counts as empty when its
                // entry does not resolve to an accessory row -- deliberately NOT a comparison
                // against a hard-coded empty sentinel, because the empty value in
                // `equipment_param_ids` has never been read off a live character here and
                // inventing one is how `wep_type 59` shipped a classifier that matched nothing.
                let worn = &pgd.equipment.chr_asm.equipment_param_ids;
                let slots: [Option<i32>; 4] = std::array::from_fn(|i| {
                    let id = worn[er_logic::auto_equip::ACCESSORY_SLOTS[i] as usize];
                    (id > 0 && repo.get::<EquipParamAccessory>(id as u32).is_some()).then_some(id)
                });

                let pos = er_logic::auto_equip::TalismanPos { ordinal, pouches };
                let Some(slot) =
                    er_logic::auto_equip::slot_for_accessory(pos, slots, param_id as i32)
                else {
                    // Already worn. ER refuses duplicate talismans, so equipping a second copy
                    // would build a loadout the menu cannot produce.
                    continue;
                };
                // BOTH counts, every time. `pouches` is what decided the slot; `raw` is the game's
                // own number and decides nothing. They should agree once the pouch grant has
                // landed (bobler's 0.3.5 log: pouch 05:05:41, raw 0 -> 1 by 05:05:43), so a line
                // where they differ is either that couple of seconds or a real disagreement -- and
                // either way it is now in the log rather than inferred.
                log::info!(
                    "auto_equip: talisman {full:#010x} -> slot {slot} (stream #{ordinal}, \
                     pouches={pouches} -> {} slot(s); unlocked_talisman_slots raw={unlocked_talisman_slots}, \
                     worn={slots:?})",
                    er_logic::auto_equip::usable_accessory_slots(pouches)
                );
                // 🛑 REPORTED, NOT REPAIRED (#342). The game knowing about MORE pouches than the
                // stream delivered is the one state the stream-derived count gets wrong, and the
                // repair -- taking the live field as a floor -- is exactly the live modulus this
                // change exists to remove, so it is not available. Say so instead.
                //
                // Two ways to reach it: the character carried pouches in from another run or
                // another seed (harmless, and the player can see why), or the seed did not place
                // Talisman Pouch as a check at all. The apworld ships all three copies as
                // randomized checks (`LOCATION_ITEM` 7770025..7770027), so the second should be
                // unreachable -- and if it ever is reached, this line is how we find out rather
                // than the player quietly losing slots 2..4.
                if u32::from(unlocked_talisman_slots) > u32::from(pouches) {
                    log::warn!(
                        "auto_equip: the character has {unlocked_talisman_slots} Talisman \
                         Pouch(es) but only {pouches} arrived through AP, so talismans will use \
                         {} slot(s) and not {} (#342)",
                        er_logic::auto_equip::usable_accessory_slots(pouches),
                        er_logic::auto_equip::usable_accessory_slots(unlocked_talisman_slots)
                    );
                }
                slot
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
