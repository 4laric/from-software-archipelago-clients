//! Region locks (Milestone B Stage 3). The pure decision is `er_logic::region_lock::kick_decision`
//! (host-tested); this module is the game-side glue: parse the region config out of slot_data, set
//! the baked KICK flag (76970) when the player is in a locked region, and open regions on receipt of
//! their unlock item.
//!
//! PURE-RUNTIME (2026-07-01/02, baker retired): both halves that used to be baked reactors are now
//! client-side. Kick enforcement = warp-out to Roundtable via `warp::warp_to_grace` (tick_kick;
//! kill only as fallback; flag 76970 still set for bake-compat). Random-start = `tick_random_start_warp`: `randomStartAreaId` (18000) is the
//! TRIGGER area (tutorial / Chapel of Anticipation -- REGION_ID_MAP.md), NOT the destination.
//! A fresh character in the trigger area gets the retired reactor's job done client-side
//! (`warp::warp_to_grace` out to the hub/rolled grace); an established character just has the
//! trigger consumed in place. Both paths latch `randomStartDoneFlag` (76968, persistent) +
//! `randomStartWarpFlag` (76969, bake-compat), and the done flag unblocks KICK's start-window
//! guard (see `kick_decision`) -- until it sets, region enforcement is silently OFF (seen live
//! 2026-07-02: area_locks=42 configured, zero kicks all day).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

use serde_json::Value;

use er_logic::region_lock::EnforcementLatch;

use crate::flags;

/// Flag the baked `common.emevd` reactor (event 6970) watches: set while the player is in a locked
/// region -> the reactor warps them to Roundtable Hold and clears it.
const KICK_FLAG: u32 = 76970;

/// Once-per-lock-entry latch for setting KICK_FLAG (rising edge of `kick_decision`); the reactor's
/// warp ejects the player and the latch re-arms once they're back in an open region. Pure er-logic type.
static KICK_LATCH: Mutex<EnforcementLatch> = Mutex::new(EnforcementLatch::new());

/// Latch so the random-start warp trigger fires once per session (the persistent `randomStartDoneFlag`
/// is the cross-session guard; this is the in-session dedup, mirroring the standalone `START_LATCHED`).
static START_LATCHED: AtomicBool = AtomicBool::new(false);

/// The open flag of the region that owns the goal locations, or 0 = unknown (world#694).
///
/// Resolved once at connect from the tracker tables' COARSE key -- "the region whose lock decides
/// in-logic" -- rather than from a location's fine display region. 0 means "could not resolve", and
/// the notice is then simply absent: it warns, it does not gate, so don't-know costs nothing.
static GOAL_ARENA_OPEN_FLAG: AtomicU32 = AtomicU32::new(0);

/// Whether this seed advertises a goal plus region apparatus and therefore has a withheld gate to
/// reconcile. This distinguishes a genuinely absent/foreign gate from a broken resolution: both
/// store open flag 0, but only the latter must take the loud fail-open path (client#268).
static GOAL_GATE_EXPECTED: AtomicBool = AtomicBool::new(false);

/// Rising-edge latch for the goal-approach notice, so it says its line once per arrival rather
/// than once per tick or once per reload.
static GOAL_APPROACH: Mutex<er_logic::goal_approach::ApproachNotice> =
    Mutex::new(er_logic::goal_approach::ApproachNotice::new());

/// The goal region's own LOCK ITEM NAME, resolved at connect beside its open flag.
///
/// The gate needs the name as well as the flag: `goal_gate::decide` excludes the goal region's own
/// lock from the requirement list, because that lock is the thing being GRANTED and requiring it
/// would be requiring the output as an input. Empty string = unresolved.
static GOAL_LOCK_ITEM: Mutex<String> = Mutex::new(String::new());

/// Latched once the gate has opened the goal region this world-load, so the convergence loop stops
/// and the log line is said once rather than per tick.
static GOAL_GATE_OPENED: AtomicBool = AtomicBool::new(false);
/// Rising-edge latch for the WITHHELD line, so "still shut, N outstanding" is not spammed.
static GOAL_GATE_SAID_SHUT: AtomicBool = AtomicBool::new(false);
static GOAL_GATE_BY_REGION_COMPLETION: AtomicBool = AtomicBool::new(false);
static GOAL_GATE_COMPLETION_PRIMED: AtomicBool = AtomicBool::new(false);

pub fn configure_goal_gate_policy(regions_completed: bool) {
    GOAL_GATE_BY_REGION_COMPLETION.store(regions_completed, Ordering::Relaxed);
    GOAL_GATE_COMPLETION_PRIMED.store(false, Ordering::Relaxed);
    log::info!(
        "goal-gate: unlock policy = {}",
        if regions_completed {
            "regions_completed"
        } else {
            "items_held"
        }
    );
}

pub fn goal_gate_uses_region_completion() -> bool {
    GOAL_GATE_BY_REGION_COMPLETION.load(Ordering::Relaxed)
}

pub fn goal_region_name() -> Option<String> {
    let lock = GOAL_LOCK_ITEM.lock().ok()?.clone();
    lock.strip_suffix(" Lock").map(str::to_owned)
}

/// Re-arm at the in-world edge (`test_gf_client_resets_are_called`).
///
/// 🛑 THIS MODULE NOW WRITES GAME STATE, so it is subject to the reset rule rather than exempt from
/// it. `tick_goal_gate` sets the goal region's open flag and grace bundle, and a map load reverts
/// flag writes -- so the latch must drop or the convergence loop will believe it already succeeded
/// and never re-apply. That is #200's exact shape: the capital reconciler wrote once, trusted the
/// latch, and left a player in a burnt world.
pub fn reset() {
    GOAL_GATE_OPENED.store(false, Ordering::Relaxed);
    GOAL_GATE_SAID_SHUT.store(false, Ordering::Relaxed);
}

/// Install the goal region's open flag. Called at connect once the tracker tables and the region
/// config both exist; `None` / unresolvable leaves it at 0 (notice absent).
pub fn configure_goal_arena(open_flag: Option<u32>, lock_item: Option<&str>, gate_expected: bool) {
    GOAL_ARENA_OPEN_FLAG.store(open_flag.unwrap_or(0), Ordering::Relaxed);
    GOAL_GATE_EXPECTED.store(gate_expected, Ordering::Relaxed);
    if let Ok(mut g) = GOAL_LOCK_ITEM.lock() {
        *g = lock_item.unwrap_or_default().to_string();
    }
    match open_flag {
        Some(f) => log::info!(
            "goal-approach: armed on the goal region's open flag {f} -- a player who reaches the \
             arena with Region Locks outstanding will be told once (world#694)"
        ),
        None if gate_expected => log::error!(
            "goal-gate: goal locations exist and region apparatus is advertised, but the withheld \
             gate did not resolve -- FAIL-OPEN armed; once the goal requirements are met every \
             advertised region/grace flag will be reconciled (client#268)"
        ),
        None => log::info!(
            "goal-approach: INERT -- no lock-bearing goal gate is advertised (foreign/legacy \
             apworld or a seed with no region apparatus)"
        ),
    }
}

/// Per-tick: warn ONCE on arriving at the goal arena while kept Region Locks are outstanding.
///
/// 🛑 THIS BLOCKS NOTHING, AND THAT IS THE RULING (world#694, option C). The complaint is SURPRISE,
/// not reachability -- a player who knows the ending will not count may still choose to walk in.
/// Option B (gate the arena) is the kick, and the kick is this repo's filed softlock precedent
/// (#589); a toast has no failure mode at all because it has no authority.
///
/// Returns the player-facing line; the WORDS live in `er_logic::goal_approach` (host-tested,
/// ASCII-swept), the same split `tick_kick` uses with `sealed_region_message`.
pub fn tick_goal_approach(
    cfg: &RegionConfig,
    item_goals: &[String],
    has_item: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let want = GOAL_ARENA_OPEN_FLAG.load(Ordering::Relaxed);
    if want == 0 {
        return None; // not resolved -> absent, never stuck
    }
    let pr = flags::play_region_id()?;
    let sub = if pr >= 1_000_000 { pr / 100 } else { pr };
    // The same covering-range lookup kick-watch does; the arena is "the buckets whose lock range
    // names the goal region's open flag".
    let in_arena = cfg
        .area_lock_flags
        .iter()
        .any(|e| sub >= e[0] && sub <= e[1] && e[2] as u32 == want);
    // `mut` on the guard: `poll` takes &mut self, and a temporary guard is not mutable.
    let mut notice = GOAL_APPROACH.lock().ok()?;
    notice.poll(in_arena, item_goals, has_item)
}

