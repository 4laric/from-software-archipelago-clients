//! scaling.rs — runtime sphere/completion enemy scaling (SPEC-runtime-enemy-scaling.md).
//!
//! Replaces the retired baker's enemy scaling. On connect we read `regionSphereTargets` /
//! `completionScalingBasis` into a pure `er_logic::scaling::ScalingConfig`; each tick (throttled) we
//! sweep the loaded enemy `ChrIns` and, for each, clear its baked vanilla `70xx` scaling SpEffect and
//! apply the sphere tier's `70xx` (the vanilla ladder). All via typed crate calls — no raw offsets.
//!
//! Basis: MVP scales every loaded enemy to the PLAYER's current `play_region` tier (enemies loaded
//! around you are effectively in your region). Per-enemy region is one field away (`chr.play_region_id`)
//! if we want the Full basis later. Stateless: we re-check each enemy's active SpEffects, so it's
//! idempotent and re-scales correctly when the player changes region or an enemy reloads.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Instant;

use eldenring::cs::{ChrIns, ChrInsExt, ChrLoadStatus, ChrSet, ChrType, WorldChrMan};
use eldenring::position::HavokPosition;
use er_logic::id_sample::IdSample;
use er_logic::scaling::{
    AreaAnchor, AreaSource, NUM_TIERS, RegionToastLedger, ScaleAction, ScalingConfig, ScalingKind,
    area_tier_from_histogram, baked_area_tier, band_native_tier, is_dlc_bucket,
    is_scaling_speffect, is_scaling_speffect_with_downstates, ladder_tier, native_tier,
    placed_by_area, placed_by_area_down, raw_target_for_region, region_name_for_bucket,
    region_scaling, region_scaling_line, resolve_area_tier, scale_action, scaling_kind,
    settled_on_downstate, settled_on_target, speffect_id_for_tier, tier_for_region, tier_rates,
};
use er_logic::scaling_settle::{SettlePolicy, SweepGate, sweep_blocked_by_death};
use fromsoftware_shared::{FromStatic, Subclass};
use serde_json::Value;

static CONFIG: Mutex<Option<ScalingConfig>> = Mutex::new(None);
static TICK: AtomicU32 = AtomicU32::new(0);

/// #993 co-op difficulty: extra sphere tiers added per co-op partner. Set once at `configure` from
/// `options.coop_difficulty`; `0` (the default) makes the whole feature inert. Read on every sweep.
static COOP_DIFFICULTY: AtomicUsize = AtomicUsize::new(0);
/// Last co-op headcount we logged, so an engaged bump announces itself once instead of every sweep.
static LAST_COOP_EXTRA: AtomicUsize = AtomicUsize::new(usize::MAX);

/// 🛑 PROVISIONAL DISCRIMINATOR -- one observed sample, not a datamine. bobler's 2026-08-21
/// seamless-co-op census printed the partner as `("summon", npc_id=8000, chr_type=5, team=10)`.
/// Spirit Ash summons share `summon_buddy_chr_set`, so the apply-site count keys on this npc_id to
/// exclude them -- but that 8000 is a HYPOTHESIS backed by a single row. A confirming census (a SOLO
/// run with a Spirit Ash out) must show ashes do NOT also carry 8000 before anyone relies on the
/// bump. Until then the feature ships off by default (`coop_difficulty = 0`), so a wrong guess here
/// changes nothing for anyone who has not opted in. Cite this row, never "a census shows".
const COOP_PARTNER_NPC_ID: i32 = 8000;

/// Apply the region's tier SpEffect only a few times a second (enemy stats don't need per-frame).
///
/// ⚠️ This is a REPEAT-sweep throttle only. It used to be checked BEFORE the settle guard, which
/// meant the first sweep after a transition also had to wait for the next multiple of 30 -- ~500 ms
/// at 60fps and a full second at 30, and post-load framerate is exactly when it is worst. It is now
/// measured as "ticks since the last sweep", so the first allowed sweep happens immediately.
const THROTTLE: u32 = 30;

/// Transition/region bookkeeping for the time-based backstop (`er_logic::scaling_settle`, host-tested;
/// see `SETTLE` for the policy). This is the PRIMARY crash guard again -- see sweepable_characters.
static GATE: Mutex<SweepGate> = Mutex::new(SweepGate::new());
/// Process-relative clock for the gate (which is pure and takes `now_ms`).
static EPOCH: Mutex<Option<Instant>> = Mutex::new(None);
/// Ticks at the last sweep -- the repeat throttle. `None` = never swept, so the first allowed tick
/// sweeps immediately instead of waiting for a modulo phase.
static LAST_SWEEP_TICK: Mutex<Option<u32>> = Mutex::new(None);
/// Everything about a sweep that does NOT vary per enemy, in one place.
///
/// Introduced 2026-08-08 because adding the per-entity `chr_load_status` pushed `scale_one` and
/// `scale_hostile_phantom` to 8 positional arguments and clippy (`-D warnings`) refused them. The
/// honest fix is not a shorter list -- it is noticing that five of those arguments are constant for
/// the whole sweep and only two describe the enemy in hand.
struct SweepCtx<'a> {
    target: i32,
    target_tier: usize,
    player_handle: &'a eldenring::cs::FieldInsHandle,
    sample_on: bool,
    area_tier: Option<usize>,
}

/// One row of the per-enemy SAMPLE line:
/// `(npc_param_id, npc_id, hp, max_hp, load_status, carried)`.
///
/// Named because clippy's `type_complexity` is right about the raw tuple, and because the field
/// order is the thing a reader of a log line has to match up against.
type SampleRow = (u64, i32, i32, i32, i32, &'static str, Vec<i32>);

/// Per-sweep tally of the `chr_load_status` of every entry we walked, indexed by `status_slot`.
/// Diagnostic ONLY -- nothing filters on it. It exists because the last thing that DID filter on it
/// rejected every enemy in the game, and no log at the time could say what the real states were.
static STATUS_HIST: [AtomicU32; 6] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];
/// One-shot release log per transition.
static RELEASE_LOGGED: Mutex<bool> = Mutex::new(true);

/// Outstanding rung writes, waiting to see whether `max_hp` followed (client#188/#186/#183).
///
/// ⭐ APPLYING A RUNG IS TWO EVENTS -- we write a speffect, and the engine recomputes `max_hp` --
/// and this census has only ever observed the first. #188's controlled pair proved a write to an
/// `unloaded` chr leaves `max_hp` on the old tier's number, and #186 is that same enemy read by two
/// instruments at two moments. `(re)scaled N` counts both, so it has been over-reporting since it
/// shipped.
static RESCALE_WATCH: Mutex<er_logic::rescale_watch::RescaleWatch> =
    Mutex::new(er_logic::rescale_watch::RescaleWatch::new());
/// The area reading this region resolved while it still had a vanilla sample to read. Pure policy +
/// host tests in `er_logic::scaling::AreaAnchor`; this is only the residence. Without it a character
/// that instantiates AFTER the region's first sweep can never be placed — see the type's docs for
/// the two-health-bar boss that found it.
static AREA_ANCHOR: Mutex<AreaAnchor> = Mutex::new(AreaAnchor::new());
/// Once-per-announcement ledger for the region-entry scaling toast (pure + host-tested in
/// `er_logic::scaling::RegionToastLedger`; the dedup policy and its tests live there). Reset in
/// `configure` so a new seed's tiers announce afresh.
static TOAST_LEDGER: Mutex<RegionToastLedger> = Mutex::new(RegionToastLedger::new());

fn now_ms() -> u64 {
    let mut epoch = match EPOCH.lock() {
        Ok(e) => e,
        Err(_) => return 0,
    };
    epoch.get_or_insert_with(Instant::now).elapsed().as_millis() as u64
}

/// Iterate a chr set's characters, recording the `chr_load_status` distribution as it goes.
///
/// 🛑 THIS FUNCTION FILTERED ON `chr_load_status == Active` FROM 2026-07-27 (98a2362) UNTIL THE SAME
/// DAY, AND IT BROKE ENEMY SCALING COMPLETELY. Alaric's repro log, a rolled Mohgwyn start:
///
///     enemy-scaling: settle release after 2512ms (...); swept 1, skipped 242 not-Active entries
///     enemy-scaling: settle release after 2508ms (...); swept 2, skipped 1219 not-Active entries
///     enemy-scaling: region 12050 -> speffect 7010 (tier 0/19, sphere target 0/10000, 1.14x HP)
///
/// The apworld was perfect -- Mohgwyn as the start region resolved to tier 0, exactly as designed.
/// But ~99.5% of entries were rejected, and the phantom census shows the one entity actually being
/// swept was `npc_id 8000` -- TORRENT. Not a single real enemy was ever scaled, so every one of them
/// kept its VANILLA rung: a player starting in Mohgwyn met endgame-strength enemies and was
/// oneshot. Reported by ShadowTL on Nexus for a Snowfield start, same mechanism.
///
/// WHAT WAS TRUE AND WHAT WAS NOT. True: `chr_load_status` exists on the ENTRY, so reading it
/// dereferences no `ChrIns`, and upstream's `characters()` genuinely ignores it. NOT true, and never
/// checked against a running game: that a loaded, fightable enemy is in state `Active`. It is not.
/// I inferred the meaning of an enum variant from its NAME and shipped it flagged
/// "UNVERIFIED IN-GAME", which is not the same as safe.
///
/// So this is back to upstream's behaviour -- every non-null entry -- which is what shipped for
/// months before 98a2362 and never had this problem. The 2500 ms `SETTLE` window is untouched and is
/// once again the primary guard, so the CTD exposure is exactly what it was, not worse.
///
/// The histogram is the part that stops this recurring: the next build's logs will say what states
/// enemies are ACTUALLY in, and only then can this be narrowed on evidence instead of on a name.
/// Is a character of chr `chr_id` loaded right now?
///
/// `None` = WorldChrMan was not reachable this tick. The caller must treat that as "don't know",
/// NEVER as "no" -- see `er_logic::boss_grants` property 3.
///
/// Walks the SAME two sets the sweep does (open-field base + every block slot), because that is
/// where mobs and bosses live; phantom sets are deliberately not consulted.
///
/// 🛑 `ChrIns` does NOT implement `AsRef`. The set iterators yield `&mut T: Subclass<ChrIns>`, and
/// the way this file already gets a `&ChrIns` out of one is to PASS IT to a fn taking `&ChrIns`
/// (see `area_sample_one`) and let the coercion happen at the call. `chr.as_ref()` does not
/// compile; that cost a CI round.
fn chr_is(chr: &ChrIns, chr_id: i32) -> bool {
    er_logic::boss_grants::is_character(chr.npc_param_id, chr_id)
}

