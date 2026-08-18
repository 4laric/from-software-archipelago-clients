//! trap feel probe -- fire a non-destructive trap effect from a function key, so a playtester can
//! tell us how it FEELS before anybody builds an item around it.
//!
//! ## Why this exists alongside `traps.rs`
//!
//! `traps.rs` answers "does the mechanism work". It cannot answer "is this fun", because it is
//! gated OFF: on 2026-08-08 a whole playtest round ran with `probes: none active` and the trap
//! measurement simply did not happen. The questions here can only be answered during somebody
//! else's session, on somebody else's machine, and a missed session cannot be retaken.
//!
//! ## 🛑 ON BY DEFAULT, and here is the argument
//!
//! `ER_TRAP_FEEL_PROBE=0` or `"probes": {"trap_feel": false}` silences it. Defaulting a probe ON is
//! the exception in this crate, and it is only defensible here because of what this one CANNOT do:
//!
//! * nothing it writes survives the session -- no param row is edited, no speffect row is claimed,
//!   no save state is touched;
//! * every effect wears off on its own, on a [`Deadline`](er_logic::trap_probe::Deadline) with a
//!   compile-time bound on its duration;
//! * nothing fires unless somebody presses a FUNCTION KEY. Idle cost is one mutex and one
//!   `idle()` check per frame.
//!
//! 🛑 Contrast `traps.rs`, which must stay off by default and should never follow this precedent:
//! F7 really takes half your runes. "The gate is the consent" is true there and irrelevant here.
//!
//! ## The three effects
//!
//! **F9 Nightfall** -- `WorldAreaTime::request_time(0,0,0)`. Instantaneous; time flows on from
//! there and a grace rest resets it, so there is nothing to restore.
//!
//! **F10 Stamina Halved** -- halves `CSChrDataModule::max_stamina` for 30s. Current stamina is
//! capped once when the effect lands and is never replenished by the probe; ordinary drain and
//! regeneration continue inside the smaller maximum. The original maximum is restored afterward.
//!
//! **F11 Blackout** -- fades the screen out for 2.5s and back in.
//! 🛑 `CSFade` carries NINE fade plates and nothing on record says which one composites over
//! ordinary play, so this fades ALL of them and restores ALL of them. That is the one unverified
//! thing in this probe. If the report is "the screen never went dark", that is a PLATE question and
//! not a timer one -- the log line says so, so the answer arrives with the report.
//!
//! ## Known limitation, stated rather than hidden
//!
//! [`tick`] runs from the overlay frame hook, which is where `traps::poll_pending` and the F7/F8
//! bindings already live. A player who hides the overlay mid-blackout therefore stops the restore
//! until they show it again. Adding a second call site on the game thread would mean writing these
//! singletons from two threads, which is a worse trade than a caveat -- and the effects are 2.5s
//! long.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{WorldAreaTime, WorldChrMan};
use er_logic::trap_probe::{
    BLACKOUT_FADE_SECONDS, FeelEffect, NIGHTFALL_TIME, ProbeState, initial_stamina_cap,
    stamina_limit,
};
use fromsoftware_shared::FromStatic;

/// Deadlines owed to effects already fired.
///
/// A `Mutex` rather than atomics because [`ProbeState`] is two related deadlines and the frame hook
/// is the only caller -- contention is impossible, and a poisoned lock simply skips the tick.
static STATE: Mutex<ProbeState> = Mutex::new(ProbeState::new());

#[derive(Clone, Copy)]
struct ActiveStaminaLimit {
    original_max: i32,
    limited_max: i32,
}

/// Kept separately from the deadline because a restore that hits a death/load teardown must remain
/// owed and retry on the next safe frame rather than being forgotten with the elapsed deadline.
static STAMINA_LIMIT: Mutex<Option<ActiveStaminaLimit>> = Mutex::new(None);

/// Is the probe on? ON unless somebody says no -- see the module note for the argument.
pub fn enabled() -> bool {
    shared::probes::enabled_by_default("ER_TRAP_FEEL_PROBE", "trap_feel")
}

