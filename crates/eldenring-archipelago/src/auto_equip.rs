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
//! `ChrAsm::operator=` at [`chr_asm_commit_rva`], which acquires the new handle and releases the
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

/// `ChrAsm::operator=` -- the refcounted commit. Per-version; see [`crate::rva_table`]. (The
/// 2.7.0.0 port found this address UNCHANGED from 2.6.2.0.) The description below is 2.6.2.0 WW. `(rcx = dest_chr_asm,
/// rdx = src_chr_asm)`. Copies both weapon ids, the xmm block, then loops 22 times calling the
/// handle-assign helper at `+0x682580` (acquire `+0x671A80` / release `+0x671B00`) for
/// `gaitem_handles`, then copies `equipment_param_ids[22]`.
fn chr_asm_commit_rva() -> usize {
    crate::rva_table::current().chr_asm_commit
}

/// First 16 bytes at [`chr_asm_commit_rva`], read from the pinned exe. A mismatch means the RVA is
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
/// its size must be exactly 22 + 10 quick + 6 pouch = 38 `u32`s, plus the trailing `unk98` dword
/// upstream folded into the struct at `0b44ede3` (it used to be the anonymous +4 gap between
/// `equipment_entries` and the physick pair). If the crate ever reshapes it, this fails the
/// build instead of silently writing the wrong field.
const _: () = assert!(size_of::<ChrAsmEquipEntries>() == 39 * 4);

/// Byte offset of the Flask of Wondrous Physick's two-tear mixture inside `EquipGameData`.
///
/// MEASURED, not guessed (see the module docs), and since upstream `0b44ede3` the crate MODELS
/// the pair itself as `physick_tears: [OptionalItemId; 2]`, so the assert pins our measured
/// offset against the real field rather than a derived sum. The reads and writes stay raw-offset
/// dword ops on purpose: the measured behaviour (unrefcounted plain stores) is documented above
/// against exactly that representation.
const PHYSICK_MIXTURE_OFF: usize = 0x3E4;
const _: () = assert!(std::mem::offset_of!(EquipGameData, physick_tears) == PHYSICK_MIXTURE_OFF);
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
    // #351: param_guard -- a mid-restream holder panics upstream; false = "not a tear", and the
    // tear path re-classifies on the next received item.
    crate::param_guard::get::<EquipParamGoods>(repo, row, "auto_equip tear check")
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

/// Does the queue currently hold a WEAPON?
///
/// #413 swap-back: an incoming AP weapon outranks restoring the player's pre-fight one. Weapons
/// held through the Rykard fight drain on the very tick the bar drops, so a restore enqueued then
/// would land after them and stomp what the player was just sent. A poisoned lock answers `true`
/// -- "something might be queued" is the arm that declines to act, which is the safe direction.
pub fn queue_has_weapon() -> bool {
    match PENDING.lock() {
        Ok(q) => q.iter().any(|&(fid, _, _)| {
            matches!(
                er_logic::auto_equip::equipable(fid),
                Some(er_logic::auto_equip::Equipable::Weapon)
            )
        }),
        Err(_) => true,
    }
}

/// The param rows currently in the two weapon slots, `[left_1, right_1]`.
///
/// `None` = the player's game data was not reachable this tick, which the caller must treat as
/// "don't know" and never as "empty". Read straight off `chr_asm.equipment_param_ids`, the same
/// array the talisman branch of `tick()` reads its occupancy from and the same one the renderer
/// polls -- so this is what the player is actually holding, not what we believe we equipped.
pub fn worn_weapon_param_ids() -> Option<[i32; 2]> {
    // SAFETY: FD4 singleton, read-only, on the single-threaded FrameBegin tick.
    let gdm = unsafe { GameDataMan::instance() }.ok()?;
    let worn = &gdm
        .main_player_game_data
        .as_ref()
        .equipment
        .chr_asm
        .equipment_param_ids;
    Some([
        worn[er_logic::auto_equip::SLOT_WEAPON_LEFT_1 as usize],
        worn[er_logic::auto_equip::SLOT_WEAPON_RIGHT_1 as usize],
    ])
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
        // 🛑 THIS SENTENCE WAS FALSE FOR A DAY AND IT COST A BUG REPORT. It read "spells are NOT
        // covered" -- written before #440 -- and stayed put when #148 shipped the memory-slot
        // write. It is the ONLY thing the log says about spells, so a player whose spell did not
        // land reads it and reasonably concludes the feature does not exist (boblerrr, 2026-08-11).
        // A banner is not a comment: it is the feature's own account of itself, and it must move
        // when the behaviour does.
        //
        // ⚠️ It also states the LIMIT, because that is the half that actually gets misread: this
        // acts on ARRIVAL only. A spell already sitting in the bag -- received under an older
        // build, or before the option was on -- is never looked at, because the receive cursor
        // persists across launches and nothing re-offers it. That is exactly what happened to
        // boblerrr's Rotten Breath and Ranni's Dark Moon, and no amount of logging inside the
        // spell path would have said so.
        log::info!(
            "auto_equip: enabled (received weapons, armour, talismans, physick tears and SPELLS \
             are equipped/memorised ON ARRIVAL). Anything already in the bag is NOT reconciled -- \
             the receive cursor persists across launches, so an item granted before this option \
             was on, or under a build that could not place it, stays where it is."
        );
    }
}