pub(crate) fn any_character_present(chr_id: i32) -> Option<bool> {
    let Ok(wcm) = (unsafe { WorldChrMan::instance() }) else {
        return None;
    };
    for chr in sweepable_characters(&wcm.open_field_chr_set.base) {
        if chr_is(chr, chr_id) {
            return Some(true);
        }
    }
    for slot in wcm.chr_sets.iter().flatten() {
        for chr in sweepable_characters(slot) {
            if chr_is(chr, chr_id) {
                return Some(true);
            }
        }
    }
    Some(false)
}

/// One sighting of `c<chr_id>`, for the #594 diagnostic. READ-ONLY.
pub(crate) struct Sighting {
    pub npc_param_id: i32,
    pub status: ChrLoadStatus,
    /// Straight-line distance to the main player. `None` when there is no main player to measure
    /// from -- never `0.0`, which would read as "standing on top of him".
    pub metres: Option<f32>,
}

/// Every instance of `c<chr_id>` the presence walk can see, and how far each one is.
///
/// MOTIVATING CASE (rule 11), #594, bobler 2026-08-12. `any_character_present` reported c4710
/// loaded and the Serpent-Hunter was granted, while matt's spoiler placed the seed's ONLY Rykard in
/// Enir-Ilim and the player was in Ancient Ruins of Rauh -- a different map. The presence test
/// returns one bool for the whole world, so the log could not say which instance answered, what
/// state it was in, or where it was. This says all three.
///
/// 🛑 Walks EXACTLY the sets `any_character_present` walks, in the same order, and adds no filter of
/// its own -- so the two can never disagree about what is present. See `sweepable_characters` for
/// why narrowing this walk is a mistake this file has already shipped once.
///
/// `None` = WorldChrMan unreachable: the same "don't know" `any_character_present` means by `None`.
pub(crate) fn sight_character(chr_id: i32) -> Option<Vec<Sighting>> {
    let Ok(wcm) = (unsafe { WorldChrMan::instance() }) else {
        return None;
    };
    let player = wcm
        .main_player
        .as_ref()
        .map(|p| p.chr_ins.modules.physics.position);
    let mut out = Vec::new();
    for (status, chr) in sweepable_characters_with_status(&wcm.open_field_chr_set.base) {
        sight_one(chr, chr_id, status, player, &mut out);
    }
    for slot in wcm.chr_sets.iter().flatten() {
        for (status, chr) in sweepable_characters_with_status(slot) {
            sight_one(chr, chr_id, status, player, &mut out);
        }
    }
    Some(out)
}

/// Takes `&ChrIns` so the caller's `&mut T: Subclass<ChrIns>` coerces at the call site -- the same
/// trick `chr_is` and `area_sample_one` use, and `chr.as_ref()` still does not compile.
fn sight_one(
    chr: &ChrIns,
    chr_id: i32,
    status: ChrLoadStatus,
    player: Option<HavokPosition>,
    out: &mut Vec<Sighting>,
) {
    if !er_logic::boss_grants::is_character(chr.npc_param_id, chr_id) {
        return;
    }
    let metres = player.map(|p| {
        let q = chr.modules.physics.position;
        let (dx, dy, dz) = (q.0 - p.0, q.1 - p.1, q.2 - p.2);
        (dx * dx + dy * dy + dz * dz).sqrt()
    });
    out.push(Sighting {
        npc_param_id: chr.npc_param_id,
        status,
        metres,
    });
}

