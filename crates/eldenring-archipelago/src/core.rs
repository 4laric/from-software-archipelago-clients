//! MILESTONE B — increment #7 (ECHO model, own_world:true). Supersedes increment #6c's core.
//!
//! With `own_world:true`, the server echoes our own checks back as received items, so the
//! received-item path is the SINGLE grant path (and it runs progressive / region-open / notify by
//! name for self-found items too). The detour + inventory-scan therefore only REPORT checks
//! (mark_checked) and suppress; they no longer grant locally. This fixes self-found progressive,
//! region keys, and notify items, which the old local-grant path silently skipped.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use archipelago_rs as ap;
use er_logic::hook::GameHook;
use er_logic::progressive::ProgressiveState;
use er_logic::receive::{GrantAction, RecvItem};
use er_logic::save_state::SaveState;
use er_logic::tracker::{HintEntry, HintSet};
use serde_json::Value;
use shared::Core as _;
use shared::CoreBase;

use crate::hook_impl::{EldenRingHook, ReceiveDispatch};

/// Tint for hinted lines in the tracker window (matches the overlay's YELLOW, 0xFCE94F).
const HINT_YELLOW: [f32; 4] = [0.9882, 0.9137, 0.3098, 1.0];

/// Tint for progression-surface check lines in the tracker window (soft orange).
const SURFACE_ORANGE: [f32; 4] = [0.9882, 0.6863, 0.2431, 1.0];

/// Dim gray for locked-region headers in the tracker window (mirrors imgui's TextDisabled).
const LOCKED_GRAY: [f32; 4] = [0.5, 0.5, 0.5, 1.0];

/// Parsed `regionAttunement` entry (attunement_gate, SPEC-gf-boss-lock-tracker). Absent/empty
/// slot_data => the feature is off. `members` are the region's freely-reachable in-region check AP
/// ids (the attunement denominator); `bloom_flags` are the graces revealed on attunement.
#[derive(Debug, Clone, Default)]
struct RegionAttunement {
    threshold: u32,
    members: HashSet<i64>,
    bloom_flags: Vec<u32>,
}

pub struct Core {
    base: CoreBase<crate::game::EldenRing, Value>,
    detour_installed: bool,
    received_through: usize,
    dispatched_through: usize,
    /// DIAGNOSTIC ONLY (2026-08-02, issues #293 / #296). Throttles the receive-path state line and
    /// owns the F2 "cursor ahead of stream" alarm. See `er_logic::receive_probe` for why the
    /// stream length beside the cursor is the line that discriminates the three freeze causes.
    /// Changes no delivery behaviour.
    recv_probe: er_logic::receive_probe::Probe,
    item_map: Option<HashMap<i64, i64>>,
    item_counts: HashMap<i64, i64>,
    region: Option<crate::region::RegionConfig>,
    /// Region-lock fog-wall visuals (cosmetic; KICK still enforces).
    fogwall: Option<crate::fogwall::FogWallConfig>,
    progressive: ProgressiveState,
    slot_data_parsed: bool,
    /// The room `seed_name` the current slot_data was parsed for. Guards the one-shot parse against
    /// a mid-session SEED CHANGE (reconnect to a DIFFERENT seed without an ER reload): when the
    /// room's seed differs from this, every per-seed table is rebuilt via [`Self::reset_for_new_seed`]
    /// before re-parsing. `None` until the first parse completes; set (never reset in place) at the
    /// end of the parse block, alongside `slot_data_parsed = true`.
    parsed_seed: Option<String>,
    my_name: Option<String>,
    save_path: Option<PathBuf>,
    save_loaded: bool,
    last_persisted_index: i64,
    valid_locations: HashSet<i64>,
    locations_loaded: bool,
    /// Bake-emitted location->flag map (apconfig) for detour-bypass checks (NPC gifts, death drops).
    flag_poll: Option<crate::flagpoll::FlagPollConfig>,
    /// slot_data dungeonSweeps: trigger location -> member locations.
    dungeon_sweeps: HashMap<i64, Vec<i64>>,
    /// slot_data sweepLockGates (BOSS_LOCKS_PATCH, Draft A): boss-defeat FLAG -> boss-lock item
    /// name that must be in the cumulative received set before that boss's flag-keyed sweep fires.
    /// Flag-keyed (u32) to match `sweep_flags`; the sweep loop looks it up by defeat flag.
    /// NOTE: `flagpoll::parse_sweep_lock_gates` MUST return `HashMap<u32, String>` for the
    /// `self.sweep_lock_gates = sweeps.1;` assignment to type-check (companion task #9 change).
    sweep_lock_gates: HashMap<u32, String>,
    /// Throttle the (potentially large) flag poll to a few times a second.
    poll_counter: u32,
    /// Guarding flags already set the first time we polled IN-WORLD (new-save defaults: the
    /// tutorial Flask of Crimson Tears flag 60000, physick / sacred-tear flags, etc.). The poll
    /// fires on a flag being SET, not on an unset->set transition, so without this a location
    /// whose flag defaults to set on a fresh save auto-checks on connect (silent) and its vanilla
    /// ware then leaks past the suppressor. Excluding this snapshot retires the ad-hoc
    /// FLAG_POLL_FALSE_POSITIVES denylist.
    flag_poll_baseline: HashSet<u32>,
    /// Whether flag_poll_baseline has been captured (once, on the first in-world poll).
    flag_poll_baseline_done: bool,
    /// Start-of-run grants (items / graces / map reveal).
    start: Option<crate::startgrants::StartConfig>,
    start_flags_done: bool,
    /// Session-scoped tidy-latch for `uniqueStartGrants` entries that have been DECIDED this
    /// session (granted-or-skipped). Deliberately NOT persisted: the obtained-FLAG is the single
    /// source of truth for "has it" (er_logic::unique_grants) -- this set only stops re-deciding
    /// (and re-logging) every tick. Losing it (reload/reconnect) is safe by construction: the
    /// flag read makes the re-run skip.
    unique_grants_ok: HashSet<usize>,
    /// Latches once every uniqueStartGrants entry is decided this session.
    unique_grants_done: bool,
    /// Session-scoped: when the player most recently entered a live world (reset on menu).
    /// Gates the start-item grant so it fires only after the load/inventory settle (clobber fix).
    in_world_since: Option<std::time::Instant>,
    /// Session-scoped last play_region seen by the start-grant gate. A change means a warp /
    /// fast-travel happened; we restart `in_world_since` so a timer-based start grant can't fire on
    /// an inventory pointer the warp's map reload may have left stale (the new-game spawn-kick CTD,
    /// Alaric 2026-07-16; also the unadvertised Chapel warp-out and any early fast-travel). `None`
    /// off-world. Only the timer path is affected -- real_pickup_seen() short-circuits it once a
    /// genuine pickup proves the pointer live, so an established character is untouched.
    grant_gate_last_play_region: Option<i32>,
    /// Pre-scout: resolves each shop reward's name/owner/ER-sell-id (pumped on the tick).
    scout: Option<crate::scout_proof::ScoutProof>,
    /// Goal-send (SPEC-goal-send-20260701.md): goalLocations split flag/checked at parse.
    goal: Option<crate::goal::GoalConfig>,
    /// Region-lock hints: the server-backed ledger of what the player has paid to reveal.
    /// Pricing/decision are pure (`er_logic::lock_hint_economy`); this holds only the socket half.
    lock_hints: crate::lock_hints::LockHints,
    /// Session latch: Goal sent once per connect (NOT persisted -- re-send is idempotent).
    sent_goal: bool,
    /// Item-tracker window visibility (overlay menu "Tracker" + F6 toggle).
    tracker_visible: bool,
    /// The play_region BUCKET (`play_region_id / 100`) the player was last seen in, cached for the
    /// tracker's scaling row. `None` off-world.
    ///
    /// 🛑 CACHED ON THE TICK, NEVER READ FROM THE RENDER. `play_region_id()` dereferences
    /// `WorldChrMan`; the imgui present thread must not touch game memory. It rides the read the
    /// grant gate already does, so this costs no extra deref.
    ///
    /// ⚠️ The sweep is throttled, so this can lag a region transition by a few frames. Fine for a
    /// row the player reads at their own pace; it would be wrong for a toast, which is why the
    /// entry announcement stays on the sweep's own path.
    scaling_here_bucket: Option<i32>,
    /// Standing hint set (SPEC-item-tracker.md option (a)): fed from streamed `Print::Hint`
    /// entries in the overlay log; dedups by location id (connect-replay re-inserts are no-ops).
    hints: HintSet,
    /// How many overlay-log entries have already been scanned for hints. v0.1 LIMITATION: the
    /// log is a bounded ring (1000 entries) -- once it fills and rotates, indices shift under
    /// this watermark and hints in the popped span are missed. DataStorage `_read_hints` is the
    /// robust follow-up (spec option (b)).
    hint_log_watermark: usize,
    /// AP location id -> FINE region display name. PER SEED, from slot_data `locationRegions`
    /// (er_logic::tracker_tables). Empty until connect, and empty means "not sent", never
    /// "no regions" -- the parse logs which.
    region_table: HashMap<u64, String>,
    /// AP location id -> COARSE region name (the in-logic key; "" = always open). Per seed, from
    /// slot_data `regionCoarseKeys`.
    coarse_table: HashMap<u64, String>,
    /// The PROGRESSION SURFACE: location ids this world's own progression may occupy (starred).
    progression_surface: HashSet<u64>,
    /// Coarse region name -> its lock item name, `"<coarse> Lock"` (absent = never locked).
    coarse_lock_items: HashMap<String, String>,
    /// Throttled `(balance, price)` for the ALWAYS-VISIBLE menu bar (issue #412). `None` while
    /// the ledger is unread or the price is underivable -- rendering nothing beats rendering a
    /// zero the player would read as "this costs nothing".
    lock_hint_hud: Option<(u64, u64)>,
    /// `toast_clock` ms at the last HUD refresh. The HUD is recomputed ~4x/second rather than per
    /// frame: it walks the checked-location set, which the always-on path should not do at 60fps.
    lock_hint_hud_at: u64,
    /// Edge latch for the "you can afford a lock hint" toast (`crossed_into_affordable`).
    lock_hint_affordable_prev: Option<bool>,
    /// One-shot "this feature exists" notice, re-armed on seed change.
    lock_hint_intro_done: bool,
    /// Tracker filter: show only checks whose coarse region is currently accessible.
    tracker_in_logic_only: bool,
    /// Tracker filter: show only progression-surface checks.
    tracker_surface_only: bool,
    /// slot_data bossLockItems (mode A, SPEC-boss-lock-tracker.md): parsed boss-defeat trophy defs
    /// (flag -> name/region/boss_ap_id, gate=None for v0.2). METADATA + a defeat-flag watch only —
    /// NOT AP items and NOT new checks; the boss's own boss_ap_id location still fires through the
    /// locationFlags poll. Drives the one-shot "Felled: <Boss>" banner + the tracker Bosses group.
    boss_defs: Vec<er_logic::boss_felled::BossDef>,
    /// Per-boss PREVIOUS defeat-flag state (flags already seen SET) for the one-shot "Felled" banner
    /// edge detector. Primed on the first in-world poll (already-dead bosses, incl. reconnect) so
    /// their banner never re-fires; then only a THIS-session kill (unset->set) fires. Persists
    /// across polls; reset on a genuine seed change so it re-arms.
    boss_flag_prev: HashSet<u32>,
    /// SWEEP VISIBILITY (2026-07-24): defeat flags whose boss-sweep banner already fired this
    /// session, so the "Boss sweep (...): N check(s) granted" line is once per group (the poll
    /// re-pushes members until the server acks them). Reset on seed change. Reconnects never
    /// re-banner regardless: already-checked members are filtered by the server checked-set, so
    /// a re-fired group grants 0 and produces no candidate.
    sweep_bannered: HashSet<u32>,
    /// Last-polled state of every sweep TRIGGER flag, for the tracker's Boss sweeps section.
    ///
    /// 🛑 CACHED ON THE TICK. `get_event_flag` reads game memory and the tracker renders on the
    /// imgui present thread, which must not. The poll already computes exactly this for
    /// `sweep_watch`, so the row costs no extra read.
    sweep_flag_state: HashMap<u32, bool>,
    /// Sweep-trigger flag watcher (diagnostic, 2026-08-07). Reports the group census once and then
    /// only CHANGES, so a trigger flag flipping is timestamped in the log instead of being
    /// inferred from the sweep that follows it. See `er_logic::sweep_watch` for the motivating
    /// 2m45s gap.
    sweep_watch: er_logic::sweep_watch::SweepWatch,
    /// SWEEP FLAG FLUSH (2026-07-24): acquisition flags owed to swept members, held until each one
    /// READS BACK set. A sweep fires exactly once -- on the poll that observes the defeat flag,
    /// which can be mid-load when the game refuses writes -- so a fire-and-forget write would leave
    /// those pickups dead in the world forever. See `er_logic::sweep_flush` (replay-tested).
    sweep_flag_pending: Vec<u32>,
    /// On-screen notices for grants the GAME cannot announce (see `er_logic::toast`). A client-
    /// APPLIED grant -- flask rungs today -- changes state without an item entering the bag, so the
    /// native ticker never fires and the player cannot tell delivery from breakage.
    toasts: er_logic::toast::Deck,
    /// Monotonic clock for the toast deck (ms since this Core was built).
    toast_clock: std::time::Instant,
    /// Last OBSERVED flask-upgrade count. `None` until primed: the count is history-agnostic, so
    /// the first observation after a connect is a baseline, not news.
    flask_seen: Option<usize>,
    /// Whether the region-unlock TOAST is live yet. `false` until the first dispatch pass after a
    /// connect has run: AP replays the whole received stream on connect, so that pass reports every
    /// lock the player already holds as freshly "unlocked". The console line can afford to repeat
    /// (it always has); six toasts on every reconnect cannot. Same shape and same reason as
    /// `flask_seen` -- the first observation after a connect is a baseline, not news.
    region_toast_primed: bool,
    /// ATTUNEMENT-RELEASE (attunement_gate, SPEC-gf-boss-lock-tracker): per-region gate data
    /// {threshold, member_ap_ids, bloom_flags}. Empty => feature off. Parsed once per seed.
    region_attunement: HashMap<String, RegionAttunement>,
    /// Per-region DEFERRED boss-payout checks: a boss killed while its region is not yet attuned has
    /// its checks (boss + sweep members) held here, burst-released the poll the region attunes.
    boss_payout_pending: HashMap<String, HashSet<i64>>,
    /// Regions whose attunement bloom has already fired this save (once-only grace-reveal latch).
    attuned_regions: HashSet<String>,
    /// Bloom baseline primed (first in-world poll): suppresses re-bannering already-attuned regions.
    attunement_primed: bool,
    /// BOSS KEYS (mode B, SPEC-gf-boss-lock-tracker "Boss Key: <Boss>"): per-boss DEFERRED own-check
    /// latch. A boss killed while its "Boss Key: <Boss>" item is not yet received has its own
    /// boss_ap_id check held here (keyed by defeat flag), burst-released the poll the key lands.
    /// Session-scoped: re-derived from the SERVER checked set + received_all on reconnect
    /// (is_local_location_checked makes re-runs idempotent). Empty gate set => unused.
    boss_key_pending: HashMap<u32, HashSet<i64>>,
    /// Boss-key sealed baseline primed (first in-world poll): a boss felled in a PRIOR session whose
    /// key is still unreceived is seeded into boss_key_pending SILENTLY so a reconnect never
    /// re-banners its seal. Mirrors boss_flag_prev / attunement_primed.
    boss_key_primed: bool,
    /// RECONCILER DRY-RUN (additive; `RECONCILE_DRYRUN=1` only): whether `reconcile_io::init` has
    /// run this session. Keeps init once-only, then `set_inputs` thereafter. Never touched unless
    /// dry-run is enabled, so the live path is unaffected.
    reconcile_inited: bool,
    /// Tracks the in-world state across ticks so a map-(re)load edge can re-arm the ItemLotParam
    /// blank passes (check_lots / enemy_drops), which otherwise latch DONE and only reset on reconnect.
    was_in_world: bool,
}

impl shared::Core for Core {
    type SlotData = Value;
    type Game = crate::game::EldenRing;

    /// Debug console commands, typed into the overlay's say input (2026-07-01, playtest tooling).
    /// Unrecognized "!" commands fall through to server chat.
    fn handle_command(&mut self, command: &str, arg: Option<&str>) -> bool {
        match command {
            "!flag" => {
                match arg.and_then(|a| a.trim().parse::<u32>().ok()) {
                    Some(f) => {
                        let v = crate::flags::get_event_flag(f);
                        self.log(ap::Print::message(format!("flag {f} = {v}")));
                    }
                    None => self.log(ap::Print::message("usage: !flag <id>".to_string())),
                }
                true
            }
            "!setflag" => {
                let parts: Vec<&str> = arg.unwrap_or("").split_whitespace().collect();
                match parts.first().and_then(|s| s.parse::<u32>().ok()) {
                    Some(f) => {
                        let on = parts.get(1).map(|s| *s != "0").unwrap_or(true);
                        let ok = crate::flags::try_set_event_flag(f, on);
                        self.log(ap::Print::message(format!(
                            "setflag {f} {on} -> {}",
                            if ok { "OK" } else { "NOT READY" }
                        )));
                    }
                    None => self.log(ap::Print::message("usage: !setflag <id> [0|1]".to_string())),
                }
                true
            }
            "!region" => {
                let pr = crate::flags::play_region_id();
                self.log(ap::Print::message(format!("play_region = {pr:?}")));
                true
            }
            "!warp" => {
                // Playtest tooling for the pure-runtime warp primitive (also unblocks a
                // random-start seed by hand if the auto-warp misfires). Full grace ENTITY id,
                // e.g. `!warp 11102950` = Table of Lost Grace (Roundtable Hold).
                match arg.and_then(|a| a.trim().parse::<u32>().ok()) {
                    Some(g) => {
                        let msg = match crate::warp::warp_to_grace(g) {
                            Ok(()) => format!("warp requested -> grace {g}"),
                            Err(e) => format!("warp FAILED: {e}"),
                        };
                        self.log(ap::Print::message(msg));
                    }
                    None => self.log(ap::Print::message(
                        "usage: !warp <grace entity id> (11102950 = Roundtable)".to_string(),
                    )),
                }
                true
            }
            "!grace" => {
                let Some(q) = arg.map(|s| s.to_lowercase()) else {
                    self.log(ap::Print::message(
                        "usage: !grace <name substring>".to_string(),
                    ));
                    return true;
                };
                let mut lines: Vec<String> = Vec::new();
                if let Some(cfg) = self.region.as_ref() {
                    for (name, &f) in &cfg.grace_items {
                        if name.to_lowercase().contains(&q) {
                            lines.push(format!(
                                "{name}: flag {f} = {}",
                                crate::flags::get_event_flag(f)
                            ));
                        }
                    }
                    for (name, fs) in &cfg.region_graces {
                        if name.to_lowercase().contains(&q) {
                            for &f in fs {
                                lines.push(format!(
                                    "{name} bundle: flag {f} = {}",
                                    crate::flags::get_event_flag(f)
                                ));
                            }
                        }
                    }
                }
                if lines.is_empty() {
                    lines.push(format!("no grace/lock matching '{q}'"));
                }
                for l in lines {
                    self.log(ap::Print::message(l));
                }
                true
            }
            "!markerprobe" => {
                // Dev harness for the save-embedded reconcile marker band (docs/EVENT-FLAG-SPACE.md).
                // Drives the ONE check er-logic's host tests cannot: that the PLACEHOLDER band is real,
                // save-persisted, and vanilla-free. Verify sequence: on a clean save `!markerprobe`
                // (scan) => expect 0 set; `set`; quit to menu + reload; `verify` => expect PASS;
                // `clear`, play normally, `!markerprobe` => expect 0 set again.
                let base = er_logic::marker::FlagBand::PLACEHOLDER.base;
                let n = er_logic::marker::FlagBand::RESERVED;
                let want = |i: u32| i.is_multiple_of(3); // recognizable, non-trivial pattern
                match arg.map(|a| a.trim()) {
                    Some("set") => {
                        let (mut ok, mut busy) = (0u32, 0u32);
                        for i in 0..n {
                            if crate::flags::try_set_event_flag(base + i, want(i)) {
                                ok += 1;
                            } else {
                                busy += 1;
                            }
                        }
                        self.log(ap::Print::message(format!(
                            "markerprobe set every-3rd across {base}..{}: {ok} written, {busy} NOT READY",
                            base + n
                        )));
                    }
                    Some("verify") => {
                        let bad: Vec<u32> = (0..n)
                            .filter(|&i| crate::flags::get_event_flag(base + i) != want(i))
                            .map(|i| base + i)
                            .collect();
                        self.log(ap::Print::message(if bad.is_empty() {
                            format!(
                                "markerprobe verify: PASS (pattern intact {base}..{})",
                                base + n
                            )
                        } else {
                            format!(
                                "markerprobe verify: FAIL ({} mismatched, first {:?})",
                                bad.len(),
                                bad.iter().take(8).collect::<Vec<_>>()
                            )
                        }));
                    }
                    Some("clear") => {
                        for i in 0..n {
                            crate::flags::try_set_event_flag(base + i, false);
                        }
                        self.log(ap::Print::message(format!(
                            "markerprobe clear: {base}..{}",
                            base + n
                        )));
                    }
                    _ => {
                        let set: Vec<u32> = (0..n)
                            .filter(|&i| crate::flags::get_event_flag(base + i))
                            .map(|i| base + i)
                            .collect();
                        self.log(ap::Print::message(format!(
                            "markerprobe scan {base}..{}: {}/{n} set{} | usage: !markerprobe set|verify|clear",
                            base + n,
                            set.len(),
                            if set.is_empty() {
                                String::new()
                            } else {
                                format!(" {set:?}")
                            }
                        )));
                    }
                }
                true
            }
            "!give" => {
                // PROBE, not a cheat: the one question params cannot answer is whether a goods row has
                // an FMG NAME (names live in the msgbnd), and the only way to see a name is to hold the
                // item. Finding the SECOND placeholder row -- the unblock for repointing shop slots
                // without hijacking a real good's shared FMG entry -- needs exactly this and nothing
                // more. Reusable on purpose: this is the third time a spare-row / FMG question would
                // have been one command away.
                //
                // Takes a FULL id (category nibble | raw), the same space `grant_full_id` and the
                // detour speak, so a goods row R is `0x40000000 | R` -- e.g. 8853 -> 0x40002295 (1073750677). The
                // nibble is REQUIRED and not inferred: guessing an id space is how ids silently never
                // match (CONTRACT rule 3), and a probe that quietly grants the wrong table is worse
                // than no probe.
                let parts: Vec<&str> = arg.unwrap_or("").split_whitespace().collect();
                let full = parts.first().and_then(|s| {
                    s.strip_prefix("0x")
                        .and_then(|h| i32::from_str_radix(h, 16).ok())
                        .or_else(|| s.parse::<i32>().ok())
                });
                match full {
                    Some(f) if f != 0 => {
                        let qty = parts.get(1).and_then(|s| s.parse::<i32>().ok()).unwrap_or(1);
                        // Report the DECODED id back before granting: if the nibble is not what the
                        // caller meant, they see it here rather than inferring it from a wrong item.
                        let (cat, raw) = (f as u32 & 0xF000_0000, er_codec::row_id_of(f as u32));
                        let ok = crate::detour::grant_full_id(f, qty);
                        let status = if ok {
                            "granted"
                        } else {
                            "NOT READY (hook down or deferred) -- retry"
                        };
                        self.log(ap::Print::message(format!(
                            "give {f} (category 0x{cat:08X}, row {raw}) x{qty} -> {status}"
                        )));
                    }
                    _ => self.log(ap::Print::message(
                        "usage: !give <fullId> [qty]  -- fullId = category nibble | raw, so goods row R                          is 0x40000000|R -- PREFER HEX: goods 8853 = 0x40002295 (decimal 1073750677); hex needs no arithmetic"
                            .to_string(),
                    )),
                }
                true
            }
            "!help" => {
                self.log(ap::Print::message(
                    "!flag <id> | !setflag <id> [0|1] | !region | !grace <name substring> | !markerprobe [set|verify|clear] | !give <fullId> [qty]"
                        .to_string(),
                ));
                true
            }
            _ => false,
        }
    }

    fn new() -> Result<Self> {
        Ok(Self {
            base: CoreBase::new("Elden Ring")?,
            detour_installed: false,
            received_through: 0,
            dispatched_through: 0,
            recv_probe: er_logic::receive_probe::Probe::default(),
            item_map: None,
            item_counts: HashMap::new(),
            region: None,
            fogwall: None,
            progressive: ProgressiveState::new(HashMap::new()),
            slot_data_parsed: false,
            parsed_seed: None,
            my_name: None,
            save_path: None,
            save_loaded: false,
            last_persisted_index: -1,
            valid_locations: HashSet::new(),
            locations_loaded: false,
            flag_poll: None,
            dungeon_sweeps: HashMap::new(),
            sweep_lock_gates: HashMap::new(),
            poll_counter: 0,
            flag_poll_baseline: HashSet::new(),
            flag_poll_baseline_done: false,
            start: None,
            start_flags_done: false,
            unique_grants_ok: HashSet::new(),
            unique_grants_done: false,
            in_world_since: None,
            grant_gate_last_play_region: None,
            scout: None,
            goal: None,
            lock_hints: crate::lock_hints::LockHints::new(),
            sent_goal: false,
            tracker_visible: false,
            scaling_here_bucket: None,
            hints: HintSet::new(),
            hint_log_watermark: 0,
            // EMPTY until slot_data arrives. There is no baked table any more: it described the
            // DEFAULT seed's regions and was wrong for every num_regions seed. Filled by the
            // slot_data parse, which logs armed-or-why-not.
            region_table: HashMap::new(),
            coarse_table: HashMap::new(),
            progression_surface: HashSet::new(),
            coarse_lock_items: HashMap::new(),
            lock_hint_hud: None,
            lock_hint_hud_at: 0,
            lock_hint_affordable_prev: None,
            lock_hint_intro_done: false,
            tracker_in_logic_only: false,
            tracker_surface_only: false,
            boss_defs: Vec::new(),
            boss_flag_prev: HashSet::new(),
            sweep_bannered: HashSet::new(),
            sweep_flag_state: HashMap::new(),
            sweep_watch: er_logic::sweep_watch::SweepWatch::new(),
            sweep_flag_pending: Vec::new(),
            toasts: er_logic::toast::Deck::new(4, 6000),
            toast_clock: std::time::Instant::now(),
            flask_seen: None,
            region_toast_primed: false,
            region_attunement: HashMap::new(),
            boss_payout_pending: HashMap::new(),
            attuned_regions: HashSet::new(),
            attunement_primed: false,
            boss_key_pending: HashMap::new(),
            boss_key_primed: false,
            reconcile_inited: false,
            was_in_world: false,
        })
    }
    fn base(&self) -> &CoreBase<Self::Game, Self::SlotData> {
        &self.base
    }
    fn base_mut(&mut self) -> &mut CoreBase<Self::Game, Self::SlotData> {
        &mut self.base
    }

    /// The slot's `death_link` option, or `None` until slot data has actually been parsed.
    ///
    /// `deathlink::is_enabled()` alone cannot answer this: its static defaults to `false`, so
    /// before the parse it reports "off" indistinguishably from a slot that really is off. Gating
    /// on `slot_data_parsed` is what makes the difference legible to the tag reconciler -- and the
    /// latch is re-armed on a genuine seed change, so this correctly goes back to `None` while a
    /// new seed's options are being read.
    fn death_link_enabled(&self) -> Option<bool> {
        self.slot_data_parsed.then(crate::deathlink::is_enabled)
    }

    /// Overlay menu-bar hook (SPEC-item-tracker.md): a "Tracker" item that toggles the window.
    ///
    /// The label carries its hotkey (as the shared overlay's "Hide (F5)" does) because F6 lived
    /// only in this file: it was in no README, no guide and no menu label, so the one feature that
    /// answers "how do I get this off my screen" was undiscoverable by design.
    fn render_overlay_menu_items(&mut self, ui: &imgui::Ui) {
        if ui.menu_item("Tracker (F6)") {
            self.tracker_visible = !self.tracker_visible;
        }
        // ---- lock-hint balance, issue #412 (the discoverability half) -------------------------
        // The economy shipped entirely behind three closed doors at once: a tracker window that
        // defaults to HIDDEN, a hotkey (F6) that appeared in no guide, and a price printed only on
        // the header of a region that had to be both locked and scrolled into view. bobler played a
        // whole 0.3.5 seed past it -- `0 hint(s) already bought` -- and spent three AP `!hint`s
        // instead. This puts the balance where he already was: the menu bar, which is drawn
        // whenever the main window is, and clicking it opens the thing that spends it.
        if let Some((have, price)) = self.lock_hint_hud {
            let label = if have >= price {
                format!("Lock hints: {have}/{price} -- ready")
            } else {
                format!("Lock hints: {have}/{price}")
            };
            if ui.menu_item(label) {
                self.tracker_visible = true;
            }
        }
    }