/// Per-tick: OPEN the goal region once every other goal item is held (world#768).
///
/// The goal region's own Lock is no longer in the item pool, so nothing arrives and the client has
/// to reach the same end state itself: set the region's open flag plus its `lock_reveal_flags`
/// bundle -- byte for byte the flags a Lock receipt produces via
/// `ItemSemantics::RegionFlags([open] + bundle)`.
///
/// 🛑 THIS ONE HAS AUTHORITY, unlike its neighbour `tick_goal_approach`, so it is written to fail in
/// the survivable direction:
///
/// * **An unresolvable gate OPENS.** The Lock is not in the pool, so a gate that cannot resolve and
///   refuses to open is an unwinnable seed -- plus every foreign item fill placed inside the region
///   (world#589: forty-two of them, on one seed). A spoiled ending is the cheaper failure.
/// * **The write RE-APPLIES until readback confirms.** `try_set_event_flag` reports whether the
///   write landed; the latch is only taken once `get_event_flag` agrees. Writing once and trusting
///   the latch is #200, which put a player in a burnt world.
/// * **The latch drops at the in-world edge** (`reset()`), because a map load reverts flag writes.
///
/// Returns a player-facing line on the rising edge only.
pub fn tick_goal_gate(
    cfg: &RegionConfig,
    item_goals: &[String],
    rune_goals: &[String],
    runes_required: usize,
    has_item: &dyn Fn(&str) -> bool,
    incomplete_regions: Option<&[String]>,
) -> Option<String> {
    if !flags::in_world() {
        return None; // writes at menu/load are silently discarded (SWEEP R3)
    }
    let want = GOAL_ARENA_OPEN_FLAG.load(Ordering::Relaxed);
    let gate_expected = GOAL_GATE_EXPECTED.load(Ordering::Relaxed);
    if want == 0 && !gate_expected {
        return None; // genuinely no gate advertised; nothing was withheld
    }
    let lock_item = GOAL_LOCK_ITEM
        .lock()
        .ok()
        .map(|g| g.clone())
        .unwrap_or_default();
    let gate = er_logic::goal_gate::GoalGate {
        item_goals: item_goals.to_vec(),
        rune_goals: rune_goals.to_vec(),
        runes_required,
        goal_lock_item: (!lock_item.is_empty()).then(|| lock_item.clone()),
    };
    let silent_reconnect_open = incomplete_regions.is_some()
        && !GOAL_GATE_COMPLETION_PRIMED.swap(true, Ordering::Relaxed)
        && incomplete_regions.is_some_and(<[String]>::is_empty);
    let decision = if let Some(incomplete) = incomplete_regions {
        let mut outstanding = incomplete.to_vec();
        let held_runes = rune_goals.iter().filter(|name| has_item(name)).count();
        if held_runes < runes_required {
            outstanding.push(format!("Great Runes ({held_runes}/{runes_required})"));
        }
        outstanding.sort();
        outstanding.dedup();
        if outstanding.is_empty() {
            er_logic::goal_gate::Decision::Open
        } else {
            er_logic::goal_gate::Decision::Withhold { outstanding }
        }
    } else {
        er_logic::goal_gate::decide(&gate, has_item)
    };

    if !decision.opens() {
        // Say the outstanding list ONCE, then stay quiet until something changes it.
        if !GOAL_GATE_SAID_SHUT.swap(true, Ordering::Relaxed) {
            log::info!(
                "{}",
                er_logic::goal_gate::status_line(&decision, &lock_item)
            );
        }
        return None;
    }

    // Converged already this world-load? Nothing to write, nothing to say.
    if GOAL_GATE_OPENED.load(Ordering::Relaxed) {
        return None;
    }

    // A resolved gate receives exactly its own Lock apparatus. If resolution failed despite an
    // advertised goal gate, open every seed-advertised apparatus: broad but survivable, and unlike
    // the old zero-sentinel early return it cannot strand a withheld goal Lock.
    let to_set = er_logic::goal_gate::flags_to_open(
        (want != 0).then_some(want),
        (!lock_item.is_empty()).then_some(lock_item.as_str()),
        &cfg.region_open_flags,
        &cfg.lock_reveal_flags,
        &cfg.region_graces,
    );
    if to_set.is_empty() {
        if !GOAL_GATE_SAID_SHUT.swap(true, Ordering::Relaxed) {
            log::error!(
                "goal-gate: FAIL-OPEN could not find any advertised region/grace flags to write; \
                 the seed contract is incomplete"
            );
        }
        return None;
    }

    // WRITE, then READ BACK. Every flag has to agree before the latch is taken; anything short of
    // that leaves the loop armed and we try again next tick.
    let mut all_confirmed = true;
    for f in &to_set {
        if !flags::get_event_flag(*f) {
            let _ = flags::try_set_event_flag(*f, true);
        }
        if !flags::get_event_flag(*f) {
            all_confirmed = false;
        }
    }
    if !all_confirmed {
        return None; // still converging -- re-applied next tick, never latched on faith
    }

    GOAL_GATE_OPENED.store(true, Ordering::Relaxed);
    log::info!(
        "goal-gate: CONVERGED -- {} flag(s) set and read back for {} ({} decision)",
        to_set.len(),
        if lock_item.is_empty() {
            "all advertised regions (unresolved goal gate)"
        } else {
            lock_item.as_str()
        },
        if matches!(
            decision,
            er_logic::goal_gate::Decision::OpenUnresolvable { .. }
        ) {
            "unresolvable-open"
        } else {
            "all goal items held"
        }
    );
    if let er_logic::goal_gate::Decision::OpenUnresolvable { why } = &decision {
        log::warn!("goal-gate: OPENED WITHOUT A GATE -- {why}");
    }
    // ASCII ONLY -- the game's font has no typographic quotes or dashes.
    let region = lock_item
        .strip_suffix(" Lock")
        .filter(|name| !name.is_empty())
        .unwrap_or("Goal region");
    (!silent_reconnect_open).then(|| format!("Region unlocked: {region}"))
}

/// kick-watch diagnostic: last play_region_id seen by tick_kick (i32::MIN = none yet).
static KICK_WATCH_LAST_PR: AtomicI32 = AtomicI32::new(i32::MIN);

/// When in_world first went true for the CURRENT world session (None while at menu/loading).
/// play_region_id can serve a STALE region for a moment after a load, so the random-start
/// trigger waits out a settle window after every world entry before trusting it.
static WARP_WORLD_SETTLE: Mutex<Option<std::time::Instant>> = Mutex::new(None);
const WARP_SETTLE_SECS: u64 = 5; // let play_region_id settle after world entry before trusting it

/// Warp destination fallback for seeds whose apworld predates the `randomStartGraceId`
/// slot_data key: grace entity 11102950 = Table of Lost Grace, Roundtable Hold (the hub the
/// shipping random-start mode warps out to). Same id the CE table's warp uses.
const ROUNDTABLE_GRACE_ID: u32 = 11102950;

/// One disjunctive clause of a natural-key trigger: satisfied when ALL `items` were received AND ALL
/// `flags` are set AND at least `count` distinct names of `count_items` were received. Ported from
/// the standalone `features.rs::NkClause`; COUNT term added 2026-07-24 (natural-progression count
/// gates: Caelid = 2-of-the-remembrances, Leyndell = N Great Runes). The pure evaluation twin is
/// `er_logic::region_lock::NkClause` / `natural_key_fired` (host-tested); keep the semantics in
/// lockstep. Absent count fields parse to `[]`/`0` (vacuous term), so old data is unchanged.
#[derive(Default)]
pub struct NkClause {
    pub items: Vec<String>,
    pub flags: Vec<u32>,
    /// COUNT term (N-of-a-set): the clause additionally requires >= `count` of these in `received`.
    pub count_items: Vec<String>,
    /// Threshold for `count_items`. `0` = no count requirement.
    pub count: usize,
}