/// Is the equip path live? Read back from the flag the queue drain actually gates on, so the
/// answer cannot drift from the behaviour. See `feature_handshake`.
pub fn is_armed() -> bool {
    ENABLED.load(Ordering::Relaxed)
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

// ---- spells: memory slots (#440) ----------------------------------------------------------

/// Pins the measurement behind the memory-slot write. `equip_magic_data` IS public on the pinned
/// crate, so this is not needed to reach it -- it is here so that a crate reshape fails the BUILD
/// instead of letting the write land somewhere else, the same guard shape as `PHYSICK_MIXTURE_OFF`.
/// Measured in game on 1.16.2: EquipGameData sits at PlayerGameData+0x2B0 and the pointer at
/// PlayerGameData+0x530.
const _: () = assert!(std::mem::offset_of!(EquipGameData, equip_magic_data) == 0x280);

/// What a received item is, for memory-slot purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpellClass {
    /// A sorcery or incantation. Takes a memory slot.
    Spell,
    /// A Memory Stone. Raises the ceiling, occupies nothing.
    MemoryStone,
    /// Anything else.
    Other,
}

/// Classify a received FullID off the game's OWN `EquipParamGoods` row, exactly as
/// [`is_physick_tear`] does, so no hardcoded id list can go stale.
///
/// 🛑🛑 **Returns `Other` when the param repo is not up, and that is DANGEROUS to the caller.**
/// A spell's memory slot is a function of its POSITION in the received stream, so a stream built on
/// a tick where this returns `Other` for everything is not merely a shorter stream -- it puts every
/// later spell in the WRONG SLOT. The classification is deliberately all-or-nothing (the repo is
/// fetched before even the id-only Memory Stone test) so a half-built stream is impossible, and the
/// caller MUST additionally gate the whole stream build on `crate::flags::in_world()`.
pub fn spell_class(full_id: i32) -> SpellClass {
    let Some(row) = er_logic::physick::goods_row(full_id) else {
        return SpellClass::Other;
    };
    // Fetched FIRST, before the id-only Memory Stone test, so this function is all-or-nothing:
    // never "stones but not spells", which would shift placement silently.
    // SAFETY: FD4 singleton, read on the game thread like every other param read in this module.
    let Ok(repo) = (unsafe { SoloParamRepository::instance() }) else {
        return SpellClass::Other;
    };
    if er_logic::spell_equip::is_memory_stone(row) {
        return SpellClass::MemoryStone;
    }
    // #351: param_guard -- a mid-restream holder panics upstream; Other is the safe
    // classification, and the stream build re-runs on the next received item.
    let Some(g) = crate::param_guard::get::<EquipParamGoods>(repo, row, "auto_equip spell_class")
    else {
        return SpellClass::Other;
    };
    if er_logic::spell_equip::is_spell(g.goods_type(), g.sort_id()) {
        SpellClass::Spell
    } else {
        SpellClass::Other
    }
}

