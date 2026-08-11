//! no_equip_load — multiply the player's equip-load ceiling so heavy kit stops being punished.
//!
//! The game recomputes max equip load every frame from Endurance, so a plain memory write to the
//! computed field reverts instantly (verified 2026-07-18: writing `PlayerGameData.max_equip_load`
//! snapped back at 0ms). So we intervene on the INPUT instead: a permanent, silent SpEffect whose
//! `allItemWeightChangeRate = 0` zeroes equipped-item weight -> equip-load ratio ~0 -> always
//! light roll. This is the data-side equivalent of the "No Weight" AOB hook (which zeroed the
//! weight-sum accumulator), and it avoids the client's first raw code hook.
//!
//! `SP_EFFECT_ID` (20012080) is a pure no-op vanilla `SpEffectParam` row: every field at its
//! default, silent (`vfxId`/`iconId` = -1), and `effectEndurance` = -1 (permanent). It is
//! referenced NOWHERE else in the regulation (verified by cross-referencing the full Smithbox param
//! dump: it occurs exactly once across all 239 param tables -- only as its own row), so editing it
//! to weightless and applying it to the player cannot affect any item, enemy, or system.
//!
//! ## 🔴 IT WAS ON THE WRONG FIELD FOR A MONTH -- `allItemWeightChangeRate` IS NOT A MULTIPLIER
//!
//! boblerrr 2026-08-09 and Alaric 2026-08-11: option on, row patched, effect resident on the
//! player, no weight change. The plumbing was never the problem and the logs proved it (`-> 0`
//! written 41 times across map loads, `speffects=[..., 20012080, ...]` on the downstate probe).
//! The FIELD was wrong, and vanilla `SpEffectParam` says so outright -- 11325 rows, counted:
//!
//! ```text
//! allItemWeightChangeRate:  1 x 11302,  0 x 23,   and NOTHING else
//! equipWeightChangeRate:    1 x 11293,  0 x 23,   1.045 1.05 1.065 1.08 1.15 1.15 1.17 1.19 4.5
//! ```
//!
//! `allItemWeightChangeRate` is never once used as a multiplier in the whole game. Its only
//! non-default value is `0`, and all 23 rows carrying it are UNNAMED -- the null row `0` and band
//! terminators (4011-4014, 4021-4024, 20011199, 20011299, ...). That is a sentinel meaning "unset",
//! not "x0"; if it meant x0 those rows would each make the player weightless.
//!
//! `equipWeightChangeRate` is the live lever, and the rows that use it identify themselves:
//! 310300/310310/310320 at 1.15/1.17/1.19 are the Arsenal Charm line (+15/+17/+19% max equip load)
//! and 310400/310410/310420 at 1.05/1.065/1.08 are Erdtree's Favor (+5/+6.5/+8%). Those are the
//! game's OWN equip-load talismans, so the field is proven to work by the game shipping it.
//!
//! 🛑 The old write was "safe" in the only sense anyone had checked. `verify_safe_speffect_row.py`
//! proved the row is UNREFERENCED, which proves an edit is harmless -- it can never prove the edit
//! DOES anything. A field nothing reads is maximally safe and completely inert.
//!
//! ## The readback exists now, and it is the point
//!
//! This module used to log that it WROTE and never that it WORKED, which is why the report took two
//! log digs to get this far. It now samples `PlayerGameData::max_equip_load` before the effect goes
//! on and again once the game has recomputed with it, and logs both. One line in a playtest log now
//! settles it: the numbers move, or the field is wrong too and we have learned that cheaply.
//!
//! ## TWO MODES SINCE er-archipelago#548, and the readback now proves WHICH ONE
//!
//! boblerrr, the moment light-roll landed: *"light roll would be to op / imagine full heavy armor +
//! light roll"*. He is right -- a fast roll in full plate with no trade-off means the equip-load
//! budget stops being a decision. So the option carries a ROLL MODE now
//! ([`er_logic::equip_load::RollMode`]), and the only thing that changes down here is the constant.
//!
//! 🛑 THE READBACK HAD TO GET STRICTER WITH IT. The old check was `after > before * 1.5`, which is
//! true for 3x and true for 100x -- so a `medium` seed that silently got `light` would have logged
//! WORKING. That is the same class of error as the month this module spent writing an inert field
//! and reporting success: an assertion loose enough to pass for the wrong reason. It now checks the
//! observed ratio against the mode's OWN multiplier, and prints the ratio either way, because that
//! printed ratio is how #548's provisional 3.0 gets tuned into a measured number.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use eldenring::cs::{ChrInsExt, GameDataMan, SoloParamRepository, SpEffectParam, WorldChrMan};
use er_logic::equip_load::{Parsed, RollMode};
use fromsoftware_shared::FromStatic;

