//! Boss-fight HP trace (er-archipelago#553): the I/O half of [`er_logic::boss_fight_sample`].
//!
//! # What it records
//!
//! While a boss healthbar is up, ~2 Hz:
//!
//! ```text
//! boss-fight START:  t=0.0s  npc_param 11050800 npc_id 5240 region 11050 boss 19200/19200 (100%) player 652/652 (100%)
//! boss-fight START carried: npc_param 11050800 npc_id 5240 speffects [7010]
//! boss-fight SAMPLE: t=12.5s npc_param 11050800 npc_id 5240 region 11050 boss  8342/19200 ( 43%) player 412/652  ( 63%)
//! boss-fight END: npc_param 11050800 outcome=BOSS DOWN t=93.5s unseen=0.0s last boss 0/19200 (0%) player 412/652 (63%)
//! ```
//!
//! SAMPLE lines are emitted **on change**, with a
//! [`er_logic::boss_fight_sample::DEFAULT_HEARTBEAT_MS`] heartbeat so a stalemate still shows the
//! fight is live. The poll is still 2 Hz -- see [`er_logic::boss_fight_sample::SampleGate`] for why
//! those are different knobs and why only the second one moved (client#185).
//!
//! Derivable from that series with no further plumbing: hits-to-kill, damage-per-hit taken,
//! time-to-kill, and -- the readback the scaler has never had -- **whether the applied tier moved
//! `max_hp` at all**, because the `START` line carries `max_hp` before any of the fight happened.
//!
//! # Design constraints, and where each one comes from
//!
//! * **OFF by default**, enabled with `ER_BOSSFIGHT_PROBE=1` or
//!   `"probes": {"boss_fight": true}`. The probe has collected the evidence it was built for;
//!   keeping it opt-in avoids adding a continuous fight trace to every player's log.
//!
//! * **Read-only.** No param writes, no game state, no flags. That is what exempts it from the
//!   `in_world`-edge re-arm rule (`test_gf_client_resets_are_called`): there is nothing a map load
//!   could revert.
//!
//! * 🛑 **The death guard applies.** `er_logic::death_guard::lists_unsafe_to_touch` exists because
//!   walking the player's lists during the death-cam teardown CTD'd this crate once already
//!   (`archipelago20260719 Copy 2.log`). A sampler that runs *during a fight* meets exactly that
//!   state, on purpose and often -- it is the single most likely module in the crate to be mid-tick
//!   when the player dies. It reads `hp` first and stops there if the player is down.
//!
//! * 🛑 **Reuse the boss-healthbar signal.** `GameDataMan.boss_health_bar_npc_param_id` (via
//!   [`crate::flags::boss_healthbar_npc_param_id`]) is the authoritative boss set and is already
//!   the spine of `boss_healthbars` / the sweep tables. Inventing an "is a fight happening" test
//!   would be a second answer to a question that already has one.
//!
//! * **The walk is the sweep's walk.** Finding the boss's `ChrIns` reuses
//!   `crate::scaling::sweepable_characters` over the same two sets the sweep uses (open-field base
//!   + every block slot). No new traversal, no new `unsafe`.
//!
//! # ⚠️ Not a clean oracle on a modded stack
//!
//! boblerrr co-loads matt's rando, whose `regulation.bin` edits base stats -- a residual of ~0.50
//! with wide spread has already been measured there. This line carries `npc_param_id` and observed
//! `max_hp`, which is exactly enough to NOTICE that offline (vanilla `NpcParam.hp` is a datamine
//! column), and it carries `npc_id` so a SWAPPED enemy signs itself. It does not average over the
//! difference, because it does not compute a ratio at all.

use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use eldenring::cs::{ChrIns, WorldChrMan};
use er_logic::boss_fight_sample::{
    FightSampler, Hp, Reading, SampleGate, Step, classify, format_end, format_sample,
};
use er_logic::scaling::partition_carried_speffects;
use fromsoftware_shared::FromStatic;

/// **OFF by default.** `ER_BOSSFIGHT_PROBE=1` or `"probes": {"boss_fight": true}` enables it.
///
/// The outcome instrument has collected the evidence requested by #553. It remains available for
/// targeted diagnostics, but ordinary sessions should not pay for or retain a ~2 Hz boss-fight HP
/// trace.
fn enabled() -> bool {
    shared::probes::enabled("ER_BOSSFIGHT_PROBE", "boss_fight")
}

