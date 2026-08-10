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

use eldenring::cs::{ChrInsExt, SoloParamRepository, SpEffectParam, WorldChrMan};
use er_logic::safe_speffect_rows::TRAP_NO_FLASK;
use er_logic::traps::{NO_FLASK_CORRECT_RATE, NO_FLASK_SECONDS, Trap, rune_thief_target};
use fromsoftware_shared::FromStatic;

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
pub fn fire(trap: Trap) -> Option<&'static str> {
    if !crate::flags::in_world() {
        log::info!("trap {}: not in world -- skipped", trap.key());
        return None;
    }
    let ok = match trap {
        Trap::RuneThief => fire_rune_thief(),
        Trap::NoFlask => fire_no_flask(),
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
