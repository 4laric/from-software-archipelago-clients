//! no_fall_damage — the player never takes fall damage. This is exactly what a SPIRIT SPRING does:
//! the geyser applies a SpEffect whose `fallDamageRate` is ~0 so the long launch-and-drop can't hurt
//! you. We do the same permanently -- repurpose a safe no-op `SpEffectParam` row to `fallDamageRate =
//! 0.0` and keep it on the player, so any fall (including a mistimed spirit-spring or a warp-in) deals
//! no damage. `fallDamageRate` is a multiplier applied when landing damage is computed, so a rate of 0
//! zeroes it (verified field: `SP_EFFECT_PARAM_ST.fall_damage_rate`; `disableFallDamage` is a
//! HIT_MTRL_PARAM field, not a SpEffect, so it can't be applied to the player this way).
//!
//! Structure is identical to `no_equip_load`: one-time param edit, then apply/strip on the player each
//! tick. `SP_EFFECT_ID` (20010827) is from the vetted safe set (no-op vanilla rows, silent, permanent,
//! referenced NOWHERE in the regulation -- cross-ref'd the full Smithbox dump), so editing its
//! `fallDamageRate` and carrying it affects nothing else.

use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{ChrInsExt, SoloParamRepository, SpEffectParam, WorldChrMan};
use fromsoftware_shared::FromStatic;

/// A repurposed vanilla no-op `SpEffectParam` row (safe set; distinct from no_equip_load's 20012080).
const SP_EFFECT_ID: i32 = er_logic::safe_speffect_rows::NO_FALL_DAMAGE;
// CLAIMED, not chosen here: `er-logic/src/safe_speffect_rows.rs` is the single source of
// truth for repurposed rows and carries the eligibility criteria + the duplicate-claim test.

static ENABLED: AtomicBool = AtomicBool::new(false);
static PARAM_PATCHED: AtomicBool = AtomicBool::new(false);

/// Set from slot_data `options.no_fall_damage` at connect.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if on {
        log::info!(
            "no_fall_damage: enabled (fallDamageRate-0 SpEffect {SP_EFFECT_ID}, spirit-spring style)"
        );
    }
}

/// Re-arm the one-time param edit. Called from core.rs's `in_world` false->true edge: a map load
/// streams SpEffectParam back in, which restores our repurposed row's vanilla `fallDamageRate`, and
/// PARAM_PATCHED would otherwise stop `tick()` ever re-zeroing it -- the player would keep carrying
/// a row that no longer does anything, silently. Clears the LATCH ONLY: `ENABLED` is slot_data
/// configuration set once at connect. Re-running costs ONE row write (`get_mut` + one setter); the
/// player-side apply is already idempotent (it only applies when the entry is absent).
pub fn reset() {
    PARAM_PATCHED.store(false, Ordering::Relaxed);
}

/// Per-tick. When enabled + in-world: patch our SpEffect row to `fallDamageRate = 0` once, then keep
/// the player carrying it. When disabled: strip it. Idempotent -- apply only when absent so entries
/// don't stack. Same gating/pattern as `no_equip_load`.
pub fn tick() {
    // MENU/BOOT GATE: the param repo and chr sets aren't settled at boot / the main menu.
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
        // from a prior on-session is a harmless fallDamageRate=0 no-op row).
        return;
    }

    // One-time param edit: fallDamageRate -> 0 on our chosen row (enabled is guaranteed here).
    if !PARAM_PATCHED.load(Ordering::Relaxed) {
        // SAFETY: FD4 singleton; only mutated on the single-threaded FrameBegin tick.
        let Ok(repo) = (unsafe { SoloParamRepository::instance_mut() }) else {
            return;
        };
        // #351: param_guard, not raw get_mut -- a mid-restream holder (res-cap 0) panics
        // upstream instead of returning None.
        match crate::param_guard::get_mut::<SpEffectParam>(
            repo,
            SP_EFFECT_ID as u32,
            "no_fall_damage",
        ) {
            Some(row) => {
                row.set_fall_damage_rate(0.0);
                PARAM_PATCHED.store(true, Ordering::Relaxed);
                log::info!("no_fall_damage: SpEffect {SP_EFFECT_ID} fallDamageRate -> 0");
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
    // DEATH GUARD: the player's chr_ins + special_effect list tear down at the death-cam transition;
    // iterating/mutating them there CTDs. hp <= 0 = dead/dying -> skip until respawn (the apply
    // re-runs once hp > 0). Reading hp here is the same access DeathLink's read_local_hp does safely.
    if er_logic::death_guard::lists_unsafe_to_touch(player.chr_ins.modules.data.hp) {
        return;
    }
    let chr = &mut player.chr_ins;
    if !chr
        .special_effect
        .entries()
        .any(|e| e.param_id == SP_EFFECT_ID)
    {
        chr.apply_speffect(SP_EFFECT_ID, false);
    }
}