/// The repurposed vanilla no-op `SpEffectParam` row (see module doc for why it is safe).
const SP_EFFECT_ID: i32 = er_logic::safe_speffect_rows::NO_EQUIP_LOAD;
// CLAIMED, not chosen here: `er-logic/src/safe_speffect_rows.rs` is the single source of
// truth for repurposed rows and carries the eligibility criteria + the duplicate-claim test.

/// How close the observed `after / before` ratio must sit to the mode's multiplier before the
/// readback will call it WORKING.
///
/// A band rather than an equality because the player may be wearing the game's OWN equip-load
/// talismans -- Arsenal Charm is +19% at most, Erdtree's Favor +8% -- and those multiply the same
/// field. 0.75 admits both stacked (1.19 * 1.08 = 1.285, so a 3x reads as high as 3.86 and a 100x
/// as high as 128.5) while still being far too tight for a `medium` seed to pass on a `light`
/// write: 100 / 3 is 33x out of band.
const RATIO_TOLERANCE: f32 = 0.75;

/// The active mode, as its wire value (see [`er_logic::equip_load`]). `AtomicU8` rather than a
/// lock: written once at connect, read on the tick.
static MODE: AtomicU8 = AtomicU8::new(er_logic::equip_load::WIRE_OFF as u8);
static PARAM_PATCHED: AtomicBool = AtomicBool::new(false);

/// `max_equip_load` sampled just BEFORE the effect was applied, as f32 bits; `NOT_SAMPLED` until
/// then. Half of the readback -- a single "after" number proves nothing without the number it came
/// from, because the player's Endurance is unknown to us.
static LOAD_BEFORE_BITS: AtomicU32 = AtomicU32::new(NOT_SAMPLED);
const NOT_SAMPLED: u32 = u32::MAX;

/// Set once the after-reading has been logged, so the readback costs one line per arm and not one
/// per frame. Cleared by `reset()` with the param latch: a map load re-streams the row, the re-arm
/// re-writes it, and the question "did it take THIS time" is worth asking again.
static VERIFIED: AtomicBool = AtomicBool::new(false);

/// Set from slot_data `options.no_equip_load` at connect.
///
/// Takes the whole [`Parsed`] rather than a [`RollMode`] so the "I did not understand that value"
/// case is stated by the feature that degraded, in the feature's own voice, instead of being
/// dropped on the floor by the caller. A silent degrade here is indistinguishable from the player
/// having left the option off -- which is the failure the handshake exists to prevent.
pub fn set_mode(parsed: Parsed) {
    if let Some(raw) = parsed.unrecognised {
        log::warn!(
            "no_equip_load: slot_data asked for mode {raw}, which this client does not know. \
             Treating it as OFF. This seed was generated by an apworld NEWER than this client -- \
             update the client, or reroll with no_equip_load at a mode this build has. (A seed \
             that really needs a newer mode should have been refused at connect by the \
             `no_equip_load_roll` feature tag; reaching this line means it was not declared.)"
        );
    }
    MODE.store(mode_to_wire(parsed.mode), Ordering::Relaxed);
    if parsed.mode.is_on() {
        log::info!(
            "no_equip_load: enabled, mode {} (SpEffect {SP_EFFECT_ID} equipWeightChangeRate -> {})",
            parsed.mode.label(),
            parsed.mode.multiplier()
        );
    }
}

fn mode_to_wire(mode: RollMode) -> u8 {
    match mode {
        RollMode::Off => er_logic::equip_load::WIRE_OFF as u8,
        RollMode::Light => er_logic::equip_load::WIRE_LIGHT as u8,
        RollMode::Medium => er_logic::equip_load::WIRE_MEDIUM as u8,
    }
}

/// Read-back for the `no_equip_load_roll` feature handshake: is the MEDIUM mode live?
///
/// 🛑 It loads the same atomic `tick()` gates on, which is what makes it a read-back rather than a
/// receipt. A probe that recorded "we called `set_mode`" would have reported ARMED throughout
/// er-archipelago#536, because `set_enabled(false)` was faithfully called with the value that never
/// arrived.
///
/// MEDIUM specifically, not `is_on()`: the apworld declares this tag only for a seed that asks for
/// medium (light and off are what every client in circulation already does), so ARMED has to mean
/// the same narrow thing DECLARED does or the subtraction is comparing two different questions.
pub fn medium_armed() -> bool {
    matches!(mode(), RollMode::Medium)
}

