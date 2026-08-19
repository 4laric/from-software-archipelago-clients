//! `reconcile_io` — the WINDOWS binding for the pure [`er_logic::reconcile`] reconciler.
//!
//! This is the ONLY place the reconciler touches the live game: it implements [`GameIo`] against the
//! `fromsoftware-rs` singletons (event flags via `crate::flags`, goods via the `GameDataMan`
//! inventory walk that `inventory.rs` / `upgrades.rs` already use, item grants via
//! `crate::detour::grant_full_id`), owns the poll-thread tick, and persists the per-save ledger
//! watermark to a file next to the client.
//!
//! ## Possession is NO LONGER bag-only (2026-08-02)
//!
//! [`GameIo::has_good`] reads the three bag lists UNION the GREAT-RUNE EQUIP SLOT UNION the
//! STORAGE BOX. Equipping a Great Rune is believed to detach its row from those lists while the
//! game still counts it possessed; a bag-only readback then reports it absent forever and the
//! reconciler re-grants it forever. That is the 2026-07-29 six-session flood (4525 grants, six
//! CTDs, two lost saves), replayed in `er_logic::reconciler_replay`.
//!
//! The equip-slot hypothesis is UNCONFIRMED IN GAME — [`inventory_forensics`] is still the
//! instrument that decides it, and its `great_rune_slot[..]` line stays in place for that reason.
//! The widening is deliberately the conservative direction: it can only make the readback MORE
//! permissive (it can suppress a re-grant, never cause one), and
//! `er_logic::reconcile::MAX_GRANT_ATTEMPTS` remains the backstop if the real cause turns out to be
//! elsewhere — the code's own ranked candidate is still the `key_items_accessor` switch, which
//! neither term touches.
//!
//! ### STORAGE is IN the union, and here is what that costs
//!
//! 🛑 This REVERSES a documented deliberate choice. Until the second commit on
//! `fix/possession-includes-equip-slot` this file said, in three places, that storage stays OUT
//! because "a good sitting in the box is not held". **Alaric's instruction (2026-08-02) is to
//! include it**, and these comments were rewritten rather than left contradicting the code.
//!
//! STATE THE CONSEQUENCE PLAINLY: a good sitting in the storage box now reads as POSSESSED, so the
//! reconciler will NOT re-grant it. An item the player deliberately stashed is never re-delivered
//! while it is in the box, and nothing tells the player why. That is the intended trade — it stops
//! an unbounded re-grant loop for any stored good, and the good is not lost, it is in the box — but
//! it is a real, silent behaviour change.
//!
//! Two things make the trade SMALLER than it sounds, one makes it LARGER:
//!
//! * SMALLER — the self-heal is suspended, not deleted. Withdraw the good and then lose it (drop,
//!   save-scum, bulk-load clobber) and it is absent from all three stores again, so the next tick
//!   re-grants it exactly as it did before.
//! * SMALLER — today's presence-diffed set is `DesiredInputs::unique_goods`: key items, Great
//!   Runes, and OWNED progressive rungs, which are all KEY items in game. The recorded finding is
//!   that the storage box does not take key items, so on a shipped seed this term should never
//!   fire. It is a guard for the class, not a fix for the motivating case — and the day it DOES
//!   fire, that is itself the finding: a good we believed unstoreable is storable.
//! * LARGER — the storage walk used to be reached only from [`inventory_forensics`], which runs at
//!   most once per stalled good. It now sits on the per-tick readback path (behind a short-circuit:
//!   only a good MISSING from every bag list gets that far). Anything unsound about reading
//!   `pgd.storage` — a non-null but not-yet-populated box mid-load, a capacity the slice
//!   constructor trusts — is now exercised orders of magnitude more often than it was.
//!   NOTE(windows-verify): watch for a first-load CTD/hang that the equip-slot commit did not have.
//!
//! ## Build / wiring status
//!
//! * This module compiles ONLY on Windows (it depends on `eldenring` / `fromsoftware-shared`), same
//!   as the rest of this crate. It is NOT host-testable — the LOGIC it drives is, in `er-logic`.
//! * It is now wired into `core.rs`'s `update_live` behind the `RECONCILE_DRYRUN` env guard
//!   (additive; the old handlers stay live and unchanged). `core.rs` is NOT truncated — an earlier
//!   note claiming so was a mount read-truncation artifact; in git it is a complete 2124-line file.
//!   The call sites are marked `INTEGRATION:` below.
//! * Phase 0 of the migration is the READ-ONLY DRY RUN (`RECONCILE_DRYRUN=1`): compute + log the diff
//!   every tick WITHOUT applying it, so the live diff can be validated against today's behavior
//!   before any mutation path is switched over.
//!
//! Everything below is straight-line glue; the decisions all live in the pure crate.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use er_logic::marker::{self, FlagBand};
use er_logic::ownership::{Class, OwnerState, Suspended};
use er_logic::reconcile::{
    ApplyClasses, CharLedger, DesiredInputs, GameIo, Reconciler, TickBudget, WorldStability,
    legacy_adopt, seed_trust, stamp_playtime,
};
use serde::{Deserialize, Serialize};

/// The save-embedded reconcile marker's flag band (`crate::marker`, minibake). The watermark +
/// (seed, slot) identity live INSIDE the save here, so a reconnect reads ground truth instead of
/// inferring identity from `play_time`. Band verified in-game 2026-07-21 (the `!markerprobe` pass).
const MARKER_BAND: FlagBand = FlagBand::PLACEHOLDER;

// Session-init verdict exported for one-time fresh-loadout work (#441). 0 = not initialized or
// refused, 1 = returning character, 2 = genuinely fresh character. Atomic because the consumer is
// a later phase of the same game-thread tick; no lock or payload is needed.
static FRESH_CHARACTER_VERDICT: AtomicU64 = AtomicU64::new(0);

/// Whether reconciler initialization proved this is a genuinely fresh character.
///
/// `None` means initialization has not settled or the save was refused. Callers must wait rather
/// than guessing from a missing sidecar file.
pub fn fresh_character_verdict() -> Option<bool> {
    match FRESH_CHARACTER_VERDICT.load(Ordering::Relaxed) {
        1 => Some(false),
        2 => Some(true),
        _ => None,
    }
}

/// Clear the per-session verdict before a different seed is initialized.
pub fn reset_fresh_character_verdict() {
    FRESH_CHARACTER_VERDICT.store(0, Ordering::Relaxed);
}

/// SENTINEL flag id used for the folded-in goal-send (Gap 1). `core::build_desired_inputs` sets
/// `SlotData.goal_flag = Some(GOAL_SENTINEL_FLAG)` and `goal_met` from the live goal predicate, so the
/// PURE desired state carries the goal as a first-class target (proven in `er_logic::reconcile`).
///
/// NOTE(windows-verify): goal-send is NOT an ER event flag — it is a `ClientStatus::Goal` network
/// send. Today only the READ-ONLY dry-run path is wired, where a would-apply `SetFlag(sentinel)` is
/// merely LOGGED (harmless). Before the ledger/goods APPLY cutover, one of the following must land
/// (glue-only — er-logic already models + tests it):
///   (a) route the `SetFlag(GOAL_SENTINEL_FLAG)` action to `client.set_status(ClientStatus::Goal)` via
///       a client seam (the reconciler's `GameIo` would need a goal callback), OR
///   (b) keep goal-send owned by the existing report-side handler in `core.rs` (§5c) and pass
///       `goal_flag: None` here — the pure fields stay available for a later seam.
/// The value is a high, deliberately-invalid event-flag id so that IF it ever reached
/// `try_set_event_flag` it is an inert no-op (invented ids no-op; see memory er-event-flag-validity)
/// rather than corrupting a real flag.
///
/// Currently unused at runtime: this cutover took option (b) — `build_desired_inputs` passes
/// `goal_flag: None` and goal-send stays on the core.rs §5c handler. Retained (allow dead_code) as
/// the ready-made target for option (a) if a `GameIo` goal seam is added later.
#[allow(dead_code)]
pub const GOAL_SENTINEL_FLAG: u32 = 0x7FFF_0001;

