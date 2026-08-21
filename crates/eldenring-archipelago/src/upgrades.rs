//! AUTO_UPGRADE + GLOBAL_SCADUTREE_BLESSING — RE holes filled via typed `eldenring` 0.14 bindings.
//!
//! Two slot_data features whose game-memory touchpoints were originally RE-stubbed. The RE is now
//! resolved entirely against the `eldenring` 0.14 typed bindings (NO raw CE offsets are used — every
//! field that the C++ client reached with a hand-walked offset chain is a NAMED field on a typed
//! struct here). Every game read/write is read-then-act, raise-only, bounds-checked, and gated on
//! `crate::flags::in_world()` so a mis-resolution degrades to a no-op rather than corrupting a save.
//!
//! C++ source of truth (the exact behaviour these port):
//!   - er_gamehook_win.cpp `AutoUpgradeWeaponIdImpl` (~666) / `RefreshAutoUpgradeTargets` (~609) /
//!     `WeaponInfo` (~542) / `CapForRT` (~530) (auto_upgrade), and `TickGlobalScaduBlessing` (~720) /
//!     `SetGlobalScaduBlessing` (~715) / `kScaduCum` (~706) (scadu).
//!   - ArchipelagoInterface.cpp ~92-100: slot_data `options.auto_upgrade` (int) and
//!     `options.global_scadutree_blessing` (int) drive `SetAutoUpgrade(int)` / `SetGlobalScaduBlessing(int)`.
//!
//! TYPED-BINDING MAP (what replaced each C++ raw-offset hole; see RE-WORKSHEET-autoupgrade-scadu.md):
//!   - GameDataMan singleton .............. `GameDataMan::instance()` / `::instance_mut()` (FromStatic).
//!   - PlayerGameData ..................... `gdm.main_player_game_data` (OwnedPtr, `.as_ref()?`/`.as_mut()?`).
//!                                          (Replaces C++ `*(GameDataMan + 0x08)`.)
//!   - stored Scadutree blessing .......... `pgd.scadutree_blessing: u8` NAMED field.
//!                                          (Replaces the raw `PlayerGameData + 0xFC` signed byte.)
//!   - held-item inventory iterator ....... `pgd.equipment.equip_inventory_data.items_data.items()`
//!                                          -> `&EquipInventoryDataListEntry` (skips empty slots).
//!                                          (Replaces the C++ EquipInventoryData container shape-scan.)
//!   - per-entry item id / category ....... `entry.item_id.param_id() -> u32` + `.category() -> ItemCategory`.
//!   - per-entry quantity ................. `entry.quantity: u32`.
//!   - weapon -> reinforceTypeId .......... `repo.get::<EquipParamWeapon>(base).reinforce_type_id() -> i16`.
//!                                          (Replaces the C++ self-calibrated s16 offset.)
//!   - reinforce cap ...................... count consecutive `repo.get::<ReinforceParamWeapon>(rt+k)`.
//!
//! WIRING (already done elsewhere — do not edit those files):
//!   - net.rs slot_data parse calls `set_auto_upgrade(..)` / `set_global_scadu_blessing(..)` at connect.
//!   - detour.rs `grant_full_id` calls `apply_auto_upgrade(full_id) -> i32` on every granted item.
//!   - mod.rs `tick()` calls `tick_global_scadu()` in the in-world `#[cfg(feature = "net")]` neighbourhood.

// doc_* allows: the module/item docs use intentional column-aligned reference tables, not markdown lists.
#![allow(
    dead_code,
    clippy::doc_lazy_continuation,
    clippy::doc_overindented_list_items
)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use eldenring::cs::{
    EquipParamWeapon, GameDataMan, ItemCategory, ReinforceParamWeapon, SoloParamRepository,
};
use fromsoftware_shared::FromStatic;

// ---- config (set from slot_data by net.rs; see module doc + worksheet) --------------------------

/// auto_upgrade mode/level from slot_data. The C++ treats it as a simple on/off int
/// (`g_autoUpgrade = on ? 1 : 0`); we keep the raw int so a future "cap at +N" variant has room.
/// 0 = off (default). Non-zero = on.
static AUTO_UPGRADE: AtomicI32 = AtomicI32::new(0);

/// global_scadutree_blessing mode from slot_data: 0 = off, 1 = player_only, 2 = scaled.
/// (Matches the C++ `g_globalScaduBlessing` tri-state.)
static GLOBAL_SCADU: AtomicI32 = AtomicI32::new(0);