/// Region-lock config, parsed from slot_data (shapes mirror the standalone `net.rs`).
#[derive(Default)]
pub struct RegionConfig {
    /// `[lo, hi, open_flag]` inclusive 5-digit subregion ranges; locked when the open flag is off.
    pub area_lock_flags: Vec<[i32; 3]>,
    /// `0` = non-random seed (no start guard); else KICK waits until this flag is set.
    pub random_start_done_flag: u32,
    /// `0` = no random start; else the baked warp trigger flag to set once you reach the start area.
    pub random_start_warp_flag: u32,
    /// `0` = no random start; else the play-region id of the rolled start area (where to fire the warp).
    pub random_start_area_id: i32,
    /// Grace ENTITY id to physically warp to on a random-start seed (pure-runtime warp primitive).
    /// `0` = not emitted by the apworld yet; `tick_random_start_warp` then falls back to the
    /// Roundtable grace when the start area is the Roundtable hub (area 18000), else logs the gap.
    pub random_start_grace_id: u32,
    /// lock item name -> the region's physical open flag.
    pub region_open_flags: HashMap<String, u32>,
    /// lock item name -> map-reveal / enforcement-open flags.
    pub lock_reveal_flags: HashMap<String, Vec<u32>>,
    /// lock item name -> grace warp-unlock flags.
    pub region_graces: HashMap<String, Vec<u32>>,
    /// GRACE ATTUNEMENT (`graceAttunement`). Region lock name -> the gate for that region: touch
    /// `threshold` of `members` (its non-anchor grace warp flags) and the rest (`bloom`) light.
    ///
    /// The apworld emits this ONLY for regions it decided to gate; a region absent here keeps the
    /// whole-bundle behaviour, which is the off default and also what small regions get (the gate is
    /// skipped where `members.len() <= threshold`, since traversal is not the problem there).
    ///
    /// ⭐⭐⭐ NO SESSION STATE, AND THAT IS NOT AN OVERSIGHT. `crate::attunement`'s check-based twin
    /// needs `pending` / `attuned_latched` / `bloom_lit` because it counts the SERVER checked set,
    /// which is external and re-snapshots on reconnect. This one counts GRACE FLAGS, which live in
    /// the player's SAVE -- so the count re-derives identically after any reconnect, and the bloom
    /// flags are their own latch exactly as `tick_grace_items` uses "the grace flag itself as the
    /// latch". Nothing to prime, nothing to replay, no double-banner to defend against.
    pub grace_attunement: HashMap<String, GraceGate>,
    /// grace_rando: "Grace: ..." item name -> that grace's warp-unlock flag (slot_data graceItems).
    pub grace_items: HashMap<String, u32>,
    /// region (lock name) -> disjunction of natural-key clauses. When ANY clause holds, the region's
    /// apparatus blooms WITHOUT an AP lock item being received (vanilla keys / world flags). The
    /// region's open flag doubles as the once-latch. (Ported from the standalone naturalKeyTriggers.)
    pub natural_key_triggers: HashMap<String, Vec<NkClause>>,
    /// lock item name -> packed FullIDs to physically grant in-game on that lock's FIRST open
    /// (slot_data `lockGrantItems`). Currently the unpooled medallions riding their locks
    /// (Rold -> Mountaintops Lock; both Secret Medallion halves -> Snowfield Lock), so the Grand
    /// Lift stays usable and medallion-triggered quest content (Ensha, Latenna) fires naturally.
    /// SPEC-region-spine-surgery.md SS3.5 (grant-on-receipt rider).
    pub lock_grant_items: HashMap<String, Vec<i32>>,
    /// BAKED-TABLE FALLBACK (bedrock interop): prepared at connect -- from the generated
    /// `er_logic::region_locks` table -- for seeds that ship NEITHER `areaLockFlags` NOR
    /// `regionOpenFlags`. Holds the scoped-but-COLD derived config until `tick_baked_fallback`
    /// merges it into the live fields on first receipt of a scoped "<Region> Lock".
    pub baked_fallback: Option<er_logic::region_lock::DerivedLocks>,
}

// --- areaLockFlags: SINGLE SOURCE OF TRUTH is the apworld's data ----------------------------------
// The region -> play_region geometry lives in exactly ONE editable place: the generator's
// features/area_locks.py, which ships the fully-resolved kick-watch ranges as slot_data
// `areaLockFlags`, and slot_data always WINS. The client's only copy of that geometry is the
// GENERATED `er_logic::region_locks` table (tools/gen_region_locks.py, CI drift-gated) -- it feeds
// the FOREIGN-apworld fallback below and is never consulted while slot_data speaks. A HAND-typed
// mirror in this file repeatedly drifted from the generator (e.g. the Consecrated Snowfield ->
// Mountaintops fold); a gen-side test (test_gf_data.py) still forbids one here, and the generated
// table cannot drift because CI regenerates and diffs it.

pub fn parse(sd: &Value) -> RegionConfig {
    // Re-arm the random-start warp latch on each fresh parse (mirrors the standalone `configure`
    // per-connect reset) so a second seed loaded in the same game process can warp again. The
    // persistent `randomStartDoneFlag` still prevents a re-warp within one save.
    START_LATCHED.store(false, Ordering::Relaxed);
    *WARP_WORLD_SETTLE.lock().unwrap() = None;
    let region_open_flags = str_to_u32(sd.get("regionOpenFlags"));
    // Kick-watch ranges come straight from the generator's `areaLockFlags` (single source of truth;
    // see the note above). parse() itself never derives ranges: an apworld that EMITS region keys
    // (either of them) said what it wanted, and a seed that ships an empty/absent table keeps an
    // empty kick-watch. The one exception lives OUTSIDE parse: for a seed that ships NEITHER
    // region key (a region-lock-ignorant foreign apworld), core.rs may prepare the baked-table
    // fallback -- see `prepare_baked_fallback` / `tick_baked_fallback` below.
    let area_lock_flags = parse_triples(sd.get("areaLockFlags"));
    RegionConfig {
        area_lock_flags,
        random_start_done_flag: sd
            .get("randomStartDoneFlag")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        random_start_warp_flag: sd
            .get("randomStartWarpFlag")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        random_start_area_id: sd
            .get("randomStartAreaId")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        random_start_grace_id: sd
            .get("randomStartGraceId")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        region_open_flags,
        lock_reveal_flags: str_to_u32vec(sd.get("lockRevealFlags")),
        region_graces: str_to_u32vec(sd.get("regionGraces")),
        grace_items: str_to_u32(sd.get("graceItems")),
        grace_attunement: parse_grace_attunement(sd.get("graceAttunement")),
        natural_key_triggers: parse_natural_keys(sd.get("naturalKeyTriggers")),
        lock_grant_items: str_to_i32vec(sd.get("lockGrantItems")),
        baked_fallback: None,
    }
}

/// Per-tick (settled / in-world): bloom regions whose natural-key trigger disjunction is now
/// satisfied. A clause fires when ALL its items are in `received` AND ALL its flags are set; ANY
/// clause fires the region. The region's open flag doubles as the once-latch, so this is idempotent
/// and cheap after the first bloom. Sets graces + open flag + reveal flags directly (the converged
/// client sets flags directly, unlike the standalone's queue). Mirrors `EvaluateNaturalKeyTriggers`.
pub fn tick_natural_key_triggers(cfg: &RegionConfig, received: &HashSet<String>) {
    if cfg.natural_key_triggers.is_empty() {
        return;
    }
    for (name, clauses) in &cfg.natural_key_triggers {
        let open_flag = match cfg.region_open_flags.get(name) {
            Some(&f) => f,
            None => continue, // no apparatus to bloom
        };
        // Reconcile-safe latch (gf-region-grace-loss-frontdoor-latch): skip only when the region
        // is FULLY bloomed -- open flag AND every grace AND every reveal flag observed set.
        // Latching on the open flag alone stranded interior graces after a save-load when the
        // front-door grace doubles as the open flag (Limgrave 73100). Pure gate host-tested by
        // region_lock_replay.
        let mut bloom_flags: Vec<u32> = Vec::new();
        if let Some(fs) = cfg.region_graces.get(name) {
            bloom_flags.extend_from_slice(fs);
        }
        if let Some(fs) = cfg.lock_reveal_flags.get(name) {
            bloom_flags.extend_from_slice(fs);
        }
        if er_logic::region_lock::region_bloom_settled(open_flag, &bloom_flags, &|f| {
            flags::get_event_flag(f)
        }) {
            continue; // fully bloomed -- reconcile-safe
        }
        let fired = clauses.iter().any(|cl| {
            cl.items.iter().all(|nm| received.contains(nm))
                && cl.flags.iter().all(|&fl| flags::get_event_flag(fl))
                && cl
                    .count_items
                    .iter()
                    .filter(|nm| received.contains(*nm))
                    .collect::<HashSet<_>>()
                    .len()
                    >= cl.count
        });
        if !fired {
            continue;
        }
        let mut n = 0u32;
        if let Some(fs) = cfg.region_graces.get(name) {
            for &f in fs {
                flags::set_event_flag(f, true);
                n += 1;
            }
        }
        flags::set_event_flag(open_flag, true);
        n += 1;
        if let Some(fs) = cfg.lock_reveal_flags.get(name) {
            for &f in fs {
                flags::set_event_flag(f, true);
                n += 1;
            }
        }
        log::info!("Natural-key '{name}' satisfied -> bloomed region ({n} flag(s) set)");
    }
}

