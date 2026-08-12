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
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{
    ChrDebugSpawnRequest, ChrInsExt, SoloParamRepository, SpEffectParam, WorldChrMan,
};
use er_logic::safe_speffect_rows::TRAP_NO_FLASK;
use er_logic::traps::{
    NO_FLASK_CORRECT_RATE, NO_FLASK_SECONDS, RUNEBEAR_SPAWN, SpawnSpec, Trap, classify_refusal,
    rune_thief_target,
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
pub fn enqueue_by_item_name(name: &str, now_ms: u64) {
    let Some(trap) = Trap::from_item_name(name) else {
        // ONE line, and the diagnosis is a value rather than a literal: the client keeps no copy
        // of the words, so there is one place to fix when they are wrong again.
        log::warn!(
            "trap item {name:?} is not one this client can fire -- ignored. {}",
            classify_refusal(name).advice()
        );
        return;
    };
    let Ok(mut guard) = PENDING.lock() else {
        return;
    };
    guard
        .get_or_insert_with(er_logic::traps::TrapQueue::new)
        .push(trap, now_ms);
    log::info!("trap {} queued from item {name:?}", trap.key());
}

/// Per-tick: deliver at most one queued trap, once the player can take it.
///
/// Returns the line to toast, or `None`. `can_fire` is formed HERE (in world) and refined inside
/// `fire` (death guard, param streamed) -- a trap that cannot land this tick goes back to waiting
/// rather than being consumed, which is the whole point of the queue.
pub fn poll_pending(now_ms: u64) -> Option<Cow<'static, str>> {
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
        // The legacy variant is a `SpawnSpec` in everything but its name, so it goes down the same
        // path -- one spawn implementation, not two that can drift.
        Trap::Runebear => fire_spawn(RUNEBEAR_SPAWN),
        Trap::Spawn(spec) => fire_spawn(spec),
    };
    ok.then(|| trap.toast())
}

fn fire_rune_thief() -> bool {
    let Some(before) = crate::runes::read() else {
        log::warn!("trap rune_thief: rune count unreadable -- skipped");
        return false;
    };
    let after = rune_thief_target(before);
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
    for _ in 0..spec.count {
        wcm.spawn_debug_character(&request);
    }
    log::info!(
        "trap spawn: {} x c{} requested -- npc={} think={} chara_init={} at ({}, {}, {})",
        spec.count,
        spec.chr_id,
        spec.npc_param_id,
        spec.think_param_id,
        SpawnSpec::CHARA_INIT_PARAM_ID,
        pos.0,
        pos.1,
        pos.2
    );
    // 🛑 They are REQUESTS: the debug creator spawns on its own schedule, so `true` here means
    // "asked", not "standing there". The player finds out within a second either way.
    true
}

fn fire_no_flask() -> bool {
    // SAFETY: FD4 singleton, mutated only on the single-threaded tick -- the contract
    // `no_equip_load` and `scadu_blessing` already rely on for their param writes.
    let Ok(repo) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return false;
    };
    let Some(row) = repo.get_mut::<SpEffectParam>(TRAP_NO_FLASK as u32) else {
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