/// mode 2 (scaled) DLC Scadutree-blessing FLOOR wire: `(lo, hi, floor)` in play_region/100 sub-id
/// space (`dlcScadutreeFloorRanges`). Set at connect; read each scadu tick to floor the player's
/// blessing to the DLC area they're standing in. Empty = no DLC / mode != 2 -> mode 2 == mode 1.
static DLC_SCADU_FLOORS: Mutex<Vec<(i32, i32, i32)>> = Mutex::new(Vec::new());

/// core.rs (connect): `set_dlc_blessing_floors(er_logic::scaling::parse_triple_ranges(sd.get("dlcScadutreeFloorRanges")))`.
pub fn set_dlc_blessing_floors(ranges: Vec<(i32, i32, i32)>) {
    let n = ranges.len();
    if let Ok(mut g) = DLC_SCADU_FLOORS.lock() {
        *g = ranges;
    }
    log::info!("global_scadu_blessing: DLC blessing-floor buckets = {n}");
}

/// The DLC blessing FLOOR level for the player's current play_region (mode 2). 0 when outside a DLC
/// bucket / no wire. Reuses the pure `er_logic::scaling::blessing_floor_for_region` (same bucket space
/// as the enemy scaler: `play_region_id / 100`).
fn dlc_blessing_floor_here() -> i32 {
    let Some(pr) = crate::flags::play_region_id() else {
        return 0;
    };
    let bucket = pr / 100;
    match DLC_SCADU_FLOORS.lock() {
        Ok(g) => er_logic::scaling::blessing_floor_for_region(&g, bucket).unwrap_or(0),
        Err(_) => 0,
    }
}

/// net.rs: `set_auto_upgrade(sd.pointer("/options/auto_upgrade").and_then(|v| v.as_i64()).unwrap_or(0) as i32)`.
pub fn set_auto_upgrade(level_or_flag: i32) {
    AUTO_UPGRADE.store(level_or_flag, Ordering::Relaxed);
    log::info!(
        "auto_upgrade: {}",
        if level_or_flag != 0 { "ENABLED" } else { "off" }
    );
}

/// net.rs: `set_global_scadu_blessing(sd.pointer("/options/global_scadutree_blessing").and_then(|v| v.as_i64()).unwrap_or(0) as i32)`.
pub fn set_global_scadu_blessing(mode: i32) {
    // 0 off | 1 anywhere | 2 anywhere + DLC catch-up | 3 dlc_only + DLC catch-up.
    // 🛑 AN UNRECOGNISED MODE CLAMPS TO OFF, deliberately: it is how a seed from a NEWER apworld
    // degrades instead of guessing. It is also exactly why a mode-3 seed declares
    // requiresClientFeatures ["dlc_blessing_catchup"] -- without the tag this clamp is a silent
    // "the setting you chose did nothing" instead of a refusal to connect.
    let m = if (1..=3).contains(&mode) { mode } else { 0 };
    GLOBAL_SCADU.store(m, Ordering::Relaxed);
    log::info!(
        "global_scadu_blessing: {}",
        if m != 0 { "ENABLED" } else { "off" }
    );
}

fn auto_upgrade_on() -> bool {
    AUTO_UPGRADE.load(Ordering::Relaxed) != 0
}
/// Scadutree Fragments DELIVERED BY THE MULTIWORLD, accumulated by core.rs over the whole received
/// stream (the same single pass that counts "Progressive Flask Upgrade"). Replaces a per-tick bag
/// walk -- see `er_logic::upgrades::fragments_from_received` for why the bag was the wrong source.
static RECEIVED_FRAGMENTS: AtomicI32 = AtomicI32::new(0);

/// core.rs (every received-stream pass): total fragment UNITS received this session.
pub fn set_received_fragments(units: i32) {
    RECEIVED_FRAGMENTS.store(units.max(0), Ordering::Relaxed);
}

fn received_fragments() -> i32 {
    RECEIVED_FRAGMENTS.load(Ordering::Relaxed)
}

fn scadu_mode() -> i32 {
    GLOBAL_SCADU.load(Ordering::Relaxed)
}

/// Is the DLC catch-up arm live? Mode 3 (`dlc_only` scope + catch-up) is the only value that needs
/// the `dlc_blessing_catchup` client-feature tag: 0/1/2 mean what they always meant, and an
/// unrecognised mode clamps to 0 in `set_global_scadu_blessing` -- which is precisely the silent
/// "your setting did nothing" the handshake exists to make loud. See `feature_handshake`.
pub fn dlc_blessing_catchup_armed() -> bool {
    scadu_mode() == 3
}