/// Say which way the switch is set, once, on the first tick that reaches it.
///
/// This module-specific line states both the cadence and the opt-in switch. The aggregate
/// `probes::log_active` line also names the probe when enabled.
fn announce_once(on: bool) {
    static SAID: AtomicBool = AtomicBool::new(false);
    if SAID.swap(true, Ordering::Relaxed) {
        return;
    }
    if on {
        log::info!(
            "boss-fight probe: ON (opt-in). Samples player and boss HP ~2 Hz while a boss \
             healthbar is up"
        );
    } else {
        log::info!(
            "boss-fight probe: OFF (default). Set ER_BOSSFIGHT_PROBE=1 or \
             probes.boss_fight=true to enable it"
        );
    }
}

/// The sampler's own state. A `Mutex` rather than a raw static because [`FightSampler`] is a small
/// state machine with five fields, and the tick is the only caller -- contention is impossible and
/// a poisoned lock simply skips the sample.
static SAMPLER: Mutex<FightSampler> = Mutex::new(FightSampler::new());

/// Wall clock. `Instant` rather than a frame counter: fight LENGTH in seconds is half the signal
/// ("too tanky" and "one-shots me" are opposite complaints that both read as "hard" without it),
/// and a frame count is not a duration on a machine whose frame rate is what is being complained
/// about.
static CLOCK: OnceLock<Instant> = OnceLock::new();

/// Whether the current fight has already reported that its boss could not be found in the live
/// sets, so that failure is stated once rather than twice a second.
static MISSING_REPORTED: AtomicBool = AtomicBool::new(false);

/// Suppresses SAMPLE lines whose numbers have not moved (client#185).
static GATE: Mutex<SampleGate> = Mutex::new(SampleGate::new());

/// The last `(boss, player)` pair this fight managed to read.
///
/// 🛑 THE END LINE CANNOT READ THE BOSS ITSELF. By the time the bar drops the boss is usually gone
/// from the live sets -- which is why the old END line quoted no HP at all. Remembering the last
/// reading costs one `Option` and is the difference between "bar down" and "BOSS DOWN".
static LAST_SEEN: Mutex<Option<(Hp, Hp)>> = Mutex::new(None);

/// Holds `START` until `play_region` stops moving (client#195).
///
/// 🛑 THE FOG-LOAD FRAME IS NOT A READING. `Step::Start` fires on the healthbar edge, inside the
/// frame the fog gate is still loading, so region / `max_hp` / carried speffects were all read one
/// frame early -- Loretta's `4214/4214` against a settled `2122/2122`, and a `START carried:`
/// naming `[7060, 7460]`, rows that were never issued to any region in that whole session. The raw
/// read is still written, marked, because `4214/2122 = 1.9859` is itself the evidence #188/#183 are
/// about.
static START_GATE: Mutex<er_logic::boss_fight_start_settle::StartGate> =
    Mutex::new(er_logic::boss_fight_start_settle::StartGate::new());

/// Set when the death guard trips during a fight.
///
/// ⭐ THE GUARD IS ALREADY THE DEATH SIGNAL, so the outcome costs nothing. `lists_unsafe_to_touch`
/// is the same test that stops the sampler, so the flag cannot disagree with the gap in the trace
/// it explains. 🛑 It is deliberately NOT read from `deathlink.rs`: that module's two `hp <= 0`
/// tests mean different things (see `er_logic::death_guard`), and borrowing one of them here would
/// re-create the "one bit, two jobs" coupling that doc exists to prevent.
static PLAYER_WENT_DOWN: AtomicBool = AtomicBool::new(false);