// ---------------------------------------------------------------------------------------------
// GameIo against the live singletons
// ---------------------------------------------------------------------------------------------

/// Live [`GameIo`] impl. Holds only the session dwell clock; all other state is read straight from
/// the game each call so a save-load can never desync it.
pub struct LiveGame {
    /// When the player most recently entered the world (reset on every world entry). Feeds the
    /// stability dwell fallback.
    in_world_since: Option<Instant>,
}

impl LiveGame {
    pub fn new() -> Self {
        LiveGame {
            in_world_since: None,
        }
    }

    /// Call each tick BEFORE reading stability so the dwell clock tracks continuous in-world time
    /// (and resets across a load screen). Mirrors `core.rs`'s in-world timer.
    pub fn refresh_dwell(&mut self) {
        if crate::flags::in_world() {
            if self.in_world_since.is_none() {
                self.in_world_since = Some(Instant::now());
            }
        } else {
            self.in_world_since = None;
        }
    }

    fn dwell_ms(&self) -> u64 {
        self.in_world_since
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0)
    }
}

impl Default for LiveGame {
    fn default() -> Self {
        Self::new()
    }
}

/// Walk the player's held goods and report whether a specific goods FullID is present.
///
/// MULTIPLAYER KEY-ITEM-LIST SWITCH (fix 2026-07-19; the Morgott's-Great-Rune re-grant loop CTD).
/// The obvious path — `items_data.items()` — is WRONG in an online session. `items()` walks
/// `current_key_entries()`, which follows `key_items_accessor`; per the crate that accessor "in
/// single-player typically points to `key_items`; in MULTIPLAYER it switches to
/// `multiplay_key_items`" — a short list holding only pots + wondrous physick tears, NO Great Runes
/// or other key items. So in a 2-player co-op session an already-held Great Rune (which lives in the
/// always-single-player `key_items` list) reads as MISSING, the reconciler re-grants it EVERY tick,
/// and the re-grant flood CTDs (Alaric + Andrew playtest, `archipelago20260719 Copy 4.log`: the
/// reconciler applied a Morgott's-Great-Rune action every frame after a Roundtable warp).
///
/// The fix scans all THREE backing lists explicitly instead of the accessor-following `items()`:
/// * `normal_entries()` — consumables, materials, most goods;
/// * `key_entries()` — the ALWAYS-single-player key items (Great Runes, quest keys); this is the
///   list `items()` stops seeing in multiplayer;
/// * `multiplay_key_entries()` — the online pots/physick-tears list.
///
/// A goods row present in ANY of them counts as held, in single-player OR co-op.
///
/// NOTE(windows-verify) — GOODS-ID MASK REVIEW (Gap 3; CANNOT be host-tested — this crate is
/// Windows-only). `goods` is the GRANT FullID `GOODS_FULLID | row` where `GOODS_FULLID = 0x4000_0000`
/// (see `er_logic::progressive::GOODS_FULLID`). In ER an `ItemId` packs the category in the top
/// nibble (category = id / 0x1000_0000; Goods = 4 -> 0x4000_0000) and the param ROW in the low 28
/// bits. So the two checks below SHOULD be right:
///   * `want_row = goods & 0x0FFF_FFFF` strips the 0x4 category nibble, leaving the bare row;
///   * `category() == ItemCategory::Goods` confirms the 0x4 nibble independently;
///   * `param_id()` is compared against the bare row.
///
/// LOOKS RIGHT: the mask matches the `0x4000_0000` goods-category convention this client grants with,
/// and the independent `category()` guard prevents a weapon/armor row with the same numeric row from
/// false-matching.
///
/// ✅ RESOLVED 2026-07-30 — suspicion 1 below is FALSIFIED AT THE SOURCE, no Windows run needed.
/// In the pinned crate rev (`Cargo.lock`: `fromsoftware-rs` @ `8c67a84`),
/// `crates/eldenring/src/cs/item_id.rs:56` declares `param_id_raw, set_param_id_raw: 27, 0` — a
/// bitfield over bits 27..0, i.e. the CATEGORY-STRIPPED row, exactly as assumed. The single-masked
/// compare below is correct and the "keep BOTH until confirmed" instruction is discharged; the
/// proposed double-mask alternative is dead and should NOT be reinstated. Field behaviour agrees:
/// were it wrong, every key item in every solo session would fail its readback and re-grant forever,
/// which is not what players see. Re-derive from the pinned rev if that dependency is bumped.
///
/// SUSPICIOUS / MUST CONFIRM ON WINDOWS with a set->readback (grant one known good, then re-read):
///   1. ~~Does `ItemId::param_id()` return the CATEGORY-STRIPPED row (assumed here), or the full
///      category-tagged id?~~ RESOLVED above — it is the stripped row.
///   2. Great Runes / key items are granted at the SAME `0x4000_0000` goods category, so they ride
///      this predicate correctly ONLY if their grant FullID also uses that nibble — verify the
///      key-item / great-rune mapper packs `GOODS_FULLID`, not a raw row or a different category.
///   3. Confirm no goods row legitimately exceeds `0x0FFF_FFFF` (rows are small, so this is safe, but
///      pin it).
///
/// DO NOT silently "fix" the mask: if a change is needed, keep the original masked compare in a
/// comment. The proposed alternative (double-mask) is noted inline below.
///
/// BEYOND THE BAG (2026-08-02). The bag walk is no longer the whole predicate. A good absent from
/// all three lists still counts as POSSESSED when it is in the GREAT-RUNE EQUIP SLOT (see
/// [`great_rune_slot_row`] for how that slot's `GaitemHandle` is resolved to a goods row) or in the
/// STORAGE BOX (see [`storage_has_goods_row`]). Storage was excluded when the equip-slot term first
/// shipped and is now IN, on Alaric's instruction; the module docstring states the trade — a
/// stashed good reads as possessed and is therefore never re-granted while it sits in the box.
fn inventory_has_goods(goods: i32) -> bool {
    use eldenring::cs::{GameDataMan, ItemCategory};
    use fromsoftware_shared::{FromStatic, NonEmptyIteratorExt};

    let gdm = match unsafe { GameDataMan::instance() } {
        Ok(g) => g,
        Err(_) => return false,
    };
    let pgd = gdm.main_player_game_data.as_ref();
    let inv = &pgd.equipment.equip_inventory_data.items_data;
    let want_row = (goods as u32 & 0x0FFF_FFFF) as i32;
    // Scan all three backing lists (NOT items(), which follows the accessor and goes blind to the
    // single-player key items — Great Runes — in an online session). key_entries() is the always-SP
    // key list; multiplay_key_entries() is the online pots/tears list; normal_entries() is the rest.
    for entry in inv
        .normal_entries()
        .iter()
        .chain(inv.key_entries().iter())
        .chain(inv.multiplay_key_entries().iter())
        .non_empty()
    {
        if entry.item_id.category() != ItemCategory::Goods {
            continue;
        }
        // Current compare (assumes param_id() is the category-stripped row):
        if entry.item_id.param_id() as i32 == want_row {
            return true;
        }
        // NOTE(windows-verify) PROPOSED ALTERNATIVE if suspicion #1 above proves true (param_id()
        // returns the full category-tagged id). Keep BOTH until confirmed on Windows; do not delete
        // the compare above without a set->readback proving this one is the correct form:
        //   if (entry.item_id.param_id() as i32 & 0x0FFF_FFFF) == want_row { return true; }
    }
    // Not in any bag list. Before declaring the good ABSENT — which is what re-emits `GrantUnique`
    // every tick — check the two other places the game keeps a good the player owns.
    //
    // 1. the great-rune equip slot, where an equipped rune may be the only place the row is visible.
    if great_rune_slot_row(pgd.equipment.equip_item_data.great_rune.gaitem_handle)
        .is_some_and(|row| er_logic::great_runes::equipped_row_satisfies(want_row, row))
    {
        return true;
    }
    // 2. the STORAGE BOX. `None` == no readable box, which is NOT a match. See the module docstring:
    // a stashed good reads as possessed, so the reconciler stops re-granting it. That is the whole
    // point and the whole cost.
    storage_has_goods_row(pgd, want_row).unwrap_or(false)
}