// ================================================================================================
// auto_upgrade
// ================================================================================================
//
// GOAL: when a REAL weapon is acquired, bump it to the player's current highest reinforce level on
// the same smithing track (normal vs somber) before the game adds it.
//
// C++ (er_gamehook_win.cpp `AutoUpgradeWeaponIdImpl`, ~666):
//   id math: base = id - (id % 100); level = id % 100. ER bakes +N into the id (id = base + N).
//   track/cap: EquipParamWeapon row(base).reinforceTypeId -> ReinforceParamWeapon; cap = number of
//     consecutive ReinforceParamWeapon rows from reinforceTypeId; cap>10 => normal (max +25),
//     cap in 1..=10 => somber (max +10).
//   target: highest +N currently HELD on that track (walk inventory), clamped to the weapon's cap;
//     only ever RAISES (returns input if already at/above target).
//
// Worksheet §A.

const REINFORCE_STEP: i32 = 100; // ER id stride per smithing level (base = id - id%100)
const NORMAL_CAP: i32 = 25; // normal smithing track tops out at +25 (reinforce-run scan bound)
// Somber cap + track classification live in er_logic::upgrades::classify_track (host-tested).

/// Cached "highest +N held" per track, refreshed on a throttle (C++ used 1500ms + a cached
/// container offset; the typed iterator removes the container scan, so we just cache the targets
/// to avoid re-walking the bag on every back-to-back grant in a reconnect replay burst).
struct UpgradeTargets {
    normal: i32,
    somber: i32,
    last_refresh: Option<Instant>,
}
static UPGRADE_TARGETS: Mutex<UpgradeTargets> = Mutex::new(UpgradeTargets {
    normal: 0,
    somber: 0,
    last_refresh: None,
});
const REFRESH_THROTTLE: Duration = Duration::from_millis(1500);

/// Given a granted weapon FullID (real item id | category nibble), return the upgraded FullID, or
/// the input unchanged if it is not an upgradeable weapon / auto_upgrade is off / it can't be
/// resolved safely. Mirrors the C++ `AutoUpgradeWeaponIdImpl`.
///
/// CALL SITE: detour.rs `grant_full_id` (every AP-granted item funnels through it). Returns the
/// input unchanged for non-weapons and whenever any read can't be done safely (raise-only by
/// construction: never lowers an already-higher granted weapon).
pub fn apply_auto_upgrade(full_id: i32) -> i32 {
    // Delegate to the host-tested decision (er_logic::upgrades::apply_auto_upgrade -- unit
    // tests + the upgrades_replay reconnect-burst tier) so the client shares ONE copy instead
    // of a drifting inline twin. The live reads still flow through EldenRingHook's
    // weapon_track_and_cap / highest_held_level (which keep the 1500ms UPGRADE_TARGETS
    // throttle); off / off-world / non-weapon / unresolvable / raise-only / cap-clamp all live
    // in the shared fn. "A fix is a predicate production must call" (CONTRIBUTING).
    let up = er_logic::upgrades::apply_auto_upgrade(
        &crate::hook_impl::EldenRingHook,
        auto_upgrade_on(),
        full_id,
    );
    if up != full_id {
        log::info!("auto_upgrade: {:#x} -> {:#x}", full_id, up);
    }
    up
}

/// The FullID auto_equip must QUEUE for a received item. Live-hook wrapper over the host-tested
/// enqueue decision (`er_logic::auto_equip::enqueue_id`), which delegates to the SAME
/// `apply_auto_upgrade` predicate the grant runs -- so the #296/#302/#303 invariant ("whatever
/// the queue holds must equal what the grant puts in the bag") is ONE call in er-logic, where
/// `upgrades_replay::auto_equip_queue_matches_bag` exercises the function production calls.
/// Before this wrapper existed the upgrade was applied inline in `auto_equip::enqueue`, a crate
/// with no test targets: deleting that line kept the whole workspace green while the bug
/// returned (2026-08-04 inert-test audit, F1).
///
/// CALL SITE: auto_equip.rs `enqueue` (the only enqueue path).
pub fn enqueue_upgrade_id(full_id: i32) -> i32 {
    let up = er_logic::auto_equip::enqueue_id(
        &crate::hook_impl::EldenRingHook,
        auto_upgrade_on(),
        full_id,
    );
    if up != full_id {
        log::info!("auto_upgrade: {:#x} -> {:#x} (enqueue)", full_id, up);
    }
    up
}

