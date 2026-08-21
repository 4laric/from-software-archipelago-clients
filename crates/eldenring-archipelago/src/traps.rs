//! traps -- fire a trap effect on demand, so a playtester can tell us how it FEELS before we build
//! the item pipeline around it.
//!
//! ## Why this is a probe and not an item yet
//!
//! Traps as AP items need a new item class, a `trap_percent` budget, an `OptionSet` of trap names
//! and new slot_data keys -- a contract move, both repos in lockstep. None of that tells us whether
//! halving somebody's runes mid-run is funny or infuriating, or whether a flask that heals nothing
//! reads as a trap or as a bug. Those are questions for a person holding a controller, and the
//! cheapest way to ask them is the same one this repo already uses for every other unknown: a
//! probe, off by default, gated in `apconfig.json`, that a playtester can turn on in one line.
//!
//! Nothing here touches the pool, the contract, or slot data. `CONTRACT_HASH` does not move.
//!
//! ## Using it
//!
//! ```json
//! { "url": "...", "slot": "...", "probes": { "traps": true } }
//! ```
//!
//! Then in-world: **F7** fires Rune Thief, **F8** fires No Flask. The active-probe line in the log
//! states whether it is on, so "I set it and nothing happened" is answerable from the log.
//!
//! 🛑 **RUNE THIEF REALLY TAKES YOUR RUNES.** It is the trap, not a simulation of it. That is why
//! the whole module is off unless somebody deliberately turned it on, and why every fire is logged
//! with the before and after totals.
//!
//! ## The two effects
//!
//! * **Rune Thief** -- `runes::write(runes::read()? / 2, ...)`. 🛑 It goes through `runes.rs` and
//!   only through it: that module documents a single-writer discipline, and a second private rune
//!   write anywhere else re-opens the hole it closes.
//! * **No Flask** -- patch our claimed row (`safe_speffect_rows::TRAP_NO_FLASK`) so both flask
//!   correct-rates read 0 and `effectEndurance` is the trap's duration, then apply it to the player.
//!   The DURATION IS THE PARAM FIELD: the game expires the row itself, so there is no timer here, no
//!   tick loop, and nothing to leak if the player quits mid-trap.
//!   🛑 The flask heals NOTHING; it is not undrinkable, and the charge is still spent. The toast
//!   says so.
//!
//! The row is re-patched on every fire rather than once behind a latch: a map load streams
//! `SpEffectParam` back in and restores the vanilla values, which is exactly the bug `no_equip_load`
//! carries a `reset()` for. Patch-then-apply costs one row write per keypress and cannot go stale.

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use eldenring::cs::{
    CSFade, ChrDebugSpawnRequest, ChrInsExt, SoloParamRepository, SpEffectParam, WorldChrMan,
};
use er_logic::safe_speffect_rows::TRAP_NO_FLASK;
use er_logic::traps::{
    NO_FLASK_CORRECT_RATE, NO_FLASK_SECONDS, RUNEBEAR_SPAWN, RuneThiefAction, SpawnSpec, Trap,
    classify_refusal, rune_thief_action,
};
use fromsoftware_shared::FromStatic;