fn now_ms() -> u64 {
    CLOCK.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Per-tick entry point. Hard no-op unless the probe is on.
pub fn tick() {
    let on = enabled();
    announce_once(on);
    if !on {
        return;
    }

    // 🛑 THE DEATH GUARD COMES FIRST, before any list is walked. Reading the player's HP is the
    // cheapest possible read and is also the guard's own input, so there is no ordering in which
    // this module touches a list before knowing whether it is safe to.
    let Some(player) = read_player() else {
        return; // not in-world: no main player, nothing to sample, no state to change
    };
    if er_logic::death_guard::lists_unsafe_to_touch(player.cur) {
        // ⭐ KEEP THE READING THAT ENDED THE FIGHT (client#201). This early return used to discard
        // it, and it is the ONLY reading in the whole fight taken after the damage that killed the
        // player -- so `LAST_SEEN` kept the last pre-death sample and every lost fight closed on
        // `player 414/414 (100%)` next to `outcome=PLAYER DOWN`. That pair was guaranteed by this
        // return, not by anything the game memory did.
        //
        // 🛑 NO LIST IS WALKED HERE. `player` is already in hand -- it is the guard's own input --
        // and the boss half of the pair is left exactly as it was, because this is precisely the
        // tick `lists_unsafe_to_touch` exists to forbid walking the live sets on.
        remember_death_reading(player);
        // 🛑 LATCH BEFORE RETURNING. This early return is the whole reason the fight length was
        // wrong: from here until the respawn the sampler is not ticked at all, so it never sees
        // the death and never sees the bar go down. One store turns that blind spot from a silent
        // 25-second overstatement into a stated outcome (client#184).
        PLAYER_WENT_DOWN.store(true, Ordering::Relaxed);
        return;
    }

    let t = now_ms();
    let bar = crate::flags::boss_healthbar_npc_param_id();
    let Ok(mut sampler) = SAMPLER.lock() else {
        return;
    };
    let step = sampler.step(t, bar);
    // Drop the lock before doing any game reads or logging: the walk below is the expensive part
    // and it does not need the sampler.
    drop(sampler);

    match step {
        Step::Idle => {}
        Step::Start { npc_param_id } => {
            MISSING_REPORTED.store(false, Ordering::Relaxed);
            PLAYER_WENT_DOWN.store(false, Ordering::Relaxed);
            set_last_seen(None);
            if let Ok(mut g) = GATE.lock() {
                // A re-fight opens on the numbers the last one closed with -- bobler fought the
                // same boss twice in four minutes. Without this the t=0 readback, the most
                // valuable line in the trace, would be suppressed as a duplicate.
                g.reset();
            }
            if let Ok(mut g) = START_GATE.lock() {
                g.open(t);
            }
            // The pre-settle read, marked. It is NOT the fight's START and nothing downstream may
            // take a number from it -- but it is kept, because the gap between it and the settled
            // line is a reading about when `max_hp` recomputes after a `maxHpRate` speffect moves.
            emit("START raw", npc_param_id, 0, player, t, EmitMode::Always);
        }
        Step::Sample {
            npc_param_id,
            elapsed_ms,
        } => {
            use er_logic::boss_fight_start_settle::StartEmit;
            // ⭐ START IS EMITTED FROM THE FIRST STABLE SAMPLE (client#195), which is #195's own
            // preferred shape: the edge stays observable on the `START raw` line above, and the
            // headline numbers come from a tick where the region has stopped moving.
            // ONE lock, both answers: `poll` decides this tick and `settled` says whether a
            // headline exists yet. Taking the lock twice would be two reads of a value that can
            // change between them.
            let (step, settled) = match START_GATE.lock() {
                // A poisoned lock must not strand the fight without a START. Degrade to the old
                // behaviour -- an early START is worse than no START, but only just, and a fight
                // with no headline line at all is unreadable.
                Err(_) => (StartEmit::Settled, true),
                Ok(mut g) => {
                    let step = g.poll(t, crate::flags::play_region_id());
                    (step, g.settled())
                }
            };
            match step {
                // Unsuppressed, and it re-primes the dedupe gate: this is the `max_hp` readback the
                // whole trace hangs off, and it must not be eaten as a duplicate of the raw line it
                // exists to correct.
                StartEmit::Settled => emit(
                    "START",
                    npc_param_id,
                    elapsed_ms,
                    player,
                    t,
                    EmitMode::Always,
                ),
                // Still settling: no SAMPLE may be written under a headline that does not exist
                // yet. The fight is still being timed and `unseen` still accounts for the gap.
                _ if !settled => {}
                _ => emit(
                    "SAMPLE",
                    npc_param_id,
                    elapsed_ms,
                    player,
                    t,
                    EmitMode::OnlyIfChanged,
                ),
            }
        }
        Step::Capped {
            npc_param_id,
            elapsed_ms,
        } => {
            log::info!(
                "boss-fight CAPPED: npc_param {npc_param_id} has been on screen for \
                 {elapsed_ms} ms and has hit the per-fight sample cap ({} lines). Sampling stops \
                 here; the fight is still being timed and END will report its true length.",
                er_logic::boss_fight_sample::MAX_SAMPLES_PER_FIGHT
            );
        }
        Step::End {
            npc_param_id,
            elapsed_ms,
            unseen_ms,
        } => {
            // The boss is usually gone from the live sets by now, so this quotes the LAST reading
            // taken rather than trying to take a new one -- and says which it is.
            let last = take_last_seen();
            let outcome = classify(
                last.map(|(boss, _)| boss),
                PLAYER_WENT_DOWN.swap(false, Ordering::Relaxed),
            );
            log::info!(
                "{}",
                format_end(npc_param_id, outcome, elapsed_ms, unseen_ms, last)
            );
        }
    }
}

/// Whether a line is unconditional or subject to the dedupe gate.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EmitMode {
    /// START. Never suppressed: it is the `max_hp` readback the whole trace hangs off.
    Always,
    /// SAMPLE. Written only when a number moved, or on the heartbeat.
    OnlyIfChanged,
}

