//! scadu_blessing — make the Scadutree blessing a game-wide power curve (Lever D).
//!
//! ## What was wrong
//!
//! `global_scadutree_blessing` shipped with a client that raised the STORED blessing byte
//! (`PlayerGameData::scadutree_blessing`, "Lever A") and an option help text asserting the engine
//! gates that byte to the DLC. On 2026-07-29 the assertion was finally measured, in-game, and it is
//! TRUE: stored byte at 20, rest at a Land of Shadow grace, `effects()` lists `20000120`; warp to
//! Limgrave and the byte is STILL 20 but the rung is gone. **So both live modes have never done
//! anything outside the DLC.** An option named "global" could not be global as built.
//!
//! ## Why, exactly — and why that is good news
//!
//! Every vanilla rung `20000100 + level` carries `effectEndurance = 0.05` — 50ms, three frames. It
//! is not a persistent buff. A refresher loop that only runs in the Land of Shadow re-applies it
//! every tick; outside, the loop simply doesn't run and the effect EXPIRES. Nothing strips anything.
//! Proven both ways in-game: a manual apply in Limgrave is present immediately and gone seconds
//! later, and the same rung with `effectEndurance = -1` is still there after 10s and halves damage
//! taken against the Tree Sentinel. **The map scoping lives only in the refresh loop; the damage
//! pipeline honours the rates wherever the effect is active.**
//!
//! That kills the obvious fix (re-apply on map load: a 0.05s effect against a 1s tick is live ~5% of
//! the time) and it makes this module's approach the correct one for a concrete reason rather than
//! as defensive over-engineering.
//!
//! ## The approach: clone the rung onto a row of our own
//!
//! Read the 18 rate fields off the vanilla rung at runtime, write them into a vetted no-op row with
//! `effectEndurance = -1`, and apply THAT. This is the pattern already shipping twice here
//! (`no_equip_load` = row 20012080, `no_fall_damage` = 20010827); ours is
//! [`er_logic::safe_speffect_rows::SCADU_BLESSING`] = 20012081. The registry lives in `er-logic`
//! rather than beside the appliers so its duplicate-claim test runs in host CI, not only on Windows.
//!
//! It is immune to every mechanism by which the engine could scope the blessing, without our having
//! to know which one is real: our row is outside `20000100..=20000120`, carries `stateInfo = 0` (not
//! the ladder's `472` category tag), and is derived from nothing the engine reads.
//!
//! 🛑 **Not** "set `effectEndurance = -1` on the vanilla rung and be done." That works — it is how
//! the mechanism was measured — but inside the DLC the engine still re-applies that row every tick,
//! and permanent-under-per-tick-reapply is untested (refresh vs stack). Cloning leaves the vanilla
//! refresh path completely untouched, which is the entire reason to clone rather than patch in place.
//!
//! 🛑 **Never hardcode 20000100.** The base id is `GameSystemCommonParam[0].baseScaduBlessingSpEffectId`
//! and `eldenring 0.14` binds it with a getter. Reading it at runtime is version-proof and keeps us
//! from carrying FromSoft's numbers (`er-foreign-list-provenance-rule`).
//!
//! ## Composition inside the DLC
//!
//! In a DLC seed a real rung `k` may be live under us. The clone therefore carries the RATIO
//! `A(t)/A(k)`, not `A(t)` — see `er_logic::upgrades::clone_rates`, which is where the arithmetic
//! lives so CI can test it. `k = 0` (the whole base game) gives the full `A(t)`, so one code path
//! covers both and there is no double-dip case to special-case.
//!
//! ## Wiring
//!
//! Driven from `upgrades::tick_global_scadu()` — it already owns the mode gate, the `in_world()`
//! gate, the ~1s throttle and the fragment walk, and reusing it means an `off` seed makes ZERO new
//! game accesses. We have an open CTD on the boss-sweep payout path; a second independent per-tick
//! speffect walk would make that one harder to triage, not easier.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use eldenring::cs::{
    ChrInsExt, GameSystemCommonParam, SoloParamRepository, SpEffectParam, WorldChrMan,
};
use fromsoftware_shared::FromStatic;

use er_logic::safe_speffect_rows::SCADU_BLESSING;