/// The traps waiting on the player being able to receive one.
///
/// A trap arriving as an ITEM cannot be dropped when the player is in a menu: the item is already
/// marked received and the server will never resend it. `TrapQueue` holds it; `poll_pending` below
/// is called from the same tick the client already runs. (A trap fired from the F7/F8 probe skips
/// the queue -- a keypress is definitionally a moment the player is in control.)
static PENDING: Mutex<Option<er_logic::traps::TrapQueue>> = Mutex::new(None);
/// One warn per overdue head, not one per tick.
static WARNED: AtomicBool = AtomicBool::new(false);
static TRAP_LINK_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn set_trap_link_enabled(enabled: bool) {
    TRAP_LINK_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn trap_link_enabled() -> bool {
    TRAP_LINK_ENABLED.load(Ordering::Relaxed)
}

/// Restore deadline for a real Blackout item. `poll_pending` drives it every overlay frame even
/// when no trap remains queued, so consuming the item can never strand the screen dark.
static BLACKOUT: Mutex<er_logic::trap_probe::Deadline> =
    Mutex::new(er_logic::trap_probe::Deadline::new());

/// Monotonic base for the burst's own timing (world#689).
///
/// 🛑 DELIBERATELY NOT `poll_pending`'s `now_ms`. That one is the UI toast clock, handed in by
/// core.rs, and `fire` is also reachable from the F7/F8 keypress path which has no such value. The
/// burst compares its own timestamps only against each other, so it needs ONE clock it always has
/// -- the same `OnceLock<Instant>` shape `boss_fight_probe` and `detour` already use.
static CLOCK: OnceLock<Instant> = OnceLock::new();

fn now_ms() -> u64 {
    CLOCK.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Live characters standing on `npc_param_id`, over the same two sets the scaling sweep walks
/// (world#689).
///
/// 🛑 `npc_param_id`, NOT `chr_id`. The model id (`c4150`) is shared by every row built on that
/// model; the row is what the trap actually asked the creator for, and it is what `find_boss` keys
/// on for the same reason. Keying the witness on the model would count a different creature as
/// ours.
///
/// Read-only, and no new traversal: `sweepable_characters` is the walk `boss_fight_probe::find_boss`
/// and the scaling sweep already share.
fn count_live_npc_param(wcm: &WorldChrMan, npc_param_id: i32) -> u32 {
    let mut n: u32 = 0;
    for chr in crate::scaling::sweepable_characters(&wcm.open_field_chr_set.base) {
        if chr.npc_param_id == npc_param_id {
            n = n.saturating_add(1);
        }
    }
    for slot in wcm.chr_sets.iter().flatten() {
        for chr in crate::scaling::sweepable_characters(slot) {
            if chr.npc_param_id == npc_param_id {
                n = n.saturating_add(1);
            }
        }
    }
    n
}

/// The remainder of a `count > 1` spawn, issued one request per tick (client#206).
///
/// 🛑 SEPARATE FROM [`PENDING`], on purpose. `TrapQueue` holds traps that have not fired yet and
/// whose delivery may be REFUSED and retried; a burst is a trap that HAS fired and is still
/// arriving. Folding a half-delivered spawn back into the queue would make it eligible for the
/// overdue warning and for re-firing, which is a different and worse bug than the one being fixed.
static SPAWN_BURST: Mutex<Option<er_logic::spawn_burst::SpawnBurst>> = Mutex::new(None);

/// Queue a trap that arrived as an AP item. Called from the receive loop, which knows the NAME.
///
/// A name this build cannot read is logged and dropped ON PURPOSE: firing the wrong effect is
/// worse than firing none, and the ids in a spawn name go straight to the game's debug creator.
///
/// 🛑 THE WORDS COME FROM er-logic, and that is the fix rather than a tidy-up. This line used to
/// say "A newer world minted it; update the client rather than guessing which effect was meant."
/// On 2026-08-12 a live seed sent `Trap: c2120 (2120/21200000/21200000 x1)` -- the OLD three-field
/// payload, from a world that had NOT taken the change this client had -- and that sentence sent
/// the maintainer into the wrong repository. `classify_refusal` establishes the direction before
/// naming one, and holds the sentence where it can be host-tested.
pub fn enqueue_by_item_name(name: &str, now_ms: u64) -> bool {
    let Some(trap) = Trap::from_item_name(name) else {
        // ONE line, and the diagnosis is a value rather than a literal: the client keeps no copy
        // of the words, so there is one place to fix when they are wrong again.
        log::warn!(
            "trap item {name:?} is not one this client can fire -- ignored. {}",
            classify_refusal(name).advice()
        );
        return false;
    };
    let Ok(mut guard) = PENDING.lock() else {
        return false;
    };
    guard
        .get_or_insert_with(er_logic::traps::TrapQueue::new)
        .push(trap, now_ms);
    log::info!("trap {} queued from item {name:?}", trap.key());
    true
}

/// Queue an inbound TrapLink using the exact same public item-name vocabulary.
/// Unknown foreign names fail closed and inbound provenance never reaches the outbound send path.
pub fn enqueue_by_link_name(name: &str, source: &str, now_ms: u64) -> bool {
    let Some(trap) = Trap::from_item_name(name) else {
        log::warn!("TrapLink: unknown foreign trap {name:?} from {source:?} -- ignored");
        return false;
    };
    let Ok(mut guard) = PENDING.lock() else {
        return false;
    };
    guard
        .get_or_insert_with(er_logic::traps::TrapQueue::new)
        .push(trap, now_ms);
    log::info!(
        "TrapLink: {} queued from {source:?} as {name:?}",
        trap.key()
    );
    true
}

/// Per-tick: deliver at most one queued trap, once the player can take it.
///
/// Returns the line to toast, or `None`. `can_fire` is formed HERE (in world) and refined inside
/// `fire` (death guard, param streamed) -- a trap that cannot land this tick goes back to waiting
/// rather than being consumed, which is the whole point of the queue.
pub fn poll_pending(now_ms: u64) -> Option<Cow<'static, str>> {
    tick_blackout();
    // 🛑 THE BURST GOES FIRST, and it returns. A spawn that is still arriving owns this tick's
    // request slot; letting a queued trap fire underneath it would put a second write on the same
    // `init_data` before the creator drained the first -- which is the exact collapse #206 is
    // about, re-created between two traps instead of within one.
    if drive_spawn_burst() {
        return None;
    }
    let Ok(mut guard) = PENDING.lock() else {
        return None;
    };
    let q = guard.as_mut()?;
    if q.is_empty() {
        WARNED.store(false, Ordering::Relaxed);
        return None;
    }
    if q.overdue(now_ms) && !WARNED.swap(true, Ordering::Relaxed) {
        // Reported, never dropped: the alternative to holding is losing the item outright.
        log::warn!(
            "trap delivery has been deferred for over {}s ({} waiting) -- the player has not been \
             in a state to receive one. Still held.",
            er_logic::traps::DEFER_WARN_MS / 1000,
            q.len()
        );
    }
    let trap = q.poll(now_ms, crate::flags::in_world())?;
    match fire(trap) {
        Some(line) => {
            WARNED.store(false, Ordering::Relaxed);
            Some(line)
        }
        None => {
            // `fire` refused (mid-death, param not streamed). Put it BACK -- consuming it here is
            // exactly the silent loss the queue exists to prevent.
            q.push(trap, now_ms);
            None
        }
    }
}

/// Is the trap probe on? Environment wins over `apconfig.json`, per `shared::probes`.
pub fn enabled() -> bool {
    shared::probes::enabled("ER_TRAP_PROBE", "traps")
}

/// Fire `trap`. Returns the line to toast, or `None` when the effect could not be applied this
/// tick (not in world, player mid-death, param not streamed in yet).
///
/// 🛑 Individually fallible and swallowed, by design: issue #114 rule 1, and the reason is F4 --
/// one bad item must not drop the whole batch. A trap that cannot fire says so and is skipped; it
/// never propagates an error into the receive path it will eventually live in.
///
/// `Cow` rather than `&'static str` because a parameterised spawn's line is minted from the ids in
/// its item name -- there is no static to borrow. Every caller already `to_string`s it.
pub fn fire(trap: Trap) -> Option<Cow<'static, str>> {
    if !crate::flags::in_world() {
        log::info!("trap {}: not in world -- skipped", trap.key());
        return None;
    }
    let ok = match trap {
        Trap::RuneThief => fire_rune_thief(),
        Trap::NoFlask => fire_no_flask(),
        Trap::Blackout => fire_blackout(),
        // The legacy variant is a `SpawnSpec` in everything but its name, so it goes down the same
        // path -- one spawn implementation, not two that can drift.
        Trap::Runebear => fire_spawn(RUNEBEAR_SPAWN),
        Trap::Spawn(spec) => fire_spawn(spec),
    };
    ok.then(|| trap.toast())
}