/// `{ "LockName": { "anyOf": [ <clause>, ... ] } }` -> region -> clause disjunction.
///
/// CLAUSE WIRE SHAPE (all fields optional; a clause fires when every present term holds):
///   `{"items": [name, ..], "flags": [u32, ..], "countItems": [name, ..], "count": N}`
///   * `items`      — ALL must be in `received` (all-of).
///   * `flags`      — ALL event flags must read set.
///   * `countItems` + `count` — at least `count` DISTINCT names of `countItems` in `received`
///     (the COUNT primitive, 2026-07-24: Caelid 2-of-the-remembrances, Leyndell N Great Runes).
///     Absent fields default to `[]`/`0`, making the term vacuous — full backward compatibility
///     with the pre-count `{"items", "flags"}` shape. `{}` (or `{"items":[],"flags":[]}`) is an
///     ALWAYS-OPEN clause (blooms on first tick).
///
/// Ported from the standalone `net.rs::parse_natural_keys`.
fn parse_natural_keys(v: Option<&Value>) -> HashMap<String, Vec<NkClause>> {
    let mut m = HashMap::new();
    if let Some(Value::Object(o)) = v {
        for (region, body) in o {
            let mut clauses = Vec::new();
            if let Some(any_of) = body.get("anyOf").and_then(|x| x.as_array()) {
                for c in any_of {
                    let items = c
                        .get("items")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    let flags = c
                        .get("flags")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_u64().map(|n| n as u32))
                                .collect()
                        })
                        .unwrap_or_default();
                    let count_items = c
                        .get("countItems")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    let count = c.get("count").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                    clauses.push(NkClause {
                        items,
                        flags,
                        count_items,
                        count,
                    });
                }
            }
            m.insert(region.clone(), clauses);
        }
    }
    m
}

/// Per-tick: when the player enters a locked region, warp them out to Roundtable Hold (the
/// retired baked reactor's behavior, now done client-side via `warp::warp_to_grace`; kill only
/// as fallback). Evaluated EVERY tick; the rising-edge latch throttles the action to once per
/// sealed-region entry and re-arms once the warp lands the player back in an open region.
/// KICK_FLAG still set for bake-compat. Returns a player-facing overlay message when the kick
/// fires (the caller logs it -- players otherwise get relocated with no explanation).
///
/// The WORDS of that message live in `er_logic::region_lock::sealed_region_message`
/// (host-tested + ASCII-swept, house split: er-logic owns the words, this owns the state). All
/// this function contributes is the state -- which region, and which of the two exits ran. See
/// that module's MOTIVATING CASE note for why the message names the region and, for the three
/// vanilla-gated regions, the vanilla key.
pub fn tick_kick(cfg: &RegionConfig) -> Option<String> {
    let pr = flags::play_region_id()?;
    let kick = er_logic::region_lock::kick_decision(
        pr,
        &cfg.area_lock_flags,
        cfg.random_start_done_flag,
        &|f| flags::get_event_flag(f),
    );
    // KICK-WATCH (2026-07-02 diagnostic, keep -- cheap and this path has burned us twice): log
    // every play-region CHANGE with the full lock evaluation, so a silent no-kick session tells
    // us exactly what the client saw (id-space mismatch vs stale pr vs open-flag state).
    {
        let last = KICK_WATCH_LAST_PR.swap(pr, Ordering::Relaxed);
        if last != pr {
            let sub = if pr >= 1_000_000 { pr / 100 } else { pr };
            let hit = cfg
                .area_lock_flags
                .iter()
                .find(|e| sub >= e[0] && sub <= e[1]);
            let gate_open = cfg.random_start_done_flag == 0
                || flags::get_event_flag(cfg.random_start_done_flag);
            match hit {
                Some(e) => log::info!(
                    "kick-watch: play_region {last} -> {pr} (sub {sub}); range [{},{}] flag {} = {} | start-gate open = {gate_open} | kick = {kick}",
                    e[0],
                    e[1],
                    e[2],
                    flags::get_event_flag(e[2] as u32)
                ),
                None => log::info!(
                    "kick-watch: play_region {last} -> {pr} (sub {sub}); NO lock range covers it ({} ranges) | start-gate open = {gate_open}",
                    cfg.area_lock_flags.len()
                ),
            }
        }
    }
    if KICK_LATCH.lock().unwrap().fire(kick) {
        // PURE-RUNTIME KICK = WARP-OUT (2026-07-02, replaces the 2026-07-01 kill stopgap): the
        // baked reactor's actual behavior was "warp to Roundtable Hold", and the latch's
        // rising-edge model DEPENDS on the player leaving the sealed region -- a kill respawns
        // them at the last grace, which can be INSIDE the region, so kick stayed true, the
        // latch never re-armed, and enforcement was one death then free roam. Warping out also
        // retires the kick rune-loss wart (P1 kick-keep-runes). Kill remains only as the
        // fallback when the warp primitive is unavailable (stale RVA on a new game build).
        flags::set_event_flag(KICK_FLAG, true);
        // Resolve the region ONCE, from config we already hold (areaLockFlags range -> open flag
        // -> regionOpenFlags name, baked table as the fallback). No new slot_data key.
        let lock_item = er_logic::region_lock::sealed_lock_item(
            pr,
            &cfg.area_lock_flags,
            &cfg.region_open_flags,
        );
        let named = lock_item.unwrap_or("<unresolved>");
        match crate::warp::warp_to_grace(ROUNDTABLE_GRACE_ID) {
            Ok(()) => {
                log::info!(
                    "RegionLock: area {pr} ({named}) LOCKED -> kick warp to Roundtable (flag {KICK_FLAG} set)"
                );
                return Some(er_logic::region_lock::sealed_region_message(
                    pr,
                    lock_item,
                    er_logic::region_lock::SealedOutcome::WarpedToHub,
                ));
            }
            Err(e) => {
                let killed = crate::deathlink::kill_local_player();
                log::warn!(
                    "RegionLock: area {pr} ({named}) LOCKED -> kick warp FAILED ({e}); fallback kill (direct={killed})"
                );
                return Some(er_logic::region_lock::sealed_region_message(
                    pr,
                    lock_item,
                    er_logic::region_lock::SealedOutcome::KickFallback,
                ));
            }
        }
    }
    None
}

