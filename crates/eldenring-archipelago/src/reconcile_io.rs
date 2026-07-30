//! `reconcile_io` — the WINDOWS binding for the pure [`er_logic::reconcile`] reconciler.
//!
//! This is the ONLY place the reconciler touches the live game: it implements [`GameIo`] against the
//! `fromsoftware-rs` singletons (event flags via `crate::flags`, goods via the `GameDataMan`
//! inventory walk that `inventory.rs` / `upgrades.rs` already use, item grants via
//! `crate::detour::grant_full_id`), owns the poll-thread tick + the dirty flag, and persists the
//! per-save ledger watermark to a file next to the client.
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use er_logic::marker::{self, FlagBand};
use er_logic::reconcile::{
    ApplyClasses, CharLedger, DesiredInputs, GameIo, Reconciler, TickBudget, WorldStability,
    legacy_adopt, seed_trust, stamp_playtime,
};
use serde::{Deserialize, Serialize};

/// The save-embedded reconcile marker's flag band (`crate::marker`, minibake). The watermark +
/// (seed, slot) identity live INSIDE the save here, so a reconnect reads ground truth instead of
/// inferring identity from `play_time`. Band verified in-game 2026-07-21 (the `!markerprobe` pass).
const MARKER_BAND: FlagBand = FlagBand::PLACEHOLDER;

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
/// SUSPICIOUS / MUST CONFIRM ON WINDOWS with a set->readback (grant one known good, then re-read):
///   1. Does `ItemId::param_id()` return the CATEGORY-STRIPPED row (assumed here), or the full
///      category-tagged id? If the latter, this compare never matches and BOTH sides must be masked:
///      `entry.item_id.param_id() as i32 & 0x0FFF_FFFF == want_row`.
///   2. Great Runes / key items are granted at the SAME `0x4000_0000` goods category, so they ride
///      this predicate correctly ONLY if their grant FullID also uses that nibble — verify the
///      key-item / great-rune mapper packs `GOODS_FULLID`, not a raw row or a different category.
///   3. Confirm no goods row legitimately exceeds `0x0FFF_FFFF` (rows are small, so this is safe, but
///      pin it).
///
/// DO NOT silently "fix" the mask: if a change is needed, keep the original masked compare in a
/// comment. The proposed alternative (double-mask) is noted inline below.
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
    false
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
// The poll-thread driver: dirty flag + tick
// ---------------------------------------------------------------------------------------------

/// Set by every event (connect / load / ItemReceived) instead of mutating the game directly.
static DIRTY: AtomicBool = AtomicBool::new(true);

/// The live reconciler + its IO + watermark store, owned by the poll thread. `OnceLock<Mutex<..>>`
/// so the net thread can nudge / swap inputs while the game thread ticks.
static DRIVER: OnceLock<Mutex<Driver>> = OnceLock::new();

/// Set at init when the save-embedded marker's identity MISMATCHES this connection's (seed, slot) —
/// i.e. this save belongs to a different seed/slot. The reconciler is NOT armed (no grants), and the
/// caller must also gate check REPORTING on this, so seed-A's save flags aren't reported as seed-B
/// checks (which would corrupt the multiworld, strictly worse than a double-grant). See `is_refused`.
static REFUSED: AtomicBool = AtomicBool::new(false);

/// Whether the current session was REFUSED by the marker identity guard (see [`REFUSED`]). `core`
/// gates check reporting on this; the reconciler simply never armed.
pub fn is_refused() -> bool {
    REFUSED.load(Ordering::Relaxed)
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

/// EVENT NUDGE — call from the net loop / connect / load handlers instead of doing the grant inline.
/// Cheap and lock-free.
pub fn mark_dirty() {
    DIRTY.store(true, Ordering::Relaxed);
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
    !dry_run() && apply_classes().flags
}
pub fn owns_goods() -> bool {
    !dry_run() && apply_classes().goods
}
pub fn owns_ledger() -> bool {
    !dry_run() && apply_classes().ledger
}

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
    //     reload never re-grants -- flask_grant_replay stays authoritative).
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
            REFUSED.store(true, Ordering::Relaxed);
            log::warn!(
                "[reconcile] REFUSED: save marker identity {stored:#010x} != this session {expected:#010x} \
                 -- this save belongs to a different seed/slot. NOT arming the reconciler; check \
                 reporting is gated. Reconnect the correct save, or start a fresh character."
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
    log::info!(
        "[reconcile] ledger seed: save_slot={save_slot:?} play_time={play_time} marker={decision:?} entry={entry:?} received_through={received_through} identity={identity:#010x} -> watermark {}",
        reconciler.applied_watermark()
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
    mark_dirty();
}

/// SWAP inputs (received prefix grew, or a reconnect). Atomic + seed-change aware inside the pure
/// reconciler (resets the ledger watermark only on a genuine seed change — the reconnect-new-seed
/// fix). Call from the net loop when `items_received` / room seed changes.
pub fn set_inputs(inputs: DesiredInputs) {
    if let Some(m) = DRIVER.get()
        && let Ok(mut d) = m.lock()
    {
        d.slot = inputs.save.0.clone(); // same character; save_slot is unchanged
        d.reconciler.set_inputs(inputs);
    }
    mark_dirty();
}

/// TICK — call once per game-thread frame (from the reconstructed `update_live`). Does nothing unless
/// dirty; the reconciler itself gates every read/write on world stability, so this is safe to call
/// during load screens (it simply no-ops).
///
/// INTEGRATION: replace the scattered `drain_start_items` / `flush_grace_flags` /
/// `open_on_received_name` / great-rune restore / map-reveal calls in `update_live` with this ONE
/// call, per the strangler phases in `docs/history/MIGRATION.md` (archived; cutover complete).
pub fn tick() {
    if !DIRTY.load(Ordering::Relaxed) {
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
        // Do NOT clear dirty in dry-run: keep logging until the operator switches modes.
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

    if out.converged {
        DIRTY.store(false, Ordering::Relaxed);
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
// INTEGRATION (now wired into core.rs::update_live behind RECONCILE_DRYRUN; core.rs is NOT
// truncated). The wiring is five calls:
//
//   // 1. after slot_data parse (once):
//   reconcile_io::init(build_desired_inputs(&slot_data, &received), client_dir.join("reconcile.json"));
//
//   // 2. every frame from update_live:
//   reconcile_io::tick();
//
//   // 3. net loop, when items_received or the room seed changes:
//   reconcile_io::set_inputs(build_desired_inputs(&slot_data, &received));
//
//   // 4. connect / load handlers (instead of the old inline grants):
//   reconcile_io::mark_dirty();
//
//   // 5. `build_desired_inputs` maps each archipelago_rs received item -> ReceivedItem with the
//   //    right ItemSemantics (RegionFlags / MapReveal / KeyItem / GreatRune / Consumable / GoalFlag),
//   //    reusing the tables the old feature modules already carry (region.rs open flags,
//   //    startgrants.rs MAP_REVEAL_FLAGS_BASE + 82001, keyitems.rs 4000xx obtained flags, the
//   //    great-rune restore goods, the flask/rune/stone FullIDs).
//
// The old per-feature idempotency bools (start_items_granted, notify_granted, session grace sets,
// region bloom latch, great-rune restore set) are then DELETED one class at a time — see
// docs/history/MIGRATION.md (archived; cutover complete).
// ---------------------------------------------------------------------------------------------