fn fire_blackout() -> bool {
    if !fade_blackout(true) {
        log::info!("trap blackout: CSFade unavailable -- deferred");
        return false;
    }
    if let Ok(mut deadline) = BLACKOUT.lock() {
        deadline.arm(now_ms(), er_logic::trap_probe::BLACKOUT_MS);
    }
    log::info!(
        "trap blackout: faded out over {}s on all 9 plates; restore armed",
        er_logic::trap_probe::BLACKOUT_FADE_SECONDS
    );
    true
}

fn tick_blackout() {
    let Ok(mut deadline) = BLACKOUT.lock() else {
        return;
    };
    if deadline.take_if_elapsed(now_ms()) {
        if fade_blackout(false) {
            log::info!("trap blackout: faded back in");
        } else {
            log::warn!(
                "trap blackout: could not reach CSFade to fade back in -- a load or grace rest \
                 will clear it"
            );
        }
    }
}

/// Fade all nine plates, matching the live-confirmed probe. Restoring the same complete set makes
/// the broad write symmetric even if only one plate composites over ordinary play.
pub(crate) fn fade_blackout(out: bool) -> bool {
    // SAFETY: FD4 singleton, mutated only from the overlay frame hook.
    let Ok(fade_ctl) = (unsafe { CSFade::instance_mut() }) else {
        return false;
    };
    for plate in fade_ctl.fade_plates.iter_mut() {
        if out {
            plate.fade_out(er_logic::trap_probe::BLACKOUT_FADE_SECONDS);
        } else {
            plate.fade_in(er_logic::trap_probe::BLACKOUT_FADE_SECONDS);
        }
    }
    true
}

