//! Boss-fight HP trace (er-archipelago#553): the I/O half of [`er_logic::boss_fight_sample`].
//!
//! # What it records
//!
//! While a boss healthbar is up, ~2 Hz:
//!
//! ```text
//! boss-fight START:  t=0.0s  npc_param 11050800 npc_id 5240 region 11050 boss 19200/19200 (100%) player 652/652 (100%)
//! boss-fight SAMPLE: t=12.5s npc_param 11050800 npc_id 5240 region 11050 boss  8342/19200 ( 43%) player 412/652  ( 63%)
//! boss-fight END:    t=93.5s npc_param 11050800 ...
//! ```
//!
//! Derivable from that series with no further plumbing: hits-to-kill, damage-per-hit taken,
//! time-to-kill, and -- the readback the scaler has never had -- **whether the applied tier moved
//! `max_hp` at all**, because the `START` line carries `max_hp` before any of the fight happened.
//!
//! # Design constraints, and where each one comes from
//!
//! * **Probe-gated and OFF by default.** `ER_BOSSFIGHT_PROBE` / `"probes": {"boss_fight": true}`.
//!   This is diagnostic volume, not something to ship on for everyone.
//!
//!   ⚠️ Deliberately UNLIKE `ER_SCALING_SAMPLE`, which defaults ON because its measurement is taken
//!   through matt's randomizer's launcher and an env var would have to survive two process spawns.
//!   That argument does not transfer: `shared::probes` reads `apconfig.json` too, so a playtester
//!   turns this on by editing a file that sits beside the DLL and that they have already edited
//!   once for the server URL. The launcher problem the scaling sample was working around no longer
//!   exists.
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
use er_logic::boss_fight_sample::{FightSampler, Hp, Reading, Step, format_sample};
use fromsoftware_shared::FromStatic;

/// Off by default. See the module doc for why this differs from `ER_SCALING_SAMPLE`.
fn enabled() -> bool {
    shared::probes::enabled("ER_BOSSFIGHT_PROBE", "boss_fight")
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

fn now_ms() -> u64 {
    CLOCK.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Per-tick entry point. Hard no-op unless the probe is on.
pub fn tick() {
    if !enabled() {
        return;
    }

    // 🛑 THE DEATH GUARD COMES FIRST, before any list is walked. Reading the player's HP is the
    // cheapest possible read and is also the guard's own input, so there is no ordering in which
    // this module touches a list before knowing whether it is safe to.
    let Some(player) = read_player() else {
        return; // not in-world: no main player, nothing to sample, no state to change
    };
    if er_logic::death_guard::lists_unsafe_to_touch(player.cur) {
        return;
    }

    let bar = crate::flags::boss_healthbar_npc_param_id();
    let Ok(mut sampler) = SAMPLER.lock() else {
        return;
    };
    let step = sampler.step(now_ms(), bar);
    // Drop the lock before doing any game reads or logging: the walk below is the expensive part
    // and it does not need the sampler.
    drop(sampler);

    match step {
        Step::Idle => {}
        Step::Start { npc_param_id } => {
            MISSING_REPORTED.store(false, Ordering::Relaxed);
            emit("START", npc_param_id, 0, player);
        }
        Step::Sample {
            npc_param_id,
            elapsed_ms,
        } => emit("SAMPLE", npc_param_id, elapsed_ms, player),
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
        } => {
            // The END line is a duration statement, not a reading: by the time the bar drops the
            // boss is usually gone from the sets, so quoting its HP here would mean quoting a
            // number we could not read. Say the length, which is the thing END is for.
            log::info!(
                "boss-fight END: npc_param {npc_param_id} bar down after {}.{}s",
                elapsed_ms / 1000,
                (elapsed_ms % 1000) / 100
            );
        }
    }
}

/// Read, format and log one line.
fn emit(tag: &str, npc_param_id: i32, elapsed_ms: u64, player: Hp) {
    let Some((npc_id, boss)) = find_boss(npc_param_id) else {
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
    log::info!(
        "{}",
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
}

/// The local player's HP. `None` = not in-world.
fn read_player() -> Option<Hp> {
    let wcm = unsafe { WorldChrMan::instance() }.ok()?;
    let player = wcm.main_player.as_ref()?;
    let data = &player.chr_ins.modules.data;
    Some(Hp::new(data.hp, data.max_hp))
}

/// `(npc_id, hp)` for the character carrying `npc_param_id`, over the same two sets the scaling
/// sweep walks.
///
/// 🛑 `ChrIns` does NOT implement `AsRef`, and `chr.as_ref()` does not compile -- the set iterators
/// yield `&mut T: Subclass<ChrIns>` and the way this crate gets a `&ChrIns` out of one is to PASS
/// it to a fn taking `&ChrIns` and let the coercion happen at the call site. `scaling.rs` pays for
/// that lesson in a comment; this follows it rather than re-learning it on a CI round.
fn find_boss(npc_param_id: i32) -> Option<(i32, Hp)> {
    let wcm = unsafe { WorldChrMan::instance() }.ok()?;
    for chr in crate::scaling::sweepable_characters(&wcm.open_field_chr_set.base) {
        if let Some(found) = match_boss(chr, npc_param_id) {
            return Some(found);
        }
    }
    for slot in wcm.chr_sets.iter().flatten() {
        for chr in crate::scaling::sweepable_characters(slot) {
            if let Some(found) = match_boss(chr, npc_param_id) {
                return Some(found);
            }
        }
    }
    None
}

/// The `&ChrIns`-taking half of [`find_boss`] -- see its doc for why this is a separate fn.
fn match_boss(chr: &ChrIns, npc_param_id: i32) -> Option<(i32, Hp)> {
    if chr.npc_param_id != npc_param_id {
        return None;
    }
    let data = &chr.modules.data;
    Some((chr.npc_id, Hp::new(data.hp, data.max_hp)))
}