fn set_last_seen(v: Option<(Hp, Hp)>) {
    if let Ok(mut slot) = LAST_SEEN.lock() {
        *slot = v;
    }
}

fn take_last_seen() -> Option<(Hp, Hp)> {
    LAST_SEEN.lock().ok().and_then(|mut slot| slot.take())
}

/// Replace the player half of the remembered pair with the death-tick reading (client#201).
///
/// Read-modify-write under ONE lock acquisition: the death tick races the END step that quotes the
/// pair, and a peek-then-set would let a fight close on the reading this call is replacing.
///
/// A `None` slot stays `None`. That is [`er_logic::boss_fight_end_guard_replay::remember_on_death`]'s
/// contract and it is the right one here: no fight in progress (the player died in the open world),
/// or a fight whose boss was never once found in the live sets. Neither has a boss reading to pair
/// this with, and `format_end` already has an honest line for the second.
fn remember_death_reading(player: Hp) {
    if let Ok(mut slot) = LAST_SEEN.lock() {
        *slot = er_logic::boss_fight_end_guard_replay::remember_on_death(*slot, player);
    }
}

/// Read, remember, and (usually) log one line.
///
/// 🛑 THE READING IS REMEMBERED EVEN WHEN THE LINE IS SUPPRESSED. The gate decides what gets
/// WRITTEN; `LAST_SEEN` is what END quotes. Tying the two together would mean a fight that ended
/// during a run of unchanged samples reported stale HP.
fn emit(tag: &str, npc_param_id: i32, elapsed_ms: u64, player: Hp, now_ms: u64, mode: EmitMode) {
    let want_carried = mode == EmitMode::Always;
    let Some((instance_key, npc_id, boss, carried, row_matches)) =
        find_boss(npc_param_id, want_carried)
    else {
        // The healthbar names a boss the live sets do not contain. Real and expected around phase
        // transitions and cutscenes -- but stated, because a silent gap in a trace is
        // indistinguishable from a boss that stopped taking damage.
        if !MISSING_REPORTED.swap(true, Ordering::Relaxed) {
            log::info!(
                "boss-fight {tag}: npc_param {npc_param_id} has a healthbar but is not in the \
                 live character sets -- HP unreadable this tick. Stated once per fight; the \
                 series resumes if it comes back."
            );
        }
        return;
    };
    set_last_seen(Some((boss, player)));

    let write = match mode {
        EmitMode::Always => {
            // Prime the gate with this reading so the first SAMPLE after an unchanged START is
            // suppressed rather than duplicating it.
            if let Ok(mut g) = GATE.lock() {
                g.admit(now_ms, boss, player);
            }
            true
        }
        EmitMode::OnlyIfChanged => match GATE.lock() {
            Ok(mut g) => g.admit(now_ms, boss, player),
            // A poisoned lock must not silence the instrument. Logging a duplicate is the
            // documented degrade; losing the trace is not.
            Err(_) => true,
        },
    };
    if !write {
        return;
    }

    log::info!(
        "{} instance {instance_key} row_matches {row_matches}",
        format_sample(
            tag,
            &Reading {
                npc_param_id,
                npc_id,
                play_region: crate::flags::play_region_id(),
                boss,
                player,
                elapsed_ms,
            }
        )
    );
    if want_carried {
        // ⭐ THE LINE THAT SETTLES client#186. In bobler's 08-12 log the fight probe read
        // npc_param 47500014 as 3826/3826 while all nine `enemy-scaling` census sightings of the
        // same row read 6080/6080, and the log could not say whether the fought instance carried a
        // rung -- because only the census printed one. Same filter as the census
        // (`is_scaling_speffect_with_downstates`) on purpose: comparability with it is the entire
        // reason this line exists, which is why the filter stayed and the dropped half moved to a
        // line of its own rather than being folded in here.
        let (scaling, other) = partition_carried_speffects(carried);
        log::info!(
            "boss-fight {tag} carried: instance {instance_key} npc_param {npc_param_id} \
             npc_id {npc_id} speffects {:?} \
             (scaling rungs + down-states only, the same filter the enemy-scaling census uses)",
            scaling
        );
        // ⭐ THE LINE THAT SETTLES client#189, and the one whose ABSENCE cost three wrong
        // hypotheses. `4410` is `maxHpRate 2.0` and 63 NpcParam rows carry it; 197 rows carry some
        // `maxHpRate != 1` row outside the native ladder slot. Without this, a boss holding a
        // vanilla 2x reads as a clean `[7010]` and its HP looks like a scaling defect.
        log::info!(
            "boss-fight {tag} speffects OTHER: instance {instance_key} npc_param \
             {npc_param_id} npc_id {npc_id} \
             {} entr(ies) {:?} -- everything the filter above drops. A row here CAN carry \
             maxHpRate (e.g. 4410 = 2.0x), so expected HP is NpcParam.hp x those rates x the rung, \
             never NpcParam.hp x the rung alone",
            other.len(),
            other
        );
    }
}