fn fire_rune_thief() -> bool {
    let Some(before) = crate::runes::read() else {
        log::warn!("trap rune_thief: rune count unreadable -- skipped");
        return false;
    };
    // ⭐ DECIDE FROM THE COUNT, rather than writing first and calling a no-op a success
    // (client#139). boblerrr fired this holding ZERO runes: it took nothing, toasted "half your
    // runes are gone", and CONSUMED the item -- already marked received, never resent.
    let after = match rune_thief_action(before) {
        RuneThiefAction::Take(after) => after,
        RuneThiefAction::Defer => {
            // 🛑 REFUSING IS WHAT PUTS IT BACK. `poll_pending` re-pushes on `None` -- the same path
            // mid-death and param-not-streamed already use -- so the trap lands when it can
            // actually bite. `DEFER_WARN_MS` covers the "never holds runes" player with a warn
            // rather than a drop, so this adds no way to lose the item.
            //
            // Deliberately silent to the player: a trap that announces itself in advance is not a
            // trap. The log line is for us.
            log::info!(
                "trap rune_thief: 0 held -- deferred, not spent (client#139). It re-queues and \
                 fires when the player has runes to lose"
            );
            return false;
        }
    };
    // Through `runes.rs`, never a private write: the module owns the single-writer discipline.
    if crate::runes::write(after, "trap: rune thief") {
        log::info!("trap rune_thief: {before} -> {after}");
        true
    } else {
        log::warn!("trap rune_thief: write refused ({before} -> {after})");
        false
    }
}