/// ER category nibble mask / weapon-category constant (er_codec mirror; weapons are category 0x0).
const ROW_ID_MASK: u32 = er_codec::ROW_ID_MASK;

// (Historical, kept as a note.) `decode_weapon_id` was a pure id-math decode: (base_row_id, level) for
// an upgradeable WEAPON FullID, else None. base = row - row%100; level = row%100. Category guard mirrored
// C++ `WeaponInfo`: `(uint32(itemId) & CATEGORY_MASK) != CATEGORY_WEAPON` rejects non-weapons; row range
// `[1_000_000, 90_000_000)` skips system/NPC ids. Weapons are er_codec::CATEGORY_WEAPON (0x0).
//
// It was a byte-for-byte copy of er_logic::upgrades::decode_weapon_id. Production never
// called it -- apply_auto_upgrade delegates to er-logic, which uses ITS copy -- so this one existed only
// to be unit-tested. A tested copy that production does not call is the exact test/prod drift the replay
// tier exists to kill (CONTRIBUTING: "a green predicate with no production caller is a spec, not a fix").
// Deleted; the tests below now exercise the one that actually runs.

/// RE-A2 RESOLVED (typed binding): resolve a weapon base id -> (reinforce cap, is_somber).
/// Cap = length of the `ReinforceParamWeapon` run from `reinforce_type_id() -> i16`; TRACK =
/// `er_logic::upgrades::classify_track` on `EquipParamWeapon.materialSetId` (2200 = somber). None
/// if the repo isn't up, the row is absent, or the run is empty. Mirrors C++ `CapForRT`/`WeaponInfo`
/// but TRACK-CORRECT: the old `cap <= 10` run-length heuristic mis-tracked the ~4 vanilla rows whose
/// somber material rides a full 26-row run (e.g. Occult Carian Knight's Shield), leaking a +10 somber
/// into the normal high-water mark. See classify_track (host-tested in er-logic).
pub(crate) fn weapon_track_and_cap(base: i32) -> Option<(i32, bool)> {
    // SAFETY: FD4 singleton; on the game thread, gated in-world by the caller. Err until built.
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    // #351: either holder mid-restream -> upstream get panics. Defer the whole classify instead
    // (None = "can't resolve", and the caller leaves the weapon unchanged rather than guessing);
    // the up-front gate keeps an empty ReinforceParamWeapon from logging once per rung below.
    if !crate::param_guard::is_available::<EquipParamWeapon>(repo, "weapon_track_and_cap")
        || !crate::param_guard::is_available::<ReinforceParamWeapon>(repo, "weapon_track_and_cap")
    {
        return None;
    }
    let weapon =
        crate::param_guard::get::<EquipParamWeapon>(repo, base as u32, "weapon_track_and_cap")?;
    let rt = weapon.reinforce_type_id() as i32;
    // Reinforce-run length = the CAP. C++: `while (k<=25 && row(rt+k)) ++k; cap = k-1`. rt can be
    // negative for non-upgradeable junk; get() just returns None then.
    let mut k = 0;
    while k <= NORMAL_CAP
        && crate::param_guard::get::<ReinforceParamWeapon>(
            repo,
            (rt + k) as u32,
            "weapon_track_and_cap",
        )
        .is_some()
    {
        k += 1;
    }
    // TRACK from materialSetId, cap from the run above -- host-tested predicate (owns the somber
    // clamp + the not-upgradeable guard).
    er_logic::upgrades::classify_track(k - 1, weapon.material_set_id())
}

/// RE-A3 RESOLVED (typed binding): highest +N currently held on the given smithing track
/// (true = somber). Walks `GameDataMan -> main_player_game_data -> equipment.equip_inventory_data
/// .items_data.items()` (the typed iterator skips empty slots). Caches the per-track maxima behind a
/// 1500ms throttle so a reconnect replay burst doesn't re-walk the bag per grant (mirrors C++
/// `RefreshAutoUpgradeTargets`). Returns None only if the bag can't be resolved AND nothing is
/// cached yet (so the caller leaves the weapon unchanged rather than guessing).
pub(crate) fn highest_held_level(somber: bool) -> Option<i32> {
    let mut targets = UPGRADE_TARGETS.lock().ok()?;
    let stale = match targets.last_refresh {
        Some(t) => t.elapsed() >= REFRESH_THROTTLE,
        None => true,
    };
    if stale {
        if let Some((normal, somber_max)) = walk_inventory_targets() {
            targets.normal = normal;
            targets.somber = somber_max;
            targets.last_refresh = Some(Instant::now());
        } else if targets.last_refresh.is_none() {
            // Never resolved the bag and nothing cached -> can't supply a target yet.
            return None;
        }
        // else: walk failed transiently but we have a cached value -> use it (no down-flicker).
    }
    Some(if somber {
        targets.somber
    } else {
        targets.normal
    })
}