/// Per-tick: on a random-start seed, set the baked warp trigger ONCE when the player reaches the
/// rolled start area. Sets `randomStartDoneFlag` (persistent guard, also unblocks KICK) +
/// `randomStartWarpFlag` (the bake's `WarpPlayer` reactor keys on this). No-op on non-random seeds
/// (all three values are 0) or after the warp has fired. Mirrors the standalone `features.rs` latch.
/// Returns a player-facing overlay message on warp request / trigger consumption (the caller
/// logs it).
///
/// SEMANTICS (corrected 2026-07-02): `randomStartAreaId` is the TRIGGER area, not the
/// destination -- REGION_ID_MAP.md: 18000 = Stranded Graveyard / Chapel of Anticipation
/// (tutorial), annotated "= randomStartAreaId"; Roundtable Hold is 11100. Baked-era flow: a
/// FRESH character spawns in the tutorial (18000), the client sets the trigger flags there, and
/// the bake's WarpPlayer reactor warped them OUT to the rolled start. The first port of this
/// function misread the id as the destination and warped the player TO the hub whenever they
/// were anywhere else -- i.e. always (seen live: 3x re-warp to Roundtable mid-run, cap, kick
/// gated forever). Pure-runtime flow now:
///   - pr == trigger area (fresh character in the tutorial): set done+warp flags, then
///     physically warp to the hub/rolled grace (the reactor's job, ours now).
///   - pr != trigger area with done unset (established character, e.g. a save from before this
///     fix): the start already happened -- consume the trigger WITHOUT warping, which arms KICK.
pub fn tick_random_start_warp(cfg: &RegionConfig) -> Option<String> {
    if cfg.random_start_warp_flag == 0
        || cfg.random_start_area_id == 0
        || cfg.random_start_done_flag == 0
    {
        return None; // not a random-start seed
    }
    if flags::get_event_flag(cfg.random_start_done_flag) {
        return None; // trigger already consumed (persisted across sessions)
    }
    if START_LATCHED.load(Ordering::Relaxed) {
        return None; // consumed this session; the persistent flag lands with the next save-sync
    }
    let pr = flags::play_region_id()?;
    // Interior play regions are 7-digit (bucket*100 + sub) -- normalize to the 5-digit bucket
    // slot_data speaks, the SAME rule kick_decision applies.
    let pr = if pr >= 1_000_000 { pr / 100 } else { pr };

    // Settle window: don't trust the play region until in_world has been continuously true for
    // WARP_SETTLE_SECS (stale pr right after a load). Resets on every menu/load.
    {
        let mut settle = WARP_WORLD_SETTLE.lock().unwrap();
        if !crate::flags::in_world() {
            *settle = None;
            return None;
        }
        let entered = settle.get_or_insert_with(std::time::Instant::now);
        if entered.elapsed() < std::time::Duration::from_secs(WARP_SETTLE_SECS) {
            return None;
        }
    }

    // R4 (SWEEP): only latch once the flag writes verifiably stuck (a discarded write would
    // otherwise keep KICK's start-window guard closed all session). Both branches consume the
    // trigger the same way; they differ only in whether a physical warp follows.
    let _ = flags::try_set_event_flag(cfg.random_start_done_flag, true);
    let _ = flags::try_set_event_flag(cfg.random_start_warp_flag, true);
    if !flags::get_event_flag(cfg.random_start_done_flag)
        || !flags::get_event_flag(cfg.random_start_warp_flag)
    {
        return None; // flag holder not ready -- retry next tick
    }
    START_LATCHED.store(true, Ordering::Relaxed);
    log::info!(
        "RandomStart: trigger consumed in area {pr} (done {} / warp {})",
        cfg.random_start_done_flag,
        cfg.random_start_warp_flag
    );

    if pr != cfg.random_start_area_id {
        // Established character already out in the world: no warp, just arm enforcement.
        return Some("Region-lock enforcement armed.".to_string());
    }

    // Fresh character in the tutorial: do the retired reactor's job and warp them out.
    let target = if cfg.random_start_grace_id != 0 {
        cfg.random_start_grace_id
    } else {
        // apworld doesn't emit randomStartGraceId yet; the Roundtable-hub mode is the only
        // shipping random-start flavor, so its grace is the fallback destination.
        ROUNDTABLE_GRACE_ID
    };
    match crate::warp::warp_to_grace(target) {
        Ok(()) => {
            log::info!("RandomStart: fresh start -> warp to grace {target} requested");
            Some("Warping to your start region...".to_string())
        }
        Err(e) => {
            log::warn!(
                "RandomStart: start warp to grace {target} FAILED ({e}) -- travel out manually (trigger already consumed, enforcement armed)"
            );
            Some("Auto-warp failed -- travel to your start region manually.".to_string())
        }
    }
}

/// Per-tick (settled / in-world): reconcile received lock items whose region never actually
/// opened. `open_on_received_name` fires ONCE per receive and its flag writes are silently
/// discarded when the game isn't ready (menu/load) -- the dispatch watermark advances anyway, so
/// the unlock (open flag + graces + reveals) was LOST for the session (seen live 2026-07-01:
/// lock received, no graces). The region open flag doubles as the latch, so this is idempotent
/// and cheap once applied. Same pattern as `tick_natural_key_triggers`.
/// KNOWN EDGE: a PARTIAL application (open flag landed, graces lost mid-batch) latches and won't
/// re-heal -- rare, since a not-ready game discards the whole batch together.
pub fn tick_reconcile_received_locks(cfg: &RegionConfig, received: &HashSet<String>) {
    // Menu/load gate (2026-07-01 playtest: retry-SPAMMED at menu -- the caller's can_grant
    // (inventory) resolves before flag writes stick, so every re-apply was discarded and
    // re-logged per tick). in_world() is the same signal the other flag writers gate on.
    if !crate::flags::in_world() {
        return;
    }
    for (name, &open_flag) in &cfg.region_open_flags {
        if !received.contains(name) || flags::get_event_flag(open_flag) {
            continue;
        }
        log::info!("RegionLock '{name}': received but never applied -- reconciling");
        open_on_received_name(cfg, name);
    }
    // BUNDLE_LOCK_GRACE_RECONCILE: grace-only bundle locks (Spelunker torches) have NO region_open_flags
    // entry, so the loop above never reconciles them -- a grant lost to a not-ready receive
    // stays lost (2026-07-04 softlock: Ghostflame Torch was the sole sphere-0 key). Re-apply
    // each received lock's graces directly, using every grace flag as its own try_set latch
    // (idempotent; only the unset flags re-try). Also heals the PARTIAL-application edge for
    // open-flag locks (open flag landed, some graces lost mid-batch) that the loop above skips.
    for (name, fs) in &cfg.region_graces {
        if !received.contains(name) {
            continue;
        }
        for &f in fs {
            if !flags::get_event_flag(f) {
                let _ = flags::try_set_event_flag(f, true);
            }
        }
    }
}

/// Per-tick (settled / in-world): light received grace_rando "Grace: ..." items. PORT-GAP wired
/// 2026-07-01: `graceItems` was emitted but consumed by NOTHING (its client half was retired with
/// the C++ client), so grace items granted from the pool did nothing in-game. Reconciled with the
/// grace flag itself as the latch, and try_set (only latch on a successful write) so a receive at
/// menu/load self-heals next settled tick. Returns names lit this tick for the overlay console.
pub fn tick_grace_items(cfg: &RegionConfig, received: &HashSet<String>) -> Vec<String> {
    let mut lit = Vec::new();
    for (name, &flag) in &cfg.grace_items {
        if received.contains(name)
            && !flags::get_event_flag(flag)
            && flags::try_set_event_flag(flag, true)
        {
            log::info!("GraceItem '{name}' -> grace flag {flag} lit");
            lit.push(name.clone());
        }
    }
    lit
}

/// One region's grace-attunement gate. `members` are the grace flags that COUNT toward attunement
/// (the region's graces minus the anchor it was already given); `bloom` are the ones lit once the
/// threshold is met.
#[derive(Debug, Clone, Default)]
pub struct GraceGate {
    pub threshold: u32,
    pub members: Vec<u32>,
    pub bloom: Vec<u32>,
}

/// `{ "<lock name>": {"threshold": N, "members": [flag,...], "bloom": [flag,...]} }`.
/// Tolerant like every other slot_data parse here: a malformed entry is skipped, not fatal.
fn parse_grace_attunement(v: Option<&Value>) -> HashMap<String, GraceGate> {
    let mut out = HashMap::new();
    let Some(obj) = v.and_then(|v| v.as_object()) else {
        return out;
    };
    for (name, entry) in obj {
        let Some(e) = entry.as_object() else { continue };
        let nums = |k: &str| -> Vec<u32> {
            e.get(k)
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|n| n.as_u64())
                        .map(|n| n as u32)
                        .collect()
                })
                .unwrap_or_default()
        };
        let gate = GraceGate {
            threshold: e.get("threshold").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            members: nums("members"),
            bloom: nums("bloom"),
        };
        // A gate with nothing to bloom is not a gate. Dropping it here keeps the tick honest and
        // means the census below counts only regions that can actually do something.
        if !gate.bloom.is_empty() {
            out.insert(name.clone(), gate);
        }
    }
    out
}

/// Per-tick (settled / in-world): light the rest of a region's graces once enough of them have been
/// touched. Returns the regions that bloomed THIS tick, for the overlay console.
///
/// 🛑 THE BLOOM FLAGS ARE THEIR OWN LATCH. We only write a flag that is currently OFF, and we only
/// report a region as bloomed when a write actually landed -- so a reconnect, a save-load, or a
/// second tick in the same frame cannot re-fire the banner or double-grant. Same discipline (and
/// same reason) as `tick_grace_items` above.
pub fn tick_grace_attunement(cfg: &RegionConfig) -> Vec<String> {
    let mut bloomed = Vec::new();
    for (name, gate) in &cfg.grace_attunement {
        // Only gate a region the player can actually be in: an unopened region's graces are all
        // off, so it can never reach threshold anyway -- this is just an early out.
        let touched = er_logic::attunement::attuned_count(
            &gate.members.iter().map(|&f| f as i64).collect(),
            |f| flags::get_event_flag(f as u32),
        );
        if touched < gate.threshold {
            continue;
        }
        let mut lit = 0usize;
        for &flag in &gate.bloom {
            if !flags::get_event_flag(flag) && flags::try_set_event_flag(flag, true) {
                lit += 1;
            }
        }
        if lit > 0 {
            log::info!(
                "grace-attunement: '{name}' attuned ({touched}/{} touched) -- lit {lit} more grace(s)",
                gate.threshold
            );
            bloomed.push(name.clone());
        }
    }
    bloomed
}