/// Put `spec.count` of a creature where the player is standing.
///
/// `WorldChrMan::spawn_debug_character` is typed and takes exactly the four ids plus a position.
/// The ids are not this crate's to invent: they arrive in the ITEM NAME and `SpawnSpec` has already
/// refused anything whose npc/think rows are outside the model's family (the failure that would
/// otherwise give one creature's body another's brain). For the Runebear they are DERIVED (see
/// `er_logic::traps` and the NpcName decode above them) rather than recalled -- the recollection I
/// started from was wrong by 330 model numbers.
///
/// 🛑 SPAWNED AT THE PLAYER'S OWN POSITION, on purpose, for three reasons:
///   1. it is the ask -- bobler's line was "enemy horde on your head";
///   2. it is the only point we KNOW is valid ground, because the player is standing on it. Any
///      offset can put a bear in a wall, off a cliff, or through a floor;
///   3. it needs no orientation maths. "In front of you" means rotating a forward vector by
///      `CSChrPhysicsModule::orientation` (a quaternion) -- a second thing to get wrong, for a joke
///      that lands better at zero range.
///
/// 🛑 NO DEATH GUARD, and that is deliberate rather than an omission: this reads
/// `modules.physics.position` and never touches `special_effect`, which is the list that CTDs at
/// the death cam. `in_world` in `fire` is its only precondition.
///
/// All `count` requests go to that ONE identical point, also on purpose: the debug creator resolves
/// the overlap itself, and any per-creature offset would re-introduce exactly the wall/cliff/floor
/// risk reason 2 above exists to avoid -- multiplied by the count.
///
/// 🛑 ONE REQUEST PER TICK, AND THAT IS THE WHOLE OF client#206. `spawn_debug_character` writes a
/// SINGLE shared `init_data` slot and raises one `spawn` flag; `CSDebugChrCreator` drains it on the
/// next frame. A `for _ in 0..count` loop therefore overwrote the same slot `count` times before
/// the creator ran once, and `traps: [basilisk]` put ONE basilisk on the ground. The remainder now
/// rides in [`SPAWN_BURST`] and is issued by [`poll_pending`], one per tick, which is the machine
/// this module already had. See `er_logic::spawn_burst` for the binding source that rules out the
/// per-copy `event_entity_id` fix: three distinct ids are still three writes to one slot.
fn fire_spawn(spec: SpawnSpec) -> bool {
    // SAFETY: FD4 singleton, mutated only on the single-threaded tick -- the same contract every
    // other player write in this crate relies on.
    let Ok(wcm) = (unsafe { WorldChrMan::instance_mut() }) else {
        return false;
    };
    let Some(player) = wcm.main_player.as_ref() else {
        return false;
    };
    let pos = player.chr_ins.modules.physics.position;
    let request = ChrDebugSpawnRequest {
        chr_id: spec.chr_id,
        chara_init_param_id: SpawnSpec::CHARA_INIT_PARAM_ID,
        npc_param_id: spec.npc_param_id,
        npc_think_param_id: spec.think_param_id,
        // Not an EMEVD entity and nothing to talk to: these exist for the joke, so they carry no
        // event id a script could key on and no talk id. One request value, reused: the fields are
        // identical for every copy, including the position.
        event_entity_id: 0,
        talk_id: 0,
        pos_x: pos.0,
        pos_y: pos.1,
        pos_z: pos.2,
    };
    // ⭐ COUNTED BEFORE ANYTHING IS ISSUED (world#689). The trap's `npc_param_id` is a curated
    // row, not a private one, and this game puts basilisks near basilisks -- so the witness below
    // reports what the trap ADDED, never what happens to be standing.
    let baseline = count_live_npc_param(wcm, spec.npc_param_id);
    let now = now_ms();
    let mut burst = er_logic::spawn_burst::SpawnBurst::new(spec, baseline, now);
    // This tick's copy. The creator drains the slot before the next one is written.
    if burst.next_request(now).is_some() {
        wcm.spawn_debug_character(&request);
    }
    log::info!(
        "trap spawn: c{} x{} opening -- npc={} think={} chara_init={} at ({}, {}, {}), {} already \
         live on this row. One request per tick; what actually appeared lands on the burst report \
         (client#206, world#689)",
        spec.chr_id,
        spec.count,
        spec.npc_param_id,
        spec.think_param_id,
        SpawnSpec::CHARA_INIT_PARAM_ID,
        pos.0,
        pos.1,
        pos.2,
        baseline
    );
    set_spawn_burst(burst);
    // 🛑 They are REQUESTS: the debug creator spawns on its own schedule, so `true` here means
    // "asked", not "standing there". The player finds out within a second either way.
    true
}

fn set_spawn_burst(burst: er_logic::spawn_burst::SpawnBurst) {
    if let Ok(mut slot) = SPAWN_BURST.lock() {
        // An in-flight burst is REPLACED, not queued behind. Two spawn traps inside three frames
        // is already a horde; holding the first one's tail would deliver it into the second's
        // arena minutes later, somewhere the player is no longer standing.
        //
        // 🛑 A SPENT BURST IS STILL KEPT (world#689). It used to be dropped the moment it had
        // nothing left to issue, which is correct for issuing and wrong for WITNESSING: a `x1`
        // spawn is spent after its first request and is exactly the case nobody has ever verified.
        // `drive_spawn_burst` closes it after the settle window instead.
        *slot = Some(burst);
    }
}