/// WHICH level of weapon base row `base` does the bag hold -- as the FullID the equip queue needs?
///
/// `None` = the bag was not reachable this tick. The caller must treat that as "don't know", NEVER
/// as "no": this is the idempotency latch for `er_logic::boss_grants`, and reading an unresolvable
/// bag as empty duplicates a unique weapon on every tick until it resolves. `Some(None)` =
/// readable, and the player owns no level of `base`.
///
/// THE ONE WAY TO ASK, ON PURPOSE (#413, boblerrr 2026-08-07 18:31:38). This replaces
/// `holds_weapon_base`, which answered the same question with a DIFFERENT tolerance: it matched
/// ANY reinforce level of the base row and said "yes, he has the spear", while the equip queue was
/// separately handed `base + today's auto_upgrade target` and looked THAT up by exact FullID --
/// which the bag did not have, because the spear had been banked at `+0`. Two reads with different
/// tolerances contradicted each other for a whole session; one read that returns the id cannot.
/// A caller wanting the old boolean derives it (`.map(|r| r.is_some())`) from THIS answer.
///
/// Same typed walk as `walk_inventory_targets`, minus the caching -- it runs only while the player
/// stands in one arena, so a fresh read every tick beats a stale answer. The choice among held
/// levels is `er_logic::auto_equip::held_row_to_equip`, not inlined here: this crate has no test
/// targets, and an inline decision here is exactly the shape the 2026-08-04 inert-test audit (F1)
/// found could be deleted with the workspace still green.
pub(crate) fn held_weapon_row(base: i32) -> Option<Option<i32>> {
    // SAFETY: FD4 singleton (read-only walk). Err/None before the player is placed.
    let gdm = unsafe { GameDataMan::instance() }.ok()?;
    let pgd = gdm.main_player_game_data.as_ref();
    let owned = pgd
        .equipment
        .equip_inventory_data
        .items_data
        .items()
        .filter(|e| e.item_id.category() == ItemCategory::Weapon)
        // into_inner() keeps the category nibble: the FullID the equip queue and its bag lookup
        // both speak. For weapons that nibble is 0, so it reads the same as the row.
        .map(|e| e.item_id.into_inner() as i32);
    Some(er_logic::auto_equip::held_row_to_equip(base, owned))
}

/// One full typed inventory walk: returns (highest normal +N, highest somber +N) across all held
/// weapons, or None if the bag isn't reachable this tick. Pure read; no writes. Each weapon entry's
/// `param_id()` is the resolved row (base + level); we re-classify its track via the same param cap
/// rule so a weapon's level is only counted toward the track it actually belongs to.
fn walk_inventory_targets() -> Option<(i32, i32)> {
    // SAFETY: FD4 singleton (read-only walk). Err/None before the player is placed.
    let gdm = unsafe { GameDataMan::instance() }.ok()?;
    let pgd = gdm.main_player_game_data.as_ref();
    let mut normal = 0i32;
    let mut somber = 0i32;
    for entry in pgd.equipment.equip_inventory_data.items_data.items() {
        if entry.item_id.category() != ItemCategory::Weapon {
            continue;
        }
        // param_id() strips the category nibble -> the resolved weapon row (base + level).
        let row = entry.item_id.param_id() as i32;
        if !(1_000_000..90_000_000).contains(&row) {
            continue;
        }
        let level = row % REINFORCE_STEP;
        let base = row - level;
        // Classify the held weapon by its OWN track (a somber weapon can never reach +25, so its
        // level must not raise the normal target, and vice-versa). Mirrors C++ WeaponInfo per entry.
        match weapon_track_and_cap(base) {
            Some((_, true)) => somber = somber.max(level),
            Some((_, false)) => normal = normal.max(level),
            None => {}
        }
    }
    Some((normal, somber))
}