/// Say which way the switch is set, once, and say what the keys are.
///
/// 🛑 A DEFAULT-ON PROBE CANNOT RIDE `probes::log_active`: that line resolves through the
/// default-OFF rule, so it would report this probe as off in exactly the case where it is on.
/// `boss_fight_probe::announce_once` exists for the same reason and this follows it.
///
/// The key map goes in the LOG rather than only in a chat message because the log is the artifact
/// that comes back to us -- "I never saw a toast" and "I never pressed the key" are different
/// reports, and only one of them is a bug.
fn announce_once(on: bool) {
    static SAID: AtomicBool = AtomicBool::new(false);
    if SAID.swap(true, Ordering::Relaxed) {
        return;
    }
    if on {
        log::info!(
            "trap-feel probe: ON (default). F9 = nightfall (instant midnight), F10 = half stamina \
             for 30s, F11 = blackout for 2s. All three wear off on their own and none of them \
             touches your save. If F11 does nothing visible, SAY SO -- that is a fade-plate \
             reading, not a broken timer. Set ER_TRAP_FEEL_PROBE=0, or \"probes\": \
             {{\"trap_feel\": false}} in apconfig.json, to silence it"
        );
    } else {
        log::info!("trap-feel probe: SILENCED by ER_TRAP_FEEL_PROBE=0 / probes.trap_feel=false");
    }
}

/// Fire `effect`. Returns the line to toast, or `None` when it could not act this tick.
///
/// 🛑 Individually fallible and swallowed, like `traps::fire` and for the same reason (#114 rule 1,
/// F4): an effect that cannot land says so in the log and is skipped. A probe must never propagate
/// an error into the frame hook it is called from.
pub fn fire(effect: FeelEffect) -> Option<&'static str> {
    if !crate::flags::in_world() {
        log::info!("trap-feel {}: not in world -- skipped", effect.key());
        return None;
    }
    let ok = match effect {
        FeelEffect::Nightfall => fire_nightfall(),
        FeelEffect::StaminaHalved => fire_stamina_halved(),
        FeelEffect::Blackout => fire_blackout(),
    };
    if !ok {
        return None;
    }
    // Arm AFTER the effect landed. Arming first would leave a deadline owing a restore for a fade
    // that never happened -- harmless for stamina, but for the blackout it means a fade-IN issued
    // over a screen nobody faded out.
    if let Ok(mut state) = STATE.lock() {
        state.arm(effect, now_ms());
    }
    Some(effect.toast())
}

/// Per-frame: sustain what is running and restore what has expired.
///
/// 🛑 DELIBERATELY NOT GATED ON [`enabled`]. A player who silences the probe from `config_watch`
/// while a blackout is owing must still get their screen back; nothing can arm while it is off, so
/// an ungated tick costs one lock and one `idle()` check.
pub fn tick() {
    // Says which way the switch is set, once, BEFORE the idle return -- an announcement behind an
    // early return is an announcement that never happens on a session where nobody pressed a key,
    // which is precisely the session whose log we most need to be able to read.
    announce_once(enabled());
    let Ok(mut state) = STATE.lock() else {
        return;
    };
    let restore_owed = STAMINA_LIMIT.lock().is_ok_and(|limit| limit.is_some());
    if state.idle() && !restore_owed {
        return;
    }
    let now = now_ms();

    if state.stamina.holds(now) {
        hold_stamina_max();
    } else if state.stamina.take_if_elapsed(now) {
        log::info!("trap-feel stamina_halved: duration elapsed; restoring maximum stamina");
    }
    if !state.stamina.is_pending() && restore_owed && restore_stamina_max() {
        log::info!("trap-feel stamina_halved: maximum stamina restored");
    }

    if state.blackout.take_if_elapsed(now) {
        if crate::traps::fade_blackout(false) {
            log::info!("trap-feel blackout: faded back in");
        } else {
            // Worth a WARN rather than an info: the player is looking at a black screen.
            log::warn!(
                "trap-feel blackout: could not reach CSFade to fade back in -- a load or a grace \
                 rest will clear it"
            );
        }
    }
}

/// Monotonic milliseconds. Its own clock rather than the caller's, so [`tick`] and [`fire`] cannot
/// disagree about what time it is -- they are the two halves of one deadline.
fn now_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static CLOCK: OnceLock<Instant> = OnceLock::new();
    CLOCK.get_or_init(Instant::now).elapsed().as_millis() as u64
}