/// Which school a spell belongs to, off the same `EquipParamGoods` row [`spell_class`] reads.
///
/// `None` = not a spell, or the param repo was not up. The caller must treat "could not read" as
/// no-preference, never as "the player cannot cast it".
pub fn spell_school(full_id: i32) -> Option<er_logic::spell_equip::School> {
    let row = er_logic::physick::goods_row(full_id)?;
    // SAFETY: FD4 singleton, read on the game thread like every other param read in this module.
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    // #351: param_guard -- a mid-restream holder panics upstream; None = "could not read", which
    // the caller treats as no-preference, never as "the player cannot cast it".
    let g = crate::param_guard::get::<EquipParamGoods>(repo, row, "auto_equip spell_school")?;
    er_logic::spell_equip::school_of(g.goods_type())
}

/// What the player is holding, as far as casting is concerned.
///
/// Reads `chr_asm.equipment_param_ids` through [`worn_weapon_param_ids`] -- what the player is
/// ACTUALLY holding, not what we believe we equipped -- and resolves each row's `wepType`, the same
/// instrument the hand/slot routing already uses. No hardcoded catalyst id list to go stale.
///
/// `None` = game data unreachable this tick, which the caller must treat as don't-know.
pub fn held_catalysts() -> Option<er_logic::spell_equip::Catalysts> {
    let worn = worn_weapon_param_ids()?;
    // SAFETY: FD4 singleton, read-only, single-threaded FrameBegin tick.
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    // `/ 100 * 100` strips the upgrade level, exactly as the equip path does at its own wep_type
    // read -- a +9 staff is not a different weapon type.
    // #351: param_guard -- a mid-restream holder panics upstream; a failed read maps to None,
    // which from_wep_types already treats as don't-know.
    let types = worn.map(|id| {
        crate::param_guard::get::<EquipParamWeapon>(
            repo,
            ((id / 100) * 100) as u32,
            "auto_equip catalysts",
        )
        .map(|w| w.wep_type())
    });
    Some(er_logic::spell_equip::Catalysts::from_wep_types(types))
}

/// Received spells awaiting a memory-slot write: `(MagicParam id, stream position)`.
///
/// The SLOT is not resolved here: [`er_logic::spell_equip::slot_for_spell`] needs the live slot
/// array to spot a spell already memorised, and that is only readable on the game thread.
static PENDING_SPELLS: Mutex<Vec<(i32, er_logic::spell_equip::SpellPos)>> = Mutex::new(Vec::new());

/// Queue a received spell for its memory slot. Self-gates on the option, like [`enqueue`].
/// `pos` is `None` for anything that is not a spell.
pub fn enqueue_spell(full_id: i32, pos: Option<er_logic::spell_equip::SpellPos>) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let (Some(pos), Some(row)) = (pos, er_logic::physick::goods_row(full_id)) else {
        // 🛑 SAY IT. `pos` is None for anything `spell_class` did not call a Spell, which is the
        // overwhelmingly common case (every weapon, every rune) and must stay quiet -- but a
        // `goods_row` miss on an item the caller ALREADY classified as a spell is a real defect
        // wearing silence, and it used to leave no trace at all. Split the two.
        if pos.is_some() {
            log::warn!(
                "auto_equip: spell {full_id:#x} classified as a Spell but has no goods row -- \
                 NOT queued, and nothing will retry it. The goods->Magic identity has broken, or \
                 the param table is modded"
            );
        }
        return;
    };
    // goods id == MagicParam id, 213/213 with zero exceptions. The one collision this identity
    // creates (Stonesword Key, goods 8000) cannot reach here: `pos` is only ever Some for an item
    // `spell_class` called a Spell.
    let magic_id = er_logic::spell_equip::magic_row_for_spell_goods(row) as i32;
    if let Ok(mut q) = PENDING_SPELLS.lock() {
        q.push((magic_id, pos));
    }
}