    /// Overlay frame hook: hotkey toggle + hint accumulation every frame (cheap -- the watermark
    /// skips already-scanned log entries), then the tracker window itself when visible.
    fn render_overlay_windows(&mut self, ui: &imgui::Ui) {
        // F6 toggles the tracker (deliberately NOT a plain letter -- those fight the say input).
        if ui.is_key_pressed(imgui::Key::F6) {
            self.tracker_visible = !self.tracker_visible;
        }

        // Deliver a queued TRAP ITEM, at most one per frame. Unlike the F7/F8 probe below this is
        // NOT gated on the probe flag: a trap that arrived as a real AP item must fire whether or
        // not anyone turned a diagnostic on.
        let now = self.toast_clock.elapsed().as_millis() as u64;
        if let Some(line) = crate::traps::poll_pending(now) {
            self.toasts.push(line.to_string(), now);
            self.log(ap::Print::message(line.to_string()));
        }

        // TRAP PROBE (traps.rs) -- off unless `probes: { "traps": true }`. Function keys for the
        // same reason F6 is one: a letter fights the say input, and a trap fired by a stray
        // keystroke while typing to the room would be indistinguishable from a bug.
        // 🛑 These are DESTRUCTIVE on purpose -- F7 really takes half your runes. The gate is the
        // consent.
        if crate::traps::enabled() {
            for (key, trap) in [
                (imgui::Key::F7, er_logic::traps::Trap::RuneThief),
                (imgui::Key::F8, er_logic::traps::Trap::NoFlask),
            ] {
                if !ui.is_key_pressed(key) {
                    continue;
                }
                // `fire` returns None when it could not act this tick (not in world, player
                // mid-death, param not streamed yet). Nothing to toast, nothing to log twice.
                let Some(line) = crate::traps::fire(trap) else {
                    continue;
                };
                let now = self.toast_clock.elapsed().as_millis() as u64;
                self.toasts.push(line.to_string(), now);
                self.log(ap::Print::message(line.to_string()));
            }
        }

        self.accumulate_hints_from_log();
        self.refresh_lock_hint_hud();

        if self.tracker_visible {
            self.render_tracker_window(ui);
        }

        self.render_toasts(ui);
    }