// ================================================================================================
// global_scadutree_blessing
// ================================================================================================
//
// GOAL: count held Scadutree Fragments, map to a blessing level via the vanilla cost curve, and
// write the stored combat-blessing field on PlayerGameData so the base game applies the buff.
//
// C++ (er_gamehook_win.cpp `TickGlobalScaduBlessing`, ~720):
//   fragments: ONE inventory stack, goods id 2010000 (FullID = 2010000 | CATEGORY_GOODS); qty = total.
//   curve: kScaduCum[0..20] cumulative-fragments-to-reach-level table -> level 0..20.
//   write: PlayerGameData + 0xFC, signed byte (the stored combat blessing). Only ever RAISE. Engine
//     recomputes the speffect from this byte on next map load / grace rest. Throttled to once/second.
//   The raw `PlayerGameData + 0xFC` offset is replaced here by the NAMED typed field
//   `PlayerGameData::scadutree_blessing: u8` (eldenring 0.14) — same datum, version-robust.
//
// Worksheet §B.

// The Scadutree Fragment goods row moved to `er_logic::upgrades::SCADU_FRAGMENT_GOODS` when the
// blessing switched to the received stream: the id is now used by the pure matcher that reads
// apIdsToItemIds, and a second copy here would be a constant to drift.

/// Maximum stored blessing level the game's combat-blessing curve defines (caps the raise-only write).
///
/// The cumulative fragments-per-level table this used to sit beside (C++ `kScaduCum`) now lives in
/// `er_logic::upgrades::SCADU_CUM` with the decision that consumes it; only the cap stayed here.
/// Confirmed 2026-07-29 against `SpEffectParam`: the vanilla ladder is `20000100..=20000120`, i.e.
/// levels 0..=20 at stride 1, so 20 is the real ceiling and not a chosen clamp.
const SCADU_MAX_LEVEL: i32 = 20;

/// Scadu writer throttle (~1s, mirrors C++ `s_lastTick`). A stored-byte watchdog doesn't need to run
/// every frame; this also keeps the bag walk cheap.
static SCADU_LAST_TICK: Mutex<Option<Instant>> = Mutex::new(None);
const SCADU_THROTTLE: Duration = Duration::from_millis(1000);