/// Drain queued spells into memory slots. Called from the FrameBegin tick beside [`tick`], but
/// deliberately INDEPENDENT of it: the four-rep `ChrAsm` commit and its RVA pin have nothing to do
/// with a memory slot, and a stale weapon pin must not stop spells landing.
///
/// The write is ONE field. Measured on 1.16.2: `charges` is inert (`{id, 0}` casts exactly like the
/// `{id, -1}` the game itself writes) and `selected_slot` is the game's own cursor, which
/// self-corrects. A spell written this way displays in the Memorize screen, casts with no menu or
/// grace round-trip, and survives a reload.
pub fn tick_spells() {
    if !ENABLED.load(Ordering::Relaxed) || !crate::flags::in_world() {
        return;
    }
    let pending: Vec<(i32, er_logic::spell_equip::SpellPos)> = match PENDING_SPELLS.lock() {
        Ok(mut q) if !q.is_empty() => std::mem::take(&mut *q),
        _ => return,
    };
    let Ok(gdm) = (unsafe { GameDataMan::instance_mut() }) else {
        if let Ok(mut q) = PENDING_SPELLS.lock() {
            q.splice(0..0, pending);
        }
        return;
    };
    // SAFETY: FD4 singleton, mutated only on the single-threaded FrameBegin tick -- same contract
    // as `tick()` above.
    //
    // 🛑 `main_player_game_data` is what makes this correct in CO-OP. More than one
    // `EquipMagicData` is live (a second sits behind the other player slot, owner stride 0xC00);
    // resolving by signature and taking the first hit would ship a co-op-only bug. This names
    // player 0 by construction, so there is nothing to get wrong.
    let pgd = &mut *gdm.main_player_game_data;
    let magic = &mut *pgd.equipment.equip_magic_data;
    for (magic_id, pos) in pending {
        let mut slots = [None::<i32>; er_logic::spell_equip::MAGIC_SLOTS];
        for (i, s) in slots.iter_mut().enumerate() {
            let id = magic.entries[i].param_id;
            *s = (id != -1).then_some(id);
        }
        let Some(slot) = er_logic::spell_equip::slot_for_spell(pos, &slots, magic_id) else {
            // 🛑 THIS IS A DROP, NOT A DEFERRAL, and it used to be a bare `continue` with a
            // comment. `pending` was taken out of the queue by the `mem::take` above, so an entry
            // that lands here is GONE -- there is no retry on a later tick and no retry on a later
            // session. Whatever the reason, it is the last moment anyone can observe it.
            //
            // The benign reason is "already memorised", which is genuinely nothing to do and is
            // the case on every replayed receive. It still gets a line, at debug, because the two
            // are indistinguishable from outside and a reader chasing a missing spell needs to
            // know which one they are looking at.
            let n = er_logic::spell_equip::usable_magic_slots(pos.stones);
            if er_logic::spell_equip::already_memorised(pos, &slots, magic_id) {
                log::debug!(
                    "auto_equip: spell {magic_id} already in a memory slot -- nothing to do \
                     (ordinal {}, {n} usable slot(s))",
                    pos.ordinal
                );
            } else {
                log::warn!(
                    "auto_equip: spell {magic_id} DROPPED -- no slot resolved (ordinal {}, \
                     stones {}, {n} usable slot(s), occupied {:?}). It is out of the queue and \
                     nothing retries it",
                    pos.ordinal,
                    pos.stones,
                    &slots[..n.min(slots.len())]
                );
            }
            continue;
        };
        let evicted = slots[slot as usize];
        magic.entries[slot as usize].param_id = magic_id;
        // ⚠️ The slot is `ordinal % n`, so a seed that sends more spells than the player has
        // memory slots OVERWRITES. That is the designed policy (the French Challenge ruling: the
        // answer to a full loadout is WHERE it lands, never WHETHER it is equipped) -- but it is
        // indistinguishable in play from "my spell vanished", so the eviction is named.
        match evicted {
            Some(old) if old != magic_id => log::info!(
                "auto_equip: spell {magic_id} -> memory slot {slot} (evicted {old}; ordinal {} \
                 over {} usable slot(s), so the loadout is cycling)",
                pos.ordinal,
                er_logic::spell_equip::usable_magic_slots(pos.stones)
            ),
            _ => log::info!("auto_equip: spell {magic_id} -> memory slot {slot}"),
        }
    }
}

/// Spells the player received on an EARLIER launch, with the stream position they have always had:
/// `(MagicParam id, SpellPos)`, in ascending AP index order.
///
/// Republished in full on every receive pass rather than appended to, so it cannot grow, cannot
/// double-count, and cannot carry a stale entry -- the receive loop rebuilds `spell_pos` from the
/// whole stream every tick anyway, so this costs one Vec and buys idempotence by construction.
/// `(MagicParam id, stream position, school)`. The school rides along so the pass can order by
/// what the player can actually cast without a second param read per spell per tick.
type BackfillEntry = (
    i32,
    er_logic::spell_equip::SpellPos,
    Option<er_logic::spell_equip::School>,
);