/// `scaduBlessingCap` from slot_data. 0 = absent => the ladder ceiling (see `apply_blessing_cap`).
static CAP: AtomicI32 = AtomicI32::new(0);

/// Last level we wrote into the clone row, plus the vanilla rung it was computed against. `-1` = we
/// have never written. Both matter: the clone must be rewritten when EITHER changes, because it
/// carries the ratio between them.
static LAST_TARGET: AtomicI32 = AtomicI32::new(-1);
static LAST_ACTIVE: AtomicI32 = AtomicI32::new(-1);

/// Set once we have logged the resolved base id, so the log line appears exactly once per session.
static LOGGED_BASE: AtomicBool = AtomicBool::new(false);

/// Set from slot_data `scaduBlessingCap` at connect. Absent/0 => no extra cap.
pub fn set_cap(cap: i32) {
    CAP.store(cap, Ordering::Relaxed);
}

/// Reset the "what did we last write" memory. Call on disconnect/seed change so a reconnect
/// re-syncs the row instead of trusting a cached level from a different seed
/// (`er-seed-change-bypasses-the-marker-guard` — a stale cross-seed cache is exactly how 229 checks
/// crossed seeds).
pub fn reset() {
    LAST_TARGET.store(-1, Ordering::Relaxed);
    LAST_ACTIVE.store(-1, Ordering::Relaxed);
}

/// Drive the applier for a computed blessing `level`.
///
/// Called from `upgrades::tick_global_scadu()` AFTER the mode gate, the `in_world()` gate and the
/// throttle, with the level `er_logic::upgrades::blessing_target` decided. Returns quietly on every
/// transient failure (param file not populated, player not placed, dead) — the caller re-runs next
/// throttle window.
pub fn drive(level: i32) {
    let target = er_logic::upgrades::apply_blessing_cap(level, CAP.load(Ordering::Relaxed));

    // ---- 1. Resolve the vanilla ladder's base id. Never hardcoded. -----------------------------
    // SAFETY: FD4 singleton; read-only; only touched on the single-threaded FrameBegin tick.
    let Ok(repo) = (unsafe { SoloParamRepository::instance() }) else {
        return;
    };
    let Some(cfg) = repo.get::<GameSystemCommonParam>(0) else {
        return; // param file not populated yet — retry next tick
    };
    let base = cfg.base_scadu_blessing_sp_effect_id();
    if base <= 0 {
        return; // nothing sane to index off
    }
    if !LOGGED_BASE.swap(true, Ordering::Relaxed) {
        log::info!(
            "scadu_blessing: ladder base = {base} (GameSystemCommonParam[0].baseScaduBlessingSpEffectId), clone row = {SCADU_BLESSING}"
        );
    }

    // ---- 2. Find the vanilla rung the engine has live under us (the DLC case). -----------------
    // DEATH GUARD FIRST: `chr_ins` and its `special_effect` list tear down at the death-cam
    // transition, and iterating there CTDs (`no_equip_load.rs:78-83`, the shipped instance). hp <= 0
    // => dead/dying => do nothing until respawn; the drive re-runs once hp > 0.
    // SAFETY: FD4 singleton; single-threaded tick.
    let Ok(wcm) = (unsafe { WorldChrMan::instance_mut() }) else {
        return;
    };
    let Some(player) = wcm.main_player.as_mut() else {
        return;
    };
    if player.chr_ins.modules.data.hp <= 0 {
        return;
    }
    let chr = &mut player.chr_ins;

    let ladder = base..=(base + er_logic::upgrades::SCADU_MAX_LEVEL);
    let active_level = chr
        .special_effect
        .entries()
        .find(|e| ladder.contains(&e.param_id))
        .map(|e| e.param_id - base)
        .unwrap_or(0);
    let already_applied = chr
        .special_effect
        .entries()
        .any(|e| e.param_id == SCADU_BLESSING);

    // ---- 3. Keep the clone row in sync with (target, active). ----------------------------------
    // Only on CHANGE: the row is already applied and permanent, so rewriting its fields changes the
    // live effect with no re-apply. A level-up is 18 float writes and nothing else — the same way
    // `no_equip_load` mutates a row already sitting on the player.
    let dirty = LAST_TARGET.load(Ordering::Relaxed) != target
        || LAST_ACTIVE.load(Ordering::Relaxed) != active_level;
    if dirty && !sync_clone_row(base, target, active_level) {
        return; // a source row wasn't readable this tick; try again next window
    }
    if dirty {
        LAST_TARGET.store(target, Ordering::Relaxed);
        LAST_ACTIVE.store(active_level, Ordering::Relaxed);
    }

    // ---- 4. Keep it applied. Idempotent: applying twice would stack duplicate entries. ----------
    if !already_applied {
        chr.apply_speffect(SCADU_BLESSING, false);
        log::info!("scadu_blessing: applied clone row {SCADU_BLESSING} (level {target})");
    }
}