/// Per-tick stored-blessing writer. Call from the in-world feature tick (mod.rs `tick()`).
/// Self-gates: returns immediately when the mode is off or out-of-world. Throttled to ~1s. Reads the
/// held Scadutree Fragment count, maps it to a level, and raises `PlayerGameData::scadutree_blessing`
/// (never lowers a real/higher DLC value). mode 1 (player_only) and mode 2 (scaled) behave the same
/// here — exactly as the C++ `TickGlobalScaduBlessing` does (it gates on `g_globalScaduBlessing`
/// being non-zero and applies the player byte for both; the "scaled" variant is a future apworld
/// concern with no extra client write, so mode 2 is a documented no-extra-op alias of mode 1).
/// Returns the Scadutree-blessing toast when the effective level changed, for the caller's deck.
/// `None` on every gate, throttle and transient failure -- an off seed still makes zero new game
/// accesses and says nothing.
#[must_use]
pub fn tick_global_scadu() -> Option<String> {
    if scadu_mode() == 0 {
        return None;
    }
    // GATE: only touch game memory in-world (params + inventory loaded). Same gate the param probe
    // uses (mod.rs `tick()` gates `spike_log_goods_rowcount` on `flags::in_world()`).
    if !crate::flags::in_world() {
        return None;
    }
    // Throttle (~1s). Cheap watchdog cadence; keeps the bag walk off the hot path.
    {
        let mut last = match SCADU_LAST_TICK.lock() {
            Ok(g) => g,
            Err(_) => return None,
        };
        if let Some(t) = *last
            && t.elapsed() < SCADU_THROTTLE
        {
            return None;
        }
        *last = Some(Instant::now());
    }

    // Fragments DELIVERED by the multiworld, not fragments currently in the bag.
    //
    // 🛑 THE BUG THIS FIXES (2026-07-31). This used to walk the inventory and sum held Scadutree
    // Fragments. Revering at a DLC grace CONSUMES them, so a player who used the blessing the way
    // the game intends dropped to a held count of 0, the derived level collapsed to 0, and the
    // game-wide blessing switched itself off. `drive()` has no raise-only clamp -- it applies
    // whatever level it is handed -- so the clone rung genuinely fell.
    //
    // The received stream cannot do that: AP replays the whole set on connect, so the count is
    // stable across reconnect, save-load and anything the game does to the bag. It also costs no
    // per-tick inventory walk, which matters while the boss-sweep CTD is open.
    let frag_qty = received_fragments();

    // THE DECISION lives in er_logic::upgrades::blessing_target (host-tested; see
    // er-logic/src/scadu_blessing_replay.rs -- the floor over a TIMELINE: enter a DLC region with no
    // fragments, leave it again, transient bag miss, reconnect, a real higher blessing). Do NOT
    // re-implement it here: an inline copy is the drift this tier exists to kill.
    //   mode 1 (anywhere)             = level from RECEIVED fragments.
    //   mode 2 (anywhere + catch-up)  = max(fragments, this region's DLC floor) -- so a DLC region
    //                                   entered with no fragments still meets its enemies'
    //                                   assumption. Composing as MAX means collected fragments
    //                                   still count above the floor.
    //   mode 3 (dlc_only + catch-up)  = the floor ALONE. The game keeps its own fragment ladder;
    //                                   we only refuse to let a DLC area run under its expectation.
    let Some(level) =
        er_logic::upgrades::blessing_target(scadu_mode(), frag_qty, dlc_blessing_floor_here())
    else {
        return None; // mode off -> never touch the byte
    };

    // LEVER D — the game-wide half. The stored byte below is DLC-only (the engine declines to apply
    // its rung outside the Land of Shadow, measured in-game 2026-07-29), so on its own this feature
    // has never touched base-game balance. `scadu_blessing::drive` clones the rung onto a row of our
    // own and applies that, which works everywhere. It is driven from here, and not from its own
    // tick, so that an `off` seed makes zero new game accesses: the mode gate, the `in_world()`
    // gate, the throttle and the bag walk above are all shared.
    // Modes 1 and 2 ONLY. Mode 3 is vanilla SCOPE: the player asked for the DLC catch-up WITHOUT
    // the Limgrave power curve, so the clone row stays untouched and the stored byte below does all
    // of the work -- which is exactly enough, because the byte is honoured inside the Land of Shadow
    // and the floor only ever applies there anyway. Driving the clone here would hand mode 3 the
    // one thing it exists to let people decline.
    let toast = if er_logic::upgrades::applies_globally(scadu_mode()) {
        crate::scadu_blessing::drive(level)
    } else {
        None
    };

    // Read the current stored blessing, then ONLY raise it (never stomp a higher real DLC revere,
    // never down-flicker). The read+write share one mutable PlayerGameData borrow inside
    // `raise_stored_blessing` so the value can't change between the compare and the store.
    match raise_stored_blessing(level) {
        Some(Some((was, now))) => {
            log::info!(
                "global_scadu_blessing: frags={} -> blessing level {} (PlayerGameData.scadutree_blessing, was {})",
                frag_qty,
                now,
                was
            );
        }
        Some(None) => {} // already >= target; nothing written
        None => {}       // PlayerGameData unreachable this tick; retry next throttle window
    }

    toast
}

// `held_scadu_fragments()` lived here and walked the bag for goods 2010000. REMOVED 2026-07-31: the
// blessing is driven by fragments RECEIVED, not held, because the game consumes held ones when you
// revere. The typed inventory walk it used is still demonstrated in `walk_inventory_targets` below.

/// RE-B2 / RE-B3 RESOLVED (typed binding): read the current stored combat-blessing level and RAISE
/// it to `level` if higher. Returns:
///   - `Some(Some((was, now)))` — wrote a new (raised) value,
///   - `Some(None)`             — current was already >= target; left untouched,
///   - `None`                   — PlayerGameData not reachable this tick.
/// Uses `PlayerGameData::scadutree_blessing: u8` (named field; replaces the raw `PGD + 0xFC` byte).
/// Read + write share one `instance_mut()` borrow so nothing can change between compare and store.
fn raise_stored_blessing(level: i32) -> Option<Option<(i32, i32)>> {
    // Clamp the computed target into the valid stored range before any write.
    let mut target = level;
    target = target.clamp(0, SCADU_MAX_LEVEL);
    // SAFETY: FD4 singleton accessed MUTABLY (we may write the byte); same idiom as
    // deathlink.rs `WorldChrMan::instance_mut()`. Err/None before the player is placed -> no-op.
    let gdm = unsafe { GameDataMan::instance_mut() }.ok()?;
    let pgd = gdm.main_player_game_data.as_mut();
    let cur = pgd.scadutree_blessing as i32;
    if cur >= target {
        return Some(None); // already >= target; never lower
    }
    pgd.scadutree_blessing = target as u8;
    Some(Some((cur, target)))
}