static SPELL_BACKFILL: Mutex<Vec<BackfillEntry>> = Mutex::new(Vec::new());

/// Magic ids already reported as having nowhere to go, so [`tick_spell_backfill`] states that once
/// per launch instead of twice a second.
static BACKFILL_NO_ROOM_REPORTED: Mutex<Vec<i32>> = Mutex::new(Vec::new());

/// Publish the below-watermark spells for [`tick_spell_backfill`]. Called from the receive loop,
/// which is the only place that can see the whole stream.
///
/// 🛑 CALL IT ONLY WHEN THE SPELL STREAM WAS ACTUALLY BUILT. `core.rs` gates the classification on
/// `spells_readable` because it reads a param row, and an unreadable tick classifies everything as
/// `Other` -- publishing then would replace a good list with an empty one and make the pass flap.
pub fn set_spell_backfill(list: Vec<BackfillEntry>) {
    if let Ok(mut g) = SPELL_BACKFILL.lock() {
        *g = list;
    }
}

/// Memorise spells the player owns from an earlier launch and that nothing will ever re-offer.
///
/// # Why this is needed at all
///
/// er-archipelago#549. `auto_equip` acts on arrival, once, and `received_through` is persisted PER
/// SAVE. boblerrr's Rotten Breath (13:14) and Ranni's Dark Moon (13:36) arrived under a 0.3.10
/// build; the 0.3.11 session that could have memorised them opened at `recv: stream=413
/// cursor=413`, already caught up. `enqueue_spell` was never called for either, and never will be.
///
/// ⭐ THE ORDINAL IS NOT INVENTED. `core.rs`'s receive loop already folds `SpellStream` over the
/// WHOLE stream every tick -- it has to, or a reconnect would report zero Memory Stones to a player
/// who found three last session -- so every stranded spell's `SpellPos` is computed correctly on
/// every tick and then dropped on the floor, because only items past the watermark reach the
/// snapshot. This reads the half that was being discarded. The AP stream order is fixed by the
/// server, so those ordinals are stable across launches without persisting anything.
///
/// # 🛑 It FILLS, it never EVICTS
///
/// See `er_logic::spell_equip::backfill_slot`. Re-running the receive path's `ordinal % n`
/// overwrite would let two congruent ordinals trade a slot forever, one pass undoing the last.
/// Filling only makes each pass either occupy one more slot or do nothing, which is why this
/// converges and then goes silent.
///
/// # ⚠️ Ownership is inferred from the watermark, not read from the bag
///
/// An item below `received_through` was granted, because the watermark is HELD when a grant fails
/// to place (`receive.rs` H3). That is a bookkeeping signal, and `start_item_backfill`'s doctrine
/// is to verify against the BAG instead. The mitigation is that this only ever writes into an EMPTY
/// slot, so the worst case of a wrong inference is a memorised spell the player does not have --
/// visible, non-destructive, and it overwrites nothing. Reading the bag here would mean a second
/// inventory walk; worth doing if that case is ever observed.
pub fn tick_spell_backfill() {
    if !ENABLED.load(Ordering::Relaxed) || !crate::flags::in_world() {
        return;
    }
    let pending: Vec<BackfillEntry> = match SPELL_BACKFILL.lock() {
        Ok(q) if !q.is_empty() => q.clone(),
        _ => return,
    };
    let Ok(gdm) = (unsafe { GameDataMan::instance_mut() }) else {
        return;
    };
    // SAFETY: FD4 singleton, mutated only on the single-threaded FrameBegin tick -- the same
    // contract as `tick_spells`, and `main_player_game_data` names player 0 so co-op is not a
    // question (a second EquipMagicData sits behind the other player slot).
    //
    // CATALYST-AWARE ORDER (#549). ⭐ A PREFERENCE, NEVER A FILTER. Nothing is withheld; castable
    // spells are simply offered the free slots first, which is the whole of the complaint when a
    // seal build with two memory slots meets a stream full of sorceries. Withholding would repeal
    // the French Challenge ruling `slot_for_spell` documents ("no 'only if the player can cast
    // it'"), and #549 asking for catalyst awareness does not repeal it.
    let held = held_catalysts().unwrap_or_default();
    let batch: Vec<(BackfillEntry, Option<er_logic::spell_equip::School>)> =
        pending.iter().map(|e| (*e, e.2)).collect();
    let ordered = er_logic::spell_equip::prefer_castable(&batch, held);

    let pgd = &mut *gdm.main_player_game_data;
    let magic = &mut *pgd.equipment.equip_magic_data;
    for (magic_id, pos, school) in ordered {
        let mut slots = [None::<i32>; er_logic::spell_equip::MAGIC_SLOTS];
        for (i, s) in slots.iter_mut().enumerate() {
            let id = magic.entries[i].param_id;
            *s = (id != -1).then_some(id);
        }
        match er_logic::spell_equip::backfill_slot(pos, &slots, magic_id) {
            // The common case on every pass after the first. Silent on purpose: this runs on the
            // frame tick, and a line here would be the whole log.
            er_logic::spell_equip::Backfill::AlreadyMemorised => {}
            er_logic::spell_equip::Backfill::Place { slot, home } => {
                magic.entries[slot as usize].param_id = magic_id;
                // "not silently placed where it cannot be cast" was #549's actual ask. It is still
                // PLACED -- withholding is the ruling this must not break -- but a player wondering
                // why a sorcery is sitting in a seal build's slots can read why, here.
                let uncastable = match school {
                    Some(sch) if !held.none() && !held.can_cast(sch) => format!(
                        ", which your equipped catalyst cannot cast ({sch:?}) -- it took a slot \
                         only because no castable spell wanted one"
                    ),
                    _ => String::new(),
                };
                log::info!(
                    "auto_equip backfill: spell {magic_id} -> memory slot {slot} (ordinal {}, \
                     stones {}{}{}). It was received on an earlier launch, below this save's \
                     receive cursor, so nothing would ever have offered it again",
                    pos.ordinal,
                    pos.stones,
                    if home {
                        ""
                    } else {
                        ", its own slot was taken so this is the first free one"
                    },
                    uncastable
                );
            }
            er_logic::spell_equip::Backfill::NoRoom { usable } => {
                // Stated ONCE per magic id per launch. It is a standing condition, not an event.
                if let Ok(mut seen) = BACKFILL_NO_ROOM_REPORTED.lock()
                    && !seen.contains(&magic_id)
                {
                    seen.push(magic_id);
                    log::info!(
                        "auto_equip backfill: spell {magic_id} is owned but not memorised, and all                          {usable} usable memory slot(s) are full. NOTHING IS EVICTED -- memorise it                          yourself, or find a Memory Stone and it lands on the next pass"
                    );
                }
            }
        }
    }
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
    let addr = base + chr_asm_commit_rva();
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

/// Normalize a genuinely fresh starting-class loadout to the challenge's one active left-hand
/// slot (#441).
///
/// `Some(n)` means the live read-back settled and `n` reserve slots were unequipped. `None` means
/// retry: the option/world/typed singleton/commit pin was unavailable, no live unarmed source
/// existed, or the read-back disagreed. The caller persists success per seed so this never touches
/// a returning player's manually curated loadout.
pub fn normalize_starting_left_slots() -> Option<usize> {
    if !ENABLED.load(Ordering::Relaxed) || !crate::flags::in_world() {
        return None;
    }
    let commit = commit_fn(current_module_base()?)?;
    // SAFETY: FD4 singleton, read/written on the single-threaded FrameBegin tick.
    let gdm = unsafe { GameDataMan::instance_mut() }.ok()?;
    let pgd = &mut *gdm.main_player_game_data;
    let equipment = &mut pgd.equipment;
    let worn: [i32; 6] = std::array::from_fn(|i| equipment.chr_asm.equipment_param_ids[i]);
    let selected_left = equipment.chr_asm.equipment.selected_slots.left_weapon_slot;
    let plan = match er_logic::auto_equip::starting_left_cleanup_plan(worn, selected_left) {
        Ok(plan) => plan,
        Err(er_logic::auto_equip::StartingLeftCleanupError::NoUnarmedSource) => {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                log::warn!(
                    "auto_equip starting loadout: Left2/3 need clearing but all six armament \
                     slots are populated -- no live unarmed representation to copy; retrying"
                );
            }
            return None;
        }
    };

    if plan.clear_slots.is_empty() && !plan.reset_selector {
        log::info!("auto_equip starting loadout: one-left-slot policy already settled");
        return Some(0);
    }

    let entries = (&raw mut equipment.equipment_entries).cast::<u32>();
    let indices = (equipment as *mut EquipGameData)
        .cast::<u8>()
        .wrapping_add(EQUIP_INDEX_OFF)
        .cast::<u32>();
    let unarmed = plan.unarmed_source.map(|source| {
        let source = source as usize;
        // SAFETY: source is one of WEAPON_SLOTS (0..=5), inside both arrays. The pure plan only
        // returns it after its live param row read as UNARMED_WEAPON_PARAM_ID.
        unsafe {
            (
                entries.add(source).read(),
                indices.add(source).read_unaligned(),
                equipment.chr_asm.gaitem_handles[source],
            )
        }
    });

    let live = &raw mut equipment.chr_asm;
    // SAFETY: `live` is a valid initialized ChrAsm; the temporary is a bitwise source for the
    // game's copy-assignment and has no Drop, matching the ordinary equip path below.
    let mut src = unsafe { std::ptr::read(live) };
    let mut unequipped = Vec::new();
    if let Some((unarmed_full, unarmed_index, unarmed_handle)) = unarmed {
        for &slot in &plan.clear_slots {
            let idx = slot as usize;
            // Record what was removed for the one-shot diagnostic before replacing each rep.
            unequipped.push((slot, worn[idx]));
            // SAFETY: target is Left2 or Left3, both inside the pinned arrays.
            unsafe {
                entries.add(idx).write(unarmed_full);
                indices.add(idx).write_unaligned(unarmed_index);
            }
            src.gaitem_handles[idx] = unarmed_handle;
            src.equipment_param_ids[idx] = er_logic::auto_equip::UNARMED_WEAPON_PARAM_ID;
        }
    }
    if plan.reset_selector {
        src.equipment.selected_slots.left_weapon_slot = 0;
    }
    // SAFETY: signature-verified game copy-assignment, same call contract as the ordinary equip
    // path. It releases the outgoing handles and acquires the copied unarmed handle.
    unsafe { commit(live, &raw const src) };

    let settled = plan.clear_slots.iter().all(|&slot| {
        equipment.chr_asm.equipment_param_ids[slot as usize]
            == er_logic::auto_equip::UNARMED_WEAPON_PARAM_ID
    }) && equipment.chr_asm.equipment.selected_slots.left_weapon_slot == 0;
    if !settled {
        log::warn!(
            "auto_equip starting loadout: one-left-slot write did not read back -- retrying"
        );
        return None;
    }
    log::info!(
        "auto_equip starting loadout: one-left-slot policy settled; unequipped {:?}, active Left1",
        unequipped
    );
    Some(plan.clear_slots.len())
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
    // #351: the drain loop reads three param holders per queued entry; a mid-restream holder
    // would panic upstream, and gating here keeps one teardown event to one warn rather than one
    // per item. The queue stays pending and re-drains next tick.
    if !crate::param_guard::is_available::<EquipParamWeapon>(repo, "auto_equip drain")
        || !crate::param_guard::is_available::<EquipParamAccessory>(repo, "auto_equip drain")
        || !crate::param_guard::is_available::<EquipParamProtector>(repo, "auto_equip drain")
    {
        return;
    }

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
                let wep_type = crate::param_guard::get::<EquipParamWeapon>(
                    repo,
                    (param_id / 100) * 100,
                    "auto_equip drain",
                )
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
                if crate::param_guard::get::<EquipParamAccessory>(
                    repo,
                    param_id,
                    "auto_equip drain",
                )
                .is_none()
                {
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
                    (id > 0
                        && crate::param_guard::get::<EquipParamAccessory>(
                            repo,
                            id as u32,
                            "auto_equip drain",
                        )
                        .is_some())
                    .then_some(id)
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
                let Some(cat) = crate::param_guard::get::<EquipParamProtector>(
                    repo,
                    param_id,
                    "auto_equip drain",
                )
                .map(|p| p.protector_category()) else {
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
