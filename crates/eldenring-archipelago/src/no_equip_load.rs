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

use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{ChrInsExt, SoloParamRepository, SpEffectParam, WorldChrMan};
use fromsoftware_shared::FromStatic;

/// The repurposed vanilla no-op `SpEffectParam` row (see module doc for why it is safe).
const SP_EFFECT_ID: i32 = 20012080;

static ENABLED: AtomicBool = AtomicBool::new(false);
static PARAM_PATCHED: AtomicBool = AtomicBool::new(false);

/// Set from slot_data `options.no_equip_load` at connect.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if on {
        log::info!("no_equip_load: enabled (weightless SpEffect {SP_EFFECT_ID})");
    }
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
                row.set_all_item_weight_change_rate(0.0);
                PARAM_PATCHED.store(true, Ordering::Relaxed);
                log::info!("no_equip_load: SpEffect {SP_EFFECT_ID} allItemWeightChangeRate -> 0");
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
    if player.chr_ins.modules.data.hp <= 0 {
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