/// Whether a protector FullID from an armour bundle is already in the bag or storage. Unlike
/// consumables, armour is observable, so this is the reconnect/exactly-once key for bundle members.
fn inventory_has_protector(full_id: i32) -> bool {
    use eldenring::cs::{GameDataMan, ItemCategory};
    use fromsoftware_shared::{FromStatic, NonEmptyIteratorExt};

    let gdm = match unsafe { GameDataMan::instance() } {
        Ok(g) => g,
        Err(_) => return false,
    };
    let pgd = gdm.main_player_game_data.as_ref();
    let want_row = (full_id as u32 & 0x0FFF_FFFF) as i32;
    let bag_has = pgd
        .equipment
        .equip_inventory_data
        .items_data
        .normal_entries()
        .iter()
        .chain(
            pgd.equipment
                .equip_inventory_data
                .items_data
                .key_entries()
                .iter(),
        )
        .chain(
            pgd.equipment
                .equip_inventory_data
                .items_data
                .multiplay_key_entries()
                .iter(),
        )
        .non_empty()
        .any(|e| {
            e.item_id.category() == ItemCategory::Protector
                && e.item_id.param_id() as i32 == want_row
        });
    if bag_has {
        return true;
    }
    let Some(storage) = pgd.storage.as_ref() else {
        return false;
    };
    storage
        .items_data
        .normal_entries()
        .iter()
        .chain(storage.items_data.key_entries().iter())
        .non_empty()
        .any(|e| {
            e.item_id.category() == ItemCategory::Protector
                && e.item_id.param_id() as i32 == want_row
        })
}

/// Whether the STORAGE BOX holds `want_row`. `None` means there is no box to read (`pgd.storage`
/// is `None`), which is NOT the same answer as "read it, the row is not in there" (`Some(false)`).
/// [`inventory_forensics`] prints the difference; [`inventory_has_goods`] treats both as "not
/// possessed here" and moves on.
///
/// THE SHARED STORAGE WALK. [`inventory_forensics`] had this walk first, as a diagnostic; as of
/// 2026-08-02 [`inventory_has_goods`] needs the same answer, so both go through here rather than
/// keeping two copies that could drift apart. Same masked compare as the bag walk above
/// (`category() == Goods` plus `param_id() == want_row`) — keep it that way: a storage hit and a
/// bag hit must mean the same thing or the forensics line stops describing the predicate.
///
/// `normal_entries()` + `key_entries()` only. The multiplay key list is the online pots/tears
/// mirror of the HELD key list; it has no meaning for a storage box, and walking a third head on a
/// struct the game may only partly populate for storage buys nothing.
fn storage_has_goods_row(pgd: &eldenring::cs::PlayerGameData, want_row: i32) -> Option<bool> {
    use eldenring::cs::ItemCategory;
    use fromsoftware_shared::NonEmptyIteratorExt;

    let storage = pgd.storage.as_ref()?;
    Some(
        storage
            .items_data
            .normal_entries()
            .iter()
            .chain(storage.items_data.key_entries().iter())
            .non_empty()
            .any(|e| {
                e.item_id.category() == ItemCategory::Goods
                    && e.item_id.param_id() as i32 == want_row
            }),
    )
}

/// The goods param row currently sitting in the player's GREAT-RUNE equip slot, or `None` when the
/// slot is empty / holds something that is not a goods row.
///
/// HOW THE HANDLE IS RESOLVED (derived from the pinned crate source, `Cargo.lock`:
/// `fromsoftware-rs` @ `8c67a84`, `crates/eldenring/src/cs/gaitem.rs` — NOT guessed, and NOT
/// compiled here, since this crate is Windows-only):
///
/// * the GAITEM TABLE is a dead end. `CSGaitemImp::gaitem_ins_by_handle` returns `None` for any
///   handle whose `is_indexed()` is false, and the crate documents that bit as "true for
///   Protectors, Weapons and Gems" — goods are never indexed. So the row must come out of the
///   handle itself.
/// * for a NON-indexed handle, `selector()` (bits 23..0, with `is_indexed` at bit 23 clear) is the
///   bare param row. That is the same field `GaitemHandle::from_parts(selector, category)` packs,
///   and the same one the crate's `Display` prints as `GaitemHandle(-1,{selector},{category})`.
/// * `category()` is the GAITEM category (`GaitemCategory::Goods == 3`), which is a DIFFERENT enum
///   from the `ItemId` category used by the bag walk above (`ItemCategory::Goods == 4`). Both an
///   all-zero handle (reads as `Weapon`) and an all-ones handle (reads as an error) fail this
///   guard, so an EMPTY slot can never match.
/// * `selector == 0` is rejected explicitly as well: "nothing equipped" must read as NOT PRESENT,
///   never as a match on row 0.
///
/// NOTE(windows-verify): confirm in game with a set->readback — equip a Great Rune, then check that
/// the reconciler still sees it (no `INERT`/stall line naming a rune the player is wearing).
fn great_rune_slot_row(handle: eldenring::cs::GaitemHandle) -> Option<i32> {
    use eldenring::cs::GaitemCategory;

    if handle.is_indexed() {
        return None;
    }
    if handle.category().ok()? != GaitemCategory::Goods {
        return None;
    }
    let row = handle.selector();
    if row == 0 { None } else { Some(row as i32) }
}

/// Why a grant could not be observed, read from the game's OWN inventory bookkeeping.
///
/// Every field here is a datum the game already maintains — `is_*_full()` is
/// `len >= capacity` inside the crate, not our arithmetic — so this reports rather than infers.
/// Ordered by what it would let us conclude:
///
/// * the three list len/capacity pairs answer "was the add REFUSED for want of a slot" — the
///   "would exceed the maximum storage" popup both 2026-07-30 reporters saw;
/// * `sp_key_list` answers whether `key_items_accessor` is currently pointed at the always-single-
///   player `key_items` list or has switched to `multiplay_key_items`. The 2026-07-19 fix widened
///   the READ across all three lists but left the WRITE on the game's accessor, so an accessor
///   switched to the pots-only multiplay list is the leading candidate for a refused key-item add.
///   That is a HYPOTHESIS, not a finding — this line is here to confirm or kill it from one log;
/// * `in_storage` answers whether the good is sitting in the storage box. As of 2026-08-02 this is
///   no longer a store the predicate ignores: `inventory_has_goods` reads the SAME box through the
///   SAME helper ([`storage_has_goods_row`]), so `in_storage=true` at a stall now means the
///   reconciler is treating the good as POSSESSED and has stopped re-granting it. The great-rune
///   EQUIP slot below is in the union for the same reason.
///
/// Returns a plain sentence on any failure to read rather than a partial line implying a fact.
fn inventory_forensics(goods: i32) -> String {
    // `ItemCategory` / `NonEmptyIteratorExt` are no longer imported here: the only walk that needed
    // them was the storage one, which moved into the shared `storage_has_goods_row`.
    use eldenring::cs::GameDataMan;
    use fromsoftware_shared::FromStatic;

    let Ok(gdm) = (unsafe { GameDataMan::instance() }) else {
        return "inventory unreadable (no GameDataMan) -- cause UNKNOWN".to_string();
    };
    let pgd = gdm.main_player_game_data.as_ref();
    let inv = &pgd.equipment.equip_inventory_data.items_data;
    let want_row = (goods as u32 & 0x0FFF_FFFF) as i32;

    // Is the key-item accessor on the always-SP list, or has it switched to the multiplay one?
    let sp_key_list = std::ptr::eq(
        inv.key_items_accessor.head.as_ptr() as *const u8,
        inv.key_items_head.as_ptr() as *const u8,
    );

    // GREAT-RUNE EQUIP SLOT. Leading hypothesis for the 2026-07-29 flood: equipping a Great Rune at
    // a grace detaches its row from the three bag lists while the game still counts it as possessed,
    // so `inventory_has_goods` reads absent forever and the re-grant is refused forever. The timeline
    // fits -- the flood began LIVE at 01:36:10, ~50s after arriving at Roundtable with a
    // just-received Morgott's Great Rune, with no world edge -- but it is UNPROVEN, and this line is
    // what proves or kills it. `selector == 0` with an empty handle means nothing is equipped.
    //
    // KEEP THIS LINE. `inventory_has_goods` now UNIONS this slot (and the storage box) into the
    // possession predicate (2026-08-02), which stops the flood whether or not the hypothesis is the
    // true cause -- but it also means a fixed client can no longer demonstrate the blindness. This
    // log line and `in_storage` are the only things that still report those two stores at the moment
    // of a stall, so they are what tell us whether the widening was the right fix or merely masked
    // the `key_items_accessor` candidate.
    let gr = &pgd.equipment.equip_item_data.great_rune;
    // NB: `GaitemHandle`'s inner u32 is NOT pub outside the crate (bitfield tuple struct), so read
    // it through its accessors + derived Debug rather than `.0`.
    let great_rune_slot = format!(
        "{:?} selector={:#x} indexed={} index={} category={:?}",
        gr.gaitem_handle,
        gr.gaitem_handle.selector(),
        gr.gaitem_handle.is_indexed(),
        gr.index,
        gr.gaitem_handle.category(),
    );

    // Shared with the possession predicate as of 2026-08-02 -- see `storage_has_goods_row`. `None`
    // is "no box to read", which must NOT be printed as `false`.
    let in_storage = match storage_has_goods_row(pgd, want_row) {
        None => "n/a".to_string(),
        Some(found) => found.to_string(),
    };

    format!(
        "normal {}/{}{} | key {}/{}{} | multiplay_key {}/{}{} | global_cap {} | \
         key_accessor={} | in_storage={} | great_rune_slot[{}]",
        inv.normal_items_len,
        inv.normal_items_capacity,
        if inv.is_normal_items_full() {
            " FULL"
        } else {
            ""
        },
        inv.key_items_len,
        inv.key_items_capacity,
        if inv.is_key_items_full() { " FULL" } else { "" },
        inv.multiplay_key_items_len,
        inv.multiplay_key_items_capacity,
        if inv.is_multiplay_key_items_full() {
            " FULL"
        } else {
            ""
        },
        inv.global_capacity,
        if sp_key_list {
            "key_items (single-player)"
        } else {
            "multiplay_key_items (SWITCHED)"
        },
        in_storage,
        great_rune_slot,
    )
}