/// Copy the 18 rate fields from the vanilla rungs into our clone, as the RATIO `A(target)/A(active)`,
/// and make it permanent. Returns false if either source row is unreadable this tick.
///
/// The two source reads and the write cannot share a borrow of the repository, so the values are
/// pulled into locals first and the `&mut` is taken after. That is not a style choice — `get` and
/// `get_mut` are `&self` / `&mut self` on the same singleton.
fn sync_clone_row(base: i32, target: i32, active_level: i32) -> bool {
    // Read A(target) and A(active). Any of the five attack channels will do: the ladder is ONE
    // scalar — all five carry the same value at every level, verified across all 21 rows.
    let (a_target, a_active) = {
        // SAFETY: FD4 singleton; read-only.
        let Ok(repo) = (unsafe { SoloParamRepository::instance() }) else {
            return false;
        };
        let Some(src) = repo.get::<SpEffectParam>((base + target) as u32) else {
            return false;
        };
        let a_target = src.atk_enemy_dmg_correct_rate_physics();
        let Some(act) = repo.get::<SpEffectParam>((base + active_level) as u32) else {
            return false;
        };
        (a_target, act.atk_enemy_dmg_correct_rate_physics())
    };

    // THE arithmetic, in er-logic so CI can test it (including the branches no corpus reaches:
    // a_active == 0, a_active > a_target, NaN).
    let (attack, cut) = er_logic::upgrades::clone_rates(a_target, a_active);

    // SAFETY: FD4 singleton; only mutated on the single-threaded FrameBegin tick.
    let Ok(repo) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return false;
    };
    let Some(dst) = repo.get_mut::<SpEffectParam>(SCADU_BLESSING as u32) else {
        return false;
    };

    // Damage DEALT — to enemies, and (the ladder does this too) to other players.
    dst.set_atk_enemy_dmg_correct_rate_physics(attack);
    dst.set_atk_enemy_dmg_correct_rate_magic(attack);
    dst.set_atk_enemy_dmg_correct_rate_fire(attack);
    dst.set_atk_enemy_dmg_correct_rate_thunder(attack);
    dst.set_atk_enemy_dmg_correct_rate_dark(attack);
    dst.set_atk_player_dmg_correct_rate_physics(attack);
    dst.set_atk_player_dmg_correct_rate_magic(attack);
    dst.set_atk_player_dmg_correct_rate_fire(attack);
    dst.set_atk_player_dmg_correct_rate_thunder(attack);
    dst.set_atk_player_dmg_correct_rate_dark(attack);

    // Damage TAKEN.
    dst.set_neutral_damage_cut_rate(cut);
    dst.set_slash_damage_cut_rate(cut);
    dst.set_blow_damage_cut_rate(cut);
    dst.set_thrust_damage_cut_rate(cut);
    dst.set_magic_damage_cut_rate(cut);
    dst.set_fire_damage_cut_rate(cut);
    dst.set_thunder_damage_cut_rate(cut);
    dst.set_dark_damage_cut_rate(cut);

    // THE load-bearing line. Without it the clone inherits 0.05s from wherever and expires three
    // frames later, exactly like the vanilla rung does outside the DLC.
    dst.set_effect_endurance(-1.0);

    // Deliberately NOT copied: `stateInfo`. The ladder sets it to 472, a category tag, and leaving
    // ours at 0 is one of the three reasons this row is immune to the engine's scoping.

    log::info!(
        "scadu_blessing: row {SCADU_BLESSING} <- level {target} over active {active_level} (attack x{attack:.4}, cut x{cut:.4})"
    );
    true
}