/// The log line for a `sight_character` result.
///
/// ⭐ The EMPTY case is the whole point and gets its own wording: "present said yes and the walk
/// found nothing" is a different fact from "present said yes and here is the one it found, 400m
/// away", and #594 cannot be settled without telling them apart.
pub(crate) fn describe_sightings(chr_id: i32, sightings: &[Sighting]) -> String {
    if sightings.is_empty() {
        return format!(
            "serpent-hunter sightings: c{chr_id} NONE -- no entry in any walked ChrSet carries a \
             matching npc_param"
        );
    }
    let body = sightings
        .iter()
        .map(|s| {
            let label = status_label(s.status);
            match s.metres {
                Some(m) => format!("npc_param {} ({label}) {m:.1}m", s.npc_param_id),
                None => format!("npc_param {} ({label}) no main player", s.npc_param_id),
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "serpent-hunter sightings: c{chr_id} x{} -- {body}",
        sightings.len()
    )
}

pub(crate) fn sweepable_characters<T>(set: &ChrSet<T>) -> impl Iterator<Item = &mut T>
where
    T: Subclass<ChrIns> + 'static,
{
    sweepable_characters_with_status(set).map(|(_status, chr)| chr)
}

/// The same walk, yielding each entry's `chr_load_status` alongside the character.
///
/// ⭐⭐⭐ ADDED 2026-08-08 TO ANSWER A QUESTION A HISTOGRAM CANNOT. boblerrr's Enir Ilim log showed
/// **19 of 24 sampled enemies take the rung into their effect list while `max_hp` never moved**,
/// held across four minutes -- and in the same sweeps the census read `active=4` while exactly
/// **4 distinct entities' `max_hp` moved**. Three settles, 4 and 4 each time.
///
/// 🛑🛑 THAT IS A CORRELATION BETWEEN TWO AGGREGATES, AND ACTING ON IT IS PRECISELY THE MISTAKE
/// THIS FILE ALREADY SHIPPED. `98a2362` narrowed the sweep to `chr_load_status == Active` on a
/// reading of the enum's NAME, rejected ~99.5% of enemies, and reached players. The postmortem's
/// rule is `COUNT, DO NOT FILTER` -- and its named follow-up is exactly this: make the per-entity
/// state visible so the next log can settle it as a per-row fact instead of a coincidence of two
/// totals. `Unloaded` is known to be the MAJORITY state of a live, fightable enemy, so the obvious
/// reading ("only Active recomputes") predicts scaling would never work at all -- which it
/// demonstrably does. The hypothesis is therefore UNDER-DETERMINED, not confirmed.
///
/// So: this changes no behaviour and filters nothing. It only lets the SAMPLE line say, per row,
/// which state each enemy was in when we wrote to it.
pub(crate) fn sweepable_characters_with_status<T>(
    set: &ChrSet<T>,
) -> impl Iterator<Item = (ChrLoadStatus, &mut T)>
where
    T: Subclass<ChrIns> + 'static,
{
    let mut current = set.entries;
    let end = unsafe { current.add(set.capacity as usize) };
    std::iter::from_fn(move || {
        while current != end {
            // Read the ENTRY (flat array slot) -- never the ChrIns behind it.
            let entry = unsafe { current.as_ref() };
            let (chr_ins, status) = (entry.chr_ins, entry.chr_load_status);
            current = unsafe { current.add(1) };
            let Some(mut chr_ins) = chr_ins else {
                continue;
            };
            // COUNT, do not filter. A tally with no filter is evidence; a filter with no tally was
            // the bug (CONTRIBUTING rule 4 cuts both ways).
            STATUS_HIST[status_slot(status)].fetch_add(1, Ordering::Relaxed);
            return Some((status, unsafe { chr_ins.as_mut() }));
        }
        None
    })
}

/// Short per-entity label for the SAMPLE line. Deliberately terse -- the sample tuple is already
/// wide, and this rides on every row.
fn status_label(s: ChrLoadStatus) -> &'static str {
    match s {
        ChrLoadStatus::Unloaded => "unloaded",
        ChrLoadStatus::Initializing => "init",
        ChrLoadStatus::Active => "active",
        ChrLoadStatus::NetworkInitializing => "netinit",
        ChrLoadStatus::ReadyForActivation => "ready",
        ChrLoadStatus::Unloading => "unloading",
    }
}

/// Stable index for a `ChrLoadStatus` in `STATUS_HIST`. Exhaustive on purpose: if upstream adds a
/// variant this stops compiling rather than silently bucketing it as something else.
fn status_slot(s: ChrLoadStatus) -> usize {
    match s {
        ChrLoadStatus::Unloaded => 0,
        ChrLoadStatus::Initializing => 1,
        ChrLoadStatus::Active => 2,
        ChrLoadStatus::NetworkInitializing => 3,
        ChrLoadStatus::ReadyForActivation => 4,
        ChrLoadStatus::Unloading => 5,
    }
}
/// How long to let a freshly-loaded map settle before sweeping, and how long the region must have
/// been stable. Pure policy; the gate that consumes it is `er_logic::scaling_settle` (host-tested).
///
/// `settle_ms` is UNCHANGED at 2500, and after the 98a2362 revert it is once again the PRIMARY
/// crash guard rather than a backstop -- `sweepable_characters` no longer filters. Do not lower it
/// without live evidence: that is the mistake this file has already made once today.
///
/// `stable_ms` is the NEW half. The old guard restarted the full 2500 on every observed region
/// change, so the transient `play_region_id` values a warp produces compounded the real window to
/// ~8s in play. Worse, the region was only SAMPLED every 30 ticks, so a flap shorter than ~500ms was
/// never seen at all -- too slow and too blind, from the same line ordering. Observation is now
/// per-tick (strictly MORE protective: those flaps now hold the gate) and a flap costs `stable_ms`
/// rather than a fresh `settle_ms`. Starts conservative at the shipping default; see scaling_settle.
const SETTLE: SettlePolicy = SettlePolicy::SHIPPING;

/// Re-arm the settle window from an EXTERNAL transition signal.
///
/// CTD guard (2026-07-24, "Beside the Rampart Gaol" warp postmortem). Two callers:
///   * `warp_hook`'s LuaWarp detour — the moment ANY warp (menu or client) is requested. The
///     region-change guard in `tick` cannot cover the warp-OUT side: `play_region` keeps its old
///     value through the first teardown frames (main_player still placed, so `in_world()` still
///     reads true), so the sweep kept walking the ORIGIN region's ChrIns sets while the engine
///     streamed them out — the same native-crash class `REGION_SETTLE` exists for (Siofra,
///     2026-07-09), just on the other edge of the transition.
///   * core's in-world false->true edge — covers a SAME-REGION reload (death respawn), where
///     `LAST_REGION` never changes so the region-change reset never fires, yet the ChrIns sets
///     were just torn down and rebuilt around the sweep.
///
/// Worst case is the documented degrade: enemies stay vanilla-statted for `REGION_SETTLE`
/// longer. No panic path (a poisoned lock is skipped; `Instant::now` cannot fail): the detour
/// caller sits inside the game's own warp call frame and must never unwind across it.
pub fn notify_transition() {
    let now = now_ms();
    if let Ok(mut gate) = GATE.lock() {
        gate.on_transition(now);
    }
    // Re-arm the one-shot release log so the NEXT release describes this transition.
    if let Ok(mut logged) = RELEASE_LOGGED.lock() {
        *logged = false;
    }
    // A load re-derives every character, so writes owed by the previous world are not owed by this
    // one -- and their instance handles are about to be reused by different enemies.
    if let Ok(mut w) = RESCALE_WATCH.lock() {
        w.reset();
    }
}

/// The tracker's "what is my scaling here" row, for `bucket` (= `play_region_id / 100`).
///
/// `None` ONLY when there is no `ScalingConfig` at all -- not connected, or the seed has scaling
/// off -- so the row can be omitted entirely rather than asserting something about a feature that
/// is not running. Every other case, INCLUDING a bucket the wire never heard of, returns a
/// sentence: see `er_logic::scaling::region_scaling_line` for why silence is the wrong answer to a
/// question the player asked on purpose.
///
/// ⚠️ Read-only and lock-scoped: this runs on the imgui present thread, so it touches CONFIG and
/// nothing else. The BUCKET is cached by core's tick rather than read here -- `play_region_id()`
/// dereferences `WorldChrMan`, which is game memory this thread has no business reading.
pub fn describe_region(bucket: i32) -> Option<String> {
    let guard = CONFIG.lock().ok()?;
    let cfg = guard.as_ref()?;
    let scaling = region_scaling(
        raw_target_for_region(cfg, bucket),
        cfg.max_target,
        cfg.floor_tier,
        cfg.ceiling_tier,
    );
    Some(region_scaling_line(
        region_name_for_bucket(bucket),
        scaling,
        cfg.floor_tier,
        cfg.ceiling_tier,
    ))
}

/// Let the region-entry toast speak again -- called from the LuaWarp hook on a grace warp.
///
/// bobler asked for this on 2026-08-07: the toast is message-keyed and per session, so warping
/// back into the region you are already standing in said nothing. A fast travel is precisely when
/// a player is re-orienting and wants to be re-told how hard where they landed is.
///
/// 🛑 DELIBERATELY *NOT* FOLDED INTO `notify_transition`. That is also called on core's in-world
/// false->true edge, which is a DEATH RESPAWN -- and re-announcing the scaling every time the
/// player dies is noise, in the one seed shape where they die most. The warp hook is the narrower
/// signal and the one that was actually asked for.
///
/// Infallible, like `notify_transition`: it runs inside the game's own warp call frame, so a
/// poisoned lock is skipped rather than unwound across FFI.
pub fn notify_grace_warp() {
    if let Ok(mut ledger) = TOAST_LEDGER.lock() {
        ledger.reset();
    }
}

/// Is a CEILING actually in force? `scaling_ceiling` is declared only by a seed that genuinely
/// caps (`completion_scaling_ceiling` below the top rung), so the probe has to mean the same thing:
/// scaling is configured AND its ceiling is below `NUM_TIERS - 1`. A seed that leaves the knob at
/// the top declares nothing and arms nothing, and both sides agree on that. See `feature_handshake`.
pub fn ceiling_is_capped() -> bool {
    CONFIG
        .lock()
        .ok()
        .and_then(|g| {
            g.as_ref()
                .map(|c| c.ceiling_tier.min(NUM_TIERS - 1) < NUM_TIERS - 1)
        })
        .unwrap_or(false)
}

/// Parse slot_data at connect. The parse itself — including the SWEEP H4 / R6 refuse-to-arm on an
/// empty/missing `regionSphereTargets` — lives in `er_logic::scaling::parse_scaling_config`
/// (host-tested); this wrapper only owns the logging and the CONFIG swap.
pub fn configure(sd: &Value) {
    let requested = er_logic::options::parse_bool_option(sd, "completion_scaling");
    let cfg = er_logic::scaling::parse_scaling_config(sd);
    COOP_DIFFICULTY.store(
        er_logic::options::parse_coop_difficulty(sd),
        Ordering::Relaxed,
    );
    match (&cfg, requested) {
        (Some(c), _) => {
            // THE BAND, NOT JUST ITS FLOOR. `completion_scaling_ceiling` reached the log only
            // inside the truncated `options = {...}` blob, and it is not cosmetic: it clamps
            // every resolved tier AND supplies the denominator the entry toast prints ("tier 3
            // of 12"). Reading a seed's difficulty off this line used to require the ceiling and
            // not have it. Multipliers alongside the indices because a tier number means nothing
            // to anyone reading a player's log.
            // SAME CLAMP ORDER as `region_scaling_toast` / `tier_for_target` -- ceiling first,
            // then floor into it -- so this headline and the in-game toast can never disagree
            // about where the seed's band starts.
            let ceiling_tier = c.ceiling_tier.min(NUM_TIERS - 1);
            let floor_tier = c.floor_tier.min(ceiling_tier);
            let floor = tier_rates(floor_tier);
            let ceil = tier_rates(ceiling_tier);
            log::info!(
                "enemy-scaling: enabled ({:?}), {} region targets, max {}, band = tier {} ({:.2}x \
                 HP / {:.2}x atk) .. tier {} ({:.2}x HP / {:.2}x atk)",
                c.basis,
                c.region_targets.len() + c.region_ranges.len(),
                c.max_target,
                floor_tier,
                floor.hp,
                floor.attack,
                ceiling_tier,
                ceil.hp,
                ceil.attack,
            );
        }
        (None, true) => {
            // R6 (SWEEP H4): with an empty/missing map, arming would resolve every region to
            // floor_tier and the sweep would strip baked vanilla scaling from EVERY loaded enemy
            // (the whole game flattens). The parse returned None: feature INERT, enemies vanilla.
            log::error!(
                "completion_scaling requested but regionSphereTargets is empty -- enemy scaling left VANILLA"
            );
        }
        (None, false) => {
            // 🛑🛑 SAY IT. This arm used to be empty, and silence was carrying four meanings at
            // once: scaling off, slot_data not parsed, the feature broken, or "you are grepping
            // for the wrong string". On 2026-08-07 I read a scaling-OFF log, told Alaric to have
            // the player grep for the decline line, and that grep would have returned nothing --
            // which I would have read as evidence of an old client. A feature that is off on
            // purpose must say so, precisely BECAUSE it then emits nothing else all session.
            log::info!(
                "enemy-scaling: DISABLED for this seed (completion_scaling is off) -- every enemy \
                 keeps its vanilla scaling everywhere, and no further 'enemy-scaling:' line will \
                 appear in this log. Difficulty complaints on this seed are vanilla or another mod."
            );
        }
    }
    *CONFIG.lock().unwrap() = cfg;
    // New config = new tiers: the region-entry announcements start over. A poisoned lock only
    // ever costs announcements, never the sweep.
    if let Ok(mut ledger) = TOAST_LEDGER.lock() {
        ledger.reset();
    }
    // ...and so does the area reading. A new seed re-tiers every region, so last seed's anchor is a
    // claim about ground that no longer exists. Same degrade: a poisoned lock costs the latch only.
    if let Ok(mut anchor) = AREA_ANCHOR.lock() {
        anchor.forget();
    }
}

/// What drove the tier for a region this sweep -- captured while `CONFIG` is locked so the emit can
/// explain the applied speffect (raw sphere target, normalization ceiling, resolved tier + its HP/atk
/// rates, and whether this is a DLC region -- a bucket with a blessing floor). Diagnostic only.
/// Per-sweep census of WHAT WE FOUND, not just what we changed (#346).
///
/// Three different failures look identical in play -- (a) the entity is never swept because it lives
/// in a `ChrSet` we don't walk, (b) it is swept but carried no vanilla ladder rung so the rung we
/// apply is a buff, (c) it is swept and the tier is simply wrong -- and until now no log line told
/// them apart. `scale_one` already computed the answer to (b) and threw it away.
///
/// 🛑 The classification is `ScalingKind`, NOT "was the cleared set empty". The clear range is the
/// whole of `7000..8000`, so a `7800`-block effect makes the cleared set non-empty while the enemy
/// still has no area-scaling tier. See `er_logic::scaling::ScalingKind`.
#[derive(Default)]
struct SweepTally {
    /// (Re)applied the tier -- the number the log line has always printed.
    ///
    /// ⚠️ THIS COUNTS WRITES, NOT RESULTS. #188 proved a write to an `unloaded` chr leaves
    /// `max_hp` on the old tier's value, so this number has always over-reported what actually
    /// changed. Read it beside `scaled_unloaded`.
    scaled: u32,
    /// Of `scaled`, how many went to an `unloaded` character. Their current instance cannot
    /// recompute in place; construction derives HP from the carried rung when they load.
    scaled_unloaded: u32,
    /// Unrunged, we HAD a native tier, and the target was WEAKER: a down state was applied (#346
    /// phase 1b). 🛑 These enemies do NOT carry the region's rung -- see `ScaleAction::Down`.
    scaled_down: u32,
    /// Already carrying exactly the down state we would have applied. ⭐ This is the CONVERGENCE
    /// number: if `scaled_down` stays high and this stays 0 across repeat sweeps of one region, the
    /// down half is churning rather than settling and `settled_on_downstate` is not matching.
    settled_down: u32,
    /// Carries one of our down states, and the area index has since gone `None` so we can no longer
    /// re-derive it. 🛑 READ THIS WITH `settled_down`: before it existed these landed in
    /// `left_vanilla`, which is the exact opposite of what happened to them. First log, region 0:
    /// 23 down-scaled, then 5 settled and 18 silently counted as untouched.
    kept_down: u32,
    /// Down-scaled on the AREA's evidence rather than its own rune reward. 🛑 READ THIS FIRST WHEN A
    /// DOWN-SCALE LOOKS WRONG: since 2026-08-06 the area vouches DOWNWARD even for the named,
    /// unrewarded characters `AREA_EXCLUDED` refuses upward -- Okina, Ancient Dragon Man, Vyke. They
    /// are the population moved on their neighbours' evidence, so a bad one surfaces here first.
    area_down: u32,
    /// Distinct `npc_param_id`s of the above, capped. Deliberately NOT merged into
    /// `area_moved_ids`: up-placement and down-placement obey different rules now, and one list
    /// would hide which rule moved a given enemy.
    area_down_ids: IdSample,
    /// Carried a down state that is no longer warranted -- stripped, with nothing applied in its
    /// place. Non-zero here means a region's target moved up under enemies we had cut.
    cleared_down: u32,
    /// Swept, carried NO ladder rung: hand-tuned. The class #346 is about.
    unrunged: u32,
    /// Unrunged AND left completely untouched -- either we have no native tier for it, or the
    /// target is not stronger than the one we have.
    left_vanilla: u32,
    /// Distinct `npc_param_id`s of the above.
    ///
    /// 🛑 THIS IS THE ACTIONABLE SET AND IT HAD NO LIST FOR MONTHS. `npc_param_ids` beside it names
    /// `unrunged`, which is the SUPERSET -- most of those settle, and the ones that ship vanilla are
    /// the residue. Roundtable Hold measured `unrunged 30 ... left vanilla 11` on all 24 sweeps of
    /// bobler's 2026-08-16 session, and the eleven were unnameable from any log: they are exactly
    /// the rows that need a `NATIVE_TIERS` entry or an `AREA_TIERS` claim, and nothing could say
    /// which they were (clients#235, world#688).
    left_vanilla_ids: IdSample,
    /// Unrunged, but we HAD a native tier and the target beat it, so it scaled up.
    scaled_by_native: u32,
    /// Writes whose `max_hp` was CONFIRMED to follow the rung, this sweep.
    ///
    /// ⭐ Without this the census reported failures and nothing else, so "does the recompute ever
    /// work?" could not be answered from a log at all -- and it is the question the whole cluster
    /// turns on. Read it against `recompute_failed_loaded`: the two together are a success rate.
    recomputed: u32,
    /// Writes that exhausted their retries while LOADED -- the anomaly class (client#188/#235).
    recompute_failed_loaded: u32,
    /// Distinct `npc_param_id`s of the above, so the failing population can be NAMED rather than
    /// counted. These are the rows any fix has to be tested against.
    recompute_failed_ids: IdSample,
    /// Carried something the clear catches that is not a rung (`7210..`, `7800..`).
    other_in_range: u32,
    /// Distinct `npc_param_id`s of unrunged entities, capped -- enough to NAME the offender in one
    /// log without turning a per-sweep line into a wall of ids. 🛑 `npc_param_id`, not `npc_id`:
    /// only the former joins to `NpcParam`, and the first version of this census logged the latter,
    /// which is why it named ids like `8000` that do not exist in the table at all.
    unrunged_ids: IdSample,
    /// Carried a BAND row and NO ladder rung. The class the band-as-native-tier ruling is about.
    band_only: u32,
    /// Carried a BAND row AND a ladder rung -- these take the Replace path today, which strips the
    /// band. Where the band is the stronger of the two, that is a DOWN-scale nobody ordered.
    band_and_rung: u32,
    /// Samples of `(ladder rung id, band id)` for entities carrying BOTH -- which the census showed
    /// is essentially every scaled enemy (198 of 198, 153 of 153).
    ///
    /// 🛑 THIS IS A CALIBRATION MEASUREMENT, NOT A DEFECT HUNT. The band is area difficulty and
    /// stripping it is the feature working as designed; that is settled. What is NOT settled is
    /// whether our ladder's top end (7.422x) is strong enough, because vanilla's effective
    /// multiplier is the PRODUCT of the two (up to ~7.4 x ~3.4). This pair is how we find out.
    ///
    /// ⭐ Report BOTH ENDS of this distribution when the time comes. Quoting one end of a band is how
    /// `7400..7680` got misidentified twice.
    rung_band_pairs: Vec<(i32, i32)>,
    /// Samples of `(band-implied tier, getSoul-table tier)` for entities that have
    /// both. The band-as-native-tier ruling PREDICTS band >= table for most carriers. If this log
    /// shows the band routinely BELOW the table, the band is not a strength statement and the ruling
    /// does not survive -- so this pair is the thing to read first, not the counts.
    band_vs_table: Vec<(usize, usize)>,
    /// Ladder-index histogram of the enemies in this sweep that are still VANILLA-SHAPED, i.e.
    /// carrying a rung AND a band. The phase-1b area signal (#346): what vanilla says the GROUND is
    /// worth, for the enemies that carry no strength statement of their own.
    ///
    /// 🛑🛑 THE BAND GATE IS NOT AN OPTIMISATION, IT IS THE MEASUREMENT. We replace rungs, so from a
    /// region's second sweep onward everything runged around us carries OUR target -- a histogram
    /// without the gate would converge on the tier we already chose and report our own output back
    /// to us, stably and convincingly. We strip bands, so anything still holding one is untouched.
    ///
    /// Uncapped and NOT deduplicated, unlike `rung_band_pairs` -- a median has to weigh bodies, and
    /// that field is a set of at most 12 distinct pairs.
    area_hist: [u32; NUM_TIERS],
    /// How many ENTITIES reached the tier only because the area vouched for them.
    ///
    /// 🛑 SEPARATE FROM `area_moved_ids` ON PURPOSE, and the reason is a bug this line already
    /// caused: the log used to print `area_moved_ids.len()`, which is a DEDUPLICATED list CAPPED at
    /// `UNRUNGED_ID_CAP`. Weeping logged `area-placed 5` for a sweep that moved an unknown number of
    /// entities across 5 distinct rows -- a number that reads like a count, caps at 12, and cannot
    /// be reasoned from. A census that cannot be reasoned from is worse than no census.
    ///
    /// ⭐ The "distinct rows" half is now `IdSample::distinct()`, which counts every id it saw
    /// rather than the ones it kept, so that number is no longer capped at 12 either (clients#235).
    /// This field stays: DISTINCT ROWS and ENTITIES MOVED are still two different questions.
    area_moved: u32,
    /// Distinct `npc_param_id`s that reached the tier ONLY because the area vouched for them --
    /// capped and deduplicated, like every other census list. This one is for READING (which rows
    /// did we infer a strength for), never for counting; `area_moved` is the count.
    area_moved_ids: IdSample,
    /// Entities found carrying our target rung AND something else in the clear space.
    ///
    /// These are now CLEARED rather than skipped (see `scale_one`), so this is a CONVERGENCE
    /// counter, not a standing defect: expect it non-zero on a region's first sweep and **0 on every
    /// sweep after**. If it stays non-zero once a region has settled, something really is
    /// re-applying an effect behind us, and that is a different bug.
    residue: u32,
    /// Per-enemy SAMPLE, for the matt's-enemy-randomizer interaction (`ER_SCALING_SAMPLE=1`).
    ///
    /// ⭐ THE POINT IS THAT IT ASSUMES NOTHING ABOUT MATT'S IMPLEMENTATION. Each tuple is
    /// `(npc_param_id, npc_id, hp, max_hp, [scaling ids carried])`. Offline we already know the
    /// VANILLA base HP for any `npc_param_id` (it is a column in `NpcParam`, which is in the
    /// datamine bundle) and the HP rate of every scaling id (`SpEffectParam`). So the expected
    /// `max_hp` is `base x product(rates)`, and the RESIDUAL between that and the observed value is
    /// the whole instrument:
    ///
    ///   * residual ~= 1.0 for every sample  -> we are the only thing scaling this enemy.
    ///   * residual is a consistent constant -> something applies a uniform extra multiplier we do
    ///     not model, and the constant names it.
    ///   * residual is unrelated to base     -> the enemy's BASE STATS were edited (a baked param
    ///     change), which no runtime clear can undo and which our tier then rides on top of.
    ///
    /// `npc_id` is the second half: it is the 4-digit chr/model id, so an enemy whose model does not
    /// belong to the `npc_param_id` it is standing on is a SWAPPED enemy, which is matt's rando
    /// signing its own work.
    ///
    /// Capped, and emitted only when the census line changes -- a settled region prints nothing.
    /// ON by default (`ER_SCALING_SAMPLE=0` silences it); see `sampling` for why.
    sample: Vec<SampleRow>,
    /// Distinct non-ladder ids found inside the clear range, capped. WITHOUT this the census says
    /// "199 of 240 enemies carried something we stripped" and cannot say WHAT -- and only 20 rows in
    /// all of `NpcParam` carry a non-ladder in-range effect innately, so nearly all of those 199 are
    /// applied at RUNTIME by something we have not identified. Naming them is how that stops being a
    /// mystery (candidates: the `7800..7902` spCategory-140 block, the `7400..7680` co-op ladder).
    other_ids: IdSample,
}

/// Cap on how many ids each census list PRINTS. A census, not a dump.
///
/// 🛑 It caps the printing, not the counting. `IdSample` keeps the distinct population behind it and
/// renders `+N more of M distinct` when it withholds anything, because for one session 63 of 76
/// census lines claimed more than they printed with no marker at all, and a reader takes the list
/// for the population (clients#235).
const UNRUNGED_ID_CAP: usize = 12;
/// Cap on `SweepTally::sample`. Enough tuples to see a pattern, few enough to read.
const SAMPLE_CAP: usize = 24;

/// Gate for the per-enemy sample. **ON by default**; set `ER_SCALING_SAMPLE=0` to silence it.
///
/// ⭐ ON, not opt-in, and the reason is the launcher chain. This measurement is taken with the game
/// started THROUGH matt's randomizer, so the env would have to survive two process spawns to reach
/// us. A probe that silently no-ops because a variable did not make that journey is worse than no
/// probe: it looks like a clean result. Defaulting on removes the failure mode entirely.
///
/// The cost is bounded by construction -- capped at `SAMPLE_CAP` per sweep, and emitted only when
/// the census line itself changes, so a settled region prints nothing at all.
fn sampling() -> bool {
    !matches!(
        std::env::var("ER_SCALING_SAMPLE").as_deref(),
        Ok("0") | Ok("false")
    )
}

impl SweepTally {
    /// 🛑 The id samples must be constructed with their cap; `Default` would give them cap 0 and
    /// every list would render as `[, +N more of N distinct]` -- honest, and useless.
    fn new() -> Self {
        Self {
            unrunged_ids: IdSample::new(UNRUNGED_ID_CAP),
            recompute_failed_ids: IdSample::new(UNRUNGED_ID_CAP),
            left_vanilla_ids: IdSample::new(UNRUNGED_ID_CAP),
            area_down_ids: IdSample::new(UNRUNGED_ID_CAP),
            area_moved_ids: IdSample::new(UNRUNGED_ID_CAP),
            other_ids: IdSample::new(UNRUNGED_ID_CAP),
            ..Default::default()
        }
    }

    fn note_unrunged(&mut self, npc_param_id: i32) {
        self.unrunged += 1;
        self.unrunged_ids.note(npc_param_id);
    }

    fn note_pair(&mut self, band: usize, table: usize) {
        if self.band_vs_table.len() < UNRUNGED_ID_CAP {
            self.band_vs_table.push((band, table));
        }
    }

    fn note_rung_band(&mut self, rung: i32, band: i32) {
        if self.rung_band_pairs.len() < UNRUNGED_ID_CAP
            && !self.rung_band_pairs.contains(&(rung, band))
        {
            self.rung_band_pairs.push((rung, band));
        }
    }

    /// Record an enemy that reached the tier only because the AREA vouched for it. Deduplicated and
    /// capped like every other census list -- this one is meant to be READ, not counted.
    fn note_area_down(&mut self, npc_param_id: i32) {
        self.area_down = self.area_down.saturating_add(1);
        self.area_down_ids.note(npc_param_id);
    }

    fn note_area_moved(&mut self, npc_param_id: i32) {
        self.area_moved = self.area_moved.saturating_add(1);
        self.area_moved_ids.note(npc_param_id);
    }

    /// Count one vanilla-shaped neighbour toward the area signal. Base-game rungs only -- a DLC rung
    /// sits on a much steeper curve at the same index and would overstate the area (`ladder_tier`).
    fn note_area_sample(&mut self, rung: i32) {
        if let Some(idx) = ladder_tier(rung) {
            self.area_hist[idx] = self.area_hist[idx].saturating_add(1);
        }
    }

    fn note_sample(&mut self, chr: &ChrIns, status: &'static str, carried: &[i32]) {
        if self.sample.len() < SAMPLE_CAP {
            self.sample.push((
                instance_key(chr),
                chr.npc_param_id,
                chr.npc_id,
                chr.modules.data.hp,
                chr.modules.data.max_hp,
                status,
                carried.to_vec(),
            ));
        }
    }

    fn note_other(&mut self, param_id: i32) {
        self.other_ids.note(param_id);
    }
}

struct RegionScaleDbg {
    tier: usize,
    raw_target: Option<i32>,
    max_target: i32,
    dlc_region: bool,
    hp: f32,
    attack: f32,
}

/// Per-tick sweep (call from `update_live`, in-world). Throttled; no-op unless configured.
///
/// Returns the region-entry scaling announcement owed this tick, if any (the production caller of
/// `er_logic::scaling::region_scaling_toast`). Resolved AFTER the settle gate and the repeat
/// throttle, so it fires when the tier actually lands rather than during teardown flaps, and
/// deduped by `RegionToastLedger` -- once per distinct announcement per session. The caller
/// (core.rs) owns the toast deck; this function owns no I/O beyond the sweep itself.
pub fn tick() -> Option<String> {
    {
        let guard = CONFIG.lock().unwrap();
        if guard.is_none() {
            return None;
        }
    }
    // ORDERING IS THE FIX (2026-07-27). The throttle used to be checked HERE, before the region was
    // read -- so the region was sampled only every 30 ticks (missing any flap shorter than that),
    // the region-change branch RESTARTED the clock at the first throttled tick (discarding the
    // arrival-edge arm from core.rs), and the settle expiry was likewise only noticed on a throttled
    // tick. Three terms of pure latency on top of the 2500ms anyone actually chose. Now: observe
    // every tick, and throttle only REPEAT sweeps.
    let tick_no = TICK.fetch_add(1, Ordering::Relaxed);
    let Ok(wcm) = (unsafe { WorldChrMan::instance() }) else {
        return None;
    };
    let player = wcm.main_player.as_ref()?;
    // DEATH GUARD (2026-07-30, generalizing the guard no_fall_damage / no_equip_load have each
    // carried for the player's OWN SpEffect list -- NOT deathlink's, whose two `hp <= 0` tests are
    // "is the player dead" and "don't re-kill", different rules that must not be unified with this
    // one; see er_logic::death_guard): at the death-cam transition the
    // engine tears chr_ins + special_effect lists down, and mutating one mid-teardown is a native
    // CTD. A death in place raises no signal this sweep's gates can see -- no warp request, no
    // region change, and the in-world edge only fires AFTER teardown -- yet scale_one mutates the
    // same list type on every swept entity. hp is read exactly as DeathLink's read_local_hp reads
    // it; the predicate + timeline live in er_logic::scaling_settle (host-tested). Degrade:
    // enemies keep their current tier while the player is dead, invisible in play.
    if sweep_blocked_by_death(player.chr_ins.modules.data.hp) {
        return None;
    }
    // SCALING_WIRE: resolve in play_region/100 sub-id space -- the same bucket the
    // region-lock kick uses and the space regionSphereTargetRanges is emitted in.
    let region = (player.play_region_id / 100) as i32;
    let player_handle = player.field_ins_handle; // skip the player itself in the sweep
    let player_team = player.chr_ins.team_type; // hostiles (invader/NPC phantoms) carry a different team

    // Time-based backstop (er_logic::scaling_settle). Not the primary CTD defence any more -- that is
    // the PRIMARY crash guard once more: the chr_load_status filter that briefly replaced it
    // rejected every enemy in the game and was reverted the same day (see sweepable_characters).
    let now = now_ms();
    let (allowed, diag) = {
        let Ok(mut gate) = GATE.lock() else {
            return None;
        };
        gate.on_region(region, now, &SETTLE); // EVERY tick, before any throttle
        (gate.sweep_allowed(now, &SETTLE), gate.release_diag(now))
    };
    if !allowed {
        return None;
    }
    // Repeat-sweep throttle, measured from the LAST SWEEP rather than a modulo phase, so the first
    // allowed sweep after a transition runs on this tick instead of waiting up to 30 more frames.
    {
        let Ok(mut last) = LAST_SWEEP_TICK.lock() else {
            return None;
        };
        match *last {
            Some(t) if tick_no.wrapping_sub(t) < THROTTLE => return None,
            _ => *last = Some(tick_no),
        }
    }
    for h in &STATUS_HIST {
        h.store(0, Ordering::Relaxed);
    }

    // Resolve the tier once, and capture the inputs that drove it so the emit below can EXPLAIN the
    // number instead of just printing it. (Before: the log showed only `-> speffect NNNN`, which
    // couldn't distinguish "sphere resolved this tier" from "DLC cap clamped it" from "unmapped ->
    // floor" -- the exact ambiguity the fable consult flagged, 2026-07-15.)
    let (target, dbg, entry_toast) = {
        let guard = CONFIG.lock().unwrap();
        let cfg = guard.as_ref()?;
        // 🛑 AN UNMAPPED REGION IS NOT A DIFFICULTY STATEMENT, SO WE DO NOT MAKE ONE. This used to
        // resolve to the floor tier and sweep anyway; the 2026-08-06 log swept 198 enemies in
        // `sub 0` (nothing resolved yet, at connect) and 42 in `sub 10010` (Chapel, not in the
        // wire), applying rungs and 1b down-states in regions we could not name. `scale_action`
        // already refuses to place an ENEMY it cannot identify; this is the same refusal one level
        // up. ⭐ Enemies already carrying our state keep it -- we stop touching the region, we do
        // not undo it.
        let Some(tier) = tier_for_region(cfg, region) else {
            drop(guard);
            note_unmapped(region);
            return None;
        };
        // #993 CO-OP DIFFICULTY. Seamless co-op raises enemy HP but leaves enemy DAMAGE at the
        // host default, so a partner halves incoming threat without the enemies hitting harder.
        // Bump the applied sphere tier by `coop_difficulty` rungs per partner in the world -- a
        // higher rung carries both HP and attack, restoring the missing threat. Each client counts
        // its own census and applies this identically (every player is on its own AP slot reading
        // the same world), so no host arbitration is needed. Knob 0 (default) -> `coop_extra`
        // irrelevant -> tier unchanged. The count discriminator is provisional (see
        // the apply-site count / `COOP_PARTNER_NPC_ID`); the feature is inert until opted into.
        let coop_knob = COOP_DIFFICULTY.load(Ordering::Relaxed);
        // Count live co-op partners exactly the way the diagnostic census does (same set, same
        // field), filtered to the co-op marker so Spirit Ashes -- which share this set -- do not
        // inflate it. Walked only when the knob is on.
        let coop_extra = if coop_knob > 0 {
            sweepable_characters(&wcm.summon_buddy_chr_set)
                .filter(|c| c.npc_id == COOP_PARTNER_NPC_ID)
                .count()
        } else {
            0
        };
        let tier = er_logic::scaling::coop_tier_bump(tier, coop_extra, coop_knob, NUM_TIERS);
        if coop_knob > 0 && LAST_COOP_EXTRA.swap(coop_extra, Ordering::Relaxed) != coop_extra {
            if coop_extra > 0 {
                log::info!(
                    "enemy-scaling: co-op difficulty engaged -- {} partner(s) x +{} tier(s) each -> region tier bumped to {}",
                    coop_extra,
                    coop_knob,
                    tier
                );
            } else {
                log::info!(
                    "enemy-scaling: co-op difficulty armed but no partners in world -- vanilla region tier this sweep"
                );
            }
        }
        let rates = tier_rates(tier);
        let dbg = RegionScaleDbg {
            tier,
            // Always `Some` by the time we get here -- an unmapped region returns above.
            raw_target: raw_target_for_region(cfg, region),
            max_target: cfg.max_target,
            dlc_region: is_dlc_bucket(cfg, region),
            hp: rates.hp,
            attack: rates.attack,
        };
        // Region-entry announcement: decided while the config is in hand, deduped per-session by
        // the ledger (message-keyed -- see er_logic::scaling::RegionToastLedger). Named buckets
        // only: the baked geometry (region_locks.rs) names exactly the buckets the wire can
        // target, so hub/tutorial buckets stay silent instead of guessing.
        let entry_toast = TOAST_LEDGER
            .lock()
            .ok()
            .and_then(|mut l| l.on_region(cfg, region, region_name_for_bucket(region)));
        (speffect_id_for_tier(tier), dbg, entry_toast)
    };
    let target_tier = dbg.tier;

    let sample_on = sampling();
    let mut tally = SweepTally::new();

    // PASS ONE -- read the region before deciding anything about it. Mutates nothing; the whole
    // point is that every enemy in pass two is judged against the SAME, COMPLETE area reading.
    for chr in sweepable_characters(&wcm.open_field_chr_set.base) {
        area_sample_one(chr, &player_handle, &mut tally);
    }
    for slot in wcm.chr_sets.iter().flatten() {
        for chr in sweepable_characters(slot) {
            area_sample_one(chr, &player_handle, &mut tally);
        }
    }
    // ⭐ THE READING IS LATCHED, BECAUSE THE SAMPLE THAT PRODUCES IT IS SELF-CONSUMING. Pass one can
    // only see vanilla-shaped (rung AND band) neighbours, and pass two strips the band off every one
    // of them -- so a region answers `Some(n)` once and `None` forever after. That is fine for the
    // enemies present at the time (they leave sweep one carrying a rung or a down state, and both
    // re-derive) and fatal for anything that arrives later, which carries neither and falls to
    // `NoTouch` at full vanilla strength. A fresh reading still wins whenever there is one; the latch
    // only speaks after the sample has gone quiet. See `AreaAnchor`.
    // ⭐⭐⭐ AND THE BAKED TABLE OUTRANKS BOTH, because it saw ground neither of them can. The live
    // histogram sees only what is LOADED and has already stripped the band off everything it
    // touched; the latch is an older copy of that same partial view. `area_tiers::AREA_TIERS`
    // measured every enemy vanilla placed in the bucket, offline, so it is right on a region's
    // FIRST sweep and on a save loaded into ground that is already converged -- the two cases the
    // latch cannot reach. The census still RUNS: it is what the log reports as the live sample, and
    // it is the fallback for the 13 buckets the table makes no claim for.
    let fresh_area_tier = area_tier_from_histogram(&tally.area_hist);
    let latched_area_tier = match AREA_ANCHOR.lock() {
        Ok(mut anchor) => anchor.resolve(region, fresh_area_tier),
        // A poisoned lock costs the latch, never the sweep -- degrade to the pre-latch behaviour.
        Err(_) => fresh_area_tier,
    };
    let (area_tier, area_source) =
        resolve_area_tier(baked_area_tier(region), fresh_area_tier, latched_area_tier);

    // PASS TWO -- decide and apply. Uses the status-carrying walk so the SAMPLE line can state each
    // enemy's load state PER ROW; nothing is filtered on it (see `sweepable_characters_with_status`).
    let ctx = SweepCtx {
        target,
        target_tier,
        player_handle: &player_handle,
        sample_on,
        area_tier,
    };
    for (status, chr) in sweepable_characters_with_status(&wcm.open_field_chr_set.base) {
        scale_one(chr, status, &ctx, &mut tally);
    }
    for slot in wcm.chr_sets.iter().flatten() {
        for (status, chr) in sweepable_characters_with_status(slot) {
            scale_one(chr, status, &ctx, &mut tally);
        }
    }
    // HOSTILE-PHANTOM SWEEP -- revised 2026-07-19 (Alaric: "scaling works for mobs/bosses; NPC
    // invaders specifically don't scale"). Mobs + bosses are `ChrIns` in the open-field / block sets
    // swept above and DO scale, so an unscaled invader is a phantom entity, not a `ChrIns` in those
    // sets. The prior sweep touched only `player_chr_set` and skipped any entry whose `team_type`
    // matched the local player's -- but an NPC invader (BloodyFingerNpc / RecusantNpc) summoned into a
    // co-op session can share the host's `team_type`, so that `== player_team` skip silently excluded
    // exactly the entities it was meant to scale. Key off the unambiguous `chr_type` instead (see
    // `is_hostile_phantom`): scale the actual hostiles (player + NPC invaders, duelists), never the
    // local player, friendly white/blue phantoms, white-summon NPCs, or cosmetic ghosts. The 70xx
    // ladder is a plain stat multiplier, so it scales a `PlayerIns` phantom just like an enemy `ChrIns`.

    // Census (set, npc_id, chr_type, team) across ALL phantom-bearing sets, printed only when the
    // population CHANGES. If an invader ever appears in a set the scaling sweep below doesn't cover,
    // this names it outright (set + chr_type) so one co-op session settles where invaders live.
    let mut census: Vec<(&'static str, i32, i32, i32)> = Vec::new();
    for p in sweepable_characters(&wcm.player_chr_set) {
        let c = &p.chr_ins;
        census.push(("player", c.npc_id, c.chr_type as i32, c.team_type as i32));
    }
    for c in sweepable_characters(&wcm.ghost_chr_set) {
        census.push(("ghost", c.npc_id, c.chr_type as i32, c.team_type as i32));
    }
    for c in sweepable_characters(&wcm.summon_buddy_chr_set) {
        census.push(("summon", c.npc_id, c.chr_type as i32, c.team_type as i32));
    }
    {
        type CensusRow = (&'static str, i32, i32, i32);
        static LAST: Mutex<Option<Vec<CensusRow>>> = Mutex::new(None);
        let mut last = LAST.lock().unwrap();
        if last.as_ref() != Some(&census) {
            log::info!(
                "enemy-scaling: phantom census (set,npc_id,chr_type,team) player_team={player_team}: {:?}",
                census
            );
            *last = Some(census.clone());
        }
    }

    // Scale hostiles wherever a phantom can live (player_chr_set + summon_buddy_chr_set): keyed off
    // chr_type, so the set an invader lands in no longer matters and no friendly is ever touched.
    // (ghost_chr_set is cosmetic bloodstain/message/replay playback -- non-interactive, left alone;
    // the census still watches it in case that assumption is ever wrong.)
    for (status, p) in sweepable_characters_with_status(&wcm.player_chr_set) {
        scale_hostile_phantom(&mut p.chr_ins, status, &ctx, &mut tally);
    }
    for (status, c) in sweepable_characters_with_status(&wcm.summon_buddy_chr_set) {
        scale_hostile_phantom(c, status, &ctx, &mut tally);
    }

    // ---- SETTLE RELEASE TELEMETRY (one line per transition) ------------------------------------
    // The old guard returned early on BOTH skip paths with no log, ever -- so a window that stacked
    // to ~8s in play was invisible to everything except a human noticing enemies felt wrong. This is
    // the "tolerance requires telemetry" line, and it is what turns `settle_ms` from a feel-based
    // constant into a measured one:
    //   +Xms       how long the player actually spent unscaled after the load
    //   flaps N    how many transient play_region values fired (each cost a full 2500ms before)
    //   inactive K entries skipped as chr_load_status != Active. K > 0 HERE, after the settle
    //              window expired, is direct evidence the timer alone never sufficed -- the exact
    //              claim the old design could not check.
    if let Ok(mut logged) = RELEASE_LOGGED.lock()
        && !*logged
    {
        *logged = true;
        let hist: Vec<u32> = STATUS_HIST
            .iter()
            .map(|h| h.load(Ordering::Relaxed))
            .collect();
        log::info!(
            "enemy-scaling: settle release after {}ms (region stable {}ms, flaps {}); swept \
             {}; chr_load_status seen unloaded={} init={} active={} netinit={} ready={} \
             unloading={}",
            diag.since_transition_ms,
            diag.since_region_change_ms,
            diag.flaps,
            tally.scaled,
            hist[0],
            hist[1],
            hist[2],
            hist[3],
            hist[4],
            hist[5],
        );
    }

    // Emit on CHANGE, not on `scaled > 0`.
    //
    // Two reasons, and the first one is a trap. (1) Unrunged entities that the bottom-rung skip
    // leaves alone never converge -- they are re-examined and re-counted every sweep and NEVER
    // scaled -- so a `scaled > 0` gate would hide the census in exactly the region the census was
    // written for. (2) Gating on the whole tuple instead is strictly QUIETER than what shipped:
    // once a region settles the numbers stop moving and the line stops repeating, which the old
    // condition did not guarantee. Same idiom as the phantom census above.
    {
        // 🛑 THE DOWN COUNTERS ARE IN THE KEY, SINCE 2026-08-09. They were not, and the omission hid
        // the exact event we most needed to see: a character arriving mid-fight moves ONLY
        // `scaled_down` / `settled_down` / `kept_down` / `area_down`, so the whole phase-2 boss
        // arrival in bobler's Enir Ilim log produced no line at all. A struct rather than a tuple
        // because this is now past the 12-element ceiling `PartialEq` is implemented to.
        #[derive(PartialEq, Eq, Clone, Copy)]
        struct ScaleLine {
            region: i32,
            target: i32,
            scaled: u32,
            unrunged: u32,
            left_vanilla: u32,
            other_in_range: u32,
            scaled_by_native: u32,
            band_only: u32,
            band_and_rung: u32,
            residue: u32,
            scaled_down: u32,
            settled_down: u32,
            kept_down: u32,
            cleared_down: u32,
            area_down: u32,
        }
        static LAST: Mutex<Option<ScaleLine>> = Mutex::new(None);
        let line = ScaleLine {
            region,
            target,
            scaled: tally.scaled,
            unrunged: tally.unrunged,
            left_vanilla: tally.left_vanilla,
            other_in_range: tally.other_in_range,
            scaled_by_native: tally.scaled_by_native,
            band_only: tally.band_only,
            band_and_rung: tally.band_and_rung,
            residue: tally.residue,
            scaled_down: tally.scaled_down,
            settled_down: tally.settled_down,
            kept_down: tally.kept_down,
            cleared_down: tally.cleared_down,
            area_down: tally.area_down,
        };
        let changed = match LAST.lock() {
            Ok(mut last) => {
                let changed = *last != Some(line);
                if changed {
                    *last = Some(line);
                }
                changed
            }
            // A poisoned lock costs telemetry, never the sweep.
            Err(_) => false,
        };
        if changed && sample_on && !tally.sample.is_empty() {
            // Its own line: wide, experiment-only, and it must not make the census line unreadable
            // for the sessions that are not running the experiment.
            log::info!(
                "enemy-scaling: SAMPLE region {region} target {target} \
                 (instance_key, npc_param_id, npc_id, hp, max_hp, load_status, carried): {:?}",
                tally.sample
            );
        }
        if changed
            && (tally.scaled > 0
                || tally.unrunged > 0
                || tally.scaled_down > 0
                || tally.cleared_down > 0)
        {
            let RegionScaleDbg {
                tier,
                raw_target,
                max_target,
                dlc_region,
                hp,
                attack,
            } = dbg;
            let tgt = raw_target.map_or_else(|| "unmapped".to_string(), |t| t.to_string());
            // #346 AREA SIGNAL -- now LOAD-BEARING (it places unrunged enemies), not just reported. Read
            // `area-index` against `tier`: where it is far BELOW the tier, this region contains
            // hand-tuned enemies we currently leave at vanilla while normalising everything around
            // them, which is the shape of the sphere-2 Vyke complaint. `None` means the sweep did
            // not see enough untouched neighbours to say -- expected on a settled region's later
            // sweeps, since the gate counts only enemies still carrying rung AND band. ⭐ When that
            // happens the LATCH answers instead (`area-index ... LATCHED`), and the `from N
            // vanilla-shaped` count beside it is the live sample, so the two together still say
            // plainly whether this sweep measured the area or remembered it.
            let area_total: u32 = tally.area_hist.iter().sum();
            let area_index = area_tier;
            let area_dist: Vec<(usize, u32)> = tally
                .area_hist
                .iter()
                .enumerate()
                .filter(|&(_, &n)| n > 0)
                .map(|(i, &n)| (i, n))
                .collect();
            // client#251, ask 5: the HP-PENDING population, session-wide. The per-sweep tallies
            // beside it say what THIS sweep did; these say what has never been CONFIRMED -- the
            // "scaled in name, stale in fact" residue that vanishes from the log after its
            // one-shot verdict line. `indefinite` is #188's transient-vs-permanent split
            // (long_stale); `cap-dropped` keeps the watch's own doctrine ("forgetting is stated,
            // never silent") true at the summary level, where a player actually looks.
            let (hp_pending, hp_pending_dropped, hp_pending_indefinite) = RESCALE_WATCH
                .lock()
                .map(|w| {
                    let (outstanding, dropped) = w.outstanding();
                    (outstanding, dropped, w.long_stale(now_ms()).len())
                })
                .unwrap_or((0, 0, 0));
            // client#301: fold this sweep's write tallies into the session counters the crash
            // report appends, so the next teardown crash answers the "had scaling just written to
            // many UNLOADED chrs?" correlation itself. Cheap atomics; see shared::crash_tallies.
            shared::crash_tallies::record_scaling_sweep(
                tally.scaled,
                tally.scaled_unloaded,
                tally.recompute_failed_loaded,
            );
            log::info!(
                "enemy-scaling: region {region} -> speffect {target} \
                 (tier {tier}/{}, sphere target {tgt}/{max_target}, {hp:.2}x HP / {attack:.2}x \
                 atk{}); (re)scaled {} enemy(ies) ({} of them UNLOADED, whose max_hp recompute \
                 is deferred until they load; recompute CONFIRMED {}, FAILED-while-loaded {} {}; \
                 hp-pending {} session-wide, {} indefinite, {} cap-dropped); \
                 unrunged {} \
                 (up-scaled by native tier {}, left vanilla {} {}, npc_param_ids {}), \
                 down-scaled {} (settled {}, kept {}, cleared {}), \
                 area-down {} across {} row(s) {}; other-in-range {} {}; band-only {}, \
                 band+rung {} {:?}, band_vs_table {:?}, residue {}; area-index {:?}{} from {} \
                 vanilla-shaped {:?}; area-placed {} unrunged across {} distinct row(s) {}, \
                 still NoTouch {}",
                NUM_TIERS - 1,
                if dlc_region { ", DLC region" } else { "" },
                tally.scaled,
                tally.scaled_unloaded,
                tally.recomputed,
                tally.recompute_failed_loaded,
                tally.recompute_failed_ids.render(),
                hp_pending,
                hp_pending_indefinite,
                hp_pending_dropped,
                tally.unrunged,
                tally.scaled_by_native,
                tally.left_vanilla,
                tally.left_vanilla_ids.render(),
                tally.unrunged_ids.render(),
                tally.scaled_down,
                tally.settled_down,
                tally.kept_down,
                tally.cleared_down,
                tally.area_down,
                tally.area_down_ids.distinct(),
                tally.area_down_ids.render(),
                tally.other_in_range,
                tally.other_ids.render(),
                tally.band_only,
                tally.band_and_rung,
                tally.rung_band_pairs,
                tally.band_vs_table,
                tally.residue,
                area_index,
                match area_source {
                    // ⭐ Name the SOURCE, not just the number: "looked it up", "measured it" and
                    // "remembered it" are three different claims, and only the log can tell them
                    // apart in a live session.
                    AreaSource::Baked => " BAKED",
                    AreaSource::Live => "",
                    AreaSource::Latched => " LATCHED",
                    AreaSource::Unknown => " UNKNOWN",
                },
                area_total,
                area_dist,
                tally.area_moved,
                tally.area_moved_ids.distinct(),
                tally.area_moved_ids.render(),
                tally.left_vanilla,
            );

            // Full id-set dump (clients#235, item 2): the capped lists above answer "name one
            // offender"; they cannot answer "which 258 were left vanilla", which is the question
            // the census exists for. IdSample already holds the whole distinct population, so the
            // dump is a render, not a new collection -- gated on a probe because a line of ~340
            // ids per region is exactly what UNRUNGED_ID_CAP keeps out of the default log.
            if shared::probes::enabled("ER_SCALING_IDS_PROBE", "scaling_ids") {
                for (name, sample) in [
                    ("unrunged", &tally.unrunged_ids),
                    ("recompute-failed", &tally.recompute_failed_ids),
                    ("left-vanilla", &tally.left_vanilla_ids),
                    ("area-down", &tally.area_down_ids),
                    ("area-moved", &tally.area_moved_ids),
                    ("other-in-range", &tally.other_ids),
                ] {
                    // Nothing withheld, nothing to add: a `[]` line per empty sample per region
                    // would bury the dumps that carry the answer.
                    if sample.withheld() > 0 {
                        log::info!(
                            "enemy-scaling ids {name}: region {region} full set of {} distinct: {}",
                            sample.distinct(),
                            sample.render_full()
                        );
                    }
                }
            }
        }
    }

    entry_toast
}

/// True for chr_types that are ACTUAL hostiles worth scaling -- player invaders (BloodyFinger /
/// Recusant / FesteringBloodyFinger), NPC invaders (BloodyFingerNpc / RecusantNpc), and arena/world
/// duelists (Duelist / GrayPhantom). False for everything friendly or cosmetic: the local player,
/// white/blue phantoms, white-summon NPCs, and every ghost variant. Keyed off chr_type because the
/// old `team_type != player_team` test wrongly skipped NPC invaders that share the host's team in co-op.
fn is_hostile_phantom(t: ChrType) -> bool {
    matches!(
        t,
        ChrType::Duelist
            | ChrType::GrayPhantom
            | ChrType::BloodyFinger
            | ChrType::Recusant
            | ChrType::FesteringBloodyFinger
            | ChrType::BloodyFingerNpc
            | ChrType::RecusantNpc
    )
}

/// PASS ONE: count this enemy toward the region's area index, mutating nothing.
///
/// 🛑 THE SWEEP HAS TO BE TWO PASSES NOW, and this is why. The area index is a property of the whole
/// region, but `scale_one` decides one enemy at a time -- so the decision for the FIRST enemy needs a
/// statistic that is only complete after the LAST one. Sampling as we went would mean early enemies
/// were judged against a partial region and late ones against a fuller one, from the same sweep.
///
/// Only vanilla-shaped enemies (rung AND band) count; see `SweepTally::area_hist` for why that gate
/// is the measurement rather than an optimisation.
fn area_sample_one(
    chr: &ChrIns,
    player_handle: &eldenring::cs::FieldInsHandle,
    tally: &mut SweepTally,
) {
    if &chr.field_ins_handle == player_handle {
        return;
    }
    let mut rung: Option<i32> = None;
    let mut has_band = false;
    for e in chr.special_effect.entries() {
        let id = e.param_id;
        if !is_scaling_speffect(id) {
            continue;
        }
        match scaling_kind(id) {
            Some(ScalingKind::Ladder) => rung = Some(id),
            Some(ScalingKind::OtherInRange) if band_native_tier(id).is_some() => has_band = true,
            _ => {}
        }
    }
    // Rung AND band, or nothing: a rung on its own is an enemy we have already processed.
    if let Some(r) = rung.filter(|_| has_band) {
        tally.note_area_sample(r);
    }
}

/// Scale one phantom-set entry ONLY if it is an actual hostile (see `is_hostile_phantom`); otherwise a
/// no-op. Logs once per hostile that gets scaled (scale_one no-ops once the entry already carries the
/// tier), so a co-op session's log names exactly what landed.
fn scale_hostile_phantom(
    chr: &mut ChrIns,
    status: ChrLoadStatus,
    ctx: &SweepCtx<'_>,
    tally: &mut SweepTally,
) {
    if !is_hostile_phantom(chr.chr_type) {
        return;
    }
    let (ty, team, npc_id) = (chr.chr_type, chr.team_type, chr.npc_id);
    let before = tally.scaled;
    scale_one(chr, status, ctx, tally);
    if tally.scaled > before {
        log::info!(
            "enemy-scaling: scaled hostile phantom (chr_type={ty:?} team={team} npc_id={npc_id})"
        );
    }
}

/// Say once, per region, that we are declining to scale it -- then stay quiet.
///
/// 🛑 THE SWEEP IS THROTTLED, NOT ONE-SHOT, so an unconditional log here would emit every few frames
/// for as long as the player stands in an unwired area. Keyed on the region so a transition still
/// reports, which is the case worth seeing: at connect the region reads `0` until the game resolves
/// one, and a sweep that silently does nothing there is indistinguishable from a broken sweep.
fn note_unmapped(region: i32) {
    static LAST: Mutex<Option<i32>> = Mutex::new(None);
    let Ok(mut last) = LAST.lock() else {
        return;
    };
    if *last == Some(region) {
        return;
    }
    *last = Some(region);
    log::info!(
        "enemy-scaling: region {region} is not in the sphere wire -- left VANILLA (no tier, no \
         down-state). Expected in Roundtable, in the tutorial/Chapel, and transiently at connect \
         before the game resolves a region. NOT expected anywhere you can fight: every region's \
         buckets are wired, and a sealed region ejects rather than letting you walk it."
    );
}

/// Ensure one enemy carries exactly `target` as its scaling SpEffect: skip if it already has it, else
/// clear any baked/stale scaling SpEffect and apply `target` -- and record what we found on the way
/// through (`SweepTally`).
///
/// 🛑 THE CLASSIFY-THEN-SKIP ORDER IS LOAD-BEARING. The bottom-rung skip returns BEFORE the remove
/// loop. Stripping an enemy's vanilla state and then declining to replace it would be worse than
/// either doing the whole thing or none of it -- and, because the sweep re-derives everything from
/// what the enemy currently carries, it would also be irreversible: the evidence of what the enemy
/// natively was is gone, so the next pass sees an unrunged enemy that genuinely has no rung.
fn scale_one(chr: &mut ChrIns, status: ChrLoadStatus, ctx: &SweepCtx<'_>, tally: &mut SweepTally) {
    let SweepCtx {
        target,
        target_tier,
        player_handle,
        sample_on,
        area_tier,
    } = *ctx;
    if &chr.field_ins_handle == player_handle {
        return; // never scale the player
    }
    // Collect first (entries() borrows immutably) then remove (borrows mutably). Collected BEFORE
    // the already-on-target check so that path can be measured -- see `SweepTally::residue`.
    let carried: Vec<i32> = chr
        .special_effect
        .entries()
        .map(|e| e.param_id)
        // 🛑 WIDER THAN THE CLEAR USED TO BE. `is_scaling_speffect` stops at `7000..8000` plus the
        // DLC range, and the down-state rows live in the DLC ally-tuning block outside BOTH -- so
        // before 1b armed, a state we applied was invisible to the very sweep that applied it and
        // would have stranded across reconnects and seed changes. An ALLOWLIST, never a widened
        // range: widening would strip legitimate effects off DLC summons.
        .filter(|&id| is_scaling_speffect_with_downstates(id))
        .collect();
    if sample_on {
        // BEFORE any early return: a settled enemy is still a data point, and once a region
        // converges the settled ones are the majority. Sampling only the mutated ones would show us
        // the population we changed rather than the population that is there.
        tally.note_sample(chr, status_label(status), &carried);
    }
    // ⭐ THE OTHER HALF OF THE WRITE (client#188/#186). Every character the sweep walks is re-read
    // here, so this is where a rung applied on an earlier tick finally reports whether `max_hp`
    // followed. Unconditional on `sample_on`: the verdict is the measurement four issues are
    // waiting on, and it is at most one line per write.
    //
    // 🛑 `Unloaded` is the only status treated as not-loaded. #188's pair contrasts `ready` with
    // `unloaded`, and `ready` recomputed -- so anything that is not explicitly unloaded counts as
    // loaded here, which is what makes a StaleLoaded verdict the anomaly rather than a definition.
    // Bobler's 2026-08-17 playtest closed both branches: loading rebuilds HP from a rung written
    // while unloaded, while three remove/re-apply cycles on `ready` entities changed nothing.
    // Observe the result; do not churn the same write as though it were a recompute primitive.
    match RESCALE_WATCH.lock() {
        Err(_) => {}
        Ok(mut w) => match w.poll(
            instance_key(chr),
            chr.modules.data.max_hp,
            !matches!(status, ChrLoadStatus::Unloaded),
            now_ms(),
        ) {
            er_logic::rescale_watch::Action::Wait => {}
            er_logic::rescale_watch::Action::Report(v) => {
                // 🛑 COUNT THE SUCCESSES. A `Recomputed` verdict used to be dropped on the floor --
                // not logged, not tallied -- so a session could show 13,487 failure lines and
                // NOTHING about whether the mechanism works at all. I read one that way and
                // concluded the recompute never succeeds; that was an absence in the LOG, not in
                // the world (this module's own controlled pair recorded 7/7 loaded writes
                // recomputing). A success rate is the first number client#235's item 2 needs, and
                // it did not exist.
                //
                // COUNTED, not logged: one line per confirmed recompute is thousands a session, and
                // the census already carries the per-sweep numbers beside it.
                if matches!(v, er_logic::rescale_watch::Verdict::Recomputed { .. }) {
                    tally.recomputed = tally.recomputed.saturating_add(1);
                } else {
                    let line = er_logic::rescale_watch::verdict_line(
                        instance_key(chr),
                        chr.npc_param_id,
                        chr.modules.data.max_hp,
                        status_label(status),
                        v,
                    );
                    if v.is_anomaly() {
                        tally.recompute_failed_loaded =
                            tally.recompute_failed_loaded.saturating_add(1);
                        tally.recompute_failed_ids.note(chr.npc_param_id);
                        log::warn!("{line}");
                    } else {
                        log::info!("{line}");
                    }
                }
            }
        },
    }
    if settled_on_target(&carried, target) {
        return; // carrying the tier and NOTHING else -- the only state worth leaving alone
    }
    // 🛑 CARRYING THE TARGET IS NOT THE SAME AS BEING DONE. This used to return on
    // `carried.contains(&target)`, before the clear -- so an enemy holding the target rung PLUS
    // anything else in the clear space kept the extra forever, because every later sweep took the
    // same short-circuit. Invisible at floor 0 (target `7010`, which almost nothing carries
    // natively); the 2026-08-05 floor-25 smoke test made the target `7060`, exactly Liurnia's native
    // rung, and 306 enemies in one sweep kept their band at 4.18x while every peer on the same tier
    // sat at 2.266x. Such an enemy now falls through and is cleared like any other.
    if carried.contains(&target) {
        tally.residue += 1;
        for &id in &carried {
            if id != target {
                tally.note_other(id);
            }
        }
    }
    let stale = carried;

    // CLASSIFY. `!stale.is_empty()` is NOT "this enemy had a vanilla tier" -- the clear range is the
    // whole 7000..8000 block and only some of it is the ladder (er_logic::scaling::ScalingKind).
    let mut carried_ladder_rung = false;
    let mut carried_downstate = false;
    let mut carried_other = false;
    let mut band_tier: Option<usize> = None;
    let mut rung_id: Option<i32> = None;
    let mut band_id: Option<i32> = None;
    for &id in &stale {
        match scaling_kind(id) {
            Some(ScalingKind::Ladder) => {
                carried_ladder_rung = true;
                rung_id = Some(id);
            }
            // OURS, from a previous sweep. Not vanilla's statement about the enemy, so it must not
            // reach `other_in_range` (that census is about what VANILLA baked on). ⭐ It is also a
            // FACT THE DECISION NEEDS: on a converged region the area index is `None` by design, so
            // this flag is the only surviving evidence that we ever placed this enemy.
            Some(ScalingKind::Downstate) => carried_downstate = true,
            Some(ScalingKind::OtherInRange) => {
                carried_other = true;
                tally.note_other(id);
                // Keep the STRONGEST band row if an entity somehow carries several.
                if let Some(t) = band_native_tier(id) {
                    if band_tier.is_none_or(|cur: usize| t > cur) {
                        band_id = Some(id);
                    }
                    band_tier = Some(band_tier.map_or(t, |cur: usize| cur.max(t)));
                }
            }
            None => {}
        }
    }
    if carried_other {
        tally.other_in_range += 1;
    }
    if let Some(band) = band_tier {
        if carried_ladder_rung {
            tally.band_and_rung += 1;
            if let (Some(r), Some(b)) = (rung_id, band_id) {
                tally.note_rung_band(r, b);
            }
        } else {
            tally.band_only += 1;
        }
        if let Some(table) = native_tier(chr.npc_param_id) {
            tally.note_pair(band, table);
        }
    }
    if !carried_ladder_rung {
        tally.note_unrunged(chr.npc_param_id);
    }

    // ...THEN decide, before anything is mutated.
    //
    // 🛑 `NoTouch` is the DEFAULT for anything we cannot place, and it is the whole point. What
    // shipped resolved an unplaceable enemy to the region's rung, which on a hand-tuned endgame NPC
    // in a shallow sphere is a multiplier stacked on stats that already assume you meet it at the
    // end of the game. Leaving it vanilla is under-scaled in deep spheres -- a blemish -- and that
    // trade is deliberate.
    match scale_action(
        carried_ladder_rung,
        carried_downstate,
        chr.npc_param_id,
        target_tier,
        area_tier,
    ) {
        ScaleAction::NoTouch => {
            tally.left_vanilla += 1;
            // NAME it. `scale_action` returned NoTouch, so this enemy carries no rung, has no
            // native tier we will claim, and the area did not vouch for it either -- it ships at
            // full vanilla strength in a region we have scaled around it. That is the #346
            // population, and until now the census could count it but not identify it.
            tally.left_vanilla_ids.note(chr.npc_param_id);
            return;
        }
        ScaleAction::Apply => {
            tally.scaled_by_native += 1;
            // 🛑 NAME what only the AREA vouched for. These are the enemies whose strength we
            // attributed from their neighbours rather than measuring on them, so this list is where
            // a wrongly-buffed hand-tuned entity shows up FIRST -- in our log, not in a report.
            if placed_by_area(chr.npc_param_id, area_tier) {
                tally.note_area_moved(chr.npc_param_id);
            }
        }
        ScaleAction::KeepDown => {
            // Behaviourally identical to NoTouch -- and that is exactly why it needed its own arm.
            // It was indistinguishable in the census, so 18 correctly-down-scaled enemies reported
            // as "left vanilla" in the first live log.
            tally.kept_down += 1;
            return;
        }
        ScaleAction::ClearDown => {
            // 🛑 STRIPS WITHOUT REPLACING, which every other path in this file is forbidden to do.
            // The prohibition protects VANILLA state, whose loss is irreversible because the sweep
            // re-derives from what the enemy carries. A down state is additive and ours: removing it
            // restores exactly what vanilla shipped, which is the correct answer here.
            for &id in &stale {
                chr.remove_speffect(id);
            }
            tally.cleared_down += 1;
            return;
        }
        ScaleAction::Down(state) => {
            // 🛑 SET-EQUAL, THEN RETURN WITHOUT MUTATING. A down state that re-applied every sweep
            // would never converge, and the census would report it as freshly "scaled" on every
            // pass -- which is exactly how the `residue 306` bug read before `settled_on_target`.
            if settled_on_downstate(&stale, state) {
                tally.settled_down += 1;
                return;
            }
            for &id in &stale {
                chr.remove_speffect(id);
            }
            for &id in state.ids {
                chr.apply_speffect(id, false);
            }
            tally.scaled_down += 1;
            // 🛑 `_down`, NOT `placed_by_area`. The up census refuses the named/unrewarded rows by
            // design; using it here would silently omit exactly the enemies this path was widened
            // to reach, and the log would read as though nothing new was happening.
            if placed_by_area_down(chr.npc_param_id, area_tier) {
                tally.note_area_down(chr.npc_param_id);
            }
            // 🛑🛑 NO `apply_speffect(target)` ON THIS PATH, AND THAT IS THE POINT. This enemy
            // carries no rung, so its base ALREADY encodes its native tier; putting the region's
            // rung on top would multiply rather than replace -- the v0.3.4 one-shotting shape, in
            // the direction we are trying to fix. The down state is the RATIO between the two
            // tiers, so the base plus the state IS the target.
            return;
        }
        ScaleAction::Replace => {}
    }

    for id in stale {
        chr.remove_speffect(id);
    }
    // Read BEFORE the write: the watch compares against the value the rung is meant to move.
    let max_hp_before = chr.modules.data.max_hp;
    chr.apply_speffect(target, false);
    tally.scaled += 1;
    // 🛑 THE WRITE IS NOT THE RESULT (client#188). An `unloaded` chr accepts the speffect and keeps
    // the old tier's `max_hp`; 7/7 loaded followed the rung in #188's pair and 0/6 unloaded did. So
    // the count is split, and the write is remembered so the next sample can say which happened.
    if matches!(status, ChrLoadStatus::Unloaded) {
        tally.scaled_unloaded += 1;
    }
    if let Ok(mut w) = RESCALE_WATCH.lock() {
        w.note_applied(instance_key(chr), chr.npc_param_id, max_hp_before, now_ms());
    }
}

/// A stable per-INSTANCE key. 🛑 `npc_param_id` is a ROW, not an instance -- #186 spent two issues
/// on ten sightings that may or may not have been one character. `FieldInsHandle` is the identity
/// the engine itself uses, and it is what `scale_one` already compares against for the player.
///
/// Hashed rather than bit-packed: `FieldInsSelector` and `BlockId` are `bitfield!` tuple structs
/// whose inner field is PRIVATE, so `.0` is not reachable from here -- but `FieldInsHandle` derives
/// `Hash`, which is a public, total function of exactly those bits. Collision across the watch's
/// 512-entry cap is not a real risk, and a collision would cost one wrong verdict line, not a wrong
/// write: nothing acts on this key.
pub(crate) fn instance_key(chr: &ChrIns) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    chr.field_ins_handle.hash(&mut h);
    h.finish()
}