impl GameIo for LiveGame {
    fn stability(&self) -> WorldStability {
        let in_world = crate::flags::in_world();
        WorldStability {
            in_game: in_world,
            player_valid: crate::flags::play_region_id().is_some(),
            dwell_ms: self.dwell_ms(),
            // The generalized Torch-fix predicate: a real game-driven AddItem proves the bulk load
            // is done and the inventory is genuinely live.
            real_pickup_seen: crate::detour::real_pickup_seen(),
            // Monotonic, load-screen-independent clock feeding the grant PACING gate.
            now_ms: session_now_ms(),
        }
    }

    fn get_flag(&self, flag: u32) -> bool {
        crate::flags::get_event_flag(flag)
    }

    fn set_flag(&mut self, flag: u32, on: bool) -> bool {
        // `try_set_event_flag` returns false when `CSEventFlagMan` isn't ready -> the reconciler
        // retries next tick.
        crate::flags::try_set_event_flag(flag, on)
    }

    fn has_good(&self, goods: i32) -> bool {
        inventory_has_goods(goods)
    }

    fn grant_good(&mut self, goods: i32, companion_flags: &[u32]) -> bool {
        // `grant_full_id` returns false until the inventory pointer is captured -> retry next tick.
        if !crate::detour::grant_full_id(goods, 1) {
            return false;
        }
        for &f in companion_flags {
            let _ = crate::flags::try_set_event_flag(f, true);
        }
        true
    }

    fn has_protector(&self, full_id: i32) -> bool {
        inventory_has_protector(full_id)
    }

    fn grant_protector(&mut self, full_id: i32) -> bool {
        if !crate::detour::grant_full_id(full_id, 1) {
            return false;
        }
        // Queue the concrete member, never the synthetic wrapper. enqueue self-gates on auto_equip.
        crate::auto_equip::enqueue(full_id, None);
        true
    }

    fn grant_ledgered(&mut self, full_id: i32, qty: i32) -> bool {
        crate::detour::grant_full_id(full_id, qty)
    }
}

// ---------------------------------------------------------------------------------------------
// Per-save watermark persistence (file next to the client)
// ---------------------------------------------------------------------------------------------

/// The live ER save-slot index (0-9), or `None` if `GameMan` isn't up (menu / load). Same singleton
/// pattern as `inventory_has_goods`.
fn read_save_slot() -> Option<i32> {
    use eldenring::cs::GameMan;
    use fromsoftware_shared::FromStatic;
    unsafe { GameMan::instance() }.ok().map(|gm| gm.save_slot)
}

/// The live character's play-time in ms (`GameDataMan.play_time`), or `None` if `GameDataMan` isn't
/// up. Monotonic per character; resets to 0 on a new game.
fn read_play_time_ms() -> Option<u32> {
    use eldenring::cs::GameDataMan;
    use fromsoftware_shared::FromStatic;
    unsafe { GameDataMan::instance() }.ok().map(|g| g.play_time)
}

/// On-disk mirror of `CharLedger` (er-logic has no serde dep; convert at the boundary).
#[derive(Serialize, Deserialize, Clone, Copy)]
struct StoredLedger {
    watermark: i64,
    play_time_ms: u32,
}

/// The reconcile.json contents. `entries` are keyed per CHARACTER (`<slot>\u{1f}<save_slot>`) and
/// carry a play-time stamp; `legacy` holds the pre-fix bare `<slot> -> watermark` entries for one-time
/// migration (er-startitems-newchar-no-regrant).
#[derive(Serialize, Deserialize, Default)]
struct StoreFile {
    #[serde(default)]
    entries: BTreeMap<String, StoredLedger>,
    #[serde(default)]
    legacy: BTreeMap<String, i64>,
}

/// Per-CHARACTER ledger-watermark persistence (was keyed by AP slot name only, which let a new ER
/// character on a slot inherit the prior character's watermark and never get its start items). Written
/// next to the client dll.
pub struct WatermarkStore {
    path: std::path::PathBuf,
    file: StoreFile,
}

