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

use er_logic::reconcile::{
    ApplyClasses, DesiredInputs, GameIo, Reconciler, SaveIdentity, TickBudget, WorldStability,
};

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
        LiveGame { in_world_since: None }
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

/// Walk the player's held goods and report whether a specific goods FullID is present. Same
/// enumeration path as `inventory::scan_synthetics` / `upgrades.rs`
/// (`GameDataMan -> main_player_game_data -> equipment.equip_inventory_data.items_data.items()`),
/// which is proven in-game.
///
/// NOTE(windows-verify) — GOODS-ID MASK REVIEW (Gap 3; CANNOT be host-tested — this crate is
/// Windows-only). `goods` is the GRANT FullID `GOODS_FULLID | row` where `GOODS_FULLID = 0x4000_0000`
/// (see `er_logic::progressive::GOODS_FULLID`). In ER an `ItemId` packs the category in the top
/// nibble (category = id / 0x1000_0000; Goods = 4 -> 0x4000_0000) and the param ROW in the low 28
/// bits. So the two checks below SHOULD be right:
///   * `want_row = goods & 0x0FFF_FFFF` strips the 0x4 category nibble, leaving the bare row;
///   * `category() == ItemCategory::Goods` confirms the 0x4 nibble independently;
///   * `param_id()` is compared against the bare row.
/// LOOKS RIGHT: the mask matches the `0x4000_0000` goods-category convention this client grants with,
/// and the independent `category()` guard prevents a weapon/armor row with the same numeric row from
/// false-matching.
/// SUSPICIOUS / MUST CONFIRM ON WINDOWS with a set->readback (grant one known good, then re-read):
///   1. Does `ItemId::param_id()` return the CATEGORY-STRIPPED row (assumed here), or the full
///      category-tagged id? If the latter, this compare never matches and BOTH sides must be masked:
///      `entry.item_id.param_id() as i32 & 0x0FFF_FFFF == want_row`.
///   2. Great Runes / key items are granted at the SAME `0x4000_0000` goods category, so they ride
///      this predicate correctly ONLY if their grant FullID also uses that nibble — verify the
///      key-item / great-rune mapper packs `GOODS_FULLID`, not a raw row or a different category.
///   3. Confirm no goods row legitimately exceeds `0x0FFF_FFFF` (rows are small, so this is safe, but
///      pin it).
/// DO NOT silently "fix" the mask: if a change is needed, keep the original masked compare in a
/// comment. The proposed alternative (double-mask) is noted inline below.
fn inventory_has_goods(goods: i32) -> bool {
    use eldenring::cs::{GameDataMan, ItemCategory};
    use fromsoftware_shared::FromStatic;

    let gdm = match unsafe { GameDataMan::instance() } {
        Ok(g) => g,
        Err(_) => return false,
    };
    let pgd = gdm.main_player_game_data.as_ref();
    let want_row = (goods as u32 & 0x0FFF_FFFF) as i32;
    for entry in pgd.equipment.equip_inventory_data.items_data.items() {
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

/// A tiny JSON map `SaveIdentity -> applied_watermark`, persisted so the consumable ledger survives a
/// reconnect / reload (the flask-double-grant fix depends on this). Written next to the client dll.
pub struct WatermarkStore {
    path: std::path::PathBuf,
    map: BTreeMap<String, i64>,
}

impl WatermarkStore {
    /// Load (or start empty) from `path`. A missing / malformed file is treated as empty (never
    /// panics — the same tolerance `save_state::from_json` uses).
    pub fn load(path: std::path::PathBuf) -> Self {
        let map = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<BTreeMap<String, i64>>(&t).ok())
            .unwrap_or_default();
        WatermarkStore { path, map }
    }

    pub fn get(&self, save: &SaveIdentity) -> i64 {
        self.map.get(&save.0).copied().unwrap_or(0)
    }

    pub fn set(&mut self, save: &SaveIdentity, watermark: i64) {
        self.map.insert(save.0.clone(), watermark);
        // Best-effort write-through; a failure just means we re-grant-check next boot (idempotent).
        if let Ok(t) = serde_json::to_string(&self.map) {
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

struct Driver {
    reconciler: Reconciler,
    io: LiveGame,
    store: WatermarkStore,
    save: SaveIdentity,
}

/// EVENT NUDGE — call from the net loop / connect / load handlers instead of doing the grant inline.
/// Cheap and lock-free.
pub fn mark_dirty() {
    DIRTY.store(true, Ordering::Relaxed);
}

/// Is dry-run mode on? (`RECONCILE_DRYRUN=1` — phase 0: compute + log the diff, never apply.)
fn dry_run() -> bool {
    std::env::var("RECONCILE_DRYRUN").map(|v| v == "1").unwrap_or(false)
}

/// Public view of [`dry_run`] so `core.rs` can gate its additive dry-run wiring on the same env var
/// (it must NOT do the reconciler snapshot/set_inputs work at all unless dry-run is on).
pub fn dry_run_enabled() -> bool {
    dry_run()
}

/// STRANGLER cutover control: which classes the reconciler is allowed to APPLY, read from
/// `RECONCILE_APPLY` (comma list of `flags`,`goods`,`ledger`, or `all`). Unset/empty => `all`
/// (full ownership — the end state). During the phased cutover each phase widens this set by one
/// class while the retired old handler is deleted in the same phase. Ignored under dry-run.
fn apply_classes() -> ApplyClasses {
    match std::env::var("RECONCILE_APPLY") {
        Err(_) => ApplyClasses::ALL,
        Ok(v) => {
            let v = v.trim();
            if v.is_empty() || v.eq_ignore_ascii_case("all") {
                return ApplyClasses::ALL;
            }
            let mut c = ApplyClasses::NONE;
            for part in v.split(',') {
                match part.trim().to_ascii_lowercase().as_str() {
                    "flags" => c.flags = true,
                    "goods" => c.goods = true,
                    "ledger" => c.ledger = true,
                    other => log::warn!("RECONCILE_APPLY: ignoring unknown class '{other}'"),
                }
            }
            c
        }
    }
}

/// Initialize the driver once, after slot_data is parsed. `persist_path` is the watermark file next
/// to the client dll.
///
/// INTEGRATION: call this from the reconstructed `core.rs` once per session, after the per-seed
/// `DesiredInputs` are built from parsed slot_data.
pub fn init(inputs: DesiredInputs, persist_path: std::path::PathBuf) {
    let save = inputs.save.clone();
    let store = WatermarkStore::load(persist_path);
    let watermark = store.get(&save);
    let reconciler = Reconciler::from_persisted(inputs, watermark);
    let driver = Driver {
        reconciler,
        io: LiveGame::new(),
        store,
        save,
    };
    let _ = DRIVER.set(Mutex::new(driver));
    mark_dirty();
}

/// SWAP inputs (received prefix grew, or a reconnect). Atomic + seed-change aware inside the pure
/// reconciler (resets the ledger watermark only on a genuine seed change — the reconnect-new-seed
/// fix). Call from the net loop when `items_received` / room seed changes.
pub fn set_inputs(inputs: DesiredInputs) {
    if let Some(m) = DRIVER.get() {
        if let Ok(mut d) = m.lock() {
            d.save = inputs.save.clone();
            d.reconciler.set_inputs(inputs);
        }
    }
    mark_dirty();
}

/// TICK — call once per game-thread frame (from the reconstructed `update_live`). Does nothing unless
/// dirty; the reconciler itself gates every read/write on world stability, so this is safe to call
/// during load screens (it simply no-ops).
///
/// INTEGRATION: replace the scattered `drain_start_items` / `flush_grace_flags` /
/// `open_on_received_name` / great-rune restore / map-reveal calls in `update_live` with this ONE
/// call, per the strangler phases in `MIGRATION.md`.
pub fn tick() {
    if !DIRTY.load(Ordering::Relaxed) {
        return;
    }
    let Some(m) = DRIVER.get() else { return };
    let Ok(mut d) = m.lock() else { return };

    d.io.refresh_dwell();
    let budget = TickBudget::default();

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

    // Persist the (possibly advanced) ledger watermark for this save.
    let wm = d.reconciler.applied_watermark();
    let save = d.save.clone();
    d.store.set(&save, wm);

    if out.converged {
        DIRTY.store(false, Ordering::Relaxed);
    }
    if !out.applied.is_empty() {
        log::debug!("[reconcile] applied {} action(s) this tick", out.applied.len());
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
// region bloom latch, great-rune restore set) are then DELETED one class at a time — see MIGRATION.md.
// ---------------------------------------------------------------------------------------------
