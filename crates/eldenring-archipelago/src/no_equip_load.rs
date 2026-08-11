//! no_equip_load — make equipment weightless so the player is always at light-roll.
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

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use eldenring::cs::{ChrInsExt, GameDataMan, SoloParamRepository, SpEffectParam, WorldChrMan};
use fromsoftware_shared::FromStatic;

/// The repurposed vanilla no-op `SpEffectParam` row (see module doc for why it is safe).
const SP_EFFECT_ID: i32 = er_logic::safe_speffect_rows::NO_EQUIP_LOAD;
// CLAIMED, not chosen here: `er-logic/src/safe_speffect_rows.rs` is the single source of
// truth for repurposed rows and carries the eligibility criteria + the duplicate-claim test.

/// Multiplier written to `equipWeightChangeRate`. Light roll is under 30% of max equip load, so
/// 100x puts any carriable load far under it -- a level-1 character at 45 equip load reads as 4500,
/// and the heaviest full kit in the game is nowhere near 1350. Deliberately a big round number
/// rather than a tuned one: this is "weightless", not a balance knob, and the game already ships a
/// 4.5 on row 511012, so values well above 1 are within what the field is built for.
const EQUIP_LOAD_MULTIPLIER: f32 = 100.0;

static ENABLED: AtomicBool = AtomicBool::new(false);
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
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if on {
        log::info!("no_equip_load: enabled (weightless SpEffect {SP_EFFECT_ID})");
    }
}

/// Re-arm the one-time param edit. Called from core.rs's `in_world` false->true edge: a map load
/// streams SpEffectParam back in, which restores our repurposed row's vanilla
/// `allItemWeightChangeRate`, and PARAM_PATCHED would otherwise stop `tick()` ever re-zeroing it --
/// the player keeps carrying a row that no longer makes anything weightless. Clears the LATCH ONLY:
/// `ENABLED` is slot_data configuration set once at connect. Re-running costs ONE row write; the
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
    let enabled = ENABLED.load(Ordering::Relaxed);
    if !enabled {
        // OFF -> FULLY INERT. The block below unconditionally iterated the PLAYER's special_effect
        // list every frame (to compute `has`), which CTD'd at the death-cam transition when the
        // player's chr_ins is being torn down (archipelago20260719 Copy 2.log). A disabled feature
        // must never touch the player. The strip-when-toggled-off path is unreachable: ENABLED is set
        // once at connect, so !enabled => never applied this session => nothing to strip (a leftover
        // from a prior on-session is a harmless allItemWeightChangeRate=0 no-op row).
        return;
    }

    // One-time param edit: allItemWeightChangeRate -> 0 on our chosen row (enabled is guaranteed here).
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
                row.set_equip_weight_change_rate(EQUIP_LOAD_MULTIPLIER);
                PARAM_PATCHED.store(true, Ordering::Relaxed);
                log::info!(
                    "no_equip_load: SpEffect {SP_EFFECT_ID} equipWeightChangeRate -> \
                     {EQUIP_LOAD_MULTIPLIER} (was 1; allItemWeightChangeRate is NOT written -- it \
                     is a sentinel, never a multiplier)"
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
    if !(before > 0.0) {
        VERIFIED.store(false, Ordering::Relaxed);
        LOAD_BEFORE_BITS.store(NOT_SAMPLED, Ordering::Relaxed);
        return;
    }
    if after > before * 1.5 {
        log::info!(
            "no_equip_load: WORKING -- max equip load {before:.1} -> {after:.1} with SpEffect \
             {SP_EFFECT_ID} resident"
        );
    } else {
        log::warn!(
            "no_equip_load: INERT -- SpEffect {SP_EFFECT_ID} is resident on the player and \
             equipWeightChangeRate is {EQUIP_LOAD_MULTIPLIER}, but max equip load did not move \
             ({before:.1} -> {after:.1}). The row is being applied and the field is not taking. \
             Most likely a co-loaded mod shipping its own regulation.bin in which {SP_EFFECT_ID} \
             is NOT a spare row -- check the mod stack before changing the field again"
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