    fn update_live(&mut self) -> Result<()> {
        if !self.detour_installed {
            match crate::detour::install() {
                Ok(()) => self.detour_installed = true,
                Err(e) => log::warn!("AddItemFunc detour install deferred: {e}"),
            }
        }
        // LuaWarp probe hook (warp_hook.rs; capital-reconciler menu-warp seam): self-guarded
        // one-shot on the game thread — a signature mismatch degrades with one log line
        // instead of erroring, so no install latch on Core is needed.
        crate::warp_hook::install();

        // Which diagnostics are on, stated once. A probe turned on from `apconfig.json` is
        // invisible to us otherwise, so "I set it and nothing happened" would cost a conversation
        // instead of a grep -- and a silently-typo'd key would look exactly like a broken probe.
        shared::probes::log_active(&[
            ("ER_ESD_PROBE", "esd"),
            ("ER_DOWNSTATE_PROBE", "downstate"),
            ("ER_DOWNSTATE_PROBE_ARM", "downstate_arm"),
            ("ER_DOWNSTATE_PROBE_PLAYER", "downstate_player"),
            ("ER_TRAP_PROBE", "traps"),
        ]);

        // ESD talk-event hook (esd_probe.rs) -- the shop-open seam SHOP AUTO-HINTS ride on
        // (er-archipelago#455, phase 2). Installed unconditionally now that phase 1 witnessed
        // command 22 firing at a real merchant with a usable ShopLineupParam row range; the
        // `ER_ESD_PROBE` / `"probes": {"esd": true}` gate survives as a VERBOSITY switch for the
        // command-id enumeration, not as the install gate. Same self-guarded one-shot shape as the
        // LuaWarp hook above: an unsupported build degrades to one refusal line AND marks shop
        // hints inactive, which the connect banner then states outright.
        crate::esd_probe::install();

        // 1. Report suppressed (world-pickup) synthetics. The echo grants them. Gated on the minibake
        // refuse guard — a wrong-seed save must not report checks (see reconcile_io::is_refused).
        let checks = crate::detour::take_pending_checks();
        if !checks.is_empty()
            && !crate::reconcile_io::is_refused()
            && let Some(client) = self.client_mut()
            && let Err(e) = client.mark_checked(checks.iter().copied())
        {
            log::warn!("mark_checked failed for {checks:?}: {e}");
        }

        // 2. Parse slot_data once -- but RE-PARSE on a genuine SEED CHANGE (reconnect to a
        //    DIFFERENT seed without reloading the ER save). `slot_data_parsed` is a one-shot latch,
        //    so without this `valid_locations` (and every other per-seed table) keeps seed A's data
        //    while archipelago_rs rebuilds `local_locations_checked` for seed B -- the stale
        //    `valid_locations` guard then passes a seed-A id absent from seed B into
        //    `is_local_location_checked`, which panics in the no-unwind FFI frame (abort). It would
        //    also strand seed B's own new checks (its tables were never built). Compute the room
        //    seed the SAME way the save-key logic does (client.seed_name()); rebuild only on a real
        //    switch (er_logic::seed_change: non-empty room seed that differs from the parsed one --
        //    a same-seed reconnect must NOT reset, or it wipes the flag-poll baseline / save
        //    persistence that reconnect-to-same-seed relies on).
        let current_room_seed = self
            .client()
            .map(|c| c.seed_name().to_string())
            .unwrap_or_default();
        if self.slot_data_parsed
            && er_logic::seed_change::is_seed_change(
                self.parsed_seed.as_deref(),
                &current_room_seed,
            )
        {
            log::warn!(
                "seed change detected (parsed {:?} -> room {current_room_seed:?}) -- rebuilding per-seed state",
                self.parsed_seed
            );
            // Per-seed TABLES are rebuilt above; the armed RECONCILER is a separate thing and
            // `reset_for_new_seed` cannot reach it (`DRIVER` is a `OnceLock` owned by reconcile_io,
            // and `reconcile_inited` stays true). Ask the reconnect guard again — see
            // `reconcile_io::disarm_if_identity_moved` for the 229-checks incident that needs it.
            crate::reconcile_io::disarm_if_identity_moved(&current_room_seed);
            self.reset_for_new_seed();
        }
        if !self.slot_data_parsed {
            let parsed = self.client().map(|client| {
                let sd = client.slot_data();
                // Full slot_data dump (playtest diagnostics): every top-level key + a truncated
                // JSON value, so a client log alone answers "what did this seed emit?" -- e.g. is
                // regionSphereTargetRanges present/non-empty, is the seed `versions`-stamped.
                // Mirrors the gen-side spoiler dump (greenfield core.py write_spoiler).
                if let Some(obj) = sd.as_object() {
                    log::info!("slot_data dump ({} keys):", obj.len());
                    let mut items: Vec<(&String, String)> = obj
                        .iter()
                        .map(|(k, v)| {
                            let s = v.to_string();
                            let s = if s.chars().count() > 200 {
                                format!("{} ...(truncated)", s.chars().take(200).collect::<String>())
                            } else {
                                s
                            };
                            (k, s)
                        })
                        .collect();
                    items.sort_by(|a, b| a.0.cmp(b.0));
                    for (k, rv) in items {
                        log::info!("  {k} = {rv}");
                    }
                }
                // ---- VERSION HANDSHAKE ------------------------------------------------------
                // The apworld and this .dll ship as SEPARATE artifacts (apworld off-site, dll on
                // Nexus), so a player mixing versions is the NORM, not an edge case -- and a stale
                // .dll against a fresh apworld looks exactly like a bug in the game. `versions`
                // carries apworld semver + the CONTRACT HASH the apworld was built from + the hash
                // of the generated DATA the seed used. Compare the contract hash to the one THIS
                // binary was compiled against and shout if they differ. Always log the whole string:
                // every bug report should carry it, or it cannot be triaged.
                let their_versions = sd.get("versions").and_then(|v| v.as_str()).unwrap_or("");
                if their_versions.is_empty() {
                    log::warn!(
                        "VERSION: apworld sent no `versions` -- it predates the version handshake. \
                         This client is contract/{} apworld/{}. Skew CANNOT be detected; if anything \
                         behaves oddly, suspect a version mismatch first.",
                        crate::contract_gen::CONTRACT_HASH,
                        crate::contract_gen::APWORLD_VERSION_EXPECTED);
                } else {
                    let their_contract = their_versions
                        .split_whitespace()
                        .find_map(|t| t.strip_prefix("contract/"))
                        .unwrap_or("?");
                    if their_contract == crate::contract_gen::CONTRACT_HASH {
                        log::info!("VERSION: OK -- {} (client contract/{})",
                                   their_versions, crate::contract_gen::CONTRACT_HASH);
                    } else {
                        log::error!(
                            "VERSION MISMATCH -- apworld sent [{}] but this client was BUILT against \
                             contract/{}. The apworld and the client .dll are from different builds. \
                             Update whichever is older; do not report bugs from this pairing -- the \
                             slot_data shapes this client expects are not the ones it is being sent.",
                            their_versions, crate::contract_gen::CONTRACT_HASH);
                    }
                }

                // FEATURE HANDSHAKE (2026-07-27). The contract-hash check above folds in CONTRACT
                // only -- NOT OPTIONS_SUBKEYS -- so an apworld can add a client-consumed OPTION
                // without moving the hash, and this binary would report "VERSION: OK" while being
                // structurally unable to see the new key. A player who set that option would get
                // silence: the setting evaporates and the game looks like the feature never existed.
                //
                // So a seed declares the features it ACTUALLY depends on, and we refuse the ones we
                // do not have rather than ignoring them. Deliberately LOUDER than the contract
                // validator below (which warns and boots): a shape mismatch degrades one key, while
                // an unknown feature tag means a setting the player chose cannot happen at all.
                // Seeds that leave those options at their defaults declare nothing and are unaffected.
                let missing = er_logic::client_features::unsupported(
                    &er_logic::client_features::required_from_slot_data(sd));
                if !missing.is_empty() {
                    log::error!(
                        "CLIENT TOO OLD: {}",
                        er_logic::client_features::refusal_message(&missing));
                }
                // The on-screen half is pushed by the CALLER: this closure already borrows `*self`
                // (via `self.client()`), so touching `self.toasts` here is E0500. Same shape as
                // `gate_warn` below -- compute in the closure, surface after it.
                let feature_warn = missing;

                // Two-sided contract validation: warn (not reject) on any slot_data mismatch
                // so a partially-compatible seed still boots but every problem is visible.
                let contract_problems = crate::contract_gen::validate(sd);
                if contract_problems.is_empty() {
                    log::info!("contract: slot_data OK ({} keys checked)", crate::contract_gen::CONTRACT.len());
                } else {
                    for p in &contract_problems { log::warn!("contract: {p}"); }
                }
                // int-or-bool tolerant (er_logic::options): the apworld serializes options
                // as ints (death_link: 1), which .as_bool() silently read as false.
                crate::deathlink::set_enabled(er_logic::options::parse_death_link(sd));
                crate::no_equip_load::set_enabled(er_logic::options::parse_no_equip_load(sd));
                // no_fall_damage: the spirit-spring fallDamageRate=0 SpEffect, kept on the player.
                crate::no_fall_damage::set_enabled(er_logic::options::parse_no_fall_damage(sd));
                // flask: history-agnostic reconciled LEVELED flask (charges + potency) driven by the
                // count of received "Progressive Flask Upgrade" items vs the slot_data `flaskLadder`.
                // Absent/empty ladder => feature OFF. No ledger; re-runs upward every tick.
                crate::flask::set_ladder(er_logic::flask_reconcile::parse(sd));
                // auto_equip: received weapons get equipped into a primary hand (same option name on
                // both apworlds). The receive loop queues equipable FullIDs -- weapons, armour and
                // talismans; auto_equip::tick drains them.
                crate::auto_equip::set_enabled(er_logic::options::parse_auto_equip(sd));
                // merchant bells (#325): opening a merchant's buy menu sets the flag the Twin
                // Maiden Husks would have set had you handed them that merchant's Bell Bearing.
                // The ESD detour does the work; this only tells it whether the seed asked for it.
                crate::merchant_bells::set_enabled(
                    er_logic::options::parse_merchant_bells_on_talk(sd),
                );
                // Accepts our `no_weapon_requirements` OR Bedrock/fswap's
                // `remove_weapon_and_spell_requirements` (same client feature, two apworld names).
                crate::no_weapon_reqs::set_enabled(er_logic::options::parse_no_weapon_reqs(sd));
                crate::upgrades::set_auto_upgrade(
                    sd.pointer("/options/auto_upgrade").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                );
                crate::upgrades::set_global_scadu_blessing(
                    sd.pointer("/options/global_scadutree_blessing").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                );
                // mode 2 (scaled): the per-DLC-region Scadutree-blessing floor wire. Absent for base
                // game / mode != 2 -> empty -> mode 2 behaves as mode 1.
                // scaduBlessingCap: the seed's ceiling for the blessing curve (tier-aware under
                // `scaled`, the ladder ceiling under `player_only`). ABSENT => 0 => the client falls
                // back to SCADU_MAX_LEVEL, never to 0 — an absent key that read as the floor would
                // ship the whole feature inert, which is the exact failure this option already had.
                crate::scadu_blessing::set_cap(
                    sd.get("scaduBlessingCap").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                );
                // A reconnect / seed change must re-sync the clone row rather than trust a level
                // cached from a different seed.
                crate::scadu_blessing::reset();
                // ...and re-announce it. Separate memo, separate lifetime: `reset()` is ALSO called
                // on the `in_world` edge, and clearing the announcement there would re-toast the
                // same level on every load screen.
                crate::scadu_blessing::reset_announced();
                crate::upgrades::set_dlc_blessing_floors(
                    er_logic::scaling::parse_triple_ranges(sd.get("dlcScadutreeFloorRanges")),
                );
                // Our `flatten_regular_upgrades` (int cap) OR Bedrock/fswap's
                // `reduce_non_somber_upgrade_cost` (bool toggle -> cap 1).
                crate::upgrade_cost::set_flatten(er_logic::options::parse_flatten_cap(sd));
                let map = i64_map(sd.get("apIdsToItemIds"));
                // The GOODS rows this seed can actually GRANT. shop_icon / shop_preview must never
                // repaint one of these: EquipParamGoods.iconId and the GoodsName FMG entry are SHARED
                // per good id, so flowering the vanilla ware behind a shop slot re-icons and renames
                // EVERY copy the player will ever hold -- 11 vanilla shop rows sell smithing stones,
                // which is why the 2026-07-12 playtest had telescope-icon stones in the world AND in
                // the inventory. Both modules fail CLOSED until this arrives.
                let real_goods: std::collections::HashSet<u32> = map
                    .values()
                    .map(|v| *v as u32)
                    .filter(|full| er_codec::item_category_of(*full) == er_codec::CATEGORY_GOODS)
                    .map(er_codec::row_id_of)
                    .collect();
                crate::shop_icon::set_real_goods(real_goods.clone());
                crate::shop_preview::set_real_goods(real_goods);
                let counts = i64_map(sd.get("itemCounts"));
                let mut region = crate::region::parse(sd);
                // Arm shop_preview to MARK region-lock rewards that land in a shop (a lock reward
                // otherwise reads as its vanilla good, e.g. "Note: Sealed Spiritsprings", with no hint
                // it's a region key). Keyed by lock item name, same set open_on_received_name uses.
                crate::shop_preview::configure_locks(
                    region.region_open_flags.keys().cloned().collect(),
                );
                // ...and give those lock slots the AP flower icon (shop_icon), same lock-name set.
                crate::shop_icon::configure_locks(
                    region.region_open_flags.keys().cloned().collect(),
                );
                // Capital-version reconciler (SPEC-capital-reconciler.md): five capital* keys,
                // parsed together; absent = INERT (logged). Also configures the shop release
                // re-key rows (shop_flags::run_capital_release, driven from the tick below).
                crate::region::configure_capital(sd);
                // BAKED REGION-LOCK FALLBACK (bedrock interop): only for a seed that speaks
                // NEITHER region key -- slot_data always wins when it speaks (region.rs). Scope
                // = the seed's apIdsToItemIds ids resolved to NAMES through the datapackage;
                // enforcement then stays COLD until a scoped "<Region> Lock" is actually
                // received (tick_baked_fallback below) -- the real foreign apworld ships its
                // whole item table even on no-lock seeds, so table presence must never arm.
                if crate::region::foreign_seed_without_region_keys(sd) {
                    let names: Vec<String> = client
                        .game(client.this_player().game())
                        .map(|g| {
                            map.keys()
                                .filter_map(|id| g.item(*id).map(|it| it.name().to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    crate::region::prepare_baked_fallback(
                        &mut region,
                        names.iter().map(|s| s.as_str()),
                    );
                }
                let fogwall = crate::fogwall::parse(sd);
                let prog_cfg = er_logic::progressive::parse(sd);
                let name = client.this_player().alias().to_string();
                // BOSS_LOCKS_PATCH: sweeps + their lock gates travel together (tuple keeps
                // the parsed-slot_data tuple arity unchanged).
                let sweeps = (
                    crate::flagpoll::parse_dungeon_sweeps(sd),
                    crate::flagpoll::parse_sweep_lock_gates(sd),
                    crate::flagpoll::parse_sweep_flags(sd),
                );
                let start = crate::startgrants::parse(sd);
                // POSSESSION DEDUP (#267): hand the startItems to the convergence loop that now OWNS
                // plain start-item delivery -- it grants whatever is absent from the bag and stops
                // when a fresh scan finds nothing missing.
                crate::start_item_backfill::set_start_items(start.start_items.clone());
                if start.unique_start_grants.is_empty() {
                    log::info!("unique start grants: inert (no uniqueStartGrants in slot_data)");
                } else {
                    log::info!(
                        "unique start grants armed with {} entr{}: {:?} (goods, obtained-flag)",
                        start.unique_start_grants.len(),
                        if start.unique_start_grants.len() == 1 { "y" } else { "ies" },
                        start.unique_start_grants
                    );
                }

                // Shop system (SHOP-SYSTEM-HANDOFF.md §3): configure from slot_data, build the scout.
                // KEY-TABLE MIGRATION (locationIdsToKeys): token 1 of a matt slot key is the
                // acquisition flag; prefer it, fall back to legacy `locationFlags` for old seeds.
                let loc_flags = {
                    let from_keys = crate::key_resolver::location_flags_from_keys(sd);
                    if from_keys.is_empty() {
                        i64_to_u32_map(sd.get("locationFlags"))
                    } else {
                        from_keys
                    }
                };
                // SHOP KEY RESOLUTION: shop slots (token1==0) carry ShopLineupParam rows in token3;
                // resolve row -> eventFlag_forStock via shipped shoplineup_flags.json and fold into
                // loc_flags so purchases self-detect through the same poller. Disjoint union.
                let loc_flags = {
                    fn shop_table_path() -> std::path::PathBuf {
                        shared::utils::mod_directory()
                            .map(|d| d.join("shoplineup_flags.json"))
                            .unwrap_or_else(|_| std::path::PathBuf::from("shoplineup_flags.json"))
                    }
                    let mut loc_flags = loc_flags;
                    let shop_table = crate::key_resolver::load_shoplineup_flags(&shop_table_path());
                    // Tolerance requires telemetry: an absent/empty table degrades every foreign
                    // shop check to "never fires", which is indistinguishable from "no shops in
                    // this seed" without this line. Announce armed/inert once, but only on the
                    // matt-key (foreign) path -- greenfield seeds carry no locationIdsToKeys and
                    // resolve shops from slot_data, so the table is legitimately irrelevant there.
                    let foreign_keys = sd.get("locationIdsToKeys").is_some();
                    if !shop_table.is_empty() {
                        let resolved = crate::key_resolver::shop_flags_from_keys(sd, &shop_table);
                        if foreign_keys {
                            // DISTINCT flags, not just resolved locations. These differ when several AP
                            // shop locations resolve to the SAME ShopLineupParam row (matt keys list the
                            // rows that sell an item in token3, and many items share a row). That matters:
                            // shop_sell inverts loc->flag into flag->loc to find the row to rewrite, and
                            // an N:1 loc->flag mapping makes that inversion LOSSY -- it can only ever
                            // rewrite one row per flag. On the 2026-07-13 Bedrock seed shop_sell saw only
                            // 87 live check rows against 410 "resolved" locations, and this is the number
                            // that says whether that gap is collapse (expected) or a lookup failure (bug).
                            let distinct: std::collections::HashSet<u32> =
                                resolved.values().copied().collect();
                            log::info!(
                                "shoplineup_flags: armed with {} rows -- {} shop location(s) resolved to {} DISTINCT stock flag(s)",
                                shop_table.len(),
                                resolved.len(),
                                distinct.len()
                            );
                        }
                        for (loc, flag) in resolved {
                            loc_flags.entry(loc).or_insert(flag);
                        }
                    } else if foreign_keys {
                        log::warn!(
                            "shoplineup_flags: INERT -- no usable table at {} (foreign shop checks will never fire)",
                            shop_table_path().display()
                        );
                    }
                    loc_flags
                };
                let preview: Vec<(i64, i32)> = i64_map(sd.get("shopPreviewGoods"))
                    .into_iter()
                    .map(|(l, g)| (l, g as i32))
                    .collect();
                crate::scout_proof::configure_item_map(map.clone());
                crate::shop_flags::configure(
                    i64_to_u32_map(sd.get("shopRowFlags"))
                        .into_iter()
                        .map(|(r, f)| (r as u32, f))
                        .collect(),
                );
                crate::shop_flags::configure_check_flags(loc_flags.values().copied().collect());

                // shopInfiniteStock: {"<row id>": [goodsId, equipType, price]} -- the per-seed reroll of
                // the 455 UNLIMITED rows (no stock flag => never checks). The PRICE rides along because
                // those rows inherit the old ware's cost (gem slots = 1 rune, 166 armor rows FREE);
                // without it every seed is a free-consumable dispenser.
                {
                    let mut roll: std::collections::HashMap<u32, (i32, u8, i32)> =
                        std::collections::HashMap::new();
                    if let Some(m) = sd.get("shopInfiniteStock").and_then(|v| v.as_object()) {
                        for (k, v) in m {
                            let (Ok(row), Some(a)) = (k.parse::<u32>(), v.as_array()) else { continue };
                            if a.len() < 3 {
                                continue;
                            }
                            let (Some(gid), Some(et), Some(pr)) =
                                (a[0].as_i64(), a[1].as_i64(), a[2].as_i64()) else { continue };
                            roll.insert(row, (gid as i32, et as u8, pr as i32));
                        }
                    }
                    if !roll.is_empty() {
                        crate::shop_stock::configure(roll);
                    }
                }

                // enemyDropRoll: {"<lot id>": [slot, goodsId, slot, goodsId, ...]} -- flat pairs.
                // UNFLAGGED ItemLotParam_enemy lots only (a flagged lot IS a check and is never sent).
                {
                    let mut roll: std::collections::HashMap<u32, Vec<(u8, i32)>> =
                        std::collections::HashMap::new();
                    if let Some(m) = sd.get("enemyDropRoll").and_then(|v| v.as_object()) {
                        for (k, v) in m {
                            let (Ok(lot), Some(a)) = (k.parse::<u32>(), v.as_array()) else { continue };
                            let mut pairs = Vec::with_capacity(a.len() / 2);
                            for ch in a.chunks(2) {
                                if ch.len() < 2 {
                                    break;
                                }
                                let (Some(sl), Some(gid)) = (ch[0].as_i64(), ch[1].as_i64()) else { continue };
                                pairs.push((sl as u8, gid as i32));
                            }
                            if !pairs.is_empty() {
                                roll.insert(lot, pairs);
                            }
                        }
                    }
                    if !roll.is_empty() {
                        crate::enemy_drops::configure(roll);
                    }
                }

                // checkLotBlank {"<lot id>": [goods slot idx, ...]} + apPlaceholderGoods.
                // Repoints each CHECK lot's goods slot at ONE placeholder id, which detour.rs then
                // suppresses UNCONDITIONALLY -- so the vanilla ware is never handed out at a check, and
                // NOTHING else has to be watched by item id (mined ore / farmed drops / bought / crafted
                // goods all pass through untouched). One placeholder suffices because checks are detected
                // by the FLAG POLL, not by the pickup id.
                {
                    let ph = sd.get("apPlaceholderGoods").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                    // TWO tables, kept apart. ItemLotParam_map and ItemLotParam_enemy can hold the
                    // SAME row id, so a merged dict loses the table and the client has to guess. It
                    // guessed map-first -- and every enemy lot colliding with a map id was therefore
                    // never blanked, so a boss that is "just an enemy" handed out its vanilla drop and
                    // fired no check. The apworld knows which CSV each lot came from; it now says so.
                    let parse_lots = |key: &str| -> std::collections::HashMap<u32, Vec<u8>> {
                        let mut out = std::collections::HashMap::new();
                        if let Some(m) = sd.get(key).and_then(|v| v.as_object()) {
                            for (k, v) in m {
                                let (Ok(lot), Some(a)) = (k.parse::<u32>(), v.as_array()) else {
                                    continue;
                                };
                                let slots: Vec<u8> =
                                    a.iter().filter_map(|x| x.as_i64()).map(|x| x as u8).collect();
                                if !slots.is_empty() {
                                    out.insert(lot, slots);
                                }
                            }
                        }
                        out
                    };
                    let mut blank_map = parse_lots("checkLotBlankMap");
                    let mut blank_enemy = parse_lots("checkLotBlankEnemy");
                    if blank_map.is_empty() && blank_enemy.is_empty() {
                        // LEGACY: an apworld whose check_lots_data.py predates the map/enemy split. It
                        // ships one merged dict keyed by lot id alone, so the table is unknown. Send it
                        // to BOTH -- check_lots only writes a lot where the row actually EXISTS, so a
                        // map-only id lands in map and an enemy-only id lands in enemy, reproducing the
                        // old behaviour. A COLLIDING id gets blanked in both, which is the old bug's
                        // blast radius inverted (it used to under-blank; now it over-blanks) -- and that
                        // is precisely why the apworld must send the table. Loud, not silent.
                        let legacy = parse_lots("checkLotBlank");
                        if !legacy.is_empty() {
                            log::warn!(
                                "check-lots: apworld sent the LEGACY merged checkLotBlank (no map/enemy \
                                 split). The param table each lot belongs to is unknown, so bosses that \
                                 are 'just an enemy' may still hand out their vanilla drop. Regenerate \
                                 the apworld (python greenfield/gen_data.py)."
                            );
                            blank_map = legacy.clone();
                            blank_enemy = legacy;
                        }
                    }
                    // The WIRE KEYS keep their historical `...Zero...` names -- renaming a slot_data
                    // key is a cross-repo contract change and would break connect for a seed rolled
                    // on an older apworld. The BEHAVIOUR they name is gone: these slots are repointed
                    // at the placeholder now, never emptied. Locals are named for what happens.
                    let non_goods_map = parse_lots("checkLotZeroMap");
                    let non_goods_enemy = parse_lots("checkLotZeroEnemy");
                    let has_lots = !(blank_map.is_empty()
                        && blank_enemy.is_empty()
                        && non_goods_map.is_empty()
                        && non_goods_enemy.is_empty());
                    if ph != 0 && has_lots {
                        // #329 -- SCOPE THE REWRITE TO CHECKS THIS SEED ACTUALLY HAS.
                        //
                        // `features/check_lots.py` sends EVERY check lot and says why: "we can only
                        // scope by region here ... a lot whose check is out of scope sits in a
                        // sealed region the player cannot reach, and rewriting it is inert." That
                        // premise is an inference about GEOMETRY and it is false. Two reporters hit
                        // the same case: the Summonwater Village Tibia Mariner pays out NOTHING on a
                        // Limgrave seed, because its Deathroot reward (f530170) is tagged Caelid
                        // while the boss stands in Mistwood. Repointed lot + unconditionally
                        // suppressed placeholder + no AP location = the player gets nothing.
                        //
                        // The static table is flag-keyed GAME data, identical for every apworld, and
                        // the FOREIGN path below already scopes with it under the comment "Scoped,
                        // NOT global: blanking a lot the seed does not check would eat a legitimate
                        // vanilla pickup". Ours is the path that skipped it. Fails toward today's
                        // behaviour: a lot the table cannot place is KEPT, never dropped.
                        let sl = load_static_lots();
                        let seed_flags: Vec<u32> = loc_flags.values().copied().collect();
                        let goods = er_logic::static_lots::scope_sent_lots(
                            &sl,
                            &seed_flags,
                            blank_map,
                            blank_enemy,
                        );
                        let non_goods = er_logic::static_lots::scope_sent_lots(
                            &sl,
                            &seed_flags,
                            non_goods_map,
                            non_goods_enemy,
                        );
                        let (blank_map, blank_enemy) = (goods.map, goods.enemy);
                        let (non_goods_map, non_goods_enemy) = (non_goods.map, non_goods.enemy);
                        let (dropped_goods, dropped_non_goods) = (goods.dropped, non_goods.dropped);
                        if dropped_goods + dropped_non_goods > 0 {
                            log::info!(
                                "check-lots: SCOPED OUT {dropped_goods} goods + {dropped_non_goods} \
                                 non-goods lot(s) whose check is not in this seed -- rewriting them \
                                 would leave a reachable pickup paying nothing (#329)"
                            );
                        } else if sl.map.is_empty() && sl.enemy.is_empty() {
                            log::warn!(
                                "check-lots: check_lots_table.json missing beside the DLL -- cannot \
                                 tell which lots belong to this seed, so every lot is rewritten as \
                                 before. An out-of-scope check will pay out nothing (#329)."
                            );
                        }
                        crate::check_lots::configure(
                            blank_map,
                            blank_enemy,
                            non_goods_map,
                            non_goods_enemy,
                            ph,
                        );
                    } else {
                        // STATIC FALLBACK -- vanilla suppression for a FOREIGN apworld.
                        //
                        // Measured in-game on the first Bedrock playtest (2026-07-13):
                        //     "vanilla suppressor INERT: checkItemFlags empty/absent in slot_data"
                        // -- every check paid out the VANILLA item AND the AP item, because only OUR
                        // apworld emits checkLotBlank*/checkItemFlags.
                        //
                        // But the blank-list is derived from ItemLotParam (flag -> lot -> goods
                        // slots): GAME data, not seed data, identical for every apworld. So we ship
                        // it (check_lots_table.json) and scope it to the flags THIS seed checks.
                        // 3018 of Bedrock's 3022 check flags (99.9%) suppressed, zero changes on his
                        // side. Same argument as shoplineup_flags.json.
                        //
                        // Scoped, NOT global: blanking a lot the seed does not check would eat a
                        // legitimate vanilla pickup.
                        let sl = load_static_lots();
                        if sl.is_empty() {
                            log::warn!(
                                "vanilla suppressor INERT: no checkLotBlank* in slot_data and no \
                                 usable check_lots_table.json beside the DLL. Every check will hand \
                                 out its VANILLA item as well as the AP item."
                            );
                        } else {
                            let seed_flags: Vec<u32> = loc_flags.values().copied().collect();
                            let (m, e) = er_logic::static_lots::blank_tables_for(&sl, &seed_flags);
                            let n = m.len() + e.len();
                            if n > 0 && sl.placeholder_goods != 0 {
                                // Foreign apworld: it emits no checkLotZero* (that table is derived from
                                // OUR gen_data), so the zero-slot tables are empty here.
                                crate::check_lots::configure(
                                    m,
                                    e,
                                    std::collections::HashMap::new(),
                                    std::collections::HashMap::new(),
                                    sl.placeholder_goods,
                                );
                                log::info!(
                                    "check-lots STATIC fallback: {} lot(s) blanked from \
                                     check_lots_table.json, scoped to this seed's {} check flag(s) \
                                     (foreign apworld -- it emits no checkLotBlank*)",
                                    n,
                                    seed_flags.len()
                                );
                            }
                        }
                    }
                }
                // START-GRANT COLLISION FIX (2026-07-24): flags the CLIENT ITSELF can set outside
                // a purchase -- uniqueStartGrants obtained-flags plus the keyitems acquire tables.
                // shop_sell must neither rewrite nor echo-arm a check detected by one of these:
                // the unique start grant set 60020/60110/60130 at connect and ECHO-DEDUP then ate
                // the echoes for locs 7770011/12/13 as "sold natively" (no sale ever happened) --
                // vanilla in the bag, three AP items lost. See er_logic::shop_echo (replay-tested).
                let echo_exempt: std::collections::HashSet<u32> = start
                    .unique_start_grants
                    .iter()
                    .map(|&(_, flag)| flag)
                    .chain(crate::keyitems::all_acquire_flags())
                    .collect();
                crate::shop_sell::configure(loc_flags.clone(), echo_exempt);
                // Same table, different consumer: shop_repoint inverts it to map a live row's
                // eventFlag_forStock back to its AP location, so it can look up that location's
                // preview good and WRITE it onto the row (er_logic::shop_repoint).
                crate::shop_repoint::configure(loc_flags.clone());
                // Third consumer of the same table, and the reason shop auto-hints move no
                // contract hash: shop_hints inverts it to turn a live row's eventFlag_forStock
                // into the AP location a shop-open should announce (er_logic::shop_hints).
                crate::shop_hints::configure(loc_flags.clone());
                // shopRunePrices: {ShopLineupParam row id (str) -> rolled rune price}.
                crate::shop_prices::configure(
                    i64_to_u32_map(sd.get("shopRunePrices"))
                        .into_iter()
                        .map(|(k, v)| (k as u32, v as i32))   // key = ShopLineupParam ROW id
                        .collect(),
                );
                // SLOT_DATA WINS, PARAMS ARE THE FALLBACK. `shopPreviewGoods` is the VANILLA ware
                // sitting in each check's shop row -- that is GAME data, not seed data, so when a
                // foreign apworld (Bedrock) omits the key we can read it straight off the live
                // ShopLineupParam instead. Do NOT configure an empty set here: configure() latches
                // CONFIGURED_SET, and shop_preview/shop_icon would then latch DONE on zero pairs
                // before shop_sell's runtime derivation ever arrives.
                //
                // The defect this fixes is a DISPLAY/ROUTING one, not a duplication one. On the
                // 2026-07-13 Bedrock playtest `shop-preview: configured 0 shop slot(s)`, so every
                // slot shop_sell could not natively rewrite (foreign, or an own-world gem/custom
                // reward) showed its VANILLA name and icon. The check still fires and the reward
                // still routes correctly -- the player simply has no way to see WHAT a slot holds or
                // WHO it belongs to, which is the whole point of a multiworld shop.
                if preview.is_empty() {
                    log::info!(
                        "shop-preview/icon: no shopPreviewGoods in slot_data -- deferring to the \
                         ShopLineupParam fallback in shop_sell"
                    );
                } else {
                    crate::shop_preview::configure(preview.clone());
                    crate::shop_icon::configure(preview);
                }
                crate::minibaker::configure(
                    sd.get("stoneswordVendorRow").and_then(|v| v.as_i64()).unwrap_or(0) as u32,
                );
                crate::scaling::configure(sd); // runtime enemy scaling (regionSphereTargets)
                // checkItemFlags: full raw item id -> check acquisition flags (the PORT-GAP
                // vanilla-suppress table; LIVE in the detour since 2026-07-01).
                let check_flags: std::collections::HashMap<u32, Vec<u32>> = sd
                    .get("checkItemFlags")
                    .and_then(|v| v.as_object())
                    .map(|o| {
                        o.iter()
                            .filter_map(|(k, v)| {
                                let id: u32 = k.parse().ok()?;
                                let fl = v.as_array()?
                                    .iter()
                                    .filter_map(|f| f.as_u64().map(|x| x as u32))
                                    .collect();
                                Some((id, fl))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                // STATIC FALLBACK for the id-keyed half (weapon/armor wares -- goods are blanked at
                // the lot above; suppressing goods BY ID would eat every Golden Rune you ever found).
                let check_flags = if check_flags.is_empty() {
                    let sl = load_static_lots();
                    let seed_flags: Vec<u32> = loc_flags.values().copied().collect();
                    let cif = er_logic::static_lots::check_item_flags_for(&sl, &seed_flags);
                    if !cif.is_empty() {
                        log::info!(
                            "checkItemFlags STATIC fallback: {} weapon/armor item id(s) suppressed \
                             from check_lots_table.json (foreign apworld emits no checkItemFlags)",
                            cif.len()
                        );
                    }
                    cif
                } else {
                    check_flags
                };
                crate::detour::configure_check_item_flags(check_flags);
                let scout = crate::scout_proof::ScoutProof::new(loc_flags.keys().copied().collect());
                // Goal-send: split goalLocations into flag-detected / checked-fallback buckets
                // against loc_flags (SPEC-goal-send-20260701.md; do NOT route through flagpoll).
                let goal_cfg = crate::goal::parse(sd, &loc_flags);
                // ...and ECHO it. `parse` logs how MANY goal locations there are; this says WHICH,
                // and names the region they sit in. The client log is the artifact we actually get
                // when a player reports a bad ending -- see goal::describe_goal for the case.
                let goal_game = client.game(client.this_player().game());
                crate::goal::log_goal(sd, |id| {
                    goal_game
                        .as_ref()
                        .and_then(|g| g.location(id).map(|l| l.name().to_string()))
                });
                // Boss-lock mode A (SPEC-boss-lock-tracker.md): parse the bossLockItems metadata
                // map into BossDef rows (gate=None for v0.2 — no sweepLockGates boss-key yet). This
                // is a presentation/defeat-flag-watch layer only; it mints no AP item and no check.
                let boss_defs = parse_boss_lock_items(sd.get("bossLockItems"));
                // ATTUNEMENT (attunement_gate): per-region {threshold, member_ap_ids, bloom_flags}.
                // Emitted only when the option is on; absent/empty => the whole feature stays off.
                let region_attunement = parse_region_attunement(sd.get("regionAttunement"));

                // Connect banner: build identity + slot_data contract version (+ gate result) so any
                // logfile self-identifies which build / contract produced it. Then a one-line
                // start-config summary of the exact fields we previously had to decompile the
                // multidata to see — startRegion, startGraces count, reveal_all_maps, the random-start
                // warp/area/done flags, and the area-lock count.
                let versions = sd.get("versions").and_then(|v| v.as_str()).unwrap_or("(none)");
                // LEGACY SEMVER GATE DELETED (2026-07-11). It fired on EVERY connect, unconditionally,
                // and cried wolf at Alaric across a whole playtest:
                //     "apworld/client version mismatch: seed wants apworld/0.2.0 contract/b68eaa15
                //      data/e4c73b06..., client is 0.1.0-beta.4 -- update the client"
                // It fed our `versions` string into er_semver::version_satisfies(), which expects a
                // semver RANGE (">=0.6.6 <0.7.0"). Ours is a DESCRIPTIVE string carrying the apworld
                // semver + contract hash + data hash, so the parse always fails and `.unwrap_or(false)`
                // turns that into "mismatch". It also compared the apworld's semver against the CLIENT
                // CRATE's version -- two independent numbering schemes that were never meant to match.
                //
                // It is fully superseded by the VERSION HANDSHAKE above (~line 413), which compares the
                // things that actually matter -- the CONTRACT HASH and the DATA HASH the binary was
                // compiled against -- and says OK / warns with specifics. An unsound duplicate that
                // always fires is worse than no gate: it trains you to ignore the real one.
                let gate_warn: Option<String> = None;
                let start_region = sd.get("startRegion").and_then(|v| v.as_str()).unwrap_or("");
                log::info!(
                    "=== ER-AP client {} | contract {versions} | slot '{name}' ===",
                    crate::game::CLIENT_BUILD
                );
                // A refused hook must SAY the feature is off. Silently hinting nothing for a whole
                // session looks exactly like a merchant with nothing on the shelf, and the player
                // would report it as "shop hints don't work" with no way to tell the two apart.
                if crate::shop_hints::is_inactive() {
                    log::warn!(
                        "shop hints INACTIVE this session -- the ESD talk hook did not install \
                         (see the SHOP HINTS INACTIVE line above). Opening a merchant will \
                         announce nothing to the multiworld; everything else is unaffected."
                    );
                }
                log::info!(
                    "startcfg: start_region={start_region:?} | startGraces={} reveal_maps={} startItems={} | randomStart warp/area/done={}/{}/{} | area_locks={}",
                    start.start_graces.len(),
                    start.reveal_all_maps,
                    start.start_items.len(),
                    region.random_start_warp_flag,
                    region.random_start_area_id,
                    region.random_start_done_flag,
                    region.area_lock_flags.len()
                );

                // RUNE COUNT AT CONNECT (world issue #259), the other half of the pair the
                // world-edge line makes. Unreadable here when connecting from the main menu, which
                // is normal and says so at WARN rather than going quiet; the first world edge then
                // supplies the opening bracket.
                crate::runes::log_sample(er_logic::rune_log::Sample::Connect);

                // Prime the fast-travel gate's known-good flag from the start graces. The client SETS
                // these at spawn, so they are really on and pointing the gate field at one is inert.
                // This removes the only case the old destructive fallback existed for -- booting
                // straight into a boss dungeon with nothing cached, which used to SET the field's flag
                // and, in a boss dungeon, that flag is the BOSS'S DEFEAT FLAG (Gael Tunnel, 2026-07-11).
                crate::fast_travel::prime_known_good(&start.start_graces);

                // Seed the config watcher with what we actually connected WITH, so its first tick is a
                // no-op instead of a spurious reconnect to the very file we booted from.
                {
                    let cfg = self.base().config_snapshot();
                    crate::config_watch::prime(&cfg.0, &cfg.1, cfg.2);
                }

                // THE PROGRESSION SURFACE = what the tracker stars. Computed HERE, inside the
                // closure where `sd` is in scope, then threaded out via the tuple and assigned below.
                //
                // "Big-ticket" is RETIRED, name and all. It was a SECOND list of "important checks"
                // that disagreed with the first: it named {MajorBoss, Remembrance, GreatRune} while
                // the apworld's progression surface is {Remembrance, Seedtree, Church, Boss, Fragment,
                // Revered}. Intersection: Remembrance alone. So this tracker starred MajorBoss/
                // GreatRune checks that the apworld FORBIDS a region Lock from ever occupying -- it
                // pointed the player at checks the locks could not be on. (Found 2026-07-12 reading a
                // spoiler: killing Malenia paid out a Smithing Stone [4].)
                //
                // NOTE THE DELETED FALLBACK. There is deliberately NO fall back to the static table
                // when the key is absent. The static table is the world's DEFAULT surface -- correct
                // for a default seed, WRONG for any seed that selected a different surface -- so
                // falling back would silently show a plausible, wrong star set. An empty star set is
                // visibly broken; a wrong one teaches the player something false. Prefer the visible
                // failure. (The earlier note here claimed the static table was "exactly the wrong
                // set". That is no longer true: tools/gen_location_regions.py now bakes the surface
                // itself, and the two are byte-identical for a default seed. The reasoning above is
                // why the fallback still stays deleted.)
                let progression_surface: std::collections::HashSet<u64> = {
                    match sd.get("progressionSurfaceLocations").and_then(|v| v.as_array()) {
                        Some(arr) => arr.iter().filter_map(|x| x.as_u64()).collect(),
                        None => {
                            log::warn!(
                                "slot_data has no progressionSurfaceLocations: the tracker will star \
                                 NOTHING. (Old apworld? bigTicketLocations is retired -- it named a \
                                 set progression could never reach.)"
                            );
                            std::collections::HashSet::new()
                        }
                    }
                };

                // THE TRACKER'S REGION MODEL, sent rather than baked (2026-07-28). The parse and
                // every rule about it live in er_logic::tracker_tables, which is host-tested; this
                // is the wiring. SAME DELETED FALLBACK as the surface above, and for a sharper
                // reason: the old baked table was built from the DEFAULT seed's regions, so on a
                // num_regions seed it grouped locations into regions the seed does not contain and
                // called them in-logic. A visibly empty tracker beats a confidently wrong one.
                let (tracker_tables, tracker_status) =
                    er_logic::tracker_tables::build_tracker_tables(
                        sd.get("locationRegions"),
                        sd.get("regionCoarseKeys"),
                    );
                match &tracker_status {
                    er_logic::tracker_tables::TablesStatus::Armed { .. } => {
                        log::info!("{}", tracker_status.describe())
                    }
                    er_logic::tracker_tables::TablesStatus::NoRegions => {
                        log::warn!("{}", tracker_status.describe())
                    }
                }

                (map, counts, region, fogwall, prog_cfg, name, sweeps, start, scout, gate_warn, loc_flags, goal_cfg, boss_defs, region_attunement, progression_surface, tracker_tables, feature_warn)
            });
            if let Some((
                map,
                counts,
                region,
                fogwall,
                prog_cfg,
                name,
                sweeps,
                start,
                scout,
                gate_warn,
                loc_flags,
                goal_cfg,
                boss_defs,
                region_attunement,
                progression_surface,
                tracker_tables,
                feature_warn,
            )) = parsed
            {
                log::info!(
                    "slot_data parsed: {} item-map, {} area-lock, {} progressive; player '{name}'",
                    map.len(),
                    region.area_lock_flags.len(),
                    prog_cfg.len()
                );
                self.item_map = Some(map);
                self.item_counts = counts;
                self.region = Some(region);
                self.fogwall = Some(fogwall);
                self.progressive = ProgressiveState::new(prog_cfg);
                self.my_name = Some(name);
                self.dungeon_sweeps = sweeps.0;
                self.sweep_lock_gates = sweeps.1;
                // F2 fix (2026-07-01): the flag-poll table travels in slot_data ("locationFlags")
                // now; baker-era apconfig.json no longer carries location_flags, so fresh installs
                // polled an EMPTY map (world pickups never sent checks -- seed looked vanilla).
                // slot_data wins; a legacy apconfig table still contributes sweep_flags / extras.
                let mut fp = crate::flagpoll::load();
                for (loc, flag) in loc_flags {
                    fp.location_flags.insert(loc, flag);
                }
                // greenfield flag-keyed dungeon sweeps (dungeonSweepFlags, parsed above into
                // sweeps.2): merge into the same sweep_flags table the legacy apconfig used, so the
                // existing poll loop fires them on boss kill. slot_data wins per flag.
                for (flag, locs) in sweeps.2 {
                    fp.sweep_flags.insert(flag, locs);
                }
                // WHETBLADE CHECK SPLIT (er_logic::whetblade): a whetblade's check flag is ALSO
                // the smithing menu's first-affinity unlock (Hexinton CE table, 2026-07-30), so
                // the poll moves each such check to the client-owned flag and whetblade_lots
                // repoints the lot's getItemFlagId to match. Runs on the MERGED table so legacy
                // apconfig entries are covered too; after this line no polled flag is ever one a
                // whetblade receive sets (keyitems), which is the false-collect fix. configure()
                // is called unconditionally so a seed without whetblade checks clears stale state.
                let whet_rewrites = er_logic::whetblade::repoint_poll_flags(&mut fp.location_flags);
                if !whet_rewrites.is_empty() {
                    log::info!(
                        "whetblade split: {} check(s) repointed off their affinity flag",
                        whet_rewrites.len()
                    );
                }
                crate::whetblade_lots::configure(whet_rewrites);
                log::info!(
                    "flag-poll table: {} location flags ({} sweep groups)",
                    fp.location_flags.len(),
                    fp.sweep_flags.len()
                );
                self.flag_poll = Some(fp);
                self.start = Some(start);
                self.scout = Some(scout);
                self.goal = Some(goal_cfg);
                log::info!(
                    "slot_data parsed: {} boss-lock def(s) (mode A Felled trophies)",
                    boss_defs.len()
                );
                self.boss_defs = boss_defs;
                self.region_attunement = region_attunement;
                log::info!(
                    "slot_data parsed: {} region attunement gate(s)",
                    self.region_attunement.len()
                );
                // Assign the progression surface parsed inside the slot_data closure above (where
                // `sd` was in scope).
                self.progression_surface = progression_surface;
                self.region_table = tracker_tables.region;
                self.coarse_table = tracker_tables.coarse;
                self.coarse_lock_items = tracker_tables.lock_items;
                self.slot_data_parsed = true;
                // Remember which seed this parse was for, so a later reconnect to a DIFFERENT seed
                // (without an ER reload) is detected above and rebuilds the per-seed state.
                self.parsed_seed = Some(current_room_seed.clone());
                // A seed that needs a client feature this build lacks: say so ON SCREEN too. A
                // player who never opens the log is exactly the one who would otherwise conclude
                // the option they set simply does nothing.
                if !feature_warn.is_empty() {
                    let now = self.toast_clock.elapsed().as_millis() as u64;
                    self.toasts.push(
                        format!("Client too old for this seed: {}", feature_warn.join(", ")),
                        now,
                    );
                }
                if let Some(warning) = gate_warn {
                    log::error!("{warning}");
                    self.log(ap::Print::message(warning));
                }
            }
        }

        // 2b. Load the persisted save once (resume watermark + progressive tiers).
        if self.slot_data_parsed && !self.save_loaded {
            // SAVE-KEY FIX (2026-07-02): key the save by the ROOM's seed_name (RoomInfo ground
            // truth), not the apconfig seed. The staged apconfig ships "seed":"", so every seed
            // shared ONE file (ap_save__<slot>.json) and a fresh world resumed at the previous
            // world's watermark -- seen live on the ER+HK seed: "resume at received index 134"
            // on a brand-new multiworld, so the first 134 receives (start items included) were
            // treated as already-granted and never placed. Region opens self-healed via the
            // reconcile ticks, which masked everything except the missing bag items.
            let room_seed = self
                .client()
                .map(|c| c.seed_name().to_string())
                .unwrap_or_default();
            let seed_key = if room_seed.is_empty() {
                // No RoomInfo seed (shouldn't happen once slot_data parsed) -- fall back to the
                // apconfig seed rather than never arming persistence.
                self.seed().to_string()
            } else {
                room_seed
            };
            if let Some(path) = save_file_path(&seed_key, self.my_name.as_deref().unwrap_or("")) {
                let st = match std::fs::read_to_string(&path) {
                    Ok(saved_text) => {
                        // R7 (SWEEP): from_json is tolerant -- a present-but-corrupt save would
                        // silently reset every watermark (duplicate start items + regrant burst).
                        if serde_json::from_str::<Value>(&saved_text).is_err() {
                            log::error!(
                                "save file {} is CORRUPT (not valid JSON) -- watermarks reset to defaults",
                                path.display()
                            );
                        }
                        SaveState::from_json(&saved_text)
                    }
                    Err(_) => SaveState::default(), // absent = fresh save (normal first run)
                };
                self.received_through = st.last_received_index.max(0) as usize;
                self.progressive.restore(
                    st.progressive_counter
                        .iter()
                        .map(|(k, &v)| (k.clone(), v))
                        .collect(),
                    st.progressive_high_index,
                );
                // `start_items_granted` is GONE (#267): possession is the dedup now, so there is
                // no per-character key to get wrong and nothing to inherit from a previous character.
                // Reuse the once-captured fresh-save baseline ACROSS reconnects
                // (gf-flagpoll-newsave-default-flags / "picked it up, got nothing"):
                // re-snapshotting the progressed save would fold mid-session pickups into the
                // baseline and strand their checks forever. Empty = fresh save, nothing
                // persisted yet -> capture below on the first in-world poll. Mirrors the pure
                // er_logic::flagpoll_baseline_replay::effective_baseline (host-tested).
                self.flag_poll_baseline = st.flag_poll_baseline.iter().copied().collect();
                self.flag_poll_baseline_done = !self.flag_poll_baseline.is_empty();
                self.last_persisted_index = st.last_received_index;
                log::info!("save persistence armed at {}", path.display());
                self.save_path = Some(path);
                log::info!(
                    "save loaded: resume at received index {}",
                    self.received_through
                );
            }
            self.save_loaded = true;
        }

        // 2c. Cache the slot's valid location set once (shop-scan dedup guard).
        if !self.locations_loaded {
            let v: HashSet<i64> = self
                .client()
                .map(|client| {
                    client
                        .checked_locations()
                        .map(|l| l.id())
                        .chain(client.unchecked_locations().map(|l| l.id()))
                        .collect()
                })
                .unwrap_or_default();
            if !v.is_empty() {
                self.valid_locations = v;
                self.locations_loaded = true;
            }
        }

        // 2d. Start grants: graces + map reveal (once, retried until the flag holder is up) and start
        //     items (Torrent etc.) once per save (persisted), gated on a captured inventory pointer.
        if self.slot_data_parsed {
            let already_flags = self.start_flags_done;
            // Same in_world tightening as can_grant (SWEEP H3): the inventory pointer never
            // resets on quit-to-menu, so menu-time start grants would write through a stale one.
            // I3 FIX (a): a refused save must not receive start grants either (see `can_grant`).
            let has_inv = crate::detour::has_inventory()
                && crate::flags::in_world()
                && !crate::reconcile_io::is_refused();
            // Start-ITEMS clobber guard (patch_greenfield_start_item_clobber.py): the static
            // inventory prime lets grants fire during the load screen, before the save/new-game
            // inventory finishes loading -- which then CLOBBERS the just-granted item (the Torch
            // never appeared in-game). Defer start-item grants until the inventory is genuinely
            // live: a real game AddItem has fired (bulk load replace done) OR we've been in-world
            // long enough for the load to settle. Timing-independent; received grants untouched.
            if crate::flags::in_world() {
                // A play_region change while in-world = a warp / fast-travel just landed. Restart the
                // settle window so a timer-based start grant can't fire on an inventory pointer the
                // map reload may have left stale (the spawn-kick CTD + the unadvertised Chapel warp-
                // out + early fast-travels). Skip the very first observation (last = None), so a fresh
                // spawn's own settle timer runs normally. real_pickup_seen() still short-circuits the
                // whole gate once a genuine pickup proves the pointer live, so this only ever DELAYS
                // the pre-first-pickup timer path -- an established character is untouched.
                let pr = crate::flags::play_region_id();
                // The tracker's scaling row rides this read (see `scaling_here_bucket`): same
                // value, one deref, and the row can never disagree with the gate about where the
                // player is. BUCKET, not the raw id -- `play_region` is 6200000 where the scaling
                // wire and `region_name_for_bucket` both speak 62000.
                self.scaling_here_bucket = pr.map(|r| r / 100);
                if pr.is_some() && pr != self.grant_gate_last_play_region {
                    if self.grant_gate_last_play_region.is_some() {
                        self.in_world_since = None;
                    }
                    self.grant_gate_last_play_region = pr;
                }
                self.in_world_since
                    .get_or_insert_with(std::time::Instant::now);
            } else {
                self.in_world_since = None;
                self.grant_gate_last_play_region = None;
                // Off-world: drop the row rather than leave it naming the last region visited.
                self.scaling_here_bucket = None;
            }
            let start_items_settled = crate::detour::real_pickup_seen()
                || self
                    .in_world_since
                    .is_some_and(|t| t.elapsed() >= std::time::Duration::from_secs(8));
            let mut did_flags = false;
            if let Some(sc) = self.start.as_ref() {
                // Gate start FLAGS on a loaded world (has_inventory), not just CSEventFlagMan being
                // up: setting grace/map flags during the load screen lets the subsequent save-data
                // load clobber them, which is the suspected cause of "no graces/maps in-game" despite
                // correct slot_data. (The standalone gated its grace flush the same way.) After
                // applying, read a sentinel grace back — only latch `done` once it sticks; a false
                // read-back means it was clobbered, so we log it and retry next tick.
                // 🛑 DO NOT add an `owns_flags()` gate here for symmetry with the item drain
                // below. This block is deliberately UNGATED: the unique-start-grants block further
                // down keys off `(already_flags || did_flags)`, and `already_flags` only ever
                // latches BECAUSE this runs even when the reconciler owns flags. Gate it and the
                // physick/whistle/bell grants die silently. (The reconciler's flags class now lands
                // map/grace flags on the FIRST in-world tick via WorldStability::flags_ready, so
                // this path is the RECONCILE_APPLY=none fallback, not the timing-critical one.)
                if !already_flags
                    && has_inv
                    && start_items_settled
                    && crate::startgrants::apply_start_flags(sc)
                {
                    let sentinel = sc.start_graces.first().copied();
                    let stuck = sentinel.is_none_or(crate::flags::get_event_flag);
                    if stuck {
                        did_flags = true;
                    } else {
                        log::warn!(
                            "start graces set but sentinel flag {sentinel:?} read back FALSE (clobbered by save load?) — retrying next tick"
                        );
                    }
                }
                // PLAIN START ITEMS are delivered by the reconciler's start-item ledger and
                // reconciled by `start_item_backfill`, whose dedup is POSSESSION (#267). The old
                // boolean-gated drain that lived here is DELETED: `start_items_granted` was keyed
                // (room seed, AP slot) with no character component, so a new character on a used
                // slot inherited "already granted" and got nothing. Both of the drain's motivating
                // bugs are re-homed onto the convergence loop and are strictly stronger there --
                // see `er_logic::start_backfill`'s `flask_dedup_survives_a_reload_via_the_bag_replay`
                // (the bag survives a reload; a boolean's absence does not) and
                // `a_clobbered_start_item_is_re_granted_not_lost_replay` (the Torch clobber is now
                // DETECTED and healed rather than avoided with a timer).
                // UNIQUE start grants (slot_data `uniqueStartGrants`, [[fullId, obtainedFlag]]):
                // grant the goods ONLY if the obtained-flag is unset, then set the flag WITH the
                // grant. The flag is the single source of truth for "has it" (the game itself
                // tracks possession with it; keyitems.rs sets the same flag on a pool receive),
                // so a re-run -- reload, reconnect, seed reset, late connect after the player
                // already found the pool copy -- skips by construction. Decision is the pure
                // er_logic::unique_grants::unique_grant_action (replay-tested); this block is glue.
                //
                // Runs REGARDLESS of reconciler ownership (unlike the plain drain above): the
                // reconciler never sees these ids -- they are deliberately absent from startItems,
                // so neither its unique_goods presence-diff nor its start-item ledger handles
                // them -- and the flag latch makes re-entry safe anyway. Gated on the start FLAGS
                // having landed (already_flags || did_flags): that proves the flag holder is up,
                // so the paired try_set_event_flag after a successful grant cannot miss.
                if !self.unique_grants_done
                    && (already_flags || did_flags)
                    && has_inv
                    && start_items_settled
                {
                    let mut all_done = true;
                    for (i, &(full_id, flag)) in sc.unique_start_grants.iter().enumerate() {
                        if self.unique_grants_ok.contains(&i) {
                            continue;
                        }
                        let goods = full_id & 0x0FFF_FFFF;
                        if !er_logic::unique_grants::unique_grant_action(
                            crate::flags::get_event_flag(flag),
                        ) {
                            log::info!(
                                "unique grant: goods {goods} ({full_id:#x}) -- flag {flag} already set, SKIP \
                                 (player already has it)"
                            );
                            self.unique_grants_ok.insert(i);
                            continue;
                        }
                        if crate::detour::grant_full_id(full_id, 1) {
                            if crate::flags::try_set_event_flag(flag, true) {
                                log::info!(
                                    "unique grant: goods {goods} ({full_id:#x}) granted + flag {flag} set"
                                );
                            } else {
                                // Should be unreachable behind the already_flags gate (holder is
                                // up). NOT retried this session -- a retry would re-grant the
                                // goods; the unset flag makes the NEXT session re-grant instead.
                                log::warn!(
                                    "unique grant: goods {goods} granted but flag {flag} write FAILED -- \
                                     possession latch missing; next session will re-grant \
                                     (fail-loud, not retried this session)"
                                );
                            }
                            self.unique_grants_ok.insert(i);
                        } else {
                            all_done = false;
                            warn_unique_grant_fail_once(i, full_id);
                        }
                    }
                    if all_done {
                        self.unique_grants_done = true;
                        if !sc.unique_start_grants.is_empty() {
                            log::info!(
                                "unique start grants settled ({} decided)",
                                sc.unique_start_grants.len()
                            );
                        }
                    }
                }
            }
            if did_flags {
                self.start_flags_done = true;
                if let Some(sc) = self.start.as_ref() {
                    log::info!(
                        "start graces + map reveal applied: {} grace flag(s), reveal_maps={}",
                        sc.start_graces.len(),
                        sc.reveal_all_maps
                    );
                }
            }
        }

        // Prime the inventory pointer from a game static (if enabled+confirmed) so grants flush
        // without waiting for the player's first in-game pickup. No-op until USE_STATIC_INVENTORY_PRIME
        // is turned on; the detour still captures the game's own pointer on a real pickup regardless.
        crate::detour::prime_inventory_if_needed();

        // I3 FIX (b), 2026-08-01: THIS BLOCK WAS HOISTED from the end of `update_live` (it used to
        // run after steps 3-5). `owns_*` now tells the truth about whether a Driver exists, and the
        // reconciler was armed AFTER the receive step that asks -- so on the first eligible tick
        // `armed()` was false while step 4 ran, and the OLD path granted the whole post-watermark
        // backlog UNPACED in one frame (the mass-grant CTD class the paced budget exists for), then
        // the reconciler re-owed the ledgered consumables when it armed a section later. Arming
        // before the step that asks removes the window; the block is otherwise UNCHANGED. It reads
        // only step-2 state (`slot_data_parsed`, `save_path`, `received_through`, the seed tables)
        // and rebuilds `recv` from the client every frame, so it never needed step 3/4 output.
        // ---- RECONCILER (strangler). DRY-RUN (`RECONCILE_DRYRUN=1`) computes + LOGS the desired-state
        //      diff WITHOUT applying; APPLY mode (`RECONCILE_APPLY` names a class, default `flags`)
        //      applies the owned classes via `reconcile_io::tick`, and the OLD handlers above skip
        //      whatever the reconciler owns (see the `owns_*` gates). Widened from the dry-run-only
        //      gate: `tick()` was previously unreachable in apply mode (the cutover wiring gap). The
        //      whole block is a no-op only when neither dry-run nor any apply class is active.
        //
        //      THE SHAPE, named (issue #237), because both halves look like something you could
        //      skip and neither is:
        //        * DESIRED side  -- rebuilt from scratch EVERY frame (the cumulative `recv` below),
        //          then handed to `set_inputs`, which EQUALITY-GUARDS it in the pure layer. The
        //          rebuild is cheap-ish and unconditional; the swap is guarded. Do not move that
        //          guard out here -- `reconcile_io::set_inputs` also re-stamps `d.slot`.
        //        * OBSERVED side -- `tick()` polls EVERY frame, unconditionally. The game emits no
        //          event when it diverges from what we wrote, so there is nothing to subscribe to
        //          and convergence is not a reason to stop looking. There is no dirty flag and no
        //          nudge; a gate here would break four already-healed bug classes. ----
        if (crate::reconcile_io::dry_run_enabled() || crate::reconcile_io::apply_active())
            && self.slot_data_parsed
        {
            // Snapshot the FULL received stream (not just the tail) -- DesiredInputs is the CUMULATIVE
            // set, and the reconciler derives idempotency from the whole set, not per-event deltas.
            let mut recv: Vec<(i64, String, i64, bool)> = Vec::new();
            if let Some(client) = self.client() {
                let my_slot = client.this_player().slot();
                for (idx, ri) in client.received_items().iter().enumerate() {
                    // ECHO-DEDUP (Gap 2): same predicate the live receive loop uses -- an echo of our
                    // own check whose rewritten shop row already sold the reward natively.
                    let echo_skip = ri.sender().slot() == my_slot
                        && crate::shop_sell::echo_skip(ri.location().id());
                    recv.push((
                        idx as i64,
                        ri.item().name().to_string(),
                        ri.item().id(),
                        echo_skip,
                    ));
                }
            }
            let inputs = self.build_desired_inputs(&recv);
            // Defer reconciler init to the first STABLE IN-WORLD tick: `reconcile_io::init` reads the
            // ER save_slot + play_time to key the per-character watermark (er-startitems-newchar-no-
            // regrant), and those are only valid once a character is loaded. The reconciler is inert
            // before world-stability anyway, so nothing is lost by waiting; received items accumulate
            // in `recv` (rebuilt each tick) and are applied in full once init runs.
            let world_loaded = crate::detour::has_inventory() && crate::flags::in_world();
            if !self.reconcile_inited {
                if world_loaded {
                    let path = self
                        .save_path
                        .as_ref()
                        .and_then(|p| p.parent().map(|d| d.join("reconcile.json")))
                        .unwrap_or_else(|| std::path::PathBuf::from("reconcile.json"));
                    // `received_through` (this save's persisted `last_received_index`) is passed to
                    // init for the positive-frontier cross-check; the per-character keying inside init
                    // decides fresh-vs-resume (see reconcile_io::init / er_logic::reconcile::seed_trust).
                    crate::reconcile_io::init(inputs, path, self.received_through as i64);
                    self.reconcile_inited = true;
                }
                // else: world not loaded yet -- wait; recv keeps accumulating for the eventual init.
            } else {
                crate::reconcile_io::set_inputs(inputs);
            }
            // POLL. Unconditional, every frame, converged or not (see `reconcile_io::tick`).
            // In dry-run it computes + logs the per-action diff and applies nothing.
            crate::reconcile_io::tick();
        }

        // 3. Snapshot the received-item stream in one client borrow (RecvItem mirrors for the
        //    seam, plus the cumulative name set the reconcile ticks need). Under own_world:true
        //    this stream ALSO carries the echoes of our own self-found checks.
        let mut disp = self.dispatched_through;
        // SWEEP H3 (verified watermark, via er_logic::receive below): both name-dispatch and
        // grants only run with a loaded world + live inventory pointer — menu-time writes go
        // through a stale pointer / get discarded, and used to advance the watermark on faith.
        // I3 FIX (a), 2026-08-01: REFUSED means QUARANTINED, not merely un-owned. Before I3 a
        // refused session stood down because `owns_*` read config-true; now that they read false,
        // the OLD grant paths would happily mutate the very save the marker guard just refused to
        // touch. Holding the cursor here is also the right H3 behaviour: reconnect the correct save
        // and the whole stream replays.
        let can_grant = crate::detour::has_inventory()
            && crate::flags::in_world()
            && !crate::reconcile_io::is_refused();
        // Cumulative set of ALL received item names — natural-key triggers need the full history
        // (a clause may require an item received many ticks ago), not just this tick's new names.
        let mut received_all: HashSet<String> = HashSet::new();
        // HISTORY-AGNOSTIC flask reconcile: total count of "Progressive Flask Upgrade" across the
        // WHOLE received stream (not gated by the watermarks below) — AP replays every received item
        // on connect, so this count is stable across reconnect/save-load and needs no ledger.
        let mut flask_upgrade_count: usize = 0;
        // Same doctrine, for the Scadutree blessing: fragments DELIVERED, not fragments held. The
        // game consumes held fragments when you revere at a DLC grace, which used to switch the
        // game-wide blessing off mid-run. Matched by ITEM ID through apIdsToItemIds rather than by
        // name, so a foreign apworld (Bedrock/fswap) that calls its fragments something else still
        // counts — a name match would fail silently on exactly the seeds we cannot test.
        let mut scadu_fragment_units: i32 = 0;
        // #342, and the SAME history-agnostic doctrine as `flask_upgrade_count` above: a talisman's
        // slot is decided by its position in the received stream and by how many Talisman Pouches
        // preceded it, both walked over the WHOLE stream rather than the watermarked tail.
        //
        // 🛑 IT HAS TO BE THE WHOLE STREAM. `received_through` is persisted per save, so a
        // reconnect only re-enqueues items past it; a tally that started at zero each connect would
        // report zero pouches to a player who found three last session and pin every talisman back
        // onto slot 1. The classification and the counting both live in
        // `er_logic::auto_equip::TalismanStream`, which is where the tests for them are.
        let mut talisman_stream = er_logic::auto_equip::TalismanStream::default();
        let mut talisman_pos: HashMap<i64, er_logic::auto_equip::TalismanPos> = HashMap::new();
        let mut snapshot: Vec<RecvItem> = Vec::new();
        // DIAGNOSTIC (#293): `received_items().len()` -- the number that appeared in NO log line
        // anywhere, and without which "the cursor is stuck" and "the stream is stuck" read the
        // same. `None` means no client this tick, which is NOT the same as a stream of length 0
        // (see the probe call at 4d). Carried out of the borrow; nothing else reads it.
        let mut stream_len: Option<usize> = None;
        if let Some(client) = self.client() {
            let items = client.received_items();
            stream_len = Some(items.len());
            if items.len() < disp {
                disp = 0; // reconnect shrank the stream -> replay name dispatch from index 0
            }
            // Items below BOTH watermarks are AlreadyPushed no-ops; skip snapshotting them.
            let floor = disp.min(self.received_through);
            let my_slot = client.this_player().slot();
            for (idx, ri) in items.iter().enumerate() {
                let name = ri.item().name().to_string();
                if name == crate::flask::FLASK_UPGRADE_ITEM {
                    flask_upgrade_count += 1;
                }
                if let Some(map) = self.item_map.as_ref() {
                    scadu_fragment_units += er_logic::upgrades::fragment_units_for(
                        ri.item().id(),
                        map,
                        &self.item_counts,
                    );
                    // Talisman Pouches and talismans, in stream order. Keyed by AP index so the
                    // enqueue below can look up the position for the item it is holding.
                    if let Some(&full) = map.get(&ri.item().id())
                        && let Some(pos) = talisman_stream.push(full as i32)
                    {
                        talisman_pos.insert(idx as i64, pos);
                    }
                }
                if can_grant && idx >= floor {
                    // ECHO-DEDUP: an echo of our own check whose rewritten shop row already
                    // sold the reward natively must not grant again (shop_sell::echo_skip).
                    let echo_skip = ri.sender().slot() == my_slot
                        && crate::shop_sell::echo_skip(ri.location().id());
                    snapshot.push(RecvItem {
                        index: idx as i64,
                        ap_item_id: ri.item().id(),
                        name: name.clone(),
                        echo_skip,
                    });
                }
                received_all.insert(name);
            }
            // Publish the fragment total for `upgrades::tick_global_scadu`.
            //
            // INSIDE the `client()` block on purpose. Out here it would run on a tick with no
            // connection and publish 0 -- so an AP disconnect mid-fight would strip the player's
            // blessing, which is a worse failure than the held-count bug this replaced. With no
            // client we simply do not update, and the last known total stays latched.
            //
            // It is still unconditional WITHIN the block, so a seed with no fragments publishes a
            // real 0 rather than inheriting a count from a previous connect
            // (`er-seed-change-bypasses-the-marker-guard`: a stale cross-seed value is how 229
            // checks once crossed seeds). A reconnect replays the whole stream, so the first pass
            // after connecting recomputes the total from scratch.
            crate::upgrades::set_received_fragments(scadu_fragment_units);
        }

        // 3b. Baked region-lock fallback arming (bedrock interop): the first received
        //     "<Region> Lock" in the prepared scope is the proof the seed really placed locks;
        //     merge the baked config into the live one so the name-dispatch below (and every
        //     later tick's kick-watch/reconcile) sees it. No-op for seeds whose slot_data spoke
        //     a region key (nothing prepared) and for foreign no-lock seeds (never armed).
        if let Some(cfg) = self.region.as_mut() {
            crate::region::tick_baked_fallback(cfg, &received_all);
        }

        // 4. The receive seam (er_logic::receive, host-tested): per item, name-dispatch when
        //    idx >= dispatched_through (keyitems fast path / region open / progressive routing,
        //    via ReceiveDispatch), then grant when idx >= received_through. received_through only
        //    advances past items whose grant VERIFIABLY placed (SWEEP H3): on a failed placement
        //    it is rolled back to the failed item and the tail retries in order next tick.
        //    dispatched_through keeps its advance regardless — name effects are idempotent and
        //    the section-6 reconcile ticks self-heal any lost flag write.
        let newly_dispatched_from = disp as i64; // BOSS_LOCKS_PATCH: notification window
        let mut dispatched = disp as i64;
        let mut pushed = self.received_through as i64;
        let mut unlocked: Vec<String> = Vec::new();
        if can_grant && !snapshot.is_empty() {
            let empty_map = HashMap::new();
            let item_map = self.item_map.as_ref().unwrap_or(&empty_map);
            let mut game = EldenRingHook;
            let mut dispatch = ReceiveDispatch {
                region: self.region.as_ref(),
                progressive: &mut self.progressive,
                hook: &mut game,
                unlocked: Vec::new(),
            };
            for ri in &snapshot {
                let pushed_before = pushed;
                let action = er_logic::receive::process_received_item(
                    ri,
                    &mut dispatched,
                    &mut pushed,
                    item_map,
                    &self.item_counts,
                    &mut dispatch,
                );
                match action {
                    GrantAction::Enqueue {
                        full_id, qty, name, ..
                    } => {
                        // auto_equip: queue a received WEAPON, PROTECTOR or TALISMAN to be
                        // equipped once it's in the bag. Independent of the grant path below
                        // (reconciler may own the actual grant), so this fires for every
                        // recognized receive.
                        // `enqueue` self-gates on the option AND on the category -- do NOT re-add
                        // an `is_weapon` filter here, which is what previously excluded armour.
                        // The category set lives in ONE place (`er_logic::auto_equip::equipable`)
                        // for the same reason: #295 was fixed by adding an arm there and nothing
                        // else, because there is no second filter to keep in step.
                        crate::auto_equip::enqueue(full_id, talisman_pos.get(&ri.index).copied());
                        // STRANGLER (goods+ledger, THE ATOMIC FLIP): this ONE call grants every
                        // received item — key items/runes (goods) AND consumables (ledger). Once the
                        // reconciler owns BOTH classes it is the sole received-item grant path (goods
                        // via GrantUnique, consumables via the ledger watermark), so skip this grant
                        // to avoid double-granting consumables on reload. NAME dispatch above and the
                        // `dispatched_through`/`pushed` advance stay; `pushed` simply advances past
                        // this item (no H3 hold — the reconciler owns placement). Runtime-revertible:
                        // drop `goods`/`ledger` from RECONCILE_APPLY and this path grants again.
                        if !(crate::reconcile_io::owns_goods()
                            && crate::reconcile_io::owns_ledger())
                        {
                            if dispatch.hook.grant_full_id(full_id, qty) {
                                // Great-rune "restored" flag is set by keyitems::set_acquire_flags
                                // (191-196); the AP item already grants the restored goods row, so
                                // there is no additive goods grant here (that double-granted the rune).
                            } else {
                                // H3: the grant did NOT place — hold received_through at this item
                                // and stop so the tail replays in order next tick (never advance the
                                // watermark past an unverified grant).
                                pushed = pushed_before;
                                log::warn!(
                                    "grant '{name}' (idx {}) failed to place -- receive watermark held for retry",
                                    ri.index
                                );
                                break;
                            }
                        }
                    }
                    GrantAction::SkipProgressive => {
                        // Tier effects already applied in the dispatch (ReceiveDispatch). Great-rune
                        // restore is handled by keyitems::set_acquire_flags (event flag), not a grant.
                    }
                    GrantAction::SkipUnmapped { ap_item_id } => {
                        // R5 (SWEEP): AP id absent from apIdsToItemIds and progressive didn't
                        // handle it — nothing granted; without this the item vanishes traceless.
                        // FALSE-ALARM SILENCE (patch_silence_regionlock_grant_warn):
                        // region-lock items are intentionally absent from apIdsToItemIds --
                        // they are handled by the NAME-dispatch above (open_on_received_name
                        // sets the region's open flag). They still reach this grant arm and
                        // would otherwise trip the misleading "no ER mapping ... contract
                        // drift?" warn. Identify them cleanly by presence in region_open_flags
                        // (the regionOpenFlags slot_data key set, keyed by lock name -- no
                        // hardcoded ap ids) and log at debug. Truly-unmapped ids keep the warn.
                        let is_region_lock = dispatch
                            .region
                            .map(|c| c.region_open_flags.contains_key(&ri.name))
                            .unwrap_or(false);
                        if is_region_lock {
                            log::debug!(
                                "region-lock '{}' (ap id {ap_item_id}) -> handled via open flag (not an ER item grant)",
                                ri.name
                            );
                        } else if ri.name.starts_with(er_logic::traps::ITEM_PREFIX) {
                            // TRAP ITEMS are synthetic tokens like Boss Keys -- deliberately absent
                            // from apIdsToItemIds, because the effect is ours to fire and there is
                            // no ER item to hand over. Queued rather than fired here: this runs in
                            // the receive loop, which can land while the player is in a menu, and a
                            // trap dropped there is GONE (the item is already marked received and
                            // the server will never resend it). traps::poll_pending delivers it.
                            crate::traps::enqueue_by_item_name(
                                &ri.name,
                                self.toast_clock.elapsed().as_millis() as u64,
                            );
                        } else if ri.name.starts_with("Boss Key: ") {
                            // Boss Keys (mode B) are SYNTHETIC gate tokens, intentionally absent from
                            // apIdsToItemIds: they gate a felled boss's reward (boss_key_pending) and its
                            // dungeon sweep (sweep_lock_gates) via the received-name set, NOT an ER item
                            // grant. Recognize them here (like region locks) so they do not trip the
                            // misleading "no ER mapping ... contract drift?" warn. Debug-log instead.
                            log::debug!(
                                "boss-key '{}' (ap id {ap_item_id}) -> mode-B gate token (not an ER item grant)",
                                ri.name
                            );
                        } else {
                            warn_unmapped_once(&ri.name, ap_item_id);
                        }
                    }
                    GrantAction::SkipNativelySold { name, full_id } => {
                        log::info!(
                            "shop-sell: echo grant skipped -- {name} was sold natively at purchase (ECHO-DEDUP)"
                        );
                        // 🛑 THE ITEM IS IN THE BAG. Only the GRANT is redundant here -- the
                        // rewritten shop row handed the player the real item at purchase time, and
                        // the watermark advances precisely because it IS delivered. auto_equip keys
                        // off DELIVERY, not off granting, so it has to be told either way.
                        //
                        // MOTIVATING CASE (rule 11): Alaric, 2026-08-03, client 0.3.2
                        // (59420f32f445), `auto_equip: 1`. He bought Godfrey Icon -- a talisman --
                        // from the Twin Maiden Husks and it did not equip. The log shows why:
                        //   [APS] Alaric found their Godfrey Icon (Roundtable Hold :: Battle Axe)
                        //   shop-sell: echo grant skipped -- Godfrey Icon ... (ECHO-DEDUP)
                        // and then nothing, because this arm logged and returned. Every one of that
                        // session's 13 receives was a shop purchase, so auto_equip was inert for
                        // the entire run while reporting itself enabled.
                        //
                        // Not talisman-specific and never was: weapons and armour bought from a
                        // repointed shop row went the same way.
                        if let Some(fid) = full_id {
                            crate::auto_equip::enqueue(fid, talisman_pos.get(&ri.index).copied());
                        }
                    }
                    GrantAction::AlreadyPushed => {}
                }
            }
            unlocked = dispatch.unlocked;
        }
        self.dispatched_through = dispatched.max(0) as usize;
        self.received_through = pushed.max(0) as usize;
        // Announce the EFFECT, not the receipt (the flask reconciler's rule, and for the same
        // reason): "you received Liurnia Lock" is a receipt the player must translate, while
        // "Region unlocked: Liurnia" is the thing that actually changed about their run. The
        // console line keeps its exact wording so there is ONE phrasing to learn, not two.
        //
        // ASCII only -- this goes through the FMG path (`every_toast_is_ascii`); region names are
        // already ASCII by construction, and an em-dash here drew as `?` in v0.2.18.
        let region_toast_live = self.region_toast_primed;
        for region in unlocked {
            // A GATED CHILD's Lock lights no graces -- the world withholds the bundle while the
            // wall is armed and emits `regionGraces["<region> Lock"] = []` DELIBERATELY (its own
            // comment: "[] and not key-absence: the client warns about a genuine lock with NO
            // regionGraces entry, and this one is intended"). So an empty-but-present bundle IS
            // the wire that says "this Lock admits you, it does not warp you", and it was already
            // arriving; nothing had ever asked it. A player read the resulting silence as the
            // feature being broken (LordChungle, Nexus 2026-08-10) -- see region_unlocked_message.
            //
            // LIVE, not static: Leyndell's wall is a Great Rune COUNT, and a seed that sets it to 0
            // disarms the wall and the Lock really does warp you in. Only the seed knows.
            let withheld = self
                .region
                .as_ref()
                .and_then(|c| c.region_graces.get(&format!("{region} Lock")))
                .is_some_and(|fs| fs.is_empty());
            let line = er_logic::region_lock::region_unlocked_message(&region, withheld);
            self.log(ap::Print::message(line.clone()));
            if region_toast_live {
                let now = self.toast_clock.elapsed().as_millis() as u64;
                self.toasts.push(line, now);
            }
        }
        // Prime AFTER the loop, so the connect replay is the baseline and everything later is news.
        // Deliberate cost: a lock that lands inside that first pass is logged but not toasted. It is
        // the only pass where a real arrival is indistinguishable from a replayed one, and silence
        // there is better than six false toasts on every reconnect.
        self.region_toast_primed = true;
        // BOSS_LOCKS_PATCH: overlay line on boss-lock receipt -- the lock item is otherwise
        // invisible in the console (no region apparatus, so "Region unlocked" never fires for
        // it). Mirrors that line's semantics, including the reconnect replay (name-dispatch
        // replays the stream). The gate itself is poll-driven, so a lock arriving after the
        // boss kill fires the held sweep within a few seconds of this line.
        // Announce received Boss Keys (mode B) so the SYNTHETIC gate token is visible in the console
        // (it has no ER item grant, so no "Region unlocked" line ever fires for it). Covers BOTH
        // sweep-gated keys AND keys that only gate a boss's own reward check (the latter are absent
        // from sweep_lock_gates, so the old sweep-only guard skipped them -> the player saw nothing).
        // Match on the SYNTHETIC name; SHOW the legible `display_key` when a boss def carries one, else
        // the boss name (the "Boss Key: " prefix stripped). Owned Strings so the immutable boss_defs /
        // sweep_lock_gates borrows end before `self.log`.
        {
            let mut announced: Vec<String> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            for ri in &snapshot {
                if ri.index < newly_dispatched_from {
                    continue;
                }
                let is_boss_key = self.sweep_lock_gates.values().any(|g| g == &ri.name)
                    || self
                        .boss_defs
                        .iter()
                        .any(|d| d.gate.as_deref() == Some(ri.name.as_str()));
                if is_boss_key && seen.insert(ri.name.clone()) {
                    let shown = self
                        .boss_defs
                        .iter()
                        .find(|d| d.gate.as_deref() == Some(ri.name.as_str()))
                        .and_then(|d| d.gate_display())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            ri.name
                                .strip_prefix("Boss Key: ")
                                .unwrap_or(ri.name.as_str())
                                .to_string()
                        });
                    announced.push(shown);
                }
            }
            for shown in announced {
                self.log(ap::Print::message(format!(
                    "Boss Key received: {shown} -- its boss reward (and any dungeon sweep) unlocks once held."
                )));
            }
        }

        // 4c. Persist on watermark advance.
        if self.received_through as i64 != self.last_persisted_index {
            self.write_save();
            self.last_persisted_index = self.received_through as i64;
        }

        // 4d. RECEIVE-PATH PROBE -- diagnostic only, changes nothing (2026-08-02).
        //
        // RULE 11, the motivating cases:
        //   * #293 -- Hazel's receive cursor sat at 172 for three days across seven sessions while
        //     her checks kept sending. "Sends work, receives dead" has exactly three causes in this
        //     client (F1 `can_grant` false all session / F2 cursor ahead of the stream / F3 the
        //     unbounded H3 hold), and on that build ALL THREE PRINT THE SAME LOG -- because
        //     `received_items().len()` was logged nowhere, so nothing could tell "the cursor is
        //     stuck" from "the stream is stuck". This is that line.
        //   * #296 -- delivery reported converged while owing ~14 items, sat dead 18s, then applied
        //     all 14 the instant the player crossed a world edge. Whether those items were in the
        //     stream during the dead window was unanswerable because nothing timestamped the
        //     stream GROWING. The probe emits on any change, so it does.
        //
        // Read AFTER the step-4 advance, so a healthy tick reads `stream == cursor` and a held
        // watermark is visible as the gap. Only when a client exists: with none there is no stream
        // to compare, and a fabricated 0 would look exactly like F2.
        if let Some(stream_len) = stream_len {
            let now = self.toast_clock.elapsed().as_millis() as u64;
            let report = self.recv_probe.observe(
                er_logic::receive_probe::RecvState {
                    stream_len,
                    cursor: self.received_through,
                    // The three `can_grant` inputs, BROKEN OUT. The conjunction alone cannot say
                    // which bug it is: no inventory pointer is a foreign AddItemFunc hook, refused
                    // is the marker guard, not-in-world is a menu tick.
                    has_inventory: crate::detour::has_inventory(),
                    in_world: crate::flags::in_world(),
                    refused: crate::reconcile_io::is_refused(),
                },
                now,
            );
            if let Some(line) = report.line {
                log::info!("{line}");
            }
            // F2. 🛑 REPORTED, NOT REPAIRED. Clamping `received_through` down to the stream length
            // would re-grant every item from there on (~172 of them in #293) -- that is a product
            // decision, not a diagnostic one, and it is Alaric's call. Nothing here writes a
            // watermark.
            if let Some(warning) = report.warning {
                log::warn!("{warning}");
            }
            // I4: a log line is invisible from the player's chair -- which is exactly how the
            // REFUSED guard went unnoticed for ~55 minutes of boblerrr's session. F2 suspends
            // delivery permanently, so it owes the same on-screen notice. Re-pushed every tick on
            // purpose: the deck refreshes identical text instead of stacking, so it is free after
            // the first frame, and the condition persists until the player acts.
            if let Some(notice) = report.toast {
                self.toasts.push(notice, now);
            }
        }

        // 5. Shop / NPC / offline discovery: synthetic placeholders that bypassed the detour are in
        //    the bag. REPORT only (echo grants); dedup by checked-location so it can't re-report.
        if can_grant && self.locations_loaded {
            let scanned = crate::inventory::scan_synthetics();
            if !scanned.is_empty() {
                let mut to_check: Vec<i64> = Vec::new();
                if let Some(client) = self.client() {
                    for s in &scanned {
                        if self.valid_locations.contains(&s.location)
                            && !client.is_local_location_checked(s.location)
                        {
                            to_check.push(s.location);
                        }
                    }
                }
                if !to_check.is_empty() && !crate::reconcile_io::is_refused() {
                    log::info!("shop/offline discovery: {} new check(s)", to_check.len());
                    if let Some(client) = self.client_mut()
                        && let Err(e) = client.mark_checked(to_check.iter().copied())
                    {
                        log::warn!("shop mark_checked failed: {e}");
                    }
                }
            }
        }

        // 5b. Flag-poll: report detour-bypass checks (NPC gifts, NPC death drops, offline pickups)
        //     whose guarding event flag has fired, plus dungeon/boss sweeps. Throttled — flags don't
        //     change fast and the map can be large. Dedup via the server's checked set (reload-safe).
        self.poll_counter = self.poll_counter.wrapping_add(1);
        // CTD guard (2026-07-24, "Beside the Rampart Gaol" warp postmortem): gate the WHOLE poll
        // on a live world. `get_event_flag` walks `CSEventFlagMan.virtual_memory_flag`, and during
        // a load screen the streamer is attaching/detaching per-map flag blocks for the outgoing
        // and incoming maps — a ~4,850-flag sweep racing that teardown/rebuild is a native-crash
        // window. Nothing is lost by waiting: checks cannot fire mid-load, and the next in-world
        // poll (sub-second) picks up exactly the same flags.
        if self.locations_loaded && self.poll_counter.is_multiple_of(15) && crate::flags::in_world()
        {
            // Capture the new-save baseline once, the first time we poll IN-WORLD (flags are
            // readable only after the save loads). Any guarding flag already set at this point
            // is a new-save default, not a pickup made this session, so it must not fire a check.
            // Computed into an owned local first so the &self.flag_poll borrow ends before the
            // &mut self.flag_poll_baseline assignment.
            if !self.flag_poll_baseline_done && crate::flags::in_world() {
                let baseline: HashSet<u32> = match self.flag_poll.as_ref() {
                    Some(fp) => fp
                        .location_flags
                        .values()
                        .copied()
                        .filter(|&f| crate::flags::get_event_flag(f))
                        .collect(),
                    None => HashSet::new(),
                };
                self.flag_poll_baseline = baseline;
                self.flag_poll_baseline_done = true;
                // Prime the boss-defeat baseline in the same shot: any boss already dead on the
                // first in-world poll (prior session / reconnect) seeds boss_flag_prev so its
                // "Felled: <name>" banner never re-fires. Only a kill made THIS session (an
                // unset->set edge after this) reaches newly_felled == true. Disjoint field access
                // (boss_defs read / boss_flag_prev write) — no self.log call inside.
                self.boss_flag_prev = self
                    .boss_defs
                    .iter()
                    .filter(|d| crate::flags::get_event_flag(d.flag))
                    .map(|d| d.flag)
                    .collect();
                log::info!(
                    "flag-poll baseline: {} guarding flags already set on connect (excluded); {} boss(es) already felled (banner suppressed)",
                    self.flag_poll_baseline.len(),
                    self.boss_flag_prev.len()
                );
                // Persist the freshly-captured baseline NOW so a reconnect before the next
                // item still loads it (persist-on-watermark-advance can lag arbitrarily).
                self.write_save();
            }
            // ATTUNEMENT (attunement_gate) -- prime the bloom baseline once, the first in-world poll.
            // Any region already attuned on connect (prior session / reconnect: the SERVER checked
            // set is authoritative and replayed) has its graces bloomed WITHOUT re-bannering; a
            // crossing made THIS session banners normally. Mirrors boss_flag_prev priming. Its OWN
            // latch (attunement_primed) -- NOT flag_poll_baseline_done, which can already be true from
            // the persisted baseline, so keying off it would strand priming and re-banner on reconnect.
            if !self.attunement_primed
                && crate::flags::in_world()
                && !self.region_attunement.is_empty()
            {
                let already: Vec<String> = match self.client() {
                    Some(client) => self
                        .region_attunement
                        .iter()
                        .filter(|(_, att)| {
                            er_logic::attunement::attuned(&att.members, att.threshold, |m| {
                                self.valid_locations.contains(&m)
                                    && client.is_local_location_checked(m)
                            })
                        })
                        .map(|(region, _)| region.clone())
                        .collect(),
                    None => Vec::new(),
                };
                let mut bloom: Vec<u32> = Vec::new();
                for region in &already {
                    if let Some(att) = self.region_attunement.get(region) {
                        bloom.extend(att.bloom_flags.iter().copied());
                    }
                }
                for f in bloom {
                    crate::flags::set_event_flag(f, true);
                }
                for region in already {
                    self.attuned_regions.insert(region);
                }
                self.attunement_primed = true;
                log::info!(
                    "attunement primed: {} region(s) already attuned on connect (bloomed, banner suppressed)",
                    self.attuned_regions.len()
                );
            }
            // BOSS KEYS (mode B) -- prime the sealed baseline once, the first in-world poll. A boss
            // felled in a PRIOR session whose "Boss Key: <Boss>" is still unreceived (its boss_ap_id
            // check never sent, so absent from the SERVER checked set) is seeded into
            // boss_key_pending SILENTLY, so a reconnect re-derives the seal WITHOUT re-bannering; a
            // kill made THIS session (after priming) banners normally. Mirrors boss_flag_prev /
            // attunement priming. received_all is the cumulative, reconnect-replayed received-name set.
            if !self.boss_key_primed
                && crate::flags::in_world()
                && self.boss_defs.iter().any(|d| d.gate.is_some())
            {
                let mut seed: Vec<(u32, i64)> = Vec::new();
                if let Some(client) = self.client() {
                    for d in &self.boss_defs {
                        if let Some(key) = d.gate.as_deref()
                            && d.boss_ap_id != 0
                            && crate::flags::get_event_flag(d.flag)
                            && !received_all.contains(key)
                            && self.valid_locations.contains(&d.boss_ap_id)
                            && !client.is_local_location_checked(d.boss_ap_id)
                        {
                            seed.push((d.flag, d.boss_ap_id));
                        }
                    }
                }
                for (flag, loc) in seed {
                    self.boss_key_pending.entry(flag).or_default().insert(loc);
                }
                self.boss_key_primed = true;
                log::info!(
                    "boss-key baseline: {} boss check(s) sealed on connect (deferred silently)",
                    self.boss_key_pending
                        .values()
                        .map(|s| s.len())
                        .sum::<usize>()
                );
            }
            let mut to_check: Vec<i64> = Vec::new();
            // SWEEP VISIBILITY (2026-07-24, Tibia Mariner playtest): a boss sweep firing was
            // invisible -- the console showed only the member items ("found their X"), never the
            // boss, so the player concluded the sweep had NOT fired when it had. Collect each
            // group that newly fires this poll -- (defeat flag, member count, a member loc for
            // the region lookup) -- inside the immutable borrow; bannered after it ends.
            let mut sweep_fired: Vec<(u32, usize, i64)> = Vec::new();
            // SWEEP WATCH (2026-08-07): every group's trigger flag as observed THIS poll, gated
            // groups included. Collected in the immutable borrow and reported after it, exactly
            // like `sweep_fired`.
            let mut sweep_obs: Vec<er_logic::sweep_watch::GroupObservation> = Vec::new();
            // (location, detection flag) for every member this poll actually granted -- the input
            // to the sweep flag flush below (er_logic::sweep_flush::flags_to_assert).
            let mut swept_members: Vec<(i64, u32)> = Vec::new();
            if let (Some(fp), Some(client)) = (self.flag_poll.as_ref(), self.client()) {
                // Refresh the vanilla-suppressor's collected-flag set: the acquisition flags of every
                // location already in the server checked-set (loc->flag via locationFlags). A location
                // enters this set only AFTER its check was reported, so the detour suppresses a
                // first-time pickup and passes only a genuine re-pickup. See detour::KNOWN_COLLECTED_FLAGS.
                // NOTE: the valid_locations guard comes first (same ordering as the poll loop
                // below); valid_locations is kept correct per-seed by reset_for_new_seed, so no
                // datapackage-unknown id reaches is_local_location_checked -- its panic path is
                // unreachable (the seed-change reset is the real fix for the reconnect panic).
                let collected: std::collections::HashSet<u32> = fp
                    .location_flags
                    .iter()
                    .filter(|&(&loc, _)| {
                        self.valid_locations.contains(&loc) && client.is_local_location_checked(loc)
                    })
                    .map(|(_, &flag)| flag)
                    .collect();
                crate::detour::set_known_collected_flags(collected);
                // 2026-07-06: some getItemFlagId flags are SET on a brand-new save (Flask of
                // Crimson Tears 60000, physick / sacred-tear flags, Leyndell Crimson Hood
                // 10007452, Black Knifeprint 400357). flag_poll_baseline (captured on the first
                // in-world poll) holds them so we never false-check them; the genuine pickup
                // still registers via the AddItemFunc detour.
                for (&loc, &flag) in &fp.location_flags {
                    if self.valid_locations.contains(&loc)
                        && !client.is_local_location_checked(loc)
                        && !self.flag_poll_baseline.contains(&flag)
                        && crate::flags::get_event_flag(flag)
                    {
                        to_check.push(loc);
                    }
                }
                for (trigger, members) in &self.dungeon_sweeps {
                    // Draft B: the location-keyed dungeon_sweeps groups carry NO gate today --
                    // sweepLockGates is flag-keyed and applied in the sweep_flags loop below. A
                    // location-keyed gate table would arrive with the future ItemLotParam join;
                    // until then these groups (minidungeons / chokepoint carves whose lock is not in
                    // this seed's pool) are ungated. (This map is empty in current seeds.)
                    if let Some(&flag) = fp.location_flags.get(trigger)
                        && crate::flags::get_event_flag(flag)
                    {
                        for &m in members {
                            if self.valid_locations.contains(&m)
                                && !client.is_local_location_checked(m)
                            {
                                to_check.push(m);
                                swept_members
                                    .push((m, fp.location_flags.get(&m).copied().unwrap_or(0)));
                            }
                        }
                    }
                }
                for (&flag, locs) in &fp.sweep_flags {
                    // 🛑 OBSERVED BEFORE THE GATE, ON PURPOSE. A group held by its lock gate and a
                    // group that does not exist logged identically before this -- as nothing -- so
                    // "armed and waiting" was unreadable and a 2m45s delay looked like a broken
                    // sweep (bobler, 2026-08-07). Read once here and reused by the fire test below,
                    // so this costs no extra flag read on the firing path.
                    let flag_set = crate::flags::get_event_flag(flag);
                    sweep_obs.push((flag, locs.len(), flag_set));
                    // Draft B: hold a gated group's sweep until its boss-lock item is in the
                    // cumulative received set. sweepLockGates is FLAG-keyed, so look it up by this
                    // sweep's boss-defeat flag; poll-driven, so a lock received AFTER the kill fires
                    // the held sweep retroactively on a later tick.
                    if !er_logic::sweep_gate::gate_open(
                        self.sweep_lock_gates.get(&flag).map(String::as_str),
                        |n| received_all.contains(n),
                    ) {
                        continue;
                    }
                    if flag_set {
                        let mut granted = 0usize;
                        let mut sample_loc = 0i64;
                        for &loc in locs {
                            if self.valid_locations.contains(&loc)
                                && !client.is_local_location_checked(loc)
                            {
                                to_check.push(loc);
                                granted += 1;
                                sample_loc = loc;
                                swept_members
                                    .push((loc, fp.location_flags.get(&loc).copied().unwrap_or(0)));
                            }
                        }
                        // Sweep-visibility banner candidates: only a group that granted something
                        // this poll and hasn't bannered this session. A reconnect stays quiet by
                        // construction -- already-checked members are filtered above, so granted
                        // == 0 and nothing is pushed.
                        if granted > 0 && !self.sweep_bannered.contains(&flag) {
                            sweep_fired.push((flag, granted, sample_loc));
                        }
                    }
                }
            }
            // SWEEP WATCH lines first, so the census precedes the first banner it explains.
            // Silent unless something changed -- the poll runs all session and a per-poll dump
            // would bury the log.
            if !sweep_obs.is_empty() {
                // Same observation feeds the tracker's Boss sweeps section (read on the tick, not
                // in the render -- see `sweep_flag_state`).
                self.sweep_flag_state = sweep_obs.iter().map(|&(f, _, set)| (f, set)).collect();
                for line in self.sweep_watch.observe(&sweep_obs) {
                    log::info!("{line}");
                }
            }
            // SWEEP VISIBILITY banners (latched once per group per session via sweep_bannered;
            // reset on seed change). Same log/overlay channel as the Felled/attunement banners.
            for (flag, granted, sample_loc) in sweep_fired {
                if !self.sweep_bannered.insert(flag) {
                    continue;
                }
                let boss = self.boss_defs.iter().find(|d| d.flag == flag).map(|d| {
                    d.name
                        .strip_prefix("Felled: ")
                        .unwrap_or(d.name.as_str())
                        .to_string()
                });
                let region = self.region_table.get(&(sample_loc as u64)).cloned();
                // 🛑 THE FLAG IS APPENDED UNCONDITIONALLY. The old chain degraded to the
                // PRETTIEST remaining label rather than the most IDENTIFYING one: with
                // `0 boss-lock def(s)` the boss name can never resolve, so every sweep fell to
                // `Boss sweep ({region})` and dropped the flag -- while the arm one line below it
                // was the one that printed it. bobler's 49-check sweep could not be mapped back to
                // a trigger at all (2026-08-07), and slot_data's own `dungeonSweepFlags` echo is
                // truncated in the log, so this was the last copy of that fact.
                let label = match (boss, region) {
                    (Some(b), Some(r)) => format!("Boss sweep ({r}): {b}"),
                    (Some(b), None) => format!("Boss sweep: {b}"),
                    (None, Some(r)) => format!("Boss sweep ({r})"),
                    (None, None) => "Boss sweep".to_string(),
                };
                let label = format!("{label} [trigger flag {flag}]");
                self.log(ap::Print::message(format!(
                    "{label} -- {granted} check(s) granted."
                )));
            }
            // SWEEP FLAG FLUSH (2026-07-24, "chests are still broken"): granting the check told
            // the SERVER the member was collected; nothing told the GAME. Its lot is already
            // neutralised to the 8852 placeholder, so the pickup sat in the world as a dead prop
            // that opens and gives nothing. Stage the owed flags, then re-assert every tick until
            // each reads back (reconcile, don't dispatch -- the sweep never fires twice).
            if !swept_members.is_empty() {
                let owed = er_logic::sweep_flush::flags_to_assert(&swept_members, |f| {
                    crate::flags::get_event_flag(f)
                });
                for f in owed {
                    if !self.sweep_flag_pending.contains(&f) {
                        self.sweep_flag_pending.push(f);
                    }
                }
            }
            if !self.sweep_flag_pending.is_empty() && crate::flags::in_world() {
                let owed_before = self.sweep_flag_pending.len();
                for &f in &self.sweep_flag_pending {
                    let _ = crate::flags::try_set_event_flag(f, true);
                }
                er_logic::sweep_flush::retire(&mut self.sweep_flag_pending, |f| {
                    crate::flags::get_event_flag(f)
                });
                let landed = owed_before - self.sweep_flag_pending.len();
                if landed > 0 {
                    log::info!(
                        "sweep-flush: {landed} swept member flag(s) confirmed set ({} still owed)",
                        self.sweep_flag_pending.len()
                    );
                }
            }
            // ATTUNEMENT-RELEASE (attunement_gate, SPEC-gf-boss-lock-tracker "Attunement-release"):
            // gate the BOSS PAYOUT -- the boss's own check + every dungeon-sweep member -- behind the
            // region's in-region attunement. Ordinary in-region pickups (which BUILD attunement) are
            // never gated. `to_check` already holds this poll's candidates; partition out any payout
            // check whose region is not yet attuned (DEFER into boss_payout_pending), then burst-
            // release a region's held checks the poll it crosses the threshold. Attunement counts from
            // the SERVER checked set (valid_locations pre-filter, then is_local_location_checked) so it
            // survives save-load / reconnect / re-snapshot. Empty regionAttunement => whole block off.
            if !self.region_attunement.is_empty() {
                // Payout checks = boss's own check (boss_ap_id) + every dungeon-sweep member (both the
                // location-keyed and the flag-keyed sweep tables). Cheap to rebuild at the 15-tick throttle.
                let mut payout_locs: HashSet<i64> = HashSet::new();
                for d in &self.boss_defs {
                    if d.boss_ap_id != 0 {
                        payout_locs.insert(d.boss_ap_id);
                    }
                }
                for members in self.dungeon_sweeps.values() {
                    payout_locs.extend(members.iter().copied());
                }
                if let Some(fp) = self.flag_poll.as_ref() {
                    for locs in fp.sweep_flags.values() {
                        payout_locs.extend(locs.iter().copied());
                    }
                }
                // Attunement state per region + partition to_check, computed under ONE immutable client
                // borrow into owned locals (so self can be mutated after the borrow ends).
                let mut att_state: HashMap<String, (u32, u32, bool)> = HashMap::new(); // region -> (count, threshold, attuned)
                let mut kept: Vec<i64> = Vec::with_capacity(to_check.len());
                let mut deferred_new: Vec<(String, i64)> = Vec::new();
                if let Some(client) = self.client() {
                    let checked = |m: i64| {
                        self.valid_locations.contains(&m) && client.is_local_location_checked(m)
                    };
                    for (region, att) in &self.region_attunement {
                        let count = er_logic::attunement::attuned_count(&att.members, checked);
                        att_state.insert(
                            region.clone(),
                            (count, att.threshold, count >= att.threshold),
                        );
                    }
                    for &loc in &to_check {
                        if payout_locs.contains(&loc)
                            && let Some(region) = self.region_table.get(&(loc as u64))
                            && let Some(&(_, _, attuned)) = att_state.get(region)
                            && !attuned
                        {
                            deferred_new.push((region.clone(), loc));
                            continue;
                        }
                        kept.push(loc);
                    }
                }
                to_check = kept;

                // Record newly-deferred payout checks (per-region debt); banner only the growth.
                let mut newly_sealed: BTreeMap<String, usize> = BTreeMap::new();
                for (region, loc) in deferred_new {
                    if self
                        .boss_payout_pending
                        .entry(region.clone())
                        .or_default()
                        .insert(loc)
                    {
                        *newly_sealed.entry(region).or_default() += 1;
                    }
                }

                // Burst-release: a region attuned this poll drains its held checks back into to_check
                // (the existing mark below sends them). Re-evaluation would re-produce them too, but the
                // explicit drain gives the release banner its count and is robust to a missed re-poll.
                let attuned_regions_now: Vec<String> = att_state
                    .iter()
                    .filter(|(_, v)| v.2)
                    .map(|(r, _)| r.clone())
                    .collect();
                let mut released: BTreeMap<String, usize> = BTreeMap::new();
                for region in &attuned_regions_now {
                    if let Some(pending) = self.boss_payout_pending.get_mut(region)
                        && !pending.is_empty()
                    {
                        let n = pending.len();
                        to_check.extend(pending.iter().copied());
                        pending.clear();
                        released.insert(region.clone(), n);
                    }
                }

                // Attunement bloom: light each newly-attuned region's graces once (latch in
                // attuned_regions, reset on seed change). Collect flags/banners first (immutable
                // region_attunement read) so the &mut self.log calls below hold no field borrow.
                let mut bloom_to_light: Vec<u32> = Vec::new();
                let mut crossed: Vec<String> = Vec::new();
                if self.attunement_primed && crate::flags::in_world() {
                    for region in &attuned_regions_now {
                        if !self.attuned_regions.contains(region)
                            && let Some(att) = self.region_attunement.get(region)
                        {
                            bloom_to_light.extend(att.bloom_flags.iter().copied());
                            crossed.push(region.clone());
                        }
                    }
                }
                for f in &bloom_to_light {
                    crate::flags::set_event_flag(*f, true);
                }
                for region in &crossed {
                    self.attuned_regions.insert(region.clone());
                }

                // Banners (suppressed until primed so a reconnect's already-known state stays quiet).
                if self.attunement_primed && crate::flags::in_world() {
                    for (region, n) in newly_sealed {
                        let (cur, thr) = att_state
                            .get(&region)
                            .map(|&(c, t, _)| (c, t))
                            .unwrap_or((0, 0));
                        self.log(ap::Print::message(format!(
                            "Boss felled -- {n} check(s) sealed; attune {cur}/{thr} {region}"
                        )));
                    }
                    for region in &crossed {
                        self.log(ap::Print::message(format!(
                            "Attuned to {region} -- all graces revealed."
                        )));
                    }
                    for (region, n) in released {
                        self.log(ap::Print::message(format!(
                            "Attunement reached -- {n} sealed check(s) released in {region}."
                        )));
                    }
                }
            }
            // BOSS KEYS (mode B, SPEC-gf-boss-lock-tracker "Boss Key: <Boss>"): gate a felled boss's
            // OWN check (boss_ap_id) behind its "Boss Key: <Boss>" item. The dungeon-sweep MEMBERS are
            // already held by sweep_lock_gates via sweep_gate::gate_open in the sweep loop above; this
            // block covers ONLY the boss's own check, which fires through the locationFlags poll and so
            // sits in to_check the moment the boss is felled. Poll-driven: a key received AFTER the kill
            // releases the held check on a later tick. Composes with attunement-release (a check must
            // clear BOTH gates). Empty gate set => block off. is_local_location_checked (server-
            // authoritative, applied in the loops above) makes a re-run idempotent.
            if self.boss_defs.iter().any(|d| d.gate.is_some()) {
                // boss_ap_id -> (defeat flag, "Felled: <Boss>" name, "Boss Key: <Boss>" key) and
                // defeat flag -> (name, key), built under an immutable boss_defs borrow (owned maps,
                // so the mutable boss_key_pending borrow below is conflict-free).
                let by_loc: HashMap<i64, (u32, String, String, String)> = self
                    .boss_defs
                    .iter()
                    .filter_map(|d| {
                        d.gate.as_ref().and_then(|g| {
                            // Draft E: carry a legible display label (display_key when present, else
                            // the synthetic gate name) alongside the synthetic gate `key`.
                            (d.boss_ap_id != 0).then(|| {
                                let display = d.gate_display().unwrap_or(g.as_str()).to_string();
                                (d.boss_ap_id, (d.flag, d.name.clone(), g.clone(), display))
                            })
                        })
                    })
                    .collect();
                let by_flag: HashMap<u32, (String, String)> = self
                    .boss_defs
                    .iter()
                    .filter_map(|d| {
                        d.gate
                            .as_ref()
                            .map(|g| (d.flag, (d.name.clone(), g.clone())))
                    })
                    .collect();

                // Partition to_check: DEFER any gated boss's own check whose key is not yet received.
                let mut kept: Vec<i64> = Vec::with_capacity(to_check.len());
                // Draft E: newly_sealed's 2nd field is the DISPLAY label (legible key when the
                // apworld shipped one). Gating still keys on the synthetic `key`.
                let mut newly_sealed: BTreeMap<u32, (String, String, usize)> = BTreeMap::new();
                for &loc in &to_check {
                    if let Some((flag, name, key, display)) = by_loc.get(&loc)
                        && !er_logic::sweep_gate::gate_open(Some(key.as_str()), |n| {
                            received_all.contains(n)
                        })
                    {
                        if self.boss_key_pending.entry(*flag).or_default().insert(loc) {
                            let e = newly_sealed
                                .entry(*flag)
                                .or_insert_with(|| (name.clone(), display.clone(), 0usize));
                            e.2 += 1;
                        }
                        continue;
                    }
                    kept.push(loc);
                }
                to_check = kept;

                // Burst-release: any held boss whose key is now in received_all drains its pending
                // checks back into to_check (the mark below sends them); cleared so a later poll can't
                // re-release (and the server set filters it anyway).
                let ready_flags: Vec<u32> = self
                    .boss_key_pending
                    .iter()
                    .filter(|(flag, pend)| {
                        !pend.is_empty()
                            && by_flag
                                .get(*flag)
                                .map(|(_, key)| received_all.contains(key))
                                .unwrap_or(false)
                    })
                    .map(|(flag, _)| *flag)
                    .collect();
                let mut released: BTreeMap<u32, (String, usize)> = BTreeMap::new();
                for flag in ready_flags {
                    if let Some(pending) = self.boss_key_pending.get_mut(&flag)
                        && !pending.is_empty()
                    {
                        let n = pending.len();
                        to_check.extend(pending.iter().copied());
                        pending.clear();
                        if let Some((name, _)) = by_flag.get(&flag) {
                            released.insert(flag, (name.clone(), n));
                        }
                    }
                }

                // Banners (in_world guard; the reconnect-seeded seal from priming inserted its loc
                // already, so newly_sealed skips it -> no re-banner). name is "Felled: <Boss>"; strip
                // the prefix for a clean boss label. key is the full "Boss Key: <Boss>".
                if crate::flags::in_world() {
                    for (_, (name, display, n)) in newly_sealed {
                        let boss = name.strip_prefix("Felled: ").unwrap_or(name.as_str());
                        // Draft E: show the legible display label; gating already used the synthetic.
                        self.log(ap::Print::message(format!(
                            "{boss} felled -- {n} check(s) sealed; awaiting {display}"
                        )));
                    }
                    for (_, (name, n)) in released {
                        let boss = name.strip_prefix("Felled: ").unwrap_or(name.as_str());
                        self.log(ap::Print::message(format!(
                            "Unsealed: {boss} -- {n} stored check(s) released."
                        )));
                    }
                }
            }
            // Gate check reporting on the minibake refuse guard: a save whose marker identity mismatches
            // this seed/slot must NOT report its (seed-A) flags as (seed-B) checks — that corrupts the
            // multiworld, strictly worse than any double-grant. The reconciler is also unarmed while refused.
            if !to_check.is_empty() && !crate::reconcile_io::is_refused() {
                to_check.sort_unstable();
                to_check.dedup();
                log::info!("flag-poll: {} new check(s)", to_check.len());
                if let Some(client) = self.client_mut()
                    && let Err(e) = client.mark_checked(to_check.iter().copied())
                {
                    log::warn!("flag-poll mark_checked failed: {e}");
                }
            }

            // Boss-lock mode A (SPEC-boss-lock-tracker.md): emit the one-shot "Felled: <Boss>"
            // banner on the unset->set edge of each boss's DEFEAT flag. Presentation only — no
            // self-send; the boss's own boss_ap_id check still fires through the locationFlags
            // poll above. Idempotent across polls via boss_flag_prev (primed on the first
            // in-world poll, so already-dead bosses don't re-banner; persists until seed change).
            // Guarded on in_world so a load-screen flag read can't fire a banner. Banners are
            // collected first (immutable &self.boss_defs borrow) then logged (&mut self.log).
            if crate::flags::in_world() {
                let mut felled_banners: Vec<String> = Vec::new();
                for def in &self.boss_defs {
                    let now = crate::flags::get_event_flag(def.flag);
                    let prev = self.boss_flag_prev.contains(&def.flag);
                    if er_logic::boss_felled::newly_felled(prev, now) {
                        // def.name is already the full "Felled: <Boss>" label.
                        felled_banners.push(def.name.clone());
                    }
                    if now {
                        self.boss_flag_prev.insert(def.flag);
                    }
                }
                for banner in felled_banners {
                    self.log(ap::Print::message(banner));
                }
            }
        }

        // 5c. Goal-send (SPEC-goal-send-20260701.md): once EVERY goalLocations entry is done —
        //     local DefeatFlag first (immune to another slot's !collect), checked-set fallback
        //     for detection-table stragglers — send ClientStatus::Goal. Same throttle as the
        //     flag poll; gated on a loaded world so flags are never read during a load screen.
        //     Session latch only: a re-send after reconnect is idempotent server-side.
        if !self.sent_goal
            && can_grant
            && self.locations_loaded
            && self.poll_counter.is_multiple_of(15)
        {
            let met = match (self.goal.as_ref(), self.client()) {
                (Some(cfg), Some(client)) => crate::goal::is_met(
                    cfg,
                    crate::flags::get_event_flag,
                    // Pre-filter against valid_locations (kept correct per-seed by reset_for_new_seed)
                    // so no datapackage-unknown id reaches is_local_location_checked.
                    |l| self.valid_locations.contains(&l) && client.is_local_location_checked(l),
                    // goalItems: the item must be HELD. `received_all` is the cumulative, reconnect-
                    // replayed received-name set, so this survives save-load and !collect.
                    |n| received_all.contains(n),
                ),
                _ => false,
            };
            if met {
                let sent = match self.client_mut() {
                    Some(client) => match client.set_status(ap::ClientStatus::Goal) {
                        Ok(_) => true,
                        Err(e) => {
                            log::warn!("goal: set_status(Goal) failed (will retry next poll): {e}");
                            false
                        }
                    },
                    None => false,
                };
                if sent {
                    self.sent_goal = true;
                    log::info!("goal: all goal locations complete -> ClientStatus::Goal sent");
                    self.log(ap::Print::message(
                        "GOAL COMPLETE! Victory sent to Archipelago.".to_string(),
                    ));
                }
            }
        }

        // 6. Region-lock KICK + random-start warp trigger (order matters: the warp sets the
        //    done-flag that KICK's start-window guard waits on, so fire it before the kick check).
        let mut graces_lit: Vec<String> = Vec::new();
        // Player-facing overlay messages from the region ticks (warp requested / arrival /
        // kick) -- collected here because cfg borrows self, logged after the borrow ends.
        let mut region_msgs: Vec<String> = Vec::new();
        if let Some(cfg) = self.region.as_ref() {
            if let Some(m) = crate::region::tick_random_start_warp(cfg) {
                region_msgs.push(m);
            }
            // Natural-key regions (Raya/Mountaintops/Snowfield/...) bloom when their vanilla-key
            // disjunction is satisfied. Gated on a loaded world (can_grant) so the flags it sets
            // aren't clobbered by the save load — same reason the start graces are gated.
            if can_grant {
                crate::region::tick_natural_key_triggers(cfg, &received_all);
                // STRANGLER (flags): the reconciler owns region-open/grace-bundle flags (RegionFlags)
                // and key-item/great-rune obtained flags (KeyItem) and self-heals them every stable
                // tick, so skip these two OLD re-appliers when it owns `flags`. `RECONCILE_APPLY=none`
                // (or dry-run) re-enables them with no rebuild. Idempotent either way (flag writes).
                if !crate::reconcile_io::owns_flags() {
                    // Re-apply lock unlocks whose one-shot receive was discarded at menu/load
                    // (lost graces/open flags -- 2026-07-01 playtest). Latched on the open flag.
                    crate::region::tick_reconcile_received_locks(cfg, &received_all);
                    // R3 (SWEEP): key-item obtained flags, same reconcile family -- the one-shot
                    // write in 4a is lost at menu/load; this re-applies with the flag as the latch.
                    crate::keyitems::tick_keyitem_flags(&received_all);
                }
                // grace_rando: light received "Grace: ..." items (graceItems port-gap, 2026-07-01).
                graces_lit = crate::region::tick_grace_items(cfg, &received_all);
                // Grace attunement: touch enough of a region's graces and the rest light. Self-
                // latching on the bloom flags, so this is safe to call every settled tick.
                for name in crate::region::tick_grace_attunement(cfg) {
                    graces_lit.push(format!("{name} (attuned)"));
                }
            }
            if let Some(m) = crate::region::tick_kick(cfg) {
                region_msgs.push(m);
            }
        }
        // 6b. Capital-version per-tick latch (self-configured; INERT until slot_data spoke and
        //     the burn-done flag is set). Holds 9116 matched to the capital the player is
        //     standing in, so the Erdtree burn never permanently strands the Royal checks.
        crate::region::tick_capital();
        // 6c. BOSS GRANTS (#413): hand the player the tool the BOSS assumes they arrived with.
        //     Keyed on the CHARACTER, never the arena -- an enemy randomiser moves bosses between
        //     rooms, so a place key would arm the wrong fight (Alaric, 2026-08-06).
        //
        //     `present` is a LOAD test -- "is c4710 instantiated anywhere" -- so it goes true the
        //     moment the area streams in. That is right for the GRANT (hand the player the tool
        //     before the fight, wherever the randomiser put him) and wrong for the HOLD.
        let boss_present = if can_grant {
            crate::scaling::any_character_present(er_logic::boss_grants::RYKARD_CHR_ID)
        } else {
            None
        };
        //     🛑 THE HOLD KEYS ON THE HEALTHBAR, NOT ON PRESENCE (#413, bobler 2026-08-07). Held
        //     from area load, it covered a fight that had not started -- minutes of a foreign
        //     weapon being refused for no reason, and the pause could arm on the very tick the
        //     spear was enqueued (the empty queue it waits for is the PRE-grant empty, since the
        //     enqueue below runs after this line). The healthbar is the game's own statement that
        //     the fight is happening NOW, so the window closes to the fight itself and the spear
        //     has long since drained by the time it opens.
        //     Read UNCONDITIONALLY, outside `can_grant`: if a lost `can_grant` skipped it the hold
        //     would STRAND at true and block every weapon equip for the rest of the session.
        //     `None` (GameDataMan down) resolves to "not paused", so losing the ability to look
        //     releases the hold rather than latching it.
        let healthbar = crate::flags::boss_healthbar_npc_param_id();
        let rykard_fight_on =
            er_logic::boss_grants::healthbar_shows(er_logic::boss_grants::RYKARD_CHR_ID, healthbar);
        // LIVE half of the serpent-hunter probe. Deliberately OUTSIDE `can_grant`: the whole
        // point is to capture what the game had during the fight even on a tick where the grant
        // path could not run, and it only reads.
        crate::serpent_hunter::probe_fight(rykard_fight_on);
        if can_grant {
            // The latch is possession, never the Serpent-Hunter's obtained-flag: that flag is what
            // check 7771816 is keyed on, so latching on it would collect a check the player never
            // found.
            // ONE BAG READ, TWO ANSWERS (#413, boblerrr 2026-08-07 18:31:38). `held_row` is
            // `None` when the bag is unreachable, `Some(None)` when no spear is held, and
            // `Some(Some(full_id))` with the EXACT id the inventory reports. `holds` is derived
            // from it rather than read separately, so the question "does he have one" and the
            // question "which one" can never disagree -- them disagreeing is the whole of the
            // defect this closes.
            let held_row =
                crate::upgrades::held_weapon_row(er_logic::boss_grants::SERPENT_HUNTER_BASE);
            let holds = held_row.map(|r| r.is_some());
            // ⭐⭐⭐ SAY WHY NOTHING HAPPENED (#413, boblerrr 2026-08-07 "no spear whatsoever").
            // A non-grant is silent: `boss_grant_action` returns None for "Rykard was never
            // loaded", "you already hold one" and "a read failed" alike, so a player log cannot
            // tell the design from a defect. Keyed on the (healthbar, presence) pair so it lands
            // at most once per boss fight entered rather than every tick, the same shape as
            // kick-watch's KICK_WATCH_LAST_PR.
            // 🛑 The `swap` is the LEFT operand on purpose: it carries the side effect of
            // recording this tick's key, so it must run on every tick regardless of what the
            // right-hand side decides. Short-circuiting can only ever skip the RIGHT side.
            let diag_key = er_logic::boss_grants::diag_key(healthbar, boss_present);
            if BOSS_GRANT_DIAG_LAST.swap(diag_key, std::sync::atomic::Ordering::Relaxed) != diag_key
                && let Some(d) =
                    er_logic::boss_grants::grant_diagnosis(healthbar, boss_present, holds)
            {
                log::info!("{d}");
            }
            let mut game = EldenRingHook;
            if let Some(m) = er_logic::boss_grants::tick(&mut game, boss_present, holds) {
                // EQUIP IT. `enqueue` self-gates on the auto_equip option AND on the category, so
                // this is a no-op for anyone who turned auto_equip off. Pass the BASE id, exactly
                // as the receive path passes its raw full_id: the auto_equip::enqueue_id seam is
                // what applies auto_upgrade, and duplicating that mapping at a call site is how the
                // +N lookup missed the bag before.
                //
                // ⭐ This enqueue is why the hold above waits for an EMPTY queue: pausing on the
                // same tick would hold the spear itself.
                crate::auto_equip::enqueue(er_logic::boss_grants::SERPENT_HUNTER_BASE, None);
                self.log(ap::Print::message(m));
            }
            // ⭐⭐⭐ THE EQUIP FOLLOWS THE FIGHT, NOT THE GRANT (#413 cause 2; boblerrr
            // 2026-08-07 16:10:50, caught by the diagnostic above). The grant is one-shot per
            // character, so a reload, a re-fight, or simply swapping weapons afterwards left the
            // player facing Rykard with the spear sitting in the bag. The grant answers "do they
            // own one"; this answers "is it in their hand for the fight".
            let latched = RYKARD_FIGHT_EQUIPPED.load(std::sync::atomic::Ordering::Relaxed);
            let (equip_now, latched_after) =
                er_logic::boss_grants::equip_for_fight(rykard_fight_on, holds, latched);
            RYKARD_FIGHT_EQUIPPED.store(latched_after, std::sync::atomic::Ordering::Relaxed);
            // QUEUE THE ID THE BAG ACTUALLY HAS, never `BASE`. Routing this through the plain
            // `enqueue` raised it to the auto_upgrade target -- `17030000 -> 17030003` in
            // bobler's log -- and the drain's exact-FullID lookup then missed forever, because
            // the spear had been granted at `+0` back when the target was `+0` and no grant was
            // coming to raise it. `equip_now` is only ever true when `holds` was `Some(true)`,
            // which is `held_row == Some(Some(_))`, so the `if let` cannot realistically fail;
            // it is written as a match rather than an unwrap so that a future caller loosening
            // `equip_for_fight` degrades to "do nothing this tick" instead of panicking.
            if equip_now && let Some(Some(row)) = held_row {
                // SNAPSHOT BOTH HANDS BEFORE WE DISPLACE ONE (#413 swap-back). The drain picks the
                // slot, not us -- `slot_for_weapon` decides right/left from `wep_type` inside
                // `auto_equip::tick()` -- so at enqueue time we do not yet know which hand the
                // spear will take. Recording both and asking which one holds the spear when the
                // bar drops needs no change to the drain at all, which is the whole reason it is
                // done this way round.
                if let Some([left, right]) = crate::auto_equip::worn_weapon_param_ids() {
                    PREV_WEAPON_LEFT.store(left, std::sync::atomic::Ordering::Relaxed);
                    PREV_WEAPON_RIGHT.store(right, std::sync::atomic::Ordering::Relaxed);
                    PREV_WEAPONS_SET.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                crate::auto_equip::enqueue_held(row);
                log::info!(
                    "boss-grant: Rykard's healthbar is up and the spear was already in the bag -- putting it in your hand (queued {row:#010x})"
                );
            }
            // GIVE IT BACK WHEN THE FIGHT IS OVER. The other half of Alaric's 2026-08-07 ask
            // ("resume auto_equip queue") needs no code: #98's hold pushes weapons onto
            // `still_pending` instead of dropping them, and `should_pause_weapon_equips` goes
            // false the moment the bar is down, so the queue resumes itself.
            let worn = crate::auto_equip::worn_weapon_param_ids();
            // Which hand has the spear, and therefore which snapshot is the one to undo. `None`
            // is an UNREADABLE loadout and stays `None` all the way into the seam -- it must not
            // collapse to "the player swapped", which would throw the snapshot away.
            let spear_slot = worn.map(|w| {
                w.iter().position(|&id| {
                    er_logic::boss_grants::is_level_of(
                        id,
                        er_logic::boss_grants::SERPENT_HUNTER_BASE,
                    )
                })
            });
            let snapshot = PREV_WEAPONS_SET
                .load(std::sync::atomic::Ordering::Relaxed)
                .then(|| match spear_slot {
                    Some(Some(0)) => PREV_WEAPON_LEFT.load(std::sync::atomic::Ordering::Relaxed),
                    _ => PREV_WEAPON_RIGHT.load(std::sync::atomic::Ordering::Relaxed),
                })
                // A slot the game reports as empty is not something to hand back.
                .filter(|&id| id > 0);
            let (restore, snapshot_after) = er_logic::boss_grants::restore_after_fight(
                rykard_fight_on,
                spear_slot.map(|p| p.is_some()),
                crate::auto_equip::queue_has_weapon(),
                snapshot,
            );
            if snapshot_after.is_none() {
                PREV_WEAPONS_SET.store(false, std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(previous) = restore {
                // 🛑🛑 RESOLVE IT THROUGH THE BAG BEFORE QUEUEING IT, OR THIS REPEATS #413 ONE
                // LAYER OUT. `enqueue_held` queues an EXACT row and `auto_equip::tick()` matches
                // by exact FullID; a row the bag does not hold never resolves, goes back on
                // `still_pending`, and retries in silence forever -- which also leaves the queue
                // permanently non-empty, and `should_pause_weapon_equips` only ARMS on an empty
                // queue, so #98's mid-fight hold would never fire again either.
                //
                // Two ways the snapshot can name such a row. `decode_weapon_id` closes the first:
                // it takes rows in `[1_000_000, 90_000_000)` only, so the UNARMED row (110000) --
                // what an empty hand reads as -- is rejected rather than "restored" as a weapon
                // nobody owns. `held_weapon_row` closes the second: the player may have dropped,
                // sold or upgraded that weapon during the fight, so we re-resolve the BASE row and
                // queue whatever level the bag actually has now.
                match er_logic::upgrades::decode_weapon_id(previous)
                    .and_then(|(base, _)| crate::upgrades::held_weapon_row(base).flatten())
                {
                    Some(row) => {
                        crate::auto_equip::enqueue_held(row);
                        log::info!(
                            "boss-grant: Rykard's fight is over -- putting your own weapon back (queued {row:#010x})"
                        );
                    }
                    None => {
                        log::info!(
                            "boss-grant: Rykard's fight is over, but the weapon you were holding \
                             ({previous:#010x}) is not a row the bag still has -- leaving your \
                             loadout alone"
                        );
                    }
                }
            }
        }
        // 🛑🛑 THE HOLD IS EVALUATED **LAST**, BELOW EVERY ENQUEUE ABOVE, AND THAT ORDER IS THE
        // WHOLE POINT. `should_pause_weapon_equips` waits for an EMPTY queue; read before the
        // enqueues it sees the PRE-enqueue empty, closes the gate on the same tick, and the spear
        // lands in its own drain -- released only when the fight ends, i.e. after the fight it was
        // for. #98 dodged that for the GRANT path by keying the hold on the healthbar (the grant
        // fires at area load, long before the bar). The fight-equip above has no such gap: it fires
        // ON the bar, the same tick the hold would arm. So the read moved down instead of the
        // hazard being argued away.
        //
        // Read UNCONDITIONALLY, outside `can_grant`: if a lost `can_grant` skipped it the hold
        // would STRAND at true and block every weapon equip for the rest of the session. `None`
        // (GameDataMan down) resolves to "not paused", so losing the ability to look releases the
        // hold rather than latching it.
        crate::auto_equip::set_weapons_paused(er_logic::boss_grants::should_pause_weapon_equips(
            rykard_fight_on,
            crate::auto_equip::queue_is_empty(),
            crate::auto_equip::weapons_paused(),
        ));
        for g in graces_lit {
            self.log(ap::Print::message(format!("{g} unlocked")));
        }
        for m in region_msgs {
            self.log(ap::Print::message(m));
        }

        // 7. DeathLink.
        let my_name = self.my_name.clone();
        for ev in self.take_events() {
            if let ap::Event::DeathLink { source, .. } = ev {
                let foreign = my_name.as_deref().map(|n| n != source).unwrap_or(true);
                if foreign {
                    // R2 (SWEEP H2): honor the slot's death_link option on the INCOMING side too
                    // (the tag is advertised unconditionally; only the outgoing send was gated).
                    if crate::deathlink::is_enabled() {
                        log::info!("DeathLink received from '{source}'");
                        crate::deathlink::latch_incoming_kill();
                    } else {
                        log::info!(
                            "DeathLink received from '{source}' but disabled for this slot -- ignored"
                        );
                    }
                }
            }
        }
        crate::deathlink::drive_kill();
        if crate::deathlink::is_enabled()
            && crate::deathlink::poll_local_death()
            && let Some(client) = self.client_mut()
        {
            log::info!("DeathLink: local death detected -> broadcasting");
            if let Err(e) = client.death_link(ap::DeathLinkOptions::default()) {
                log::warn!("DeathLink: broadcast failed: {e}");
            }
        }

        // 8. Scadutree blessing writer.
        // The blessing is applied SILENTLY -- a repurposed SpEffect row on the player -- so until
        // now the only evidence it worked was a log line. Toast the level when it CHANGES.
        if let Some(blessing) = crate::upgrades::tick_global_scadu() {
            let now = self.toast_clock.elapsed().as_millis() as u64;
            self.toasts.push(blessing, now);
        }

        // 8b. no_weapon_requirements runtime param zeroing (latched once applied).
        crate::no_weapon_reqs::tick();

        // 8b1. serpent_hunter: the wave SpEffect into the spear's resident slot (latched once
        // applied, re-armed on the in_world edge below -- a map load restores the vanilla row).
        crate::serpent_hunter::tick();
        // 8b1b. ...and the same SpEffect on the PLAYER, every tick. The resident slot binds at
        // EQUIP time (measured 2026-08-07 19:56:29), so the row write above is inert while the
        // spear is already in hand -- which, across a map load, is the normal case.
        crate::serpent_hunter::ensure_applied();

        // 8b2. no_equip_load: weightless-equipment SpEffect on the player (param edit + apply).
        crate::no_equip_load::tick();

        // 8b2b. no_fall_damage: fallDamageRate-0 SpEffect on the player (spirit-spring trick).
        crate::no_fall_damage::tick();

        // 8b2c. flask: reconcile the leveled flask (charges + potency) UP to the rung implied by the
        // count of received "Progressive Flask Upgrade" items. History-agnostic, upward-only,
        // idempotent; no-op unless the slot_data `flaskLadder` armed it.
        // The flask reconciler changes max_hp_flask/max_fp_flask -- no item enters the bag, so the
        // game shows nothing. Announce the EFFECT, not the receipt: an early rung can ask for fewer
        // charges than the player already has, and toasting "Flask upgraded" while the allocation
        // sat still would be a nicer-looking version of the same lie the silent no-op told
        // (2026-07-24: "I'm getting the sacred tear but no extra charge"). Only a real raise talks.
        // `flask_seen` still primes on connect so a reconnect cannot replay old grants.
        let applied = crate::flask::tick(flask_upgrade_count);
        if let Some((before, after)) = applied
            && self.flask_seen.is_some()
        {
            let now = self.toast_clock.elapsed().as_millis() as u64;
            self.toasts
                .push(format!("Flask charges {before} -> {after}"), now);
        }
        self.flask_seen = Some(flask_upgrade_count);

        // 8b2c. merchant bells (#325): the ESD detour set the flag inside the game's own talk
        // frame, where there is no `&mut Client` and therefore no toast deck. It left the notice
        // behind; this is the tick that owns the deck. Drained in a loop because a single frame can
        // in principle carry more than one open.
        while let Some(notice) = crate::merchant_bells::take_notice() {
            let now = self.toast_clock.elapsed().as_millis() as u64;
            self.toasts.push(notice, now);
        }

        // 8b3. auto_equip: drain queued received weapons into a primary hand (once each is in the bag).
        crate::auto_equip::tick();

        // 8b4. physick_probe (#334 phase 2): READ-ONLY RE diagnostic, hard no-op unless
        // ER_PHYSICK_PROBE is set. Placed after auto_equip because it reads the same inventory the
        // auto-equip pass has just finished with, and self-throttles to one scan per 500ms.
        crate::physick_probe::tick();

        // 8b5. downstate_probe (#346 phase 1b): the one live measurement the down-states are gated
        // on. Hard no-op unless ER_DOWNSTATE_PROBE is set, and read-only unless ER_DOWNSTATE_PROBE_ARM
        // is set too. Latches after a single subject.
        crate::downstate_probe::tick();

        // 8c. Ticker-only pickup notifs: set showDialogCondType=0 game-wide so AP grants show the
        //     native right-side ticker, not the blocking "NEW Y:OK" modal (was a retired-baker
        //     regulation edit; ported to runtime, latched once applied).
        crate::notif_ticker::tick();

        // 9. Shop system (SHOP-SYSTEM-HANDOFF.md tick order). Pump the scout first (needs client_mut;
        //    take() to dodge the self double-borrow), then run each shop edit in order. Each self-gates
        //    on cache_ready / param-repo and latches DONE after one in-world pass.
        let mut scout = self.scout.take();
        if let Some(sp) = scout.as_mut()
            && let Some(client) = self.client_mut()
        {
            sp.pump(client);
        }
        self.scout = scout;
        // Region-lock hint ledger. Same take()/put-back dance as the scout above, for the same
        // reason: pump needs client_mut and would otherwise double-borrow self.
        let mut lh = std::mem::take(&mut self.lock_hints);
        if let Some(client) = self.client_mut() {
            lh.pump(client);
        }
        self.lock_hints = lh;
        // Shop auto-hints: the ESD detour plans on the game thread and queues; the send happens
        // HERE, where a live client is free. One create_hints packet per shop open.
        if let Some(client) = self.client_mut() {
            crate::shop_hints::pump(client);
        }
        // Re-arm the ItemLotParam blank passes on a map-(re)load edge. check_lots / enemy_drops latch
        // DONE after their first successful in-world pass and are otherwise reset ONLY on reconnect
        // (configure()). But a map load streams params back in -- notably the DLC (Land of Shadow)
        // ItemLotParam rows -- reverting our rewrites, and the latched passes never re-apply them. That
        // is the DLC "vanilla ware leaks well into the session" bug (Alaric, 2026-07-21): the connect-
        // time blank ran (`0 missing`) yet a DLC treasure opened later handed out the real ware. Detect
        // the in_world false->true edge (a load completed) and reset the latches so the next tick
        // re-applies the blanks against the freshly-loaded params. Idempotent: the passes self-gate on
        // the param repo being up and re-latch after one clean pass, so this costs one re-blank per load.
        let now_in_world = crate::flags::in_world();
        if now_in_world && !self.was_in_world {
            crate::check_lots::reset();
            // Same stream-in revert, same re-arm: the whetblade getItemFlagId repoints are
            // ItemLotParam writes too, and a reverted one flips a whetblade check back onto the
            // affinity flag keyitems sets (the false-collect this split exists to kill).
            crate::whetblade_lots::reset();
            crate::enemy_drops::reset();
            // THE CTD (2026-07-24, symbolized): the inventory pointer grant_full_id hands to the
            // game's AddItemFunc is captured once and was trusted forever. A load frees that
            // object, so the next grant made the GAME dereference freed memory
            // (eldenring.exe+0x560714, AV read at 0x1ffa585e148, reached via
            // grant_full_id <- Reconciler::tick_with_classes <- classify_received). Retire it here
            // and let the next tick re-prime. See er_logic::inv_ptr (replay-tested).
            let world_epoch = crate::detour::on_world_edge();
            // RUNE COUNT AT THE EDGE (world issue #259). A rollback, a keep-runes restore, a
            // save-load clobber and a legitimate boss payout are identical in a single sample --
            // they are only distinguishable as a PAIR of readings either side of an edge. One line
            // here, one at connect, and an Alt-F4/reconnect report stops being unfalsifiable.
            // Deliberately not per tick: rune count moves constantly in normal play.
            crate::runes::log_sample(er_logic::rune_log::Sample::WorldEdge { epoch: world_epoch });
            // shop_sell was MISSING from this edge (2026-07-24): the same stream-in reverts
            // ShopLineupParam, so every rewritten check row went back to selling its VANILLA
            // ware after the session's first load -- while ECHO_SKIP survived and kept eating
            // the AP echoes of post-load purchases ("Note: Waypoint Ruins" Kalé repro; ~548
            // armed shop checks exposed). Re-arm so the next tick re-rewrites; echo_skip()'s
            // live-row guard (er_logic::shop_echo) covers the one-tick window and any revert
            // path this edge misses.
            crate::shop_sell::reset();
            // Same revert, same re-arm: the load just streamed ShopLineupParam back in, so the
            // repointed rows are selling their vanilla wares again until the next pass rewrites
            // them. Without this the shop display is correct until the player's first load and
            // wrong for the rest of the run (er_logic::shop_repoint_replay pins the timeline).
            crate::shop_repoint::reset();
            crate::shop_hints::reset();
            crate::shop_prices::reset();
            // shop_stock was MISSING from this list until 2026-07-29 -- the THIRD writer to make the
            // same mistake (shop_sell 07-24, shop_icon earlier today). Its reset() existed and was
            // never called, so the 455 rerolled infinite-stock rows applied once on connect, the
            // first map load streamed ShopLineupParam back in and reverted them, and the DONE latch
            // meant they never re-applied. Every below-value rune price in a seed lives in that
            // table -- Alaric reported three times over that he had "never seen a single rune priced
            // below its value ... nothing remotely close to the 0 end", and he was exactly right:
            // his seed had a Golden Rune [5] for 4 runes and a [1] for 2, and neither existed in his
            // game past the first load. Must follow shop_repoint like the rest.
            crate::shop_stock::reset();
            // shop_icon was MISSING from this list until 2026-07-29 -- the only one of the six
            // param writers with no reset() at all. It writes EquipParamGoods.iconId, so a load
            // reverted it and the DONE latch meant it never re-applied: every repointed shop slot
            // fell back to the literal telescope after the first load. Reported as "telescope icon"
            // on Nexus. Must run AFTER shop_repoint (same ordering as the rest): repoint decides
            // WHICH rows are ours, icon dresses them.
            crate::shop_icon::reset();
            // shop_preview was MISSING until 2026-08-03 -- the FOURTH writer, and the only
            // FMG writer with no reset() at all, so the gate could not even see it (
            // test_gf_client_resets_are_called enumerates modules that DEFINE reset()). Its name/
            // info/caption overrides applied once on connect; the first load reverted the category
            // pointer and check_lots' own correct re-dress then republished a block with only
            // the placeholder, discarding these. Measured in the 2026-08-02 log: 153 placeholder
            // swaps across 51 edges vs 3 preview swaps across one. Must run AFTER shop_repoint,
            // same as shop_icon: repoint decides WHICH rows are ours, preview names them.
            crate::shop_preview::reset();
            // --- 2026-08-04: the rest of the latched game-state writers, ruled on together. ------
            // Alaric: "resets for everything should be handled in the same way". These seven sat in
            // the world gate's _UNRULED_WRITERS debt ledger -- each writes game state, latches, and
            // had no re-arm here, which is the shop_sell / shop_icon / shop_stock / shop_preview
            // shape with nobody having checked yet. Every one is read-then-write-if-different or
            // lower-only, so a pass over rows a load did NOT revert writes nothing; the ones that
            // log a count therefore also MEASURE whether their param is re-streamed.
            //
            // ShopLineupParam again -- the same table that cost three of the four. A load restores
            // the vanilla eventFlag_forStock on the 45 rewrite rows (the flag poll then watches a
            // flag the purchase never sets, so the check stops firing), the vanilla sellQuantity on
            // every check row (a one-time check becomes re-buyable: the duplication exploit the
            // 2026-07-14 clamp closes), and the vanilla eventFlag_forRelease on the capital re-key
            // rows. Both of this module's latches are cleared; its slot_data config is not.
            crate::shop_flags::reset();
            // EquipMtrlSetParam: APPLIED holds the cap it applied and its only other clearer is
            // set_flatten() at slot_data parse -- a CONNECT-scoped re-arm, which is the shop_stock
            // bug with the latch one scope out. A reverted ladder leaves APPLIED == cap, so
            // maybe_apply() short-circuits forever and the flattened upgrade curve is gone.
            crate::upgrade_cost::reset();
            // showDialogCondType=0 across the five grantable equip param types. If a load reverts
            // it, every AP grant from that point on shows the BLOCKING "NEW Y:OK" modal instead of
            // the ticker -- loud, player-visible, and easy to mis-attribute to the receive path.
            crate::notif_ticker::reset();
            // EquipParamWeapon.proper_* / Magic.requirement_*. Opt-in option, so a post-load revert
            // is quiet and reads to the player as the option simply not working.
            crate::no_weapon_reqs::reset();
            // EquipParamWeapon 17030000's resident SpEffect slot. Same revert-on-load shape as
            // the line above, and the same reason it must re-arm: without this the spear's waves
            // work until the player's first load and then stop, which reads as flaky rather than
            // absent and is strictly harder to diagnose.
            crate::serpent_hunter::reset();
            // The SpEffectParam pair, ruled on together as their docs ask: one row write each
            // (20012080 allItemWeightChangeRate, 20010827 fallDamageRate). The player keeps
            // carrying the row across a load, so a reverted row is a buff that is present and does
            // nothing -- the least audible failure of the eight.
            crate::no_equip_load::reset();
            crate::no_fall_damage::reset();
            // The clone row (20012081) is SpEffectParam too, including the load-bearing
            // effectEndurance = -1. reset() was already called from the slot_data parse; that is a
            // SEED-scoped re-arm and it is why this gate scopes its scan to this block. Its
            // LAST_TARGET/LAST_ACTIVE memo is a DONE latch by another name: after a load reverts the
            // row the memo still equals the target, drive() sees !dirty and the blessing is gone.
            crate::scadu_blessing::reset();
            // NOT here on purpose: fmg_inject. It is the one writer a load cannot break -- its
            // MODE_INJECT path has nothing to inject (nothing in this crate creates a synthetic
            // goods row; vanilla EquipParamGoods tops out at 2,220,010), so its swap is an IDENTITY
            // rebuild and a revert costs nothing. Re-arming it would leak one VirtualAlloc'd
            // GoodsName block per load for no behavioural change. Every FMG write the player can
            // SEE goes through check_lots::dress_placeholder / shop_preview, both re-armed above.
            // See the module doc and the fmg_inject row in the world repo's _UNRULED_WRITERS.
            // ------------------------------------------------------------------------------------
            // CTD guard (2026-07-24): a load just completed — re-arm the enemy-scaling settle
            // window too. A SAME-REGION reload (death respawn) never trips the sweep's
            // region-change reset (LAST_REGION is unchanged), yet the ChrIns sets were just torn
            // down and rebuilt; give them REGION_SETTLE to finish streaming before the next walk
            // (scaling::notify_transition; same native-crash class as Siofra 2026-07-09).
            crate::scaling::notify_transition();
        }
        // THE MENU EDGE (in_world true->false): the player quit to the main menu, which is the one
        // moment a latched marker refusal may be released. `reconcile_io::REFUSED` had no writer
        // that ever stored `false` and `reconcile_inited` is set the moment `init` RETURNS -- refuse
        // path included, which returns before building a `Driver` -- so a wrong-save player who
        // followed the toast's own instruction ("start a fresh character") loaded the new character
        // into a gated, silent, permanently inert session. Only a game restart cleared it, and
        // nothing said so (Alaric, 2026-08-10).
        //
        // Clearing `reconcile_inited` is the other half and must not be skipped: without it `init`
        // never runs again, so the released latch would just leave an unarmed session behind.
        //
        // This edge, not the false->true one, deliberately. `init` runs in-world, EARLIER in this
        // same tick, so releasing on the load edge would clear the refusal the tick it was latched
        // and loop. Releasing on the way OUT means the next load re-asks the guard cleanly.
        // `clear_refusal_if_rearmable` decides via `er_logic::marker::release_verdict`, which holds
        // a mid-session room change forever (its `Driver` is armed for the old room and `DRIVER` is
        // a `OnceLock`) -- see the 229-check incident in `disarm_if_identity_moved`.
        if !now_in_world && self.was_in_world && crate::reconcile_io::clear_refusal_if_rearmable() {
            self.reconcile_inited = false;
        }
        self.was_in_world = now_in_world;
        if crate::flags::in_world() {
            let _ = crate::fmg_inject::run();
            let _ = crate::shop_flags::run(&[]);
            // Capital release re-key: Enia's 9116-released Maliketh armor rows -> burn-done
            // flag, write-guarded (SPEC-capital-reconciler.md). Own latch; retries until the
            // param repo is up.
            let _ = crate::shop_flags::run_capital_release();
            let _ = crate::upgrade_cost::maybe_apply();
            let _ = crate::shop_sell::run();
            let _ = crate::shop_stock::run();
            let _ = crate::enemy_drops::run();
            let _ = crate::check_lots::run();
            // The whetblade check-flag split (repoint getItemFlagId; see whetblade_lots.rs).
            let _ = crate::whetblade_lots::run();
            // Cosmetic, and deliberately AFTER the rewrite: dresses the placeholder (AP flower
            // iconId + "Archipelago Item" + caption) so its pickup toast is not ER's nameless-goods
            // render, `[ERROR]`. Own latch — the MSG repo comes up later than the param repo and
            // must not stall the rewrite.
            let _ = crate::check_lots::dress_placeholder();
            let _ = crate::shop_preview::run();
            let _ = crate::shop_icon::run();
            // AFTER shop_sell has latched (it gates on shop_sell::is_done) and after the two display
            // overrides: writes the preview good onto the check row so those overrides are something
            // the player can actually SEE. Until this existed the row kept selling its vanilla ware
            // and every foreign/lock slot read as that ware (Alaric in-game 2026-07-25).
            let _ = crate::shop_repoint::run();
            // Disjoint from the two above: they own the WARE, this owns the PRICE. A rune reward on a
            // slot that kept its old (much higher) cost is a check nobody collects.
            let _ = crate::shop_prices::run();
            let _ = crate::minibaker::run();
            // Region-entry scaling announcement (er_logic::scaling::region_scaling_toast, wired
            // v0.2.19): the sweep just resolved a tier this session had not yet said out loud --
            // put the NUMBER on screen. v0.2.18 made `maximum_enemy_difficulty: auto` the
            // default, which quietly lowers a short seed's ceiling (5 regions cap at 4.12x, not
            // 7.42x); a line the player can read is the only way that change is perceptible in
            // game at all. Dedup lives in er_logic (once per distinct announcement per session);
            // the deck's refresh-on-identical-text is the backstop.
            if let Some(entry_toast) = crate::scaling::tick() {
                let now = self.toast_clock.elapsed().as_millis() as u64;
                self.toasts.push(entry_toast, now);
            }
            // A REFUSED session is the one state where the mod deliberately does NOTHING, and it is
            // indistinguishable from a broken install unless we say so: checks stop reporting,
            // items stop arriving, and the only trace was a `log::warn!`. boblerrr played ~55
            // minutes like that on 2026-07-30. Re-pushed every tick on purpose -- the condition
            // persists until the player acts, and the deck refreshes identical text rather than
            // stacking, so this is free after the first frame.
            if let Some(refusal) = crate::reconcile_io::refusal_toast() {
                let now = self.toast_clock.elapsed().as_millis() as u64;
                self.toasts.push(refusal, now);
            }
            // I4 (2026-08-01): the OTHER silent no-delivery state. A session configured to let the
            // reconciler grant, whose Driver never armed (the inventory pointer never captured --
            // a foreign AddItemFunc hook does this), used to look identical to a healthy one: checks
            // still reported, nothing ever arrived, no warn, no toast. I3 hands granting back to the
            // old path here; this says so out loud. Same re-push-every-tick rationale as above.
            if crate::flags::in_world() {
                let now = self.toast_clock.elapsed().as_millis() as u64;
                crate::reconcile_io::note_in_world(now);
                if let Some(notice) = crate::reconcile_io::suspension_toast(now) {
                    self.toasts.push(notice, now);
                }
            }
            // Anti-stuck: keep the FieldArea fast-travel gate open so a dungeon/catacomb can never
            // strand the player (SELF-CALIBRATING field overwrite; see fast_travel.rs). Game-thread.
            crate::fast_travel::tick();

            // Config hot-reload: a tester changes server/slot by editing apconfig.json and alt-tabbing
            // back, instead of fighting the game for input in the overlay (ER has no InputBlocker, so
            // clicking closes the ER menu and Escape closes the client's window). The decision is
            // er_logic::config_reload::reload_action -- host-tested: one reconnect per REAL change, no
            // storm from our own save, and a half-written file never drops a live session.
            if let Some(next) = crate::config_watch::poll()
                && let Err(e) = self.base_mut().update_connection_info(
                    &next.url,
                    &next.slot,
                    next.password.clone(),
                )
            {
                log::warn!("config hot-reload: reconnect failed: {e}");
            }
            // Region-lock fog-wall visuals (cosmetic marker at locked borders; the KICK reactor,
            // not this, does the blocking). Runs on the game thread (FrameBegin task) so the
            // CSWorldGeomMan::spawn_geometry call is main-thread-safe.
            if let Some(fw) = self.fogwall.as_mut() {
                crate::fogwall::tick(fw);
            }
        }

        // START-ITEM DELIVERY, reconciled against the BAG (#267). After the world has SETTLED (so
        // the reconciler's ledger has had its pass), grant any startItems still absent and keep
        // going until a fresh scan finds nothing missing. Possession is the dedup: it is
        // per-character for free, cannot be inherited by a new character on a used slot, and cannot
        // go stale the way the deleted `start_items_granted` boolean did.
        let start_backfill_settled = crate::detour::real_pickup_seen()
            || self
                .in_world_since
                .is_some_and(|t| t.elapsed() >= std::time::Duration::from_secs(10));
        // I3 FIX (a): a refused save is QUARANTINED -- the backstop must not write to it either.
        crate::start_item_backfill::tick(
            start_backfill_settled && !crate::reconcile_io::is_refused(),
        );

        Ok(())
    }
}

impl Core {
    // ---- RECONCILER DRY-RUN mapper (additive; only called under RECONCILE_DRYRUN) --------------
    //
    // build_desired_inputs folds the parsed slot_data tables + the server-delivered received-item
    // stream into the pure reconciler's `DesiredInputs`. It reuses the SAME tables the live grant
    // path uses (item_map, region_open_flags/lock_reveal_flags, the progressive config, the
    // keyitems obtained/restored table) so the reconciler's plan can be validated against today's
    // behavior in the dry run.
    //
    // SCOPE / ASSUMPTIONS (documented in docs/history/MIGRATION.md, archived):
    //   * Maps the RECEIVED-ITEM STREAM *and* (Gap 1) the slot-data BULK grants: start graces, the
    //     unconditional + reveal_all_maps world-map flags, start items (ledgered once), and the goal.
    //     They are folded from the SAME tables the old startgrants/goal handlers use.
    //   * `seal_flags` is left EMPTY on purpose: the authoritative seal set (area_lock / attunement
    //     flags) is not yet reproduced here, and seeding it wrongly would make the diff propose bogus
    //     ClearFlag actions. Received region LOCKS still SET their open flag.
    //   * consumable `qty` defaults to the item_counts entry or 1; `echo_skip` (Gap 2) dedups a
    //     native-sold shop echo.
    //   * NOTE(windows-verify): `goal_flag` is a SENTINEL (see `reconcile_io::GOAL_SENTINEL_FLAG`).
    //     In dry-run this only LOGS a would-apply SetFlag. Before the ledger/goods APPLY cutover the
    //     client must either route that sentinel action to `ClientStatus::Goal` (a client seam) OR
    //     keep goal-send on the existing `core.rs` handler and pass `goal_flag: None` here. The pure
    //     `SlotData.goal_flag/goal_met` fields are tested in er-logic so either wiring is glue-only.
    fn build_desired_inputs(
        &self,
        received: &[(i64, String, i64, bool)],
    ) -> er_logic::reconcile::DesiredInputs {
        use er_logic::reconcile::{DesiredInputs, ReceivedItem, SaveIdentity, SlotData, StartItem};
        let seed = self.parsed_seed.clone().unwrap_or_default();
        let save = SaveIdentity(self.my_name.clone().unwrap_or_default());
        let items: Vec<ReceivedItem> = received
            .iter()
            .map(|(index, name, ap_id, echo_skip)| ReceivedItem {
                index: *index,
                name: name.clone(),
                semantics: self.classify_received(name, *ap_id, *echo_skip),
            })
            .collect();
        // Gap 1: fold slot-data bulk grants from the SAME tables the live handlers use.
        let sc = self.start.as_ref();
        let slot_data = SlotData {
            seal_flags: Vec::new(),
            start_graces: sc.map(|s| s.start_graces.clone()).unwrap_or_default(),
            always_map_flags: sc
                .map(crate::startgrants::always_map_flags_for)
                .unwrap_or_else(|| vec![crate::startgrants::UNDERGROUND_MAP_VIEW_UNLOCK]),
            reveal_all_maps: sc.map(|s| s.reveal_all_maps).unwrap_or(false),
            map_reveal_flags: sc
                .map(crate::startgrants::reveal_flags_for)
                .unwrap_or_default(),
            start_items: sc
                .map(|s| {
                    s.start_items
                        .iter()
                        .map(|&full_id| StartItem { full_id, qty: 1 })
                        .collect()
                })
                .unwrap_or_default(),
            // Option (b) from the runbook: goal-send stays on the core.rs §5c `ClientStatus::Goal`
            // handler (a network send, NOT an ER flag), so the reconciler never plans the synthetic
            // GOAL_SENTINEL_FLAG SetFlag. `goal_met` is still surfaced for parity/logging.
            goal_flag: None,
            goal_met: self.reconcile_goal_met(),
        };
        DesiredInputs {
            seed,
            save,
            received: items,
            slot_data,
        }
    }

    /// Mirror of the `core.rs` 5c goal-send predicate for the reconciler glue: every goal location is
    /// done (flag goals via live event flags; checked goals via the server-truth checked set,
    /// pre-filtered against `valid_locations` so no datapackage-unknown id reaches the checked query).
    fn reconcile_goal_met(&self) -> bool {
        match (self.goal.as_ref(), self.client()) {
            (Some(cfg), Some(client)) => {
                // goalItems: HELD, not killed. The reconciler has no `received_all` in scope (it is a
                // &self path), so derive the held-name set straight from the received stream -- the
                // same source `received_all` is built from, so the two agree by construction.
                let held: HashSet<String> = client
                    .received_items()
                    .iter()
                    .map(|ri| ri.item().name().to_string())
                    .collect();
                crate::goal::is_met(
                    cfg,
                    crate::flags::get_event_flag,
                    |l| self.valid_locations.contains(&l) && client.is_local_location_checked(l),
                    |n| held.contains(n),
                )
            }
            _ => false,
        }
    }

    /// Classify one received AP item into its reconciler [`ItemSemantics`], reusing the live tables.
    /// Order matters: progressive -> region lock -> key item / great rune -> plain grant.
    fn classify_received(
        &self,
        name: &str,
        ap_id: i64,
        echo_skip: bool,
    ) -> er_logic::reconcile::ItemSemantics {
        use er_logic::reconcile::{ItemSemantics, ProgTier};
        // 1. Progressive item (tier goods packed to grant FullIDs, exactly like the live path).
        if let Some(tiers) = self.progressive.tiers_for(name) {
            let tiers = tiers
                .iter()
                .map(|t| ProgTier {
                    goods: t
                        .goods
                        .iter()
                        .map(|&g| (g as i32) | er_logic::progressive::GOODS_FULLID)
                        .collect(),
                    flags: t.flags.clone(),
                    consumed: t.consumed,
                })
                .collect();
            return ItemSemantics::Progressive {
                tiers,
                overflow_full_id: (er_logic::progressive::LORDS_RUNE_GOODS as i32)
                    | er_logic::progressive::GOODS_FULLID,
            };
        }
        // 2. Region-open lock (intentionally absent from item_map; classified by NAME). Fold in the
        //    lock's revealed grace bundle so those graces self-heal too.
        if let Some(cfg) = self.region.as_ref()
            && let Some(&open) = cfg.region_open_flags.get(name)
        {
            let mut flags = vec![open];
            if let Some(bundle) = cfg.lock_reveal_flags.get(name) {
                flags.extend(bundle.iter().copied());
            }
            return ItemSemantics::RegionFlags(flags);
        }
        // 3. Key item / great rune: the base grant gives the (restored) goods, plus vanilla
        //    obtained/restored companion flags from the keyitems table. Both classes are a unique
        //    good + set-only companion flags, so both map to KeyItem.
        let full_id = self.item_map.as_ref().and_then(|m| m.get(&ap_id)).copied();
        let acq = crate::keyitems::acquire_flags(name);
        if !acq.is_empty()
            && let Some(fid) = full_id
        {
            return ItemSemantics::KeyItem {
                goods: fid as i32,
                obtained_flags: acq,
            };
        }
        // 4. Plain grant: mapped -> ledgered consumable; unmapped -> inert (region locks / boss keys
        //    fell out at step 2 / are name-gated, so an unmapped id here is genuinely effect-less).
        match full_id {
            Some(fid) => {
                let qty = self.item_counts.get(&ap_id).copied().unwrap_or(1) as i32;
                // Gap 2: a native-sold shop echo is ledgered but NOT re-granted (watermark advances).
                ItemSemantics::Consumable {
                    full_id: fid as i32,
                    qty,
                    echo_skip,
                }
            }
            None => ItemSemantics::Inert,
        }
    }

    /// Rebuild all per-seed / per-save state when a reconnect targets a DIFFERENT seed without an
    /// ER reload (see the `parsed_seed` guard). Clears every table that slot_data or the save file
    /// repopulates, so the one-shot parse and save-load run fresh; seed-scoped tables (region_table,
    /// coarse_table, coarse_lock_items, progression_surface) are CLEARED, while tracker UI prefs
    /// and install-once globals (detour_installed) are left intact.
    /// Recovered after commit 4bb3c95 accidentally dropped the body while leaving the call sites.
    fn reset_for_new_seed(&mut self) {
        // Owed sweep flags belong to the OLD seed's location ids; carrying them across would write
        // flags the new seed never earned.
        self.sweep_flag_pending.clear();
        self.flask_seen = None;
        self.region_toast_primed = false;
        self.received_through = 0;
        self.dispatched_through = 0;
        self.item_map = None;
        self.item_counts.clear();
        self.region = None;
        self.fogwall = None;
        self.progressive = ProgressiveState::new(HashMap::new());
        self.slot_data_parsed = false;
        self.save_path = None;
        self.save_loaded = false;
        self.last_persisted_index = -1;
        self.valid_locations.clear();
        self.locations_loaded = false;
        self.flag_poll = None;
        self.dungeon_sweeps.clear();
        self.sweep_lock_gates.clear();
        self.poll_counter = 0;
        self.flag_poll_baseline.clear();
        self.flag_poll_baseline_done = false;
        self.start = None;
        self.start_flags_done = false;
        self.unique_grants_ok.clear();
        self.unique_grants_done = false;
        self.in_world_since = None;
        self.grant_gate_last_play_region = None;
        self.scout = None;
        self.goal = None;
        self.sent_goal = false;
        self.hints = HintSet::new();
        self.hint_log_watermark = 0;
        // Session counter + any undrained notice. The hand-ins themselves are EVENT FLAGS in the
        // player's save and are deliberately NOT undone -- a bell handed in stays handed in, the
        // same as one the player carried to the Maidens.
        crate::merchant_bells::reset();
        // CLEAR the tracker's seed-scoped model so a new seed cannot inherit the prior seed's
        // regions or surface. The slot_data parse re-applies this seed's own (or leaves them empty
        // and shows nothing -- see the deleted-fallback note there). Under num_regions two seeds
        // routinely disagree about which regions EXIST, so inheriting is not a cosmetic bug.
        self.progression_surface = HashSet::new();
        self.region_table = HashMap::new();
        self.coarse_table = HashMap::new();
        self.coarse_lock_items = HashMap::new();
        // The hint ledger is keyed per SLOT, not per seed, so a new seed must not inherit the old
        // seed's purchases -- and its location ids would be meaningless here anyway. Re-read from
        // the server on the next pump.
        self.lock_hints.reset();
        // ...and so must everything derived from it: a new seed's balance is a different number
        // against a different set of locks, and the intro notice is news again.
        self.lock_hint_hud = None;
        self.lock_hint_hud_at = 0;
        self.lock_hint_affordable_prev = None;
        self.lock_hint_intro_done = false;
        // Boss-lock mode A: drop the parsed defs AND re-arm the felled-edge state, so the new
        // seed re-parses bossLockItems and re-primes its baseline on the next in-world poll.
        self.boss_defs.clear();
        self.boss_flag_prev.clear();
        // SWEEP VISIBILITY: re-arm the per-group sweep banner for the new seed.
        self.sweep_bannered.clear();
        self.sweep_watch.reset();
        // ATTUNEMENT-RELEASE: drop the parsed gate + all per-save latches so the new seed re-parses
        // regionAttunement and re-primes / re-blooms from scratch.
        self.region_attunement.clear();
        self.boss_payout_pending.clear();
        self.attuned_regions.clear();
        self.attunement_primed = false;
        // BOSS KEYS (mode B): drop the deferred own-check latch + its prime flag so the new seed
        // re-parses gates and re-seeds silently on its next in-world poll.
        self.boss_key_pending.clear();
        self.boss_key_primed = false;
    }

    /// Scan NEW overlay-log entries for `Print::Hint`s and fold them into [Self::hints].
    ///
    /// Hint semantics (SPEC-item-tracker.md option (a)): the hint's `sender` is the player whose
    /// world CONTAINS the hinted location; `receiver` is the player who gets the item. `for_us` =
    /// we are the sender, i.e. the location is in OUR world -- that's what the checks tree marks.
    /// Only our-world hints are inserted (`for_us`, or the id resolving in our region table as a
    /// fallback) so cross-world location-id collisions don't mismark the tree.
    fn accumulate_hints_from_log(&mut self) {
        // Our slot name comes from the client; before we're connected, leave the watermark alone
        // so any early entries still get scanned once names are resolvable.
        let Some(our_name) = self.client().map(|c| c.this_player().name()) else {
            return;
        };
        let log_len = self.base().logs().len();
        let start = self.hint_log_watermark.min(log_len);
        // Two-phase (collect, then insert): the log iterator immutably borrows self, so the
        // HintSet inserts have to wait until the scan ends.
        let mut new_hints: Vec<HintEntry> = Vec::new();
        for (print, _) in self.base().logs().skip(start) {
            let ap::Print::Hint { item, .. } = print else {
                continue;
            };
            let location_id = item.location().id() as u64;
            let for_us = item.sender().name() == our_name;
            if !for_us && !self.region_table.contains_key(&location_id) {
                continue; // another world's location -- not ours to mark
            }
            let other = if for_us {
                item.receiver()
            } else {
                item.sender()
            };
            new_hints.push(HintEntry {
                location_id,
                item_name: item.item().name().to_string(),
                other_player: other.name().to_string(),
                for_us,
            });
        }
        self.hint_log_watermark = log_len;
        for entry in new_hints {
            self.hints.insert(entry);
        }
    }

    /// Recompute the always-visible lock-hint balance, and fire the two notices that make the
    /// feature findable at all (issue #412).
    ///
    /// # Why a notice, and not just a better button
    ///
    /// bobler's 0.3.5 log is the whole argument: `lock hints: ledger loaded from er_lockhints_2 --
    /// 0 hint(s) already bought`, followed by three AP `!hint`s three minutes apart. The economy
    /// loaded, priced itself correctly, and was never touched, because nothing the player could see
    /// ever mentioned it. Two moments are worth interrupting for, and no others: the first time we
    /// know there is a lock to spend on, and the first time the balance is actually enough. Both
    /// latch, so neither can become an advertisement that re-fires every frame.
    ///
    /// Throttled to ~4x/second: this walks the checked-location set, which the always-on path must
    /// not do at frame rate.
    fn refresh_lock_hint_hud(&mut self) {
        const HUD_REFRESH_MS: u64 = 250;
        let now = self.toast_clock.elapsed().as_millis() as u64;
        if self.lock_hint_hud.is_some()
            && now.saturating_sub(self.lock_hint_hud_at) < HUD_REFRESH_MS
        {
            return;
        }
        self.lock_hint_hud_at = now;
        // The ledger gates the HUD for the same reason it gates the button: a balance computed
        // against an unread ledger is free money printed on screen.
        if !self.lock_hints.is_ready() {
            self.lock_hint_hud = None;
            return;
        }
        let surface: HashSet<i64> = self
            .progression_surface
            .iter()
            .map(|&id| id as i64)
            .collect();
        let mut checked: HashSet<i64> = HashSet::new();
        let mut unchecked_n: u64 = 0;
        let mut points_per_hint: u64 = 0;
        let mut connected = false;
        if let Some(client) = self.client() {
            connected = true;
            for loc in client.checked_locations() {
                checked.insert(loc.id());
            }
            for _ in client.unchecked_locations() {
                unchecked_n += 1;
            }
            points_per_hint = client.points_per_hint();
        }
        if !connected {
            self.lock_hint_hud = None;
            return;
        }
        let total_locations = checked.len() as u64 + unchecked_n;
        let surface_checked = er_logic::lock_hint_economy::surface_checked(&checked, &surface);
        let Some((have, price)) = er_logic::lock_hint_economy::status(
            surface.len() as u64,
            surface_checked,
            self.lock_hints.purchases(),
            points_per_hint,
            total_locations,
        ) else {
            self.lock_hint_hud = None;
            return;
        };
        self.lock_hint_hud = Some((have, price));

        // Both notices are gated on there BEING a lock to spend on. A seed with every region open
        // has nothing to advertise, and saying so anyway is noise.
        let open = self.open_coarse_regions();
        let any_locked = self
            .coarse_lock_items
            .keys()
            .any(|r| !r.is_empty() && !open.contains(r));
        if !any_locked {
            return;
        }
        // ASCII only -- toasts go through the FMG path (`every_toast_is_ascii`) and an em-dash
        // draws as `?` in game.
        if !self.lock_hint_intro_done {
            self.lock_hint_intro_done = true;
            self.log(ap::Print::message(format!(
                "Lock hints: {have}/{price} progression-surface checks. Tracker (F6) -> Hint next lock."
            )));
            self.toasts.push(
                format!("Lock hints: {have}/{price} -- F6 for the tracker"),
                now,
            );
        }
        let affordable = have >= price;
        if er_logic::lock_hint_economy::crossed_into_affordable(
            self.lock_hint_affordable_prev,
            affordable,
        ) {
            self.log(ap::Print::message(
                "You can afford a lock hint -- Tracker (F6) -> Hint next lock.".to_string(),
            ));
            self.toasts.push("You can afford a lock hint -- F6", now);
        }
        self.lock_hint_affordable_prev = Some(affordable);
    }

    /// Coarse regions currently accessible: a coarse region is open iff its lock item's physical
    /// open flag is set -- OR it has no lock at all / the lock isn't part of this seed's pool.
    /// ("" coarse names are the always-open bucket; er-logic treats those as in-logic itself.)
    fn open_coarse_regions(&self) -> HashSet<String> {
        let mut open = HashSet::new();
        let region_open = self.region.as_ref().map(|c| &c.region_open_flags);
        for coarse in self.coarse_table.values() {
            if coarse.is_empty() || open.contains(coarse) {
                continue; // always-open bucket / already decided
            }
            let accessible = match self.coarse_lock_items.get(coarse) {
                None => true, // no lock mapping -> open
                Some(lock) => match region_open.and_then(|m| m.get(lock)) {
                    None => true, // lock absent this seed -> unlocked
                    Some(&flag) => crate::flags::get_event_flag(flag),
                },
            };
            if accessible {
                open.insert(coarse.clone());
            }
        }
        open
    }

    /// Bottom-left, borderless, input-transparent: a notice must never eat a click or steal focus
    /// from the game. Nothing is drawn when the deck is empty, so this costs a length check a frame.
    fn render_toasts(&mut self, ui: &imgui::Ui) {
        let now = self.toast_clock.elapsed().as_millis() as u64;
        self.toasts.expire(now);
        if self.toasts.is_empty() {
            return;
        }
        let height = ui.io().display_size[1];
        ui.window("###ap-toasts")
            .position([24.0, height - 160.0], imgui::Condition::Always)
            .no_decoration()
            .always_auto_resize(true)
            .movable(false)
            .bg_alpha(0.55)
            .build(|| {
                for t in self.toasts.visible() {
                    let a = self.toasts.alpha(t, now);
                    ui.text_colored([1.0, 0.85, 0.35, a], t.text.as_str());
                }
            });
    }

    /// Build the per-frame tracker snapshot and draw the window (SPEC-item-tracker.md Phase 1).
    /// Everything the imgui closure touches is a local snapshot -- `self` stays out of it so the
    /// window's close button can just write a local.
    fn render_tracker_window(&mut self, ui: &imgui::Ui) {
        // One client borrow: location id sets (+ id -> display name) and received-item names.
        let mut checked: Vec<u64> = Vec::new();
        let mut unchecked: Vec<u64> = Vec::new();
        let mut loc_names = HashMap::new(); // id -> Ustr (Copy, interned)
        let mut received: HashSet<String> = HashSet::new();
        let mut points_per_hint: u64 = 0;
        if let Some(client) = self.client() {
            for loc in client.checked_locations() {
                let id = loc.id() as u64;
                loc_names.insert(id, loc.name());
                checked.push(id);
            }
            for loc in client.unchecked_locations() {
                let id = loc.id() as u64;
                loc_names.insert(id, loc.name());
                unchecked.push(id);
            }
            for ri in client.received_items() {
                received.insert(ri.item().name().to_string());
            }
            // Lock-hint economy inputs, read in the SAME client borrow. `points_per_hint()` is
            // `total_locations * hint_cost% / 100`, so dividing it back out below recovers the
            // HOST's setting -- the price tracks their `hint_cost` instead of a number we invented.
            points_per_hint = client.points_per_hint();
        }
        let total_locations = (checked.len() + unchecked.len()) as u64;

        // Scaling row (#346 follow-up, bobler 2026-08-07 "how to view my scaling"). Resolved HERE,
        // outside the closure, like every other snapshot in this function. `None` = no
        // ScalingConfig at all (not connected, or the seed has scaling off), and then the row is
        // omitted entirely -- an unscaled SEED and an unscaled SPOT are different facts and the
        // row must not blur them.
        let scaling_here = self
            .scaling_here_bucket
            .and_then(crate::scaling::describe_region);

        // ---- Boss sweeps (Alaric, 2026-08-07: "display how many sweep checks are attached to a
        // boss"). A sweep is the single largest payout in the game -- 49 and 50 checks in one of
        // bobler's sessions -- and it was invisible until it fired. Assembled here, outside the
        // closure, from tables that are all pure memory: `sweep_flag_state` was read on the tick.
        let checked_set: HashSet<u64> = checked.iter().copied().collect();
        let mut sweep_rows: Vec<(String, String)> = Vec::new();
        let mut sweep_header = String::new();
        if let Some(fp) = self.flag_poll.as_ref() {
            // Fully OWNED first, borrowed second. An earlier draft built the tuples out of
            // references into another Vec and needed `**flag` to read a u32 -- unreadable, and
            // unverifiable here since this crate does not compile off Windows. Owning costs one
            // allocation per sweep group, of which a seed has ~17.
            struct Row {
                boss: Option<String>,
                region: Option<String>,
                flag: u32,
                members: usize,
                checked: usize,
                fired: bool,
                gate: Option<String>,
            }
            let mut rows: Vec<Row> = fp
                .sweep_flags
                .iter()
                .map(|(&flag, locs)| Row {
                    // slot_data WINS; the baked table is the fallback. `bossLockItems` is emitted
                    // only when boss LOCKS are on, so most seeds report `0 boss-lock def(s)` and
                    // every row used to degrade to its region name -- eight identical "Scadu Altus"
                    // rows separable only by a raw flag (boblerrr, 2026-08-08).
                    boss: self
                        .boss_defs
                        .iter()
                        .find(|d| d.flag == flag)
                        .map(|d| {
                            d.name
                                .strip_prefix("Felled: ")
                                .unwrap_or(d.name.as_str())
                                .to_string()
                        })
                        .or_else(|| {
                            er_logic::sweep_boss_names::boss_name(flag).map(str::to_string)
                        }),
                    region: locs
                        .first()
                        .and_then(|l| self.region_table.get(&(*l as u64)))
                        .cloned(),
                    flag,
                    members: locs.len(),
                    checked: locs
                        .iter()
                        .filter(|l| checked_set.contains(&(**l as u64)))
                        .count(),
                    fired: self.sweep_flag_state.get(&flag).copied().unwrap_or(false),
                    gate: self.sweep_lock_gates.get(&flag).cloned(),
                })
                .collect();
            // Stable order: region, then flag. A section that reshuffles every frame is unreadable.
            rows.sort_by(|a, b| a.region.cmp(&b.region).then(a.flag.cmp(&b.flag)));
            let views: Vec<er_logic::sweep_view::SweepGroupView<'_>> = rows
                .iter()
                .map(|r| er_logic::sweep_view::SweepGroupView {
                    flag: r.flag,
                    region: r.region.as_deref(),
                    boss: r.boss.as_deref(),
                    members: r.members,
                    checked: r.checked,
                    fired: r.fired,
                    gated_on: r.gate.as_deref(),
                })
                .collect();
            sweep_header = er_logic::sweep_view::section_header(&views);
            sweep_rows = views
                .iter()
                .map(|v| {
                    (
                        er_logic::sweep_view::group_label(v),
                        er_logic::sweep_view::group_state(v),
                    )
                })
                .collect();
        }

        // Region-lock accessibility snapshot (bound to a local BEFORE the model borrows &self
        // fields -- keeps the borrows sequential).
        let open_coarse = self.open_coarse_regions();
        let model = er_logic::tracker::build_tracker_model(
            &checked,
            &unchecked,
            &received,
            &self.region_table,
            &self.coarse_table,
            &self.progression_surface,
            &open_coarse,
            &self.hints,
        );
        let mut hint_list: Vec<HintEntry> = self.hints.iter().cloned().collect();
        hint_list.sort_by(|a, b| a.item_name.cmp(&b.item_name));
        // Bosses group snapshot (mode A/B, SPEC-boss-lock-tracker). Built here -- before the imgui
        // closure -- so the closure stays self-free (mirrors `open_coarse`). flag_set reads the live
        // event flags; received is this frame's cumulative received-name set. RE-AUTHORED (this boss
        // tracker post-dates core.rs.bak_rlwarn; reconcile against reflog if an intact one exists).
        let boss_group = er_logic::boss_felled::build_boss_group(
            &self.boss_defs,
            crate::flags::get_event_flag,
            |n| received.contains(n),
        );

        let display_loc = |id: u64| -> String {
            loc_names
                .get(&id)
                .map(|n| n.as_str().to_string())
                .unwrap_or_else(|| format!("(location {id})"))
        };

        // ---- lock-hint economy snapshot (see er_logic::lock_hint_economy) --------------------
        // Everything the button needs, resolved before the imgui closure so the closure never
        // borrows self. Clicks are collected into `buy_clicks` and committed after `.build()`.
        let surface_checked_n = er_logic::lock_hint_economy::surface_checked(
            &checked.iter().map(|&id| id as i64).collect(),
            &self
                .progression_surface
                .iter()
                .map(|&id| id as i64)
                .collect(),
        );
        let surface_total_n = self.progression_surface.len() as u64;
        let lock_scout = crate::scout_proof::item_names_by_location();
        let mut hinted_locs: HashSet<i64> =
            self.hints.iter().map(|h| h.location_id as i64).collect();
        // Treat a paid-for location as hinted immediately, so the button cannot be clicked twice
        // in the window between the purchase and the server's hint broadcast coming back.
        hinted_locs.extend(self.lock_hints.bought().iter().copied());
        let ledger_ready = self.lock_hints.is_ready();
        let purchases_n = self.lock_hints.purchases();
        let lock_item_of: HashMap<String, String> = self.coarse_lock_items.clone();
        // location id -> coarse region. NOT keyed by region name: `coarse_table` is
        // HashMap<u64, RegionId>. tracker.rs:117 states every location in a tracker region shares
        // one coarse region, so any of the region's location ids resolves it.
        let coarse_of: HashMap<u64, String> = self.coarse_table.clone();
        let mut buy_clicks: Vec<i64> = Vec::new();
        // "Hint next lock" (#412). The FRONTIER is the one lock whose region is still shut but
        // whose ITEM already sits somewhere open -- the answer to "which lock can I go get now",
        // which is the question a player asking "what is my 2nd lock" is really asking. Resolved
        // out here, like everything else the closure reads, so the closure never borrows self.
        let coarse_of_i64: HashMap<i64, String> = coarse_of
            .iter()
            .map(|(&id, region)| (id as i64, region.clone()))
            .collect();
        let next_offer = if ledger_ready {
            er_logic::lock_hint_economy::next_offer(
                &lock_item_of,
                &open_coarse,
                &coarse_of_i64,
                &lock_scout,
                &hinted_locs,
                surface_total_n,
                surface_checked_n,
                purchases_n,
                points_per_hint,
                total_locations,
            )
        } else {
            er_logic::lock_hint_economy::NextLockOffer::Idle
        };
        let hud = er_logic::lock_hint_economy::status(
            surface_total_n,
            surface_checked_n,
            purchases_n,
            points_per_hint,
            total_locations,
        );

        let mut open = true;
        // Filter state as locals (the closure stays self-free); written back to self after.
        let mut in_logic_only = self.tracker_in_logic_only;
        let mut surface_only = self.tracker_surface_only;
        ui.window("Item Tracker###ap-tracker")
            .size([480.0, 520.0], imgui::Condition::FirstUseEver)
            .opened(&mut open)
            .build(|| {
                ui.text(format!("checks: {}/{}", model.done, model.total));
                ui.text(format!(
                    "in-logic: {}/{}   surface: {}/{}",
                    model.in_logic_done,
                    model.in_logic_total,
                    model.surface_done,
                    model.surface_total
                ));
                // ---- lock hints, at the TOP (#412) ------------------------------------------
                // Above the filters, not on a region header: the balance and the one control that
                // spends it must be visible the moment the window opens, without scrolling and
                // without a locked region happening to be on screen.
                if let Some((have, price)) = hud {
                    ui.text(format!("lock hints: {have}/{price} surface checks"));
                    if ui.is_item_hovered() {
                        ui.tooltip_text(
                            "You earn 1 per progression-surface check -- the * rows below.\nSpend them to publish a real Archipelago hint for a region lock.",
                        );
                    }
                    use er_logic::lock_hint_economy::NextLockOffer as Next;
                    match &next_offer {
                        Next::Buyable { price, location, .. } => {
                            ui.same_line();
                            if ui.small_button(format!("Hint next lock ({price})###trk-buy-next")) {
                                buy_clicks.push(*location);
                            }
                            if ui.is_item_hovered() {
                                // 🛑 The region is deliberately NOT named here. Telling the player
                                // which region is next for free hands over half of exactly what
                                // the price is charged for.
                                ui.tooltip_text(
                                    "Hints the next lock you can actually reach, whichever it is.\nWhich region that turns out to be is what you are paying to find out.",
                                );
                            }
                        }
                        Next::Insufficient { price, have } => {
                            ui.same_line();
                            ui.text_disabled(format!("Hint next lock ({price} -- have {have})"));
                        }
                        Next::AllFrontierHinted => {
                            ui.same_line();
                            ui.text_colored(HINT_YELLOW, "next lock already hinted");
                        }
                        Next::Spilled { regions } => {
                            // The dead-end the ruling called out: a lock in another player's world
                            // is invisible to our scout, so say so and name the tool.
                            ui.same_line();
                            ui.text_disabled(format!(
                                "next lock is in another world ({}) -- use !hint",
                                regions.join(", ")
                            ));
                        }
                        Next::NoneReachable => {
                            ui.same_line();
                            ui.text_disabled("no lock within reach -- you are gated on something else");
                        }
                        Next::Idle => {}
                    }
                    ui.separator();
                }
                // ---- where you are standing, and how hard it is ----------------------------
                // The entry toast says this ONCE and is then gone; this is the same sentence,
                // askable. Above the filters because it describes the player, not the list.
                if let Some(line) = &scaling_here {
                    ui.text(line);
                    if ui.is_item_hovered() {
                        ui.tooltip_text(
                            "Enemy scaling where you are standing right now.\nThe tier is this seed's own band, not the full ladder -- tier 0 is your easiest region, not 'unscaled'.",
                        );
                    }
                    ui.separator();
                }
                // ---- Boss sweeps -----------------------------------------------------------
                // Collapsed by default: it is reference, not a control. The HEADER carries the
                // number worth glancing at ("N still behind a boss") so the section answers the
                // question without being opened.
                if !sweep_rows.is_empty()
                    && ui.collapsing_header(
                        format!("{sweep_header}###trk-sweeps"),
                        imgui::TreeNodeFlags::empty(),
                    )
                {
                    for (label, state) in &sweep_rows {
                        ui.text(format!("  {label}"));
                        ui.same_line();
                        ui.text_disabled(format!("-- {state}"));
                    }
                    ui.separator();
                }
                ui.checkbox("in-logic only", &mut in_logic_only);
                ui.same_line();
                ui.checkbox("progression surface only", &mut surface_only);
                ui.separator();
                if model.total == 0 {
                    ui.text_disabled("No location data yet -- connect to a session.");
                }

                // (b) Per-region rollups. The ### id is the region name alone so the header's
                // open state survives the done/total counters changing.
                // TWO PASSES, CONCEALED ROWS LAST (2026-08-09). Rendered in one pass, a concealed
                // row keeps its ALPHABETICAL slot -- "Locked region" sitting between Belurat and
                // Roundtable Hold tells you its initial is somewhere in C..R, which is most of the
                // way to naming it. Sinking them to the bottom leaves only their order relative to
                // EACH OTHER, which names nothing on its own. Two passes over the same body rather
                // than a sorted index: the body renders interactive widgets, so it cannot be
                // buffered and replayed.
                for conceal_pass in [false, true] {
                    for region in &model.regions {
                    // Filter pass first so fully-filtered regions can be skipped outright.
                    let shown: Vec<_> = region
                        .unchecked
                        .iter()
                        .filter(|u| {
                            (!in_logic_only || u.in_logic) && (!surface_only || u.on_surface)
                        })
                        .collect();
                    if (in_logic_only || surface_only) && shown.is_empty() {
                        continue;
                    }
                    // THE MASK. Computed from the same offer the button below renders, so the row
                    // and its reveal can never disagree -- and defaulting to CONCEALED while the
                    // ledger is still loading, because a row that names itself during the pause
                    // leaks exactly what this hides. See lock_hint_economy::conceal_region.
                    let row_offer = if region.accessible || !ledger_ready {
                        None
                    } else {
                        let lock_item = region
                            .unchecked
                            .first()
                            .and_then(|u| coarse_of.get(&u.location_id))
                            .and_then(|c| lock_item_of.get(c))
                            .map(|s| s.as_str());
                        Some(er_logic::lock_hint_economy::offer(
                            lock_item,
                            &lock_scout,
                            &hinted_locs,
                            surface_total_n,
                            surface_checked_n,
                            purchases_n,
                            points_per_hint,
                            total_locations,
                        ))
                    };
                    let concealed = match &row_offer {
                        Some(o) => er_logic::lock_hint_economy::conceal_region(region.accessible, o),
                        None => !region.accessible,
                    };
                    if concealed != conceal_pass {
                        continue;
                    }
                    let lock_tag = if region.accessible { "" } else { "  [locked]" };
                    // THE COUNTS ARE PART OF THE NAME. The thirteen DLC region sizes are all
                    // distinct, so `0/85` identifies Enir Ilim as surely as the word does. A
                    // concealed row shows neither -- but it keeps the REAL region name after the
                    // `###`, which imgui uses as the widget id and never draws, so a row's
                    // open/closed state survives the reveal.
                    let header = if concealed {
                        format!(
                            "Locked region  0/??{}###trk-region-{}",
                            lock_tag, region.region
                        )
                    } else {
                        format!(
                            "{}  {}/{}{}###trk-region-{}",
                            region.region, region.done, region.total, lock_tag, region.region
                        )
                    };
                    // Dim the header text while the region's coarse region is locked. The token
                    // pops on drop -- released right after the header so the rows keep their
                    // own colors.
                    let dim = (!region.accessible)
                        .then(|| ui.push_style_color(imgui::StyleColor::Text, LOCKED_GRAY));
                    let expanded = ui.collapsing_header(header, imgui::TreeNodeFlags::empty());
                    drop(dim);
                    // ---- "hint lock" button ---------------------------------------------------
                    // Only on a LOCKED region, and only once the ledger has been read back: a
                    // balance computed against an unread ledger looks like free money.
                    if let Some(offer) = row_offer {
                        // THE PURCHASE IS THE REVEAL (Alaric, 2026-08-09), so the button stays on
                        // a concealed row. Buying blind is the point: it is what turns "Locked
                        // region" into a name, and hiding the button until something else revealed
                        // the region would leave the economy with nothing to sell.
                        use er_logic::lock_hint_economy::LockHintOffer as Offer;
                        match offer {
                            Offer::Buyable { price, location } => {
                                ui.same_line();
                                if ui.small_button(format!(
                                    "hint lock ({price})###trk-buy-{}",
                                    region.region
                                )) {
                                    buy_clicks.push(location);
                                }
                            }
                            Offer::Insufficient { price, have, .. } => {
                                // DISABLED WITH THE COST, never hidden: a player who cannot see
                                // the price, or that they are making progress toward it, learns
                                // nothing from the mechanic.
                                ui.same_line();
                                ui.text_disabled(format!("hint lock ({price} -- have {have})"));
                            }
                            Offer::AlreadyHinted { .. } => {
                                ui.same_line();
                                ui.text_colored(HINT_YELLOW, "hinted");
                            }
                            Offer::Spilled => {
                                ui.same_line();
                                ui.text_disabled("lock is in another world -- use !hint");
                            }
                            Offer::Unknown => {}
                        }
                    }
                    // A CONCEALED ROW HAS NO CONTENTS. The location names leak more than the
                    // header ever did -- ours carry the region as a "<Region> :: ..." prefix, and
                    // the individual place names give it away even where they do not.
                    if expanded && concealed {
                        ui.text_disabled("  hidden until this region is unlocked or hinted");
                    }
                    if expanded && !concealed {
                        if shown.is_empty() {
                            ui.text_disabled("  complete");
                        }
                        for u in shown {
                            let name = display_loc(u.location_id);
                            let star = if u.on_surface { "* " } else { "" };
                            let line = if u.hinted {
                                format!("  {star}[hint] {name}")
                            } else {
                                format!("  {star}{name}")
                            };
                            if u.hinted {
                                ui.text_colored(HINT_YELLOW, line);
                            } else if u.on_surface {
                                ui.text_colored(SURFACE_ORANGE, line);
                            } else if !u.in_logic {
                                ui.text_disabled(line);
                            } else {
                                ui.text(line);
                            }
                        }
                    }
                }
                }
                // (b2) Bosses group (mode A/B). RE-AUTHORED tail -- no bak_rlwarn equivalent; the
                // boss tracker post-dates that backup. Rendered from the pure `boss_group` snapshot.
                if !boss_group.rows.is_empty() {
                    ui.separator();
                    let header = format!(
                        "Bosses  {}/{}###trk-bosses",
                        boss_group.defeated(),
                        boss_group.total()
                    );
                    if ui.collapsing_header(header, imgui::TreeNodeFlags::empty()) {
                        for row in &boss_group.rows {
                            // `name` is the full "Felled: <Boss>" label; strip for a clean line.
                            let boss = row
                                .name
                                .strip_prefix("Felled: ")
                                .unwrap_or(row.name.as_str());
                            match row.state {
                                er_logic::boss_felled::BossState::Locked => {
                                    ui.text_disabled(format!("  {boss}  [{}]", row.region));
                                }
                                er_logic::boss_felled::BossState::Felled => {
                                    let line = match &row.display_key {
                                        Some(key) => format!("  {boss}  felled -- awaiting {key}"),
                                        None => format!("  {boss}  felled"),
                                    };
                                    ui.text_colored(SURFACE_ORANGE, line);
                                }
                                er_logic::boss_felled::BossState::Released => {
                                    ui.text_colored(HINT_YELLOW, format!("  {boss}  released"));
                                }
                            }
                        }
                    }
                }

                ui.separator();

                // (c) Received items (raw cumulative names; sorted by the model).
                if ui.collapsing_header(
                    format!(
                        "Items received ({})###trk-items",
                        model.received_items.len()
                    ),
                    imgui::TreeNodeFlags::empty(),
                ) {
                    for item in &model.received_items {
                        ui.text(format!("  {item}"));
                    }
                }

                // (d) Standing hints.
                if ui.collapsing_header(
                    format!("Hints ({})###trk-hints", hint_list.len()),
                    imgui::TreeNodeFlags::empty(),
                ) {
                    if hint_list.is_empty() {
                        ui.text_disabled("  none yet");
                    }
                    for h in &hint_list {
                        let who = if h.for_us {
                            format!("for {}", h.other_player)
                        } else {
                            format!("hinted by {}", h.other_player)
                        };
                        ui.text_colored(
                            HINT_YELLOW,
                            format!("  {} @ {} ({who})", h.item_name, display_loc(h.location_id)),
                        );
                    }
                }
            });
        // Commit outside the closure -- inside it, `self` is not borrowable.
        for loc in buy_clicks {
            self.lock_hints.buy(loc);
        }
        if !open {
            self.tracker_visible = false;
        }
        self.tracker_in_logic_only = in_logic_only;
        self.tracker_surface_only = surface_only;
    }

    fn write_save(&self) {
        let Some(path) = self.save_path.as_ref() else {
            return;
        };
        let (counter, high) = self.progressive.snapshot();
        let st = SaveState {
            last_received_index: self.received_through as i64,
            flag_poll_baseline: self.flag_poll_baseline.iter().copied().collect(),
            notify_granted: Default::default(),
            progressive_counter: counter.into_iter().collect::<BTreeMap<_, _>>(),
            progressive_high_index: high,
        };
        let tmp = path.with_extension("json.tmp");
        // R7 (SWEEP): surface write/rename failures -- a silently-lost save resets the
        // watermarks next session (duplicate start items + regrant burst).
        match std::fs::write(&tmp, st.to_json()) {
            Ok(()) => {
                if let Err(e) = std::fs::rename(&tmp, path) {
                    log::error!(
                        "save persistence: rename {} -> {} FAILED: {e}",
                        tmp.display(),
                        path.display()
                    );
                }
            }
            Err(e) => log::error!("save persistence: write {} FAILED: {e}", tmp.display()),
        }
    }
}

static UNIQUE_GRANT_FAIL_LOGGED: std::sync::Mutex<Option<HashSet<usize>>> =
    std::sync::Mutex::new(None);

/// Fail-loud (once per uniqueStartGrants index) when a unique grant does not land despite a
/// captured inventory pointer. Mirrors [`warn_start_item_fail_once`]; the block retries the
/// FAILED entry each tick, so without this a stuck grant is silent.
fn warn_unique_grant_fail_once(idx: usize, full_id: i32) {
    let mut guard = UNIQUE_GRANT_FAIL_LOGGED.lock().unwrap();
    if guard.get_or_insert_with(HashSet::new).insert(idx) {
        log::warn!(
            "unique grant #{idx} ({full_id:#x}) failed to grant (inventory captured but AddItem              rejected) -- retrying each tick; if this persists the grant is stuck"
        );
    }
}

/// R5 (SWEEP): one warning per unmapped AP item id -- the grant loop would otherwise drop the
/// item with no trace, every session, on every replay. (Doc re-attached to the static it actually
/// describes; it had drifted one static too high, onto the now-deleted START_ITEM_FAIL_LOGGED.)
static UNMAPPED_LOGGED: std::sync::Mutex<Option<HashSet<i64>>> = std::sync::Mutex::new(None);

/// #413 boss-grant diagnostic: the last (boss healthbar, `c4710` presence) pair already reported,
/// so the line lands on a CHANGE instead of every tick. `i64::MIN` = nothing reported yet, and
/// `diag_key` is tested never to produce it.
static BOSS_GRANT_DIAG_LAST: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(i64::MIN);

/// #413: have we already put the spear in the player's hand for the CURRENT Rykard fight? Re-armed
/// when the healthbar goes down, so every fight equips exactly once instead of every tick.
static RYKARD_FIGHT_EQUIPPED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// #413 swap-back: the two weapon slots as they were the moment before the fight-equip displaced
/// one. Param rows, never `GaitemHandle`s -- a handle goes stale when the inventory moves and
/// `auto_upgrade` can change an id under us, so the row is re-resolved through the bag at restore
/// time (the `held_row_to_equip` lesson). `PREV_WEAPONS_SET` is the presence bit: slot 0 is a
/// legitimate-looking value and cannot double as "nothing recorded".
static PREV_WEAPON_LEFT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static PREV_WEAPON_RIGHT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static PREV_WEAPONS_SET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn warn_unmapped_once(name: &str, ap_id: i64) {
    let mut guard = UNMAPPED_LOGGED.lock().unwrap();
    if guard.get_or_insert_with(HashSet::new).insert(ap_id) {
        log::warn!(
            "item '{name}' (ap id {ap_id}) has no ER mapping -- NOT granted (contract drift?)"
        );
    }
}

fn save_file_path(seed: &str, name: &str) -> Option<PathBuf> {
    let dir = match shared::utils::mod_directory() {
        Ok(d) => d,
        Err(e) => {
            // R7 (SWEEP): was `.ok()?` -- save persistence silently never armed.
            log::error!(
                "save persistence UNAVAILABLE ({e}) -- watermarks will reset every session"
            );
            return None;
        }
    };
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    };
    Some(dir.join(format!("ap_save_{}_{}.json", safe(seed), safe(name))))
}

/// Parse slot_data `bossLockItems` (mode A/B, SPEC-boss-lock-tracker) into [`BossDef`] rows.
/// `{ "<boss_flag>": {name, region, boss_ap_id, gate?, display_key?} }`. Tolerant: skips any
/// entry whose key is not a u32 or whose value is not an object. Absent/empty => no boss tracking.
/// Load the shipped `check_lots_table.json` from the DLL/mod directory (same place as
/// `shoplineup_flags.json`). Absent/garbage -> empty, and suppression simply stays off, which is
/// exactly today's behaviour -- never a panic mid-connect.
fn load_static_lots() -> er_logic::static_lots::StaticLots {
    let path = shared::utils::mod_directory()
        .map(|d| d.join("check_lots_table.json"))
        .unwrap_or_else(|_| std::path::PathBuf::from("check_lots_table.json"));
    match std::fs::read_to_string(&path) {
        Ok(t) => er_logic::static_lots::parse(&t),
        Err(_) => er_logic::static_lots::StaticLots::default(),
    }
}

fn parse_boss_lock_items(v: Option<&Value>) -> Vec<er_logic::boss_felled::BossDef> {
    let mut out = Vec::new();
    let Some(obj) = v.and_then(|v| v.as_object()) else {
        return out;
    };
    for (k, entry) in obj {
        let (Ok(flag), Some(e)) = (k.parse::<u32>(), entry.as_object()) else {
            continue;
        };
        out.push(er_logic::boss_felled::BossDef {
            flag,
            name: e
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            region: e
                .get("region")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            boss_ap_id: e.get("boss_ap_id").and_then(|x| x.as_i64()).unwrap_or(0),
            gate: e.get("gate").and_then(|x| x.as_str()).map(str::to_string),
            display_key: e
                .get("display_key")
                .and_then(|x| x.as_str())
                .map(str::to_string),
        });
    }
    out
}

/// Parse slot_data `regionAttunement` (attunement_gate) into per-region [`RegionAttunement`].
/// `{ "<region>": {threshold, member_ap_ids, bloom_flags} }`. Absent/empty => feature off.
/// `members` is a HashSet<i64> (matches the struct + er_logic::attunement's `&HashSet<i64>` inputs).
fn parse_region_attunement(v: Option<&Value>) -> HashMap<String, RegionAttunement> {
    let mut out = HashMap::new();
    let Some(obj) = v.and_then(|v| v.as_object()) else {
        return out;
    };
    for (region, entry) in obj {
        let Some(e) = entry.as_object() else { continue };
        out.insert(
            region.clone(),
            RegionAttunement {
                threshold: e.get("threshold").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                members: e
                    .get("member_ap_ids")
                    .and_then(|x| x.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
                    .unwrap_or_default(),
                bloom_flags: e
                    .get("bloom_flags")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_u64().map(|n| n as u32))
                            .collect()
                    })
                    .unwrap_or_default(),
            },
        );
    }
    out
}

fn i64_map(v: Option<&Value>) -> HashMap<i64, i64> {
    let mut m = HashMap::new();
    if let Some(obj) = v.and_then(|v| v.as_object()) {
        for (k, val) in obj {
            if let (Ok(key), Some(value)) = (k.parse::<i64>(), val.as_i64()) {
                m.insert(key, value);
            }
        }
    }
    m
}

/// `{ "<i64>": <u32> }` slot_data object -> `i64 -> u32`. Tolerant: skips malformed entries. Used by
/// the shop system (locationFlags / shopRowFlags).
fn i64_to_u32_map(v: Option<&Value>) -> HashMap<i64, u32> {
    let mut m = HashMap::new();
    if let Some(obj) = v.and_then(|v| v.as_object()) {
        for (k, val) in obj {
            if let (Ok(key), Some(value)) = (k.parse::<i64>(), val.as_u64()) {
                m.insert(key, value as u32);
            }
        }
    }
    m
}

#[cfg(test)]
mod tests {
    /// RETIRED 2026-07-11 -- this test pinned a FICTION and that is why it never fired.
    ///
    /// It asserted the crate version sits inside a semver BAND (">=0.1.0-beta.4 <0.1.0-beta.5")
    /// that it CONSTRUCTED ITSELF, rather than the string the apworld actually sends. So when the
    /// version handshake (apworld 24e261c) changed `versions` from a band to a descriptive
    ///     "apworld/0.2.0 contract/b68eaa15 data/e4c73b06b595e0de"
    /// the test stayed green while `version_gate` -- fed a string that is not a semver range at all --
    /// failed to parse it, `.unwrap_or(false)`'d, and warned "update the client" on EVERY connect for a
    /// whole playtest. A test that builds its own input cannot catch a change in the real input.
    ///
    /// The gate is gone (the VERSION HANDSHAKE supersedes it: it compares the CONTRACT HASH and the
    /// DATA HASH, which is what actually matters). What replaces the test is the assertion that the
    /// apworld's real `versions` string is the shape the handshake parses -- i.e. test the CONTRACT,
    /// not a hand-built stand-in.
    /// The crate version and the apworld's must MOVE TOGETHER.
    ///
    /// They sat apart for months -- client 0.1.0-beta.4 against apworld 0.2.0 -- because the thing
    /// that used to force them together was the semver BAND gate, retired 2026-07-11 when `versions`
    /// became a descriptive string. Nothing replaced the pressure, so the number stopped moving and
    /// its Cargo.toml comment went on describing a mechanism that no longer existed.
    ///
    /// Nothing in PRODUCTION reads the crate version (version_gate is test-only now), so this is not
    /// protecting a runtime behaviour -- it is protecting the bug report. `versions` is the string
    /// every report carries, and a client that names a version unrelated to the apworld it was built
    /// against makes triage guesswork. They ship as one bundle; they get one number.
    ///
    /// If patch-level drift ever needs to be allowed, compare only MAJOR.MINOR here -- but do it
    /// deliberately, not by letting this rot again.
    #[test]
    fn client_version_matches_the_apworld_it_was_built_against() {
        assert_eq!(
            env!("CARGO_PKG_VERSION"),
            crate::contract_gen::APWORLD_VERSION_EXPECTED,
            "client crate version and APWORLD_VERSION (via generated contract_gen.rs) have drifted. \
             Bump crates/eldenring-archipelago/Cargo.toml to match, or the apworld's \
             contract.py APWORLD_VERSION if the client is the one that is right."
        );
    }

    #[test]
    fn versions_string_is_what_the_handshake_parses_not_a_semver_band() {
        // Exactly what greenfield/eldenring/contract.py version_string() emits.
        let real = "apworld/0.2.0 contract/b68eaa15 data/e4c73b06b595e0de";
        let sd = serde_json::json!({ "versions": real });
        let v = sd.get("versions").and_then(|x| x.as_str()).unwrap();

        // The handshake pulls the contract hash out of it and compares against the compiled-in one.
        let their_contract = v
            .split_whitespace()
            .find_map(|t| t.strip_prefix("contract/"))
            .expect("`versions` must carry contract/<hash> -- the handshake keys off it");
        assert_eq!(
            their_contract.len(),
            8,
            "contract hash is the 8-char prefix"
        );

        // And it is NOT a semver range: the old gate treated it as one and warned on every connect.
        // (The old gate called er_semver::version_satisfies on `real` and got Err -- but er_semver is not
        // even a dependency of this crate; it was only ever reachable through the gate that is now gone.
        // Assert the contract er_logic actually implements instead: a descriptive string is NOT a semver
        // band, so version_gate must never report a clean PASS on one. That is exactly the bug the old
        // gate had -- it turned an unparseable input into `false` and warned on every single connect.)
        assert_ne!(
            er_logic::version::version_gate(&sd, env!("CARGO_PKG_VERSION")),
            Some(true),
            "`versions` is a descriptive string, not a semver band -- nothing may gate on it as one"
        );
    }
}