/// Issue at most one queued spawn request this tick. `true` = this tick belonged to the burst.
///
/// 🛑 NO DEATH GUARD, for the same reason [`fire_spawn`] has none: this reads
/// `modules.physics.position` and never touches `special_effect`. `in_world` is the precondition,
/// and a burst that meets a load screen simply stops issuing -- the copies already standing are
/// the joke, and re-opening the burst on the far side would drop creatures into a new map.
fn drive_spawn_burst() -> bool {
    let Ok(mut slot) = SPAWN_BURST.lock() else {
        return false;
    };
    // Copied out, not borrowed: closing the burst below writes through `slot`, and `SpawnBurst` is
    // `Copy` precisely so this costs nothing.
    let Some(mut burst) = *slot else {
        return false;
    };
    let now = now_ms();

    // A burst that meets a load screen stops issuing, and cannot be verified: the creatures it did
    // place are in a map the player has left, so no count taken here means anything.
    if !crate::flags::in_world() {
        log::info!(
            "{}",
            er_logic::spawn_burst::burst_report(
                burst.spec_chr_id(),
                burst.requested(),
                burst.issued(),
                None
            )
        );
        *slot = None;
        return false;
    }

    // ⭐ THE WITNESS (world#689). The burst is spent and the settle window has closed, so a count
    // taken now is a statement about what the creator actually made. Until then the burst is held:
    // counting on the tick after the last request would read a creature that has not loaded and
    // report a failure that did not happen.
    if burst.verify_due(now) {
        let observed = match unsafe { WorldChrMan::instance() } {
            Ok(wcm) => {
                let live = count_live_npc_param(wcm, burst.spec_npc_param_id());
                Some(burst.observed_delta(live))
            }
            // Unreadable this tick. `None` prints as "not countable", never as agreement -- the
            // whole point of #689 is that a number we do not have must not read as a success.
            Err(_) => None,
        };
        log::info!(
            "{}",
            er_logic::spawn_burst::burst_report(
                burst.spec_chr_id(),
                burst.requested(),
                burst.issued(),
                observed
            )
        );
        *slot = None;
        return false;
    }
    // Spent but still settling: this tick belongs to the burst, so nothing else may write the
    // creator's slot underneath it.
    let Some(one) = burst.next_request(now) else {
        *slot = Some(burst);
        return true;
    };

    // SAFETY: FD4 singleton, mutated only on the single-threaded tick -- the same contract
    // `fire_spawn` relies on.
    let Ok(wcm) = (unsafe { WorldChrMan::instance_mut() }) else {
        return false; // burst held, retried next tick
    };
    let Some(player) = wcm.main_player.as_ref() else {
        return false;
    };
    let pos = player.chr_ins.modules.physics.position;
    let request = ChrDebugSpawnRequest {
        chr_id: one.chr_id,
        chara_init_param_id: SpawnSpec::CHARA_INIT_PARAM_ID,
        npc_param_id: one.npc_param_id,
        npc_think_param_id: one.think_param_id,
        event_entity_id: 0,
        talk_id: 0,
        pos_x: pos.0,
        pos_y: pos.1,
        pos_z: pos.2,
    };
    wcm.spawn_debug_character(&request);
    // Only now is the issue banked: an early return above leaves the burst exactly as it was.
    *slot = Some(burst);
    true
}

fn fire_no_flask() -> bool {
    // SAFETY: FD4 singleton, mutated only on the single-threaded tick -- the contract
    // `no_equip_load` and `scadu_blessing` already rely on for their param writes.
    let Ok(repo) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return false;
    };
    let Some(row) =
        crate::param_guard::get_mut::<SpEffectParam>(repo, TRAP_NO_FLASK as u32, "trap no_flask")
    else {
        // Not streamed in yet. Retry is a keypress away; a warn here would fire on every boot.
        log::info!("trap no_flask: SpEffect {TRAP_NO_FLASK} not loaded yet -- skipped");
        return false;
    };
    row.set_change_hp_estus_flask_correct_rate(NO_FLASK_CORRECT_RATE);
    row.set_change_mp_estus_flask_correct_rate(NO_FLASK_CORRECT_RATE);
    // 🛑🛑 THE FINITE ENDURANCE IS THE SAFETY PROPERTY, not a detail. The row ships `-1` =
    // PERMANENT, which is what makes it eligible as a claimed row in the first place; applying it
    // unwritten would end the character's flask for the session.
    row.set_effect_endurance(NO_FLASK_SECONDS);

    let Ok(wcm) = (unsafe { WorldChrMan::instance_mut() }) else {
        return false;
    };
    let Some(player) = wcm.main_player.as_mut() else {
        return false;
    };
    // DEATH GUARD before ANY `special_effect` access -- iterating that list at the death cam is
    // this crate's original CTD. Not a failure; the player can press the key again.
    if er_logic::death_guard::lists_unsafe_to_touch(player.chr_ins.modules.data.hp) {
        log::info!("trap no_flask: player not safe to touch this tick -- skipped");
        return false;
    }
    player.chr_ins.apply_speffect(TRAP_NO_FLASK, false);
    log::info!(
        "trap no_flask: applied SpEffect {TRAP_NO_FLASK} (flask correct-rates -> \
         {NO_FLASK_CORRECT_RATE}, endurance {NO_FLASK_SECONDS}s)"
    );
    true
}