fn fire_nightfall() -> bool {
    // SAFETY: FD4 singleton, mutated only from the frame hook -- the same contract `traps::fire`
    // and every other player write in this crate rely on.
    let Ok(time) = (unsafe { WorldAreaTime::instance_mut() }) else {
        log::info!("trap-feel nightfall: WorldAreaTime unavailable -- skipped");
        return false;
    };
    let (hour, minute, second) = NIGHTFALL_TIME;
    time.request_time(hour, minute, second);
    log::info!("trap-feel nightfall: requested {hour:02}:{minute:02}:{second:02}");
    true
}

fn fire_stamina_halved() -> bool {
    // The first application is the same write the sustain loop makes; if it cannot land now it will
    // not land this tick either, and the deadline is not armed.
    if !apply_stamina_limit() {
        log::info!("trap-feel stamina_halved: player unreachable -- skipped");
        return false;
    }
    log::info!("trap-feel stamina_halved: maximum halved for 30s");
    true
}

fn fire_blackout() -> bool {
    if crate::traps::fade_blackout(true) {
        log::info!(
            "trap-feel blackout: faded out over {BLACKOUT_FADE_SECONDS}s on all 9 plates -- if \
             nothing went dark, the plate is the finding"
        );
        true
    } else {
        log::info!("trap-feel blackout: CSFade unavailable -- skipped");
        false
    }
}

/// Fade every plate out (`out == true`) or back in. Returns whether `CSFade` was reachable.
///
/// All nine, because nothing on record says which one composites over ordinary play. Restoring all
/// nine is what makes that guess safe: a plate we should not have faded is faded back by the same
/// loop, and any load or grace rest re-drives them anyway.
fn player_data_mut() -> Option<&'static mut eldenring::cs::CSChrDataModule> {
    // SAFETY: as `fire_nightfall`.
    let Ok(wcm) = (unsafe { WorldChrMan::instance_mut() }) else {
        return None;
    };
    let Some(player) = wcm.main_player.as_mut() else {
        return None;
    };
    let data = &mut player.chr_ins.modules.data;
    // The teardown guard. This module never walks `special_effect`, but `chr_ins` itself is torn
    // down at the death cam, and a ceiling is meaningless on a corpse -- the effect resumes on the
    // next tick with `hp > 0`, which is the documented degrade every caller of this guard takes.
    if er_logic::death_guard::lists_unsafe_to_touch(data.hp) {
        return None;
    }
    Some(data)
}

/// Install the smaller maximum. Re-firing extends the deadline without halving an already-halved
/// value again. Current stamina is touched only here, and only downward.
fn apply_stamina_limit() -> bool {
    let Some(data) = player_data_mut() else {
        return false;
    };
    let Ok(mut active) = STAMINA_LIMIT.lock() else {
        return false;
    };
    let limit = active.unwrap_or_else(|| ActiveStaminaLimit {
        original_max: data.max_stamina,
        limited_max: stamina_limit(data.max_stamina),
    });
    if limit.original_max <= 0 || limit.limited_max <= 0 {
        return false;
    }
    data.max_stamina = limit.limited_max;
    if let Some(clamped) = initial_stamina_cap(data.stamina, limit.limited_max) {
        data.stamina = clamped;
    }
    *active = Some(limit);
    true
}

/// Re-assert only the maximum; never write current stamina from the sustain loop.
fn hold_stamina_max() -> bool {
    let Some(data) = player_data_mut() else {
        return false;
    };
    let Ok(active) = STAMINA_LIMIT.lock() else {
        return false;
    };
    let Some(limit) = *active else {
        return false;
    };
    data.max_stamina = limit.limited_max;
    true
}

fn restore_stamina_max() -> bool {
    let Some(data) = player_data_mut() else {
        return false;
    };
    let Ok(mut active) = STAMINA_LIMIT.lock() else {
        return false;
    };
    let Some(limit) = *active else {
        return true;
    };
    data.max_stamina = limit.original_max;
    *active = None;
    true
}