/// lockGrantItems rider check: the packed FullIDs to grant for `name`, but ONLY when this is the
/// lock's FIRST open (its open flag is still OFF -- the same once-latch the natural-key bloom
/// uses). Call BEFORE `open_on_received_name` (which sets the flag). Reconnect replays re-run
/// `on_item_received` for every item; the latch keeps the physical grant once-per-save.
pub fn first_open_grants(cfg: &RegionConfig, name: &str) -> Vec<i32> {
    match (
        cfg.lock_grant_items.get(name),
        cfg.region_open_flags.get(name),
    ) {
        (Some(ids), Some(&f)) if !flags::get_event_flag(f) => ids.clone(),
        _ => Vec::new(),
    }
}

/// On receiving an unlock item (by name): open its region + reveal/grace flags. Idempotent. Returns
/// true if `name` is a region-lock item (so the caller can surface a console notification).
pub fn open_on_received_name(cfg: &RegionConfig, name: &str) -> bool {
    let mut opened = false;
    if let Some(&f) = cfg.region_open_flags.get(name) {
        flags::set_event_flag(f, true);
        log::info!("RegionLock '{name}' received -> open flag {f}");
        opened = true;
    }
    if let Some(fs) = cfg.lock_reveal_flags.get(name) {
        for &f in fs {
            flags::set_event_flag(f, true);
        }
    }
    if let Some(fs) = cfg.region_graces.get(name) {
        // INSTRUMENT (patch_log_grace_readback): set each grace flag, then read it back so the
        // log pins whether these writes actually land in EventFlagMan.
        let mut set = 0usize;
        let mut failed: Vec<u32> = Vec::new();
        for &f in fs {
            flags::set_event_flag(f, true);
            if flags::get_event_flag(f) {
                set += 1;
            } else {
                failed.push(f);
            }
        }
        log::info!(
            "RegionLock '{name}' graces: {} requested, {} set, {} failed{}",
            fs.len(),
            set,
            failed.len(),
            if failed.is_empty() {
                String::new()
            } else {
                format!(" = {failed:?}")
            }
        );
    } else if opened {
        // Only a genuine region-lock (its open flag matched) that is missing its grace bundle is a
        // real drift worth flagging. A normal non-lock item (opened == false) is silently ignored --
        // this function is called for EVERY received item to test lock-ness, so without this guard
        // every filler/gear pickup spammed a false "NO region_graces entry" warning (264 in one run).
        log::warn!(
            "RegionLock '{name}': NO region_graces entry (cfg.region_graces empty or key mismatch)"
        );
    }
    opened
}

// --- baked-table region-lock fallback (bedrock interop) ------------------------------------------
// Decision logic + the WHY of the arming rule live in er_logic::region_lock (host-tested); these
// three are the game-side glue. Flow: core.rs gates on `foreign_seed_without_region_keys`, calls
// `prepare_baked_fallback` at connect with the datapackage-resolved names of the seed's
// apIdsToItemIds entries, and `tick_baked_fallback` each tick with the cumulative received-name
// set. Nothing is enforced until a scoped "<Region> Lock" is actually RECEIVED (measured on the
// real foreign apworld: its item table carries lock NAMES even on no-lock seeds, so table
// presence alone must never arm -- the foreign_apworld_degrade contract).

/// True when slot_data speaks NEITHER region-lock key -- the apworld is region-lock-ignorant
/// (Bedrock-shaped), so the baked fallback MAY apply. Key PRESENCE is the test, not emptiness:
/// an apworld that emits `regionOpenFlags` without `areaLockFlags` made a choice (see the note
/// at `parse`) and we do not second-guess it with baked geometry.
pub fn foreign_seed_without_region_keys(sd: &Value) -> bool {
    sd.get("areaLockFlags").is_none() && sd.get("regionOpenFlags").is_none()
}

/// Derive + stash (do NOT arm) the baked fallback from the seed's item names. Unknown
/// "<X> Lock" granularities and geometry-only (flagless) regions are logged and dropped --
/// never a panic, never a guessed region.
pub fn prepare_baked_fallback<'a>(
    cfg: &mut RegionConfig,
    seed_item_names: impl IntoIterator<Item = &'a str>,
) {
    let d = er_logic::region_lock::derive_region_locks(seed_item_names);
    if !d.unknown.is_empty() {
        log::warn!(
            "baked region-lock fallback: unknown lock name(s) {:?} -- a foreign region granularity this client cannot map; ignored (rename to the baked '<Region> Lock' names to enforce them)",
            d.unknown
        );
    }
    if !d.ungateable.is_empty() {
        log::warn!(
            "baked region-lock fallback: {:?} name baked region(s) with no resolved open flag --              cannot gate; ignored",
            d.ungateable
        );
    }
    if d.is_empty() {
        return;
    }
    log::info!(
        "baked region-lock fallback PREPARED: {} region(s), {} kick range(s) -- COLD until a scoped '<Region> Lock' is received",
        d.open_flags.len(),
        d.ranges.len()
    );
    cfg.baked_fallback = Some(d);
}

/// Arm the prepared fallback once ANY scoped lock has been RECEIVED, merging the derived config
/// into the live fields every existing path already consumes (`open_on_received_name` opens on
/// the merged name->flag map, `tick_kick` watches the merged ranges, the reconcile ticks
/// self-heal lost writes). One-shot: the stash is consumed on arming. A scoped region whose lock
/// never arrives afterwards stays sealed -- that is a sealed region, not a special case.
pub fn tick_baked_fallback(cfg: &mut RegionConfig, received: &HashSet<String>) -> bool {
    let armed = cfg
        .baked_fallback
        .as_ref()
        .is_some_and(|d| er_logic::region_lock::fallback_armed(d, received));
    if !armed {
        return false;
    }
    let d = cfg.baked_fallback.take().expect("armed implies prepared");
    for (name, flag) in &d.open_flags {
        cfg.region_open_flags.entry(name.clone()).or_insert(*flag);
    }
    cfg.area_lock_flags.extend(d.ranges.iter().copied());
    log::info!(
        "baked region-lock fallback ARMED: {} region(s), {} kick range(s) (first scoped Lock          received)",
        d.open_flags.len(),
        d.ranges.len()
    );
    true
}

// --- slot_data parse helpers (shapes from the standalone net.rs) ---------------------------------

fn parse_triples(v: Option<&Value>) -> Vec<[i32; 3]> {
    v.and_then(|v| v.as_array())
        .map(|outer| {
            outer
                .iter()
                .filter_map(|row| row.as_array())
                .filter(|r| r.len() >= 3)
                .map(|r| {
                    [
                        r[0].as_i64().unwrap_or(0) as i32,
                        r[1].as_i64().unwrap_or(0) as i32,
                        r[2].as_i64().unwrap_or(0) as i32,
                    ]
                })
                .collect()
        })
        .unwrap_or_default()
}

fn str_to_u32(v: Option<&Value>) -> HashMap<String, u32> {
    let mut m = HashMap::new();
    if let Some(Value::Object(o)) = v {
        for (k, val) in o {
            if let Some(n) = val.as_u64() {
                m.insert(k.clone(), n as u32);
            }
        }
    }
    m
}

fn str_to_i32vec(v: Option<&Value>) -> HashMap<String, Vec<i32>> {
    // lockGrantItems values are GOODS-packed FullIDs (er_code | 0x40000000), all < i32::MAX.
    let mut m = HashMap::new();
    if let Some(Value::Object(o)) = v {
        for (k, val) in o {
            if let Some(arr) = val.as_array() {
                m.insert(
                    k.clone(),
                    arr.iter()
                        .filter_map(|x| x.as_i64().map(|n| n as i32))
                        .collect(),
                );
            }
        }
    }
    m
}

fn str_to_u32vec(v: Option<&Value>) -> HashMap<String, Vec<u32>> {
    let mut m = HashMap::new();
    if let Some(Value::Object(o)) = v {
        for (k, val) in o {
            if let Some(arr) = val.as_array() {
                m.insert(
                    k.clone(),
                    arr.iter()
                        .filter_map(|x| x.as_u64().map(|n| n as u32))
                        .collect(),
                );
            }
        }
    }
    m
}