/// GameHook seam (hook_impl.rs `EldenRingHook::scadutree_blessing`): read the stored
/// combat-blessing byte. None = PlayerGameData unreachable this tick. Read-only.
pub(crate) fn stored_blessing() -> Option<i32> {
    // SAFETY: FD4 singleton (read-only); Err before the player is placed.
    let gdm = unsafe { GameDataMan::instance() }.ok()?;
    let pgd = gdm.main_player_game_data.as_ref();
    Some(pgd.scadutree_blessing as i32)
}

/// GameHook seam (hook_impl.rs `EldenRingHook::set_scadutree_blessing`): raw stored-blessing
/// write. The er-logic caller has already clamped/compared per the trait contract; we re-clamp
/// defensively to the valid stored range. No-op if PlayerGameData is unreachable.
pub(crate) fn write_stored_blessing(level: i32) {
    // SAFETY: FD4 singleton accessed mutably; same idiom as `raise_stored_blessing`.
    let Ok(gdm) = (unsafe { GameDataMan::instance_mut() }) else {
        return;
    };
    let pgd = gdm.main_player_game_data.as_mut();
    pgd.scadutree_blessing = level.clamp(0, SCADU_MAX_LEVEL) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scadu_curve_matches_cpp_table() {
        // Boundary checks against kScaduCum.
        assert_eq!(er_logic::upgrades::level_for_fragments(0), 0);
        assert_eq!(er_logic::upgrades::level_for_fragments(1), 1);
        assert_eq!(er_logic::upgrades::level_for_fragments(2), 1); // 2 frags still level 1 (next is 3)
        assert_eq!(er_logic::upgrades::level_for_fragments(3), 2);
        assert_eq!(er_logic::upgrades::level_for_fragments(49), 19);
        assert_eq!(er_logic::upgrades::level_for_fragments(50), 20);
        assert_eq!(er_logic::upgrades::level_for_fragments(999), 20); // capped at 20
    }

    #[test]
    fn weapon_id_math() {
        // base/level split for a +7 weapon row (category 0x0 weapon; row 1000007 -> base 1000000, level 7).
        assert_eq!(
            er_logic::upgrades::decode_weapon_id(1_000_007),
            Some((1_000_000, 7))
        );
        // a weapon at +0 decodes to (base, 0).
        assert_eq!(
            er_logic::upgrades::decode_weapon_id(2_000_000),
            Some((2_000_000, 0))
        );
        // out-of-range weapon row ids decode to None.
        assert_eq!(er_logic::upgrades::decode_weapon_id(500), None);
        // a GOODS-category id (category nibble 0x4) is NOT a weapon -> None even if row is in range.
        assert_eq!(
            er_logic::upgrades::decode_weapon_id((er_codec::CATEGORY_GOODS | 2_010_000) as i32),
            None
        );
        // a PROTECTOR-category id (nibble 0x1) is rejected too.
        assert_eq!(
            er_logic::upgrades::decode_weapon_id((er_codec::CATEGORY_PROTECTOR | 1_000_000) as i32),
            None
        );
    }

    #[test]
    fn apply_auto_upgrade_off_is_identity() {
        // With the feature off, apply_auto_upgrade is a pure identity (no game access).
        set_auto_upgrade(0);
        assert_eq!(apply_auto_upgrade(1_000_007), 1_000_007);
        assert_eq!(
            apply_auto_upgrade((er_codec::CATEGORY_GOODS | 2_010_000) as i32),
            (er_codec::CATEGORY_GOODS | 2_010_000) as i32
        );
    }

    #[test]
    fn scadu_mode_clamp() {
        // set_global_scadu_blessing clamps to {0,1,2,3}.
        set_global_scadu_blessing(0);
        assert_eq!(scadu_mode(), 0);
        set_global_scadu_blessing(1);
        assert_eq!(scadu_mode(), 1);
        set_global_scadu_blessing(2);
        assert_eq!(scadu_mode(), 2);
        // 3 = dlc_only scope + DLC catch-up (2026-08-06). It was OUT OF RANGE until the option
        // split, so this assertion is the one that would have caught shipping the wire value
        // without the arm that reads it -- the clamp would have turned every mode-3 seed off.
        set_global_scadu_blessing(3);
        assert_eq!(scadu_mode(), 3);
        set_global_scadu_blessing(7); // out of range -> off
        assert_eq!(scadu_mode(), 0);
        set_global_scadu_blessing(0);
    }
}