impl WatermarkStore {
    /// Load (or start empty) from `path`. A missing/malformed file is empty; a pre-fix bare
    /// `{slot: watermark}` file is read into `legacy` for migration. Never panics.
    pub fn load(path: std::path::PathBuf) -> Self {
        let file = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| {
                serde_json::from_str::<StoreFile>(&t).ok().or_else(|| {
                    // pre-fix format: a bare `{slot: watermark}` map -> park it as legacy.
                    serde_json::from_str::<BTreeMap<String, i64>>(&t)
                        .ok()
                        .map(|legacy| StoreFile {
                            entries: BTreeMap::new(),
                            legacy,
                        })
                })
            })
            .unwrap_or_default();
        WatermarkStore { path, file }
    }

    /// Composite per-character key: AP slot name + the ER save-slot index (unit-separated).
    fn key(slot: &str, save_slot: i32) -> String {
        format!("{slot}\u{1f}{save_slot}")
    }

    /// The persisted entry for this character, or `None` if it has never been reconciled.
    pub fn get(&self, slot: &str, save_slot: i32) -> Option<CharLedger> {
        self.file
            .entries
            .get(&Self::key(slot, save_slot))
            .map(|s| CharLedger {
                watermark: s.watermark,
                play_time_ms: s.play_time_ms,
            })
    }

    /// TAKE this slot's pre-fix (slot-keyed, play-time-less) watermark for one-time migration: the
    /// caller adopts it for the live character via `legacy_adopt` and it is removed so no other
    /// character can inherit it.
    pub fn legacy_take(&mut self, slot: &str) -> Option<i64> {
        self.file.legacy.remove(slot)
    }

    pub fn set(&mut self, slot: &str, save_slot: i32, entry: CharLedger) {
        self.file.entries.insert(
            Self::key(slot, save_slot),
            StoredLedger {
                watermark: entry.watermark,
                play_time_ms: entry.play_time_ms,
            },
        );
        // Best-effort write-through; a failure just means we re-grant-check next boot (idempotent).
        if let Ok(t) = serde_json::to_string(&self.file) {
            let _ = std::fs::write(&self.path, t);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The poll-thread driver
// ---------------------------------------------------------------------------------------------

/// The live reconciler + its IO + watermark store. `OnceLock<Mutex<..>>` because `set_inputs` and
/// `tick` are separate entry points; both are called from the game thread today, and the mutex keeps
/// that an implementation detail rather than a requirement.
static DRIVER: OnceLock<Mutex<Driver>> = OnceLock::new();

/// Set at init when the save-embedded marker's identity MISMATCHES this connection's (seed, slot) —
/// i.e. this save belongs to a different seed/slot. The reconciler is NOT armed (no grants), and the
/// caller must also gate check REPORTING on this, so seed-A's save flags aren't reported as seed-B
/// checks (which would corrupt the multiworld, strictly worse than a double-grant). See `is_refused`.
///
/// Cleared in exactly ONE place -- [`clear_refusal_if_rearmable`], on the menu edge, and only for
/// the refusals `er_logic::marker::release_verdict` calls releasable. It was a write-once latch
/// until 2026-08-10, which made the toast's own "start a fresh character" instruction impossible to
/// follow; see that function for the whole story.
static REFUSED: AtomicBool = AtomicBool::new(false);

/// Whether the current session was REFUSED by the marker identity guard (see [`REFUSED`]). `core`
/// gates check reporting on this; the reconciler simply never armed.
pub fn is_refused() -> bool {
    REFUSED.load(Ordering::Relaxed)
}

/// WHY the session was refused, so the player can be told what to DO about it. `REFUSED` stays an
/// `AtomicBool` because `is_refused` is on the check-reporting hot path; this is read once a tick.
static REFUSAL: Mutex<Option<marker::Refusal>> = Mutex::new(None);

/// The on-screen notice owed while this session is refused, or `None` when it is healthy.
///
/// Re-computed every tick by design: the condition PERSISTS (a refused session never heals without
/// player action), and `er_logic::toast::Deck::push` refreshes identical text instead of stacking,
/// so a per-tick re-push keeps the notice on screen for free. Same shape as
/// `scaling::tick() -> Option<String>`: this owns no I/O, the caller owns the deck.
pub fn refusal_toast() -> Option<String> {
    if !is_refused() {
        return None;
    }
    let refusal = (*REFUSAL.lock().ok()?)?;
    Some(marker::refusal_toast(refusal))
}

/// Latch the refusal + its reason together, so `is_refused` and `refusal_toast` can never disagree.
fn set_refused(refusal: marker::Refusal) {
    if let Ok(mut r) = REFUSAL.lock() {
        *r = Some(refusal);
    }
    REFUSED.store(true, Ordering::Relaxed);
}

/// RELEASE a latched refusal, if this is one of the refusals that may be released. Returns `true`
/// iff the latch was actually cleared, in which case the caller MUST also clear its own
/// `reconcile_inited` so [`init`] runs again for the next character.
///
/// 🛑 WHY THIS EXISTS (2026-08-10, Alaric): [`refusal_toast`] tells a wrong-save player to start a
/// fresh character, and that instruction could not work. `REFUSED` had no writer that ever stored
/// `false`, and `core` sets `reconcile_inited` the moment [`init`] RETURNS -- including the refuse
/// path, which returns before building a `Driver`. So the player quit to the menu, rolled a new
/// character, and loaded into a session that was gated, silent and permanently inert: no checks
/// reported, no items granted, the same toast still on screen. The only recovery was restarting the
/// game, and nothing said so.
///
/// The decision is [`marker::release_verdict`], NOT a local `if` -- test/prod drift is exactly what
/// the replay tier exists to kill, and the timelines live in `er_logic::marker_replay`. It is
/// deliberately asymmetric: a `WrongSaveAtConnect` refusal never built a driver, so re-running
/// `init` is a genuine first init; a `RoomChangedMidSession` disarm holds forever, because `DRIVER`
/// is a `OnceLock` that cannot be replaced and releasing would un-gate the pipeline while keeping
/// the old room's reconciler. `DRIVER.get().is_some()` is passed in so that invariant is CHECKED
/// rather than assumed.
///
/// Called from the `in_world` true->false edge (the player left to the menu), never mid-world: the
/// question this re-opens is one only a fresh `init` can answer, and `init` only runs in-world.
pub fn clear_refusal_if_rearmable() -> bool {
    if !is_refused() {
        return false;
    }
    // Hold the REFUSAL lock across the `REFUSED` store, for the same reason `set_refused` sets them
    // together: `is_refused` and `refusal_toast` must never disagree about the current state.
    let Ok(mut guard) = REFUSAL.lock() else {
        return false; // poisoned: a refusal we cannot evaluate must stay latched (fail closed)
    };
    let Some(refusal) = *guard else {
        return false;
    };
    match marker::release_verdict(refusal, DRIVER.get().is_some()) {
        marker::RefusalRelease::Hold => false,
        marker::RefusalRelease::Rearm => {
            *guard = None;
            REFUSED.store(false, Ordering::Relaxed);
            log::info!(
                "[reconcile] refusal RELEASED at the menu ({refusal:?}) -- session init will run \
                 again for the next character. Loading the same wrong save simply refuses again; \
                 this re-asks the guard's question, it does not answer it."
            );
            true
        }
    }
}

/// Re-ask the reconnect guard's question about an ALREADY-ARMED reconciler, and disarm it if the
/// session's identity has moved out from under it.
///
/// 🛑 WHY THIS EXISTS (2026-07-30, boblerrr): the marker guard in [`init`] is evaluated ONCE, on the
/// first stable in-world tick, behind `core`'s `reconcile_inited` latch — and `reset_for_new_seed`
/// does not clear that latch. So a player who reconnects to a DIFFERENT room WITHOUT restarting the
/// game kept an armed reconciler built for the old room: **229 of room A's checks were reported into
/// room B** before the next game restart let `init` run and refuse. That is the exact corruption the
/// doc on [`REFUSED`] calls "strictly worse than a double-grant", and it cannot be undone — the
/// checks are already on someone else's server.
///
/// Fail-CLOSED by construction: this only ever SETS `REFUSED`, never clears it. Re-arming a
/// reconciler mid-session would mean rebuilding the `Driver` for the new identity, and `DRIVER` is a
/// `OnceLock` — it cannot be replaced, so a "clear the latch and re-init" fix would silently keep
/// the OLD driver while looking correct. Gating until the player restarts the game is the honest
/// bound, and a restart is exactly what the message asks for.
///
/// ⚠️ [`clear_refusal_if_rearmable`] (2026-08-10) can now clear `REFUSED` — but NOT this refusal.
/// `marker::release_verdict` holds every `RoomChangedMidSession` unconditionally, precisely because
/// of the `OnceLock` argument above, and it is host-tested as
/// `marker_replay::the_menu_edge_never_releases_a_mid_session_room_change`. The reason this
/// paragraph is an invariant with a test instead of a comment is that it now has a neighbour that
/// looks like a counter-example.
///
/// No-op when nothing is armed: [`init`] has not run, so its own guard still covers that session.
pub fn disarm_if_identity_moved(room_seed: &str) {
    let Some(m) = DRIVER.get() else {
        return; // never armed -> init's guard is still ahead of us, nothing to disarm
    };
    let Ok(d) = m.lock() else {
        return; // poisoned: a refusal we cannot evaluate must not panic the game thread
    };
    match marker::armed_verdict(d.identity, room_seed, &d.slot) {
        marker::ArmedVerdict::Keep => {}
        marker::ArmedVerdict::Disarm { armed, live } => {
            set_refused(marker::Refusal::RoomChangedMidSession);
            log::warn!(
                "[reconcile] DISARMED: the armed reconciler belongs to identity {armed:#010x} but \
                 this connection is {live:#010x} (room seed {room_seed:?}, slot {:?}) -- the room \
                 changed under a live session. NOT applying anything further and check reporting is \
                 gated, so this save's checks are not reported into the new room. RESTART the game \
                 to play the new room, or reconnect to the original one.",
                d.slot
            );
        }
    }
}

struct Driver {
    reconciler: Reconciler,
    io: LiveGame,
    store: WatermarkStore,
    /// AP slot name (the `SaveIdentity`) + the ER save-slot index: together the per-character
    /// watermark key. `save_slot < 0` means it was unreadable at init (never persisted under it).
    slot: String,
    save_slot: i32,
    /// The marker identity for this session = `hash(room seed, AP slot name)`. Written into the save's
    /// marker band alongside the watermark on every tick commit; the reconnect guard compares it.
    identity: u32,
}

/// A PROCESS-monotonic clock in ms that — unlike the per-world dwell clock — never resets on a load
/// screen. The grant PACING gate needs a steady wall-clock tick so a large received-item delta drains
/// a burst at a time (spaced by real time) instead of flooding `AddItemFunc` in one frame.
fn session_now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// The LIVE per-tick budget, PACED so a large delta can't grant a flood of items in one frame (the
/// mass-grant CTD). Tunable at runtime with NO rebuild:
///   * `RECONCILE_GRANT_BURST`       — goods/ledger grants per interval (default 2; must be > 0),
///   * `RECONCILE_GRANT_INTERVAL_MS` — min ms between grant bursts (default 150; `0` disables pacing).
///
/// Flags stay cheap and unpaced (`CSEventFlagMan` writes don't drive the acquisition popup / phantom-
/// check machinery that the item-grant flood does), so region-open / map-reveal never stall behind a
/// held goods class.
fn paced_budget() -> TickBudget {
    fn env_usize(k: &str, d: usize) -> usize {
        std::env::var(k)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(d)
    }
    fn env_u64(k: &str, d: u64) -> u64 {
        std::env::var(k)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(d)
    }
    TickBudget {
        goods: env_usize("RECONCILE_GRANT_BURST", 2),
        flags: 32,
        min_grant_interval_ms: env_u64("RECONCILE_GRANT_INTERVAL_MS", 150),
    }
}

/// Is dry-run mode on? (`RECONCILE_DRYRUN=1` — phase 0: compute + log the diff, never apply.)
fn dry_run() -> bool {
    std::env::var("RECONCILE_DRYRUN")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Public view of [`dry_run`] so `core.rs` can gate its additive dry-run wiring on the same env var
/// (it must NOT do the reconciler snapshot/set_inputs work at all unless dry-run is on).
pub fn dry_run_enabled() -> bool {
    dry_run()
}

/// APPLY-mode active: NOT dry-run AND at least one class is enabled. `core.rs` widens its reconciler
/// gate on this so the apply path is reachable when `RECONCILE_APPLY` names a class (the dry-run gate
/// alone left `tick()` uncallable in apply mode — the wiring gap this cutover fixes).
pub fn apply_active() -> bool {
    if dry_run() {
        return false;
    }
    let c = apply_classes();
    c.flags || c.goods || c.ledger
}

/// Per-class ownership predicates for the strangler: a class is owned by the reconciler ONLY when
/// not in dry-run and that class is enabled. `core.rs` skips the corresponding OLD handler when the
/// reconciler owns the class, so the two never both mutate (no double-grant), and `RECONCILE_APPLY`
/// (or `RECONCILE_DRYRUN=1`) is a runtime fallback to the old path with no rebuild.
pub fn owns_flags() -> bool {
    owner_state().owns(Class::Flags)
}
pub fn owns_goods() -> bool {
    owner_state().owns(Class::Goods)
}
pub fn owns_ledger() -> bool {
    owner_state().owns(Class::Ledger)
}

/// I3 (2026-08-01): is a `Driver` actually built for this session?
///
/// `init` only builds one at the first STABLE IN-WORLD tick, which never arrives if the inventory
/// pointer is never captured (a foreign AddItemFunc hook, or the static prime being off). Before I3
/// the `owns_*` predicates never asked, so `core.rs` stood down for an owner that did not exist.
fn armed() -> bool {
    DRIVER.get().is_some()
}

/// The live ownership facts, in one snapshot, for the pure `er_logic::ownership` seam.
fn owner_state() -> OwnerState {
    OwnerState {
        configured: apply_classes(),
        dry_run: dry_run(),
        armed: armed(),
        refused: is_refused(),
    }
}

/// Wall-clock ms at which this session first observed an in-world tick, or `NOT_YET`. Feeds the
/// never-armed grace period — arming legitimately takes a load screen, so the notice must not fire
/// during one.
static FIRST_IN_WORLD_MS: AtomicU64 = AtomicU64::new(NOT_YET);
const NOT_YET: u64 = u64::MAX;

/// Called from `core`'s tick whenever the world is loaded, so the never-armed grace can be measured
/// in IN-WORLD time rather than process time (a player sitting in the main menu is not stuck).
pub fn note_in_world(now_ms: u64) {
    let _ =
        FIRST_IN_WORLD_MS.compare_exchange(NOT_YET, now_ms, Ordering::Relaxed, Ordering::Relaxed);
}

/// The on-screen notice owed when an owner is configured but is not going to deliver (I4).
///
/// Returns `None` for a healthy session AND for the deliberate baseline/dry-run modes. `Refused`
/// defers to [`refusal_toast`], which carries the per-refusal actionable text; this covers the
/// NEVER-ARMED state that had no symptom at all before I3.
pub fn suspension_toast(now_ms: u64) -> Option<&'static str> {
    let first = FIRST_IN_WORLD_MS.load(Ordering::Relaxed);
    if first == NOT_YET {
        return None; // never been in-world: nothing is expected to have armed yet
    }
    let in_world_ms = now_ms.saturating_sub(first);
    match owner_state().suspended(in_world_ms)? {
        Suspended::Refused => None, // refusal_toast owns this message
        s => {
            if !SUSPENSION_LOGGED.swap(true, Ordering::Relaxed) {
                log::warn!(
                    "[reattach] delivery SUSPENDED ({s:?}) after {in_world_ms}ms in-world -- \
                     configured={:?} armed={} refused={} -- the old grant path is now authoritative \
                     and holds its watermark on any failed placement",
                    apply_classes(),
                    armed(),
                    is_refused()
                );
            }
            Some(er_logic::ownership::suspended_toast(s))
        }
    }
}

/// One-shot guard for the suspension warn (the toast itself re-pushes every tick by design).
static SUSPENSION_LOGGED: AtomicBool = AtomicBool::new(false);

/// One-line summary of the active reconcile mode for the startup log, so a test session's log states
/// exactly what the reconciler is doing rather than leaving it to be inferred: `dry-run`, the owned
/// apply classes (`apply=flags`, `apply=flags,goods,ledger`), or `baseline` (owns nothing).
pub fn mode_desc() -> String {
    if dry_run() {
        return "dry-run (logs plan, applies nothing)".to_string();
    }
    let c = apply_classes();
    let mut on = Vec::new();
    if c.flags {
        on.push("flags");
    }
    if c.goods {
        on.push("goods");
    }
    if c.ledger {
        on.push("ledger");
    }
    if on.is_empty() {
        "baseline (owns no class; old handlers authoritative)".to_string()
    } else {
        format!("apply={}", on.join(","))
    }
}

/// STRANGLER cutover control: which classes the reconciler is allowed to APPLY, read from
/// `RECONCILE_APPLY` (comma list of `flags`,`goods`,`ledger`, or `all`/`none`). The DEFAULT scope
/// when unset/empty is now **`all`** (see [`DEFAULT_APPLY`]): the plain binary builds straight into
/// the FULL cutover — the reconciler owns flags + goods + ledger and the old grant handlers step
/// aside. NARROW at runtime with no rebuild — `RECONCILE_APPLY=flags` or `flags,goods` to keep goods
/// / ledger on the old path, `=none` or `RECONCILE_DRYRUN=1` to fall back to today's baseline / log-
/// only. Ignored under dry-run.
const DEFAULT_APPLY: ApplyClasses = ApplyClasses::ALL;
fn apply_classes() -> ApplyClasses {
    match std::env::var("RECONCILE_APPLY") {
        Err(_) => DEFAULT_APPLY,
        Ok(v) => {
            let v = v.trim();
            if v.is_empty() {
                return DEFAULT_APPLY;
            }
            if v.eq_ignore_ascii_case("all") {
                return ApplyClasses::ALL;
            }
            if v.eq_ignore_ascii_case("none") {
                return ApplyClasses::NONE;
            }
            let mut c = ApplyClasses::NONE;
            for part in v.split(',') {
                match part.trim().to_ascii_lowercase().as_str() {
                    "flags" => c.flags = true,
                    "goods" => c.goods = true,
                    "ledger" => c.ledger = true,
                    "none" => {}
                    other => log::warn!("RECONCILE_APPLY: ignoring unknown class '{other}'"),
                }
            }
            c
        }
    }
}

/// Initialize the driver once, at the first STABLE IN-WORLD tick (NOT at connect: `save_slot` and
/// `play_time` are only readable once a character is loaded, and the reconciler is inert before
/// stability anyway). `persist_path` is the watermark file next to the client dll.
///
/// INTEGRATION: call this from the reconstructed `core.rs` once per session, after the per-seed
/// `DesiredInputs` are built AND the world is loaded (`has_inventory() && in_world()`).
pub fn init(inputs: DesiredInputs, persist_path: std::path::PathBuf, received_through: i64) {
    reset_fresh_character_verdict();
    log::info!("[reconcile] mode: {}", mode_desc());
    let b = paced_budget();
    log::info!(
        "[reconcile] grant pacing: burst={} per {}ms (0ms = unpaced); env RECONCILE_GRANT_BURST / RECONCILE_GRANT_INTERVAL_MS",
        b.goods,
        b.min_grant_interval_ms
    );
    let slot = inputs.save.0.clone();
    let mut store = WatermarkStore::load(persist_path);
    let save_slot = read_save_slot();
    let play_time = read_play_time_ms().unwrap_or(0);

    // PER-CHARACTER ledger seeding (er-startitems-newchar-no-regrant). The watermark is keyed by
    // (AP slot name, ER save_slot) and stamped with play_time; the pure `seed_trust` decides:
    //   * no entry for this character (or a pre-fix legacy entry adopted for it) -> FRESH: re-owe
    //     everything from the ledger floor (its start items AND its received stream). A NEW character
    //     on a slot whose prior character was granted no longer inherits that watermark.
    //   * entry present, live play_time >= the stamp -> RESUME from its watermark (a same-character
    //     reload never re-grants -- the possession dedup in start_backfill is authoritative).
    //   * entry present, play_time REWOUND below the stamp -> FRESH (delete+recreate in the slot, or
    //     a restored pre-grant backup).
    // `received_through` is passed through to `seeded` for the positive-frontier cross-check
    // (er-reconciler-received-grant-regression); a fresh character re-owes it too, which is correct
    // because a slot-keyed `last_received_index` also can't belong to a new character.
    let entry = match save_slot {
        Some(ss) => store.get(&slot, ss).or_else(|| {
            store
                .legacy_take(&slot)
                .map(|wm| legacy_adopt(wm, play_time))
        }),
        None => None, // save slot unreadable (shouldn't happen in-world) -> fresh (safe: re-owe)
    };
    // MINIBAKE: read the save-embedded marker and let its (seed, slot) identity decide, instead of
    // inferring identity from play_time. The marker's GameIo is the SAME LiveGame seam the reconciler
    // uses. A not-ready flag holder reads all-clear -> Absent -> the safe seed_trust migration below.
    let identity = marker::identity_hash(&inputs.seed, &slot);
    let decision = marker::decide(marker::read(&LiveGame::new(), MARKER_BAND), identity);

    // `fresh_character` governs the reconcile.json play_time re-stamp (reset for a new character,
    // monotonic for a resume). On the marker Resume path the marker is authoritative, so it's false.
    let (reconciler, fresh_character) = match decision {
        marker::InitDecision::Refuse { stored, expected } => {
            set_refused(marker::Refusal::WrongSaveAtConnect);
            log::warn!(
                "[reconcile] REFUSED: save marker identity {stored:#010x} != this session {expected:#010x} \
                 -- this save belongs to a different seed/slot. NOT arming the reconciler; check \
                 reporting is gated. Quit to the main menu, then load this room's save or start a \
                 new character -- the menu edge releases this refusal (clear_refusal_if_rearmable)."
            );
            return; // no Driver -> tick() no-ops; is_refused() gates check reporting in core
        }
        // Marker present + matches: resume from the save's OWN cursor. No play_time inference.
        marker::InitDecision::Resume { watermark } => {
            (Reconciler::from_persisted(inputs, watermark), false)
        }
        // No marker yet (pre-minibake save, or a genuinely new character): keep the battle-tested
        // seed_trust migration. The tick commit then writes a marker, so future connects Resume.
        marker::InitDecision::Fresh => {
            let (fresh_character, persisted) = seed_trust(entry, play_time);
            (
                Reconciler::seeded(inputs, persisted, received_through, fresh_character),
                fresh_character,
            )
        }
    };
    FRESH_CHARACTER_VERDICT.store(if fresh_character { 2 } else { 1 }, Ordering::Relaxed);
    // [reattach] ONE-BLOCK STATE DUMP (2026-08-01). Every reattach incident so far has cost three
    // rounds of theory because the log carried the inputs to the decision but not the decision's
    // whole context. This is every fact that governs "what has already been delivered to THIS
    // character", in one grep-able block, so a player's log alone closes the triage.
    log::info!(
        "[reattach] identity={identity:#010x} marker={decision:?} slot={slot:?} save_slot={save_slot:?} \
         play_time={play_time} legacy_entry={entry:?} ap_recv_watermark={received_through} \
         ledger_watermark={} mode={} armed=true refused={} has_inventory={} band={}",
        reconciler.applied_watermark(),
        mode_desc(),
        is_refused(),
        crate::detour::has_inventory(),
        MARKER_BAND.base
    );
    // Re-stamp the ledger NOW with the correctly-read seed-time play_time. The tick-tail persist
    // (below) can run when `read_play_time_ms()` momentarily reads 0, freezing the stamp and
    // silently disabling the save-slot-reuse guard in `seed_trust` (observed 2026-07-20:
    // play_time_ms stuck at 0 across sessions on a multi-minute save). `stamp_playtime` keeps it
    // monotonic for a resuming character and resets it for a fresh one.
    if let Some(ss) = save_slot {
        let stored = entry.as_ref().map(|e| e.play_time_ms);
        store.set(
            &slot,
            ss,
            CharLedger {
                watermark: reconciler.applied_watermark(),
                play_time_ms: stamp_playtime(stored, play_time, fresh_character),
            },
        );
    }
    let driver = Driver {
        reconciler,
        io: LiveGame::new(),
        store,
        slot,
        save_slot: save_slot.unwrap_or(-1),
        identity,
    };
    let _ = DRIVER.set(Mutex::new(driver));
    log::info!(
        "[reconcile] grant-stall guard ARMED: a unique good is parked after {} accepted-but-\
         unobservable grants, and re-armed on every world edge",
        er_logic::reconcile::MAX_GRANT_ATTEMPTS
    );
}

/// SWAP inputs (received prefix grew, or a reconnect). Atomic + seed-change aware inside the pure
/// reconciler (resets the ledger watermark only on a genuine seed change — the reconnect-new-seed
/// fix). `core.rs` calls this every frame with a freshly rebuilt `DesiredInputs`; the pure
/// `Reconciler::set_inputs` equality-guards identical inputs, so the repeat cost is one comparison.
/// The `d.slot` stamp below runs either way — that is why the guard lives in the pure layer.
pub fn set_inputs(inputs: DesiredInputs) {
    if let Some(m) = DRIVER.get()
        && let Ok(mut d) = m.lock()
    {
        d.slot = inputs.save.0.clone(); // same character; save_slot is unchanged
        d.reconciler.set_inputs(inputs);
    }
}

/// TICK — call once per game-thread frame (from the reconstructed `update_live`), UNCONDITIONALLY.
/// It polls: it re-reads the observed game state whether or not anything changed on our side,
/// because the game's own divergences emit no events. A refused grant that goes blind, a flag
/// vanilla EMEVD re-clears, an accessor that switches to the multiplay key list, a save-scum
/// rollback — none of them announce themselves, so convergence is never a reason to stop looking.
/// The reconciler itself gates every read/write on world stability, so this is safe to call during
/// load screens (it simply no-ops), and `refresh_dwell` below is what keeps that gate honest across
/// one.
///
/// 🛑 Do NOT reintroduce a dirty/convergence gate here. There used to be one (a `static DIRTY` that
/// `set_inputs` re-stored `true` every frame, so it never once observed `false`). It was deleted in
/// issue #237 along with its twin in `er_logic`, because a live one would break four already-healed
/// bug classes — the acceptance test is
/// `er_logic::reconcile::tests::observed_only_divergence_heals_with_no_nudge_replay`, and a
/// convergence gate reds it plus `stall_guard_leaves_the_save_scum_self_heal_intact_replay`.
///
/// INTEGRATION: replace the scattered `drain_start_items` / `flush_grace_flags` /
/// `open_on_received_name` / great-rune restore / map-reveal calls in `update_live` with this ONE
/// call, per the strangler phases in `docs/history/MIGRATION.md` (archived; cutover complete).
pub fn tick() {
    // A REFUSED session must not apply, and after `disarm_if_identity_moved` a DRIVER can exist
    // while refused — so `DRIVER.get()` alone is no longer the whole gate. Without this line the
    // disarm is cosmetic: the old room's reconciler keeps granting and reporting.
    if is_refused() {
        return;
    }
    let Some(m) = DRIVER.get() else { return };
    let Ok(mut d) = m.lock() else { return };

    d.io.refresh_dwell();
    // PACED budget (env-tunable): drains a large delta a burst at a time instead of flooding
    // AddItemFunc in one frame — the mass-grant CTD guard.
    let budget = paced_budget();

    if dry_run() {
        // PHASE 0: READ-ONLY. `dry_run_actions` snapshots the live game via our `GameIo` and diffs
        // against desired WITHOUT applying anything (no flag write, no grant, no watermark advance),
        // so we can validate the exact per-action plan against today's live behavior before flipping
        // any mutation path. Nothing here mutates the game or the store.
        let stab = d.io.stability();
        let planned = d.reconciler.dry_run_actions(&d.io);
        log::info!(
            "[reconcile dryrun] stable={} desired(flags={} unique_goods={} ledger={}) would-apply {} action(s): {:?}",
            stab.stable(),
            d.reconciler.desired().flags.len(),
            d.reconciler.desired().unique_goods.len(),
            d.reconciler.desired().ledgered.len(),
            planned.len(),
            planned,
        );
        // READ-ONLY: return before the apply path. The poll keeps logging every frame so the
        // operator sees the live plan, including one that only changed because the GAME moved.
        return;
    }

    let classes = apply_classes();
    // Reborrow the MutexGuard once to a plain &mut State so `reconciler` and `io`
    // split-borrow as disjoint fields (field access through DerefMut cannot).
    let d = &mut *d;
    let out = d.reconciler.tick_with_classes(&mut d.io, budget, classes);

    // MINIBAKE: commit the (seed, slot) identity + watermark INTO the save's marker band. Idempotent
    // (a no-op when the active cursor already equals the watermark), so this every-tick call is cheap;
    // it writes the marker on a fresh save's first stable tick and keeps the cursor current after. The
    // commit is double-buffered + present-last, so a crash mid-write can't corrupt it.
    let wm = d.reconciler.applied_watermark();
    marker::commit(&mut d.io, MARKER_BAND, d.identity, wm);

    // Persist the (possibly advanced) ledger watermark for THIS CHARACTER, re-stamped with the live
    // play_time. Skip if the save slot was unreadable at init or play_time isn't readable now: a
    // 0/garbage stamp under a bad key could let a later character wrongly trust it. Idempotent —
    // the next stable tick persists again. (`wm` computed above for the marker commit.)
    if d.save_slot >= 0
        && let Some(live) = read_play_time_ms()
    {
        let slot = d.slot.clone();
        let save_slot = d.save_slot;
        // MONOTONIC: never let a transient low/0 read regress a known-good stamp (see init above).
        let stored = d.store.get(&slot, save_slot).map(|e| e.play_time_ms);
        d.store.set(
            &slot,
            save_slot,
            CharLedger {
                watermark: wm,
                play_time_ms: stamp_playtime(stored, live, false),
            },
        );
    }

    // GRANT STALL — the ONE place a permanently-refused unique good gets announced. Fires once per
    // good per arming window (the pure reconciler owns that de-duplication), never per frame.
    //
    // This is the telemetry the 2026-07-30 softlock had none of: pre-fix, a refused grant re-fired
    // ~6x/second and the ONLY log line was `applied 1 action(s) (converged=true)` — reporting
    // success while the item hit the floor. Every number below is read from the game's own
    // inventory bookkeeping rather than inferred, so the next report NAMES the cause instead of
    // leaving us to theorise about it (three rounds of plausible theories is the house failure mode).
    for g in &out.newly_stalled {
        log::warn!(
            "[reconcile] INERT: goods {g:#x} accepted {} grant(s) and was never observable -- \
             no longer re-granting until the next load. {} | {}",
            er_logic::reconcile::MAX_GRANT_ATTEMPTS,
            inventory_forensics(*g),
            crate::detour::add_item_return_for(*g)
        );
    }
    for f in &out.newly_stalled_flags {
        log::warn!(
            "[reconcile] INERT: flag {f} was written {} time(s) and never read back at the \
             written value -- no longer re-asserting until the next load. Either the id has no \
             flag-block descriptor on this build (CSEventFlagMan::set_flag silently discards \
             unknown ids) or something un-sets it within the same tick. A flag vanilla merely \
             CONTESTS (clears a frame later) reads back fine and is never parked.",
            er_logic::reconcile::MAX_FLAG_ATTEMPTS,
        );
    }

    if !out.applied.is_empty() {
        log::info!(
            "[reconcile] applied {} action(s) this tick (converged={})",
            out.applied.len(),
            out.converged
        );
    }
}

// ---------------------------------------------------------------------------------------------
// INTEGRATION (wired into core.rs::update_live). The wiring is THREE calls, all from the game
// thread, and there are no event handlers:
//
//   // 1. once, on the first stable in-world tick:
//   reconcile_io::init(build_desired_inputs(&slot_data, &received), path, received_through);
//
//   // 2. every frame: rebuild the CUMULATIVE desired inputs and swap them in. Identical inputs
//   //    are equality-guarded inside the pure reconciler, so the steady-state cost is a compare.
//   reconcile_io::set_inputs(build_desired_inputs(&slot_data, &received));
//
//   // 3. every frame: poll. Not "when something changed" — every frame. See `tick`.
//   reconcile_io::tick();
//
// There is deliberately NO fourth "nudge from the connect / load / net-loop handler" step. An
// earlier version of this comment described one and no such handler was ever written; the polling
// that replaced it is the design, not a stopgap (issue #237). The desired side is a per-frame
// rebuild plus an equality-guarded swap; the observed side is a per-frame poll.
//
//   // `build_desired_inputs` maps each archipelago_rs received item -> ReceivedItem with the
//   //    right ItemSemantics (RegionFlags / MapReveal / KeyItem / GreatRune / Consumable / GoalFlag),
//   //    reusing the tables the old feature modules already carry (region.rs open flags,
//   //    startgrants.rs MAP_REVEAL_FLAGS_BASE + 82001, keyitems.rs 4000xx obtained flags, the
//   //    great-rune restore goods, the flask/rune/stone FullIDs).
//
// The old per-feature idempotency bools (start_items_granted, notify_granted, session grace sets,
// region bloom latch, great-rune restore set) are then DELETED one class at a time — see
// docs/history/MIGRATION.md (archived; cutover complete).
// ---------------------------------------------------------------------------------------------