/// The local player's HP. `None` = not in-world.
fn read_player() -> Option<Hp> {
    let wcm = unsafe { WorldChrMan::instance() }.ok()?;
    let player = wcm.main_player.as_ref()?;
    let data = &player.chr_ins.modules.data;
    Some(Hp::new(data.hp, data.max_hp))
}

/// `(npc_id, hp, carried scaling speffects)` for the character carrying `npc_param_id`, over the
/// same two sets the scaling sweep walks.
///
/// `want_carried` is false on the 2 Hz path: the speffect walk is only asked for on START, where
/// one allocation per fight is free, rather than twice a second for a list that does not move.
///
/// 🛑 `ChrIns` does NOT implement `AsRef`, and `chr.as_ref()` does not compile -- the set iterators
/// yield `&mut T: Subclass<ChrIns>` and the way this crate gets a `&ChrIns` out of one is to PASS
/// it to a fn taking `&ChrIns` and let the coercion happen at the call site. `scaling.rs` pays for
/// that lesson in a comment; this follows it rather than re-learning it on a CI round.
fn find_boss(npc_param_id: i32, want_carried: bool) -> Option<(u64, i32, Hp, Vec<i32>, usize)> {
    let wcm = unsafe { WorldChrMan::instance() }.ok()?;
    let mut found = None;
    let mut matches = 0usize;
    for chr in crate::scaling::sweepable_characters(&wcm.open_field_chr_set.base) {
        if let Some(candidate) = match_boss(chr, npc_param_id, want_carried) {
            matches += 1;
            found.get_or_insert(candidate);
        }
    }
    for slot in wcm.chr_sets.iter().flatten() {
        for chr in crate::scaling::sweepable_characters(slot) {
            if let Some(candidate) = match_boss(chr, npc_param_id, want_carried) {
                matches += 1;
                found.get_or_insert(candidate);
            }
        }
    }
    found.map(|(key, npc_id, hp, carried)| (key, npc_id, hp, carried, matches))
}

/// The `&ChrIns`-taking half of [`find_boss`] -- see its doc for why this is a separate fn.
fn match_boss(
    chr: &ChrIns,
    npc_param_id: i32,
    want_carried: bool,
) -> Option<(u64, i32, Hp, Vec<i32>)> {
    if chr.npc_param_id != npc_param_id {
        return None;
    }
    let data = &chr.modules.data;
    // 🛑 UNFILTERED ON PURPOSE (client#189). This used to filter to
    // `is_scaling_speffect_with_downstates` here, which threw away the ids the caller now needs --
    // `4410` (`maxHpRate 2.0`) is not a scaling row, so a boss holding a 2x multiplier logged as
    // `speffects [7010]`. The split happens at the log site instead, via
    // `er_logic::scaling::partition_carried_speffects`, so both halves survive the walk.
    let carried: Vec<i32> = if want_carried {
        chr.special_effect.entries().map(|e| e.param_id).collect()
    } else {
        Vec::new()
    };
    Some((
        crate::scaling::instance_key(chr),
        chr.npc_id,
        Hp::new(data.hp, data.max_hp),
        carried,
    ))
}