// --- capital-version reconciler (SPEC-capital-reconciler.md; apworld features/capital.py) -------
// Leyndell is TWO mutually exclusive map versions on one save-persisted flag (9116: OFF = Royal
// m11_00, ON = Ashen m11_05 + Elden Throne m19), and vanilla only ever SETS it (Maliketh's
// death), so the Erdtree burn permanently strands the ~152 Royal checks. The DECISIONS are pure
// er-logic (`er_logic::capital`, host-tested by `capital_replay`); this is the game-side glue:
// parse the five `capital*` slot_data keys at connect, hold the per-tick latch in `tick_capital`,
// and write 9116 from the warp TARGET in `capital_warp_intercept` (called by
// `warp::warp_to_grace`, so kick / random-start / `!warp` all get the intercept). Everything is
// armed on the burn-done latch (118, monotonic): the first burn stays 100% the game's own
// sequence. Reconcile-don't-dispatch: write on readback mismatch only, re-apply per tick until
// it sticks, never advance past an unverified write. The shop release re-key rides
// `shop_flags::run_capital_release` (configured here so the five keys stay one parse).

/// Parsed capital-reconciler config (None = INERT: option off / old apworld / malformed keys).
static CAPITAL: Mutex<Option<er_logic::capital::CapitalConfig>> = Mutex::new(None);
/// One-time telemetry latch: log "capital reconciler armed" the first time the burn-done flag
/// reads set in a session (re-armed on each configure).
static CAPITAL_ARMED_LOGGED: AtomicBool = AtomicBool::new(false);

/// States a per-tick decline once per CHANGE, not once per tick (client#200).
///
/// The latch runs every frame and the answer is `AlreadyCorrect` almost always -- bobler's whole
/// 2026-08-15 log is that case, 66 warps' worth, and it produced no evidence because nothing
/// logged it. A transition is the event worth a line; `ContradictedOn` appearing at all is the
/// event this exists for.
static CAPITAL_DECLINE: Mutex<er_logic::capital_guard::DeclineLatch> =
    Mutex::new(er_logic::capital_guard::DeclineLatch::new());

/// Warp-target decision plus the play-region observed when the asynchronous warp began. While the
/// position reader still reports that source, the target wins over the stale position.
static CAPITAL_PENDING_WARP: Mutex<Option<(Option<i32>, bool)>> = Mutex::new(None);

/// Called by core.rs once slot_data is parsed (beside `region::parse`). The five `capital*`
/// keys travel together; absent keys are the off-wire (`capital_reconciler: false`, or an
/// apworld that predates the feature) -- the client logs INERT and never touches 9116.
pub fn configure_capital(sd: &Value) {
    CAPITAL_ARMED_LOGGED.store(false, Ordering::Relaxed);
    *CAPITAL_PENDING_WARP.lock().unwrap() = None;
    let cfg = er_logic::capital::parse(sd);
    match &cfg {
        Some(c) => log::info!(
            "capital reconciler configured: burn flag {}, world-burn {:?}, pre-burn {:?}, done latch {}, ashen {:?}, royal {:?}, {} release row(s)",
            c.burn_flag,
            c.world_burn_flag,
            c.pre_burn_flag,
            c.burn_done_flag,
            c.sets.ashen,
            c.sets.royal,
            c.release_rows.len()
        ),
        None => log::info!(
            "capital reconciler INERT: capital* slot_data keys absent (option off or old apworld)"
        ),
    }
    crate::shop_flags::configure_capital_release(
        cfg.as_ref()
            .map(|c| c.release_rows.clone())
            .unwrap_or_default(),
    );
    *CAPITAL.lock().unwrap() = cfg;
}

fn capital_state(cfg: &er_logic::capital::CapitalConfig) -> er_logic::capital::CapitalState {
    er_logic::capital::CapitalState {
        burn: flags::get_event_flag(cfg.burn_flag),
        world_burn: cfg.world_burn_flag.map(flags::get_event_flag),
        pre_burn: cfg.pre_burn_flag.map(flags::get_event_flag),
    }
}

fn apply_capital_writes(
    cfg: &er_logic::capital::CapitalConfig,
    writes: er_logic::capital::CapitalWrites,
) -> bool {
    // Entering Ashen follows common.emevd's safe order: retire the opposite state, establish the
    // burnt world, then select its map. That prevents an Ashen load without the finale arena.
    if let (Some(flag), Some(value)) = (cfg.pre_burn_flag, writes.pre_burn) {
        let _ = flags::try_set_event_flag(flag, value);
    }
    if let (Some(flag), Some(value)) = (cfg.world_burn_flag, writes.world_burn) {
        let _ = flags::try_set_event_flag(flag, value);
    }
    if let Some(value) = writes.burn {
        let _ = flags::try_set_event_flag(cfg.burn_flag, value);
    }

    writes.pre_burn.is_none_or(|value| {
        cfg.pre_burn_flag
            .is_none_or(|f| flags::get_event_flag(f) == value)
    }) && writes.world_burn.is_none_or(|value| {
        cfg.world_burn_flag
            .is_none_or(|f| flags::get_event_flag(f) == value)
    }) && writes
        .burn
        .is_none_or(|value| flags::get_event_flag(cfg.burn_flag) == value)
}

/// Per-tick capital-state latch: standing in Ashen/Throne holds 9116 + world-burn ON and pre-burn
/// OFF; every other known position holds the reversible selectors OFF. Defends against the goal
/// gate or vanilla flipping the state mid-session. Self-configured; call every update tick.
pub fn tick_capital() {
    let guard = CAPITAL.lock().unwrap();
    let Some(cfg) = guard.as_ref() else { return };
    if !crate::flags::in_world() {
        return; // menu/load: neither play_region_id nor the flag holder is trustworthy
    }
    let armed = flags::get_event_flag(cfg.burn_done_flag);
    if !armed {
        return; // pre-burn: 9116-OFF *is* vanilla, and mid-burn a write would fight $Event(900)
    }
    if !CAPITAL_ARMED_LOGGED.swap(true, Ordering::Relaxed) {
        log::info!(
            "capital reconciler armed: burn-done flag {} is set -- {} now reconciled to the player's capital",
            cfg.burn_done_flag,
            cfg.burn_flag
        );
    }
    let play_region = flags::play_region_id();
    let position_desired =
        play_region.and_then(|pr| er_logic::capital::capital_flag_state(&cfg.sets, pr));
    let (desired, explicit_warp_target) = {
        let mut pending = CAPITAL_PENDING_WARP.lock().unwrap();
        match *pending {
            Some((source, warp_desired)) => {
                let (desired, keep) = er_logic::capital_guard::desired_across_warp(
                    source,
                    play_region,
                    warp_desired,
                    position_desired,
                );
                if !keep {
                    *pending = None;
                }
                (desired, keep)
            }
            None => (position_desired, false),
        }
    };
    let current = capital_state(cfg);
    // A warp target is explicit player intent. A position is only an observation, and may be the
    // wrong map version produced by stale flags. Do not let an unburnt-world contradiction turn
    // itself into permanent Ashen save state (capital_guard/client#200).
    let decision = if explicit_warp_target {
        er_logic::capital_guard::decide(armed, desired, current.burn)
    } else {
        er_logic::capital_guard::decide_from_position(
            armed,
            desired,
            current.burn,
            current.world_burn,
        )
    };
    let guarded_desired = match decision {
        er_logic::capital_guard::Decision::Write(want)
        | er_logic::capital_guard::Decision::Declined(
            er_logic::capital_guard::Decline::AlreadyCorrect(want),
        ) => Some(want),
        er_logic::capital_guard::Decision::Declined(_) => None,
    };
    let writes = er_logic::capital::reconcile_state(armed, guarded_desired, current);
    if writes != er_logic::capital::CapitalWrites::default() {
        if let Ok(mut l) = CAPITAL_DECLINE.lock() {
            l.on_write();
        }
        let stuck = apply_capital_writes(cfg, writes);
        log::info!(
            "capital reconcile: writes {:?} (play_region {:?}, before {:?}); readback {}",
            writes,
            flags::play_region_id(),
            current,
            if stuck {
                "STUCK (the writes took)"
            } else {
                "PENDING (a write was lost) -- re-applying next tick"
            }
        );
    } else if let er_logic::capital_guard::Decision::Declined(d) = decision {
        let admitted = CAPITAL_DECLINE.lock().ok().and_then(|mut l| l.admit(d));
        if let Some(d) = admitted {
            log::info!(
                "capital reconcile: no write (play_region {:?}, state {:?}) -- {}",
                flags::play_region_id(),
                current,
                d.reason()
            );
        }
    }
}