fn mode() -> RollMode {
    match i64::from(MODE.load(Ordering::Relaxed)) {
        er_logic::equip_load::WIRE_LIGHT => RollMode::Light,
        er_logic::equip_load::WIRE_MEDIUM => RollMode::Medium,
        // Only WIRE_OFF can reach here: `mode_to_wire` is the sole writer and it is total.
        _ => RollMode::Off,
    }
}

/// Re-arm the one-time param edit. Called from core.rs's `in_world` false->true edge: a map load
/// streams SpEffectParam back in, which restores our repurposed row's vanilla
/// `allItemWeightChangeRate`, and PARAM_PATCHED would otherwise stop `tick()` ever re-zeroing it --
/// the player keeps carrying a row that no longer makes anything weightless. Clears the LATCH ONLY:
/// `MODE` is slot_data configuration set once at connect. Re-running costs ONE row write; the
/// player-side apply is already idempotent (it only applies when the entry is absent).
pub fn reset() {
    PARAM_PATCHED.store(false, Ordering::Relaxed);
    VERIFIED.store(false, Ordering::Relaxed);
    LOAD_BEFORE_BITS.store(NOT_SAMPLED, Ordering::Relaxed);
}

/// Per-tick. When enabled + in-world: patch our SpEffect row to weightless once, then keep the
/// player carrying it. When disabled: strip it from the player. Idempotent -- applying only when
/// the player doesn't already have it avoids stacking duplicate entries.
pub fn tick() {
    // MENU/BOOT GATE: the param repo and chr sets aren't settled at boot / the main menu, and
    // rows_mut/main_player panic or read stale there. Same signal every other param writer gates on.
    if !crate::flags::in_world() {
        return;
    }
    let mode = mode();
    if !mode.is_on() {
        // OFF -> FULLY INERT. The block below unconditionally iterated the PLAYER's special_effect
        // list every frame (to compute `has`), which CTD'd at the death-cam transition when the
        // player's chr_ins is being torn down (archipelago20260719 Copy 2.log). A disabled feature
        // must never touch the player. The strip-when-toggled-off path is unreachable: MODE is set
        // once at connect, so Off => never applied this session => nothing to strip (a leftover
        // from a prior on-session is a harmless no-op row).
        return;
    }
    let multiplier = mode.multiplier();

    // One-time param edit: equipWeightChangeRate on our row (the mode is on, guaranteed here).
    if !PARAM_PATCHED.load(Ordering::Relaxed) {
        // SAFETY: FD4 singleton; only mutated on the single-threaded FrameBegin tick.
        let Ok(repo) = (unsafe { SoloParamRepository::instance_mut() }) else {
            return;
        };
        match repo.get_mut::<SpEffectParam>(SP_EFFECT_ID as u32) {
            Some(row) => {
                // 🛑 ONE FIELD, and it is `equipWeightChangeRate`. See the module doc: the old
                // `allItemWeightChangeRate = 0` write is inert because that field is never a
                // multiplier anywhere in the game. Writing BOTH would make the next playtest
                // unreadable -- if the player goes light we would not know which one did it, and
                // that ambiguity is the whole reason this took two reports to find.
                row.set_equip_weight_change_rate(multiplier);
                PARAM_PATCHED.store(true, Ordering::Relaxed);
                log::info!(
                    "no_equip_load: SpEffect {SP_EFFECT_ID} equipWeightChangeRate -> {multiplier} \
                     for mode {} (was 1; allItemWeightChangeRate is NOT written -- it is a \
                     sentinel, never a multiplier)",
                    mode.label()
                );
            }
            None => return, // param file not populated yet -- retry next tick
        }
    }

    // Apply to the player. SAFETY: FD4 singleton; single-threaded tick.
    let Ok(wcm) = (unsafe { WorldChrMan::instance_mut() }) else {
        return;
    };
    let Some(player) = wcm.main_player.as_mut() else {
        return;
    };
    // DEATH GUARD -- THE canonical predicate, not a private copy. This module is where the CTD
    // was first observed (archipelago20260719 Copy 2.log); the rule now lives once in
    // er_logic::death_guard. Reading hp here is the same access DeathLink's read_local_hp does.
    if er_logic::death_guard::lists_unsafe_to_touch(player.chr_ins.modules.data.hp) {
        return;
    }
    let chr = &mut player.chr_ins;
    let resident = chr
        .special_effect
        .entries()
        .any(|e| e.param_id == SP_EFFECT_ID);
    if !resident {
        // Sample BEFORE applying -- once the effect is on, the pre-effect number is unrecoverable.
        if let Some(load) = read_max_equip_load() {
            LOAD_BEFORE_BITS.store(load.to_bits(), Ordering::Relaxed);
        }
        chr.apply_speffect(SP_EFFECT_ID, false);
        return; // the game recomputes max_equip_load on its own schedule; read it next tick
    }

    // RESIDENT: say whether it actually did anything. Once per arm.
    if VERIFIED.swap(true, Ordering::Relaxed) {
        return;
    }
    let before_bits = LOAD_BEFORE_BITS.load(Ordering::Relaxed);
    let (Some(after), true) = (read_max_equip_load(), before_bits != NOT_SAMPLED) else {
        // Could not read one of the two numbers. Do NOT claim anything -- an unverified feature
        // that says it is fine is the failure this readback exists to end.
        VERIFIED.store(false, Ordering::Relaxed);
        return;
    };
    let before = f32::from_bits(before_bits);
    // 🛑 A zero "before" is a BAD SAMPLE, not a real ceiling -- max equip load is never 0 on a
    // placed character, and `after > 0.0 * 1.5` would report WORKING for any reading at all. Re-arm
    // and ask again rather than answer from it; this is the exact shape of assertion that made the
    // old "we wrote the row" log worthless.
    // NOT `!(before > 0.0)` -- clippy::neg_cmp_op_on_partial_ord rejects a negated comparison on a
    // partially ordered type, and it is right to: the NaN case is the whole reason the negation was
    // there, so say it outright. `is_finite` covers NaN and both infinities; `<= 0.0` covers zero
    // and negatives. Same set, no negated comparison.
    if !before.is_finite() || before <= 0.0 {
        VERIFIED.store(false, Ordering::Relaxed);
        LOAD_BEFORE_BITS.store(NOT_SAMPLED, Ordering::Relaxed);
        return;
    }
    // 🛑 THE RATIO, NOT A FLOOR. `after > before * 1.5` was true for 3x AND for 100x, so a
    // `medium` seed silently handed `light` would have logged WORKING -- an assertion that passes
    // for the wrong reason, which is the exact failure this readback was added to end. The observed
    // ratio is printed either way: it is the number #548's provisional 3.0 gets tuned from, and a
    // log line nobody can compute from is not a measurement.
    let ratio = after / before;
    let expected = mode.multiplier();
    if ratio >= expected * RATIO_TOLERANCE {
        log::info!(
            "no_equip_load: WORKING -- mode {} wrote equipWeightChangeRate {expected}, max equip \
             load {before:.1} -> {after:.1} (ratio {ratio:.2}) with SpEffect {SP_EFFECT_ID} \
             resident",
            mode.label()
        );
    } else if ratio > 1.0 + f32::EPSILON {
        // It MOVED, but not by what this mode asked for. Distinct from INERT on purpose: the field
        // is taking, so the mod-stack diagnosis below would be the wrong advice.
        log::warn!(
            "no_equip_load: WRONG MULTIPLIER -- mode {} asked for {expected}x, max equip load \
             moved {before:.1} -> {after:.1} (ratio {ratio:.2}). The field IS taking, so this is \
             not a spare-row collision; something else is writing equipWeightChangeRate on this \
             player, or the mode that reached the client is not the mode the seed was rolled with",
            mode.label()
        );
    } else {
        log::warn!(
            "no_equip_load: INERT -- SpEffect {SP_EFFECT_ID} is resident on the player and \
             equipWeightChangeRate is {expected} for mode {}, but max equip load did not move \
             ({before:.1} -> {after:.1}). The row is being applied and the field is not taking. \
             Most likely a co-loaded mod shipping its own regulation.bin in which {SP_EFFECT_ID} \
             is NOT a spare row -- check the mod stack before changing the field again",
            mode.label()
        );
    }
}

/// `PlayerGameData::max_equip_load`, the game's own computed ceiling. READ ONLY -- writing it was
/// measured snapping back at 0 ms on 2026-07-18, which is why this module drives the input instead.
fn read_max_equip_load() -> Option<f32> {
    // SAFETY: FD4 singleton, read-only walk. Err/None before the player is placed.
    let gdm = unsafe { GameDataMan::instance() }.ok()?;
    Some(gdm.main_player_game_data.as_ref().max_equip_load)
}