/// Warp-target intercept: decide 9116 from the TARGET before the load resolves, so the player
/// always loads the capital version encoded by the selected target. Ashen-map graces -> ON;
/// Royal-map graces and every other resolvable target -> OFF. Called by
/// `warp::warp_to_grace` right after the warp request (the warp is asynchronous; the write
/// lands before the load screen resolves). No-op while INERT or pre-burn.
pub fn capital_warp_intercept(warp_target: u32) {
    let guard = CAPITAL.lock().unwrap();
    let Some(cfg) = guard.as_ref() else { return };
    let armed = flags::get_event_flag(cfg.burn_done_flag);
    let desired = er_logic::capital::capital_flag_state_for_warp_target(&cfg.sets, warp_target);
    let current = capital_state(cfg);
    if let Some(want) = desired {
        *CAPITAL_PENDING_WARP.lock().unwrap() = Some((flags::play_region_id(), want));
    }
    // A warp TARGET is explicit player intent. Apply the whole state before the asynchronous load
    // resolves so an Ashen target gets its arena and every other target restores Royal.
    let writes = er_logic::capital::reconcile_state(armed, desired, current);
    if writes != er_logic::capital::CapitalWrites::default() {
        let stuck = apply_capital_writes(cfg, writes);
        log::info!(
            "capital warp intercept: target {warp_target} -> writes {:?} (before {:?}); readback {}",
            writes,
            current,
            if stuck {
                "STUCK (the writes took)"
            } else {
                "PENDING (a write was lost) -- the per-tick latch converges only in a capital bucket"
            }
        );
    } else {
        match er_logic::capital_guard::decide(armed, desired, current.burn) {
            // ⭐ ONE LINE PER DECLINED WARP, unconditionally. bobler's log carries 66 warps and not one
            // intercept line, because all 66 were `AlreadyCorrect` and nothing said so -- which was
            // indistinguishable from the reconciler being inert. 66 lines a session is nothing.
            er_logic::capital_guard::Decision::Declined(d) => log::info!(
                "capital warp intercept: target {warp_target} -> no write -- {}",
                d.reason()
            ),
            er_logic::capital_guard::Decision::Write(_) => {
                unreachable!("state planner lost a write")
            }
        }
    }
}

#[cfg(test)]
mod foreign_apworld_degrade {
    //! A FOREIGN APWORLD MUST YIELD A PLAYABLE VANILLA SEED, NOT AN ERROR.
    //!
    //! Bedrock's apworld (fswap/archipelago@er) emits none of the region-lock keys -- it has no
    //! region lock at all; `region lock ideas.md` is a wishlist asking someone else to build one.
    //! Alaric promised him, in writing: "when these arguments aren't present, they fall back to
    //! vanilla behaviour". THESE TESTS ARE THAT PROMISE. If one fails, we have silently made our
    //! client refuse to drive anyone else's world.
    //!
    //! Vanilla = every region open, no kick-watch, no random start, no warp.
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_slot_data_yields_vanilla_region_config() {
        let c = parse(&json!({}));
        assert!(
            c.region_open_flags.is_empty(),
            "no regionOpenFlags => no locks; every region open"
        );
        assert!(
            c.area_lock_flags.is_empty(),
            "no areaLockFlags => NO kick-watch. A foreign seed must never be kicked out of a region \
             it does not know is locked."
        );
        assert!(c.lock_reveal_flags.is_empty());
        assert_eq!(
            c.random_start_done_flag, 0,
            "0 = non-random seed, no start guard"
        );
        assert_eq!(c.random_start_warp_flag, 0, "0 = no random start");
        assert_eq!(c.random_start_area_id, 0);
        assert_eq!(c.random_start_grace_id, 0);
    }

    #[test]
    fn a_bedrock_shaped_slot_data_parses_and_stays_vanilla() {
        // Exactly the keys Bedrock's fill_slot_data emits (his words, 2026-07-06): item map, the
        // matt key table, and the goal. No regionOpenFlags, no startRegion, no areaLockFlags.
        let sd = json!({
            "apIdsToItemIds":   { "7770001": 1073750026u64 },
            "locationIdsToKeys":{ "7770001": "301200,0:0000520110::" },
            "goalLocations":    [7770875, 7770876],
        });
        let c = parse(&sd);
        assert!(c.region_open_flags.is_empty());
        assert!(c.area_lock_flags.is_empty());
        assert_eq!(c.random_start_warp_flag, 0);
    }

    #[test]
    fn a_partial_region_table_does_not_invent_kick_ranges() {
        // regionOpenFlags present but areaLockFlags absent: we must NOT derive kick ranges
        // client-side (see the note at parse()). No table => no enforcement, by design.
        let sd = json!({ "regionOpenFlags": { "Caelid Lock": 73202u64 } });
        let c = parse(&sd);
        assert_eq!(c.region_open_flags.len(), 1);
        assert!(
            c.area_lock_flags.is_empty(),
            "kick ranges are the generator's job; deriving them here would enforce a lock the \
             apworld never asked for"
        );
        // ...and the baked fallback is out of bounds too: the seed SPOKE a region key.
        assert!(
            !foreign_seed_without_region_keys(&sd),
            "a seed that emits either region key never gets baked geometry"
        );
    }

    // --- the baked-table fallback keeps the same promise -----------------------------------
    // (pure derivation + arming rule are host-tested in er_logic::region_lock::derive_tests;
    // these cover the glue: gating, cold-until-receipt, and the merge.)

    /// A real baked lock name + flag, read from the generated table (never hand-pinned).
    fn baked_example() -> (&'static str, u32) {
        let r = er_logic::region_locks::REGION_LOCKS
            .iter()
            .find(|r| r.open_flag.is_some())
            .expect("baked table has flagged regions");
        (r.lock_item, r.open_flag.unwrap())
    }

    #[test]
    fn fallback_gate_is_key_presence_not_content() {
        assert!(foreign_seed_without_region_keys(&json!({})));
        assert!(foreign_seed_without_region_keys(&json!({
            "apIdsToItemIds": { "7770001": 1073750026u64 },
            "locationIdsToKeys": { "7770001": "301200,0:0000520110::" },
        })));
        // Emitted-but-empty still counts as SPOKEN.
        assert!(!foreign_seed_without_region_keys(
            &json!({ "areaLockFlags": [] })
        ));
        assert!(!foreign_seed_without_region_keys(
            &json!({ "regionOpenFlags": {} })
        ));
    }

    #[test]
    fn prepared_fallback_stays_cold_until_a_scoped_lock_is_received() {
        // The degrade promise, fallback edition: a foreign seed whose item TABLE names locks
        // (the real one always does -- it ships its whole item table) must not kick anyone
        // until a lock is RECEIVED. Names here: one real baked name + one synthetic foreign
        // granularity (hand-written shape, not the foreign world's data).
        let (lock, flag) = baked_example();
        let mut c = parse(&json!({}));
        prepare_baked_fallback(&mut c, [lock, "Zzz Nonexistent Region Lock", "Uchigatana"]);
        assert!(c.baked_fallback.is_some(), "scoped");
        assert!(
            c.area_lock_flags.is_empty(),
            "COLD: no kick-watch before receipt"
        );
        assert!(c.region_open_flags.is_empty());

        let mut received: HashSet<String> = ["Uchigatana".to_string()].into();
        assert!(
            !tick_baked_fallback(&mut c, &received),
            "non-lock receipts never arm"
        );
        assert!(c.area_lock_flags.is_empty());

        received.insert(lock.to_string());
        assert!(
            tick_baked_fallback(&mut c, &received),
            "first scoped lock arms"
        );
        assert_eq!(c.region_open_flags.get(lock), Some(&flag));
        assert!(!c.area_lock_flags.is_empty(), "kick-watch live");
        assert!(c.area_lock_flags.iter().all(|r| r[2] as u32 == flag));
        assert!(
            !tick_baked_fallback(&mut c, &received),
            "one-shot: stash consumed"
        );
    }

    #[test]
    fn a_seed_with_no_lock_names_never_prepares_never_arms() {
        let mut c = parse(&json!({}));
        prepare_baked_fallback(&mut c, ["Uchigatana", "Golden Seed"]);
        assert!(c.baked_fallback.is_none(), "nothing scoped");
        let received: HashSet<String> = ["Caelid Lock".to_string()].into();
        assert!(
            !tick_baked_fallback(&mut c, &received),
            "no prepared scope -> nothing to arm, whatever arrives"
        );
        assert!(c.area_lock_flags.is_empty() && c.region_open_flags.is_empty());
    }
}
