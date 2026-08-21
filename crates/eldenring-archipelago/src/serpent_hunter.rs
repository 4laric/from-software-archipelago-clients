//! Keep the Serpent-Hunter's wave moveset ON during the Rykard fight and OFF everywhere else.
//!
//! boblerrr, 2026-08-07 15:23: "it equipped but weapon dont work" -- Alaric: "oh like it doesn't
//! have the special effect?" -- "exactly". #102 gets the spear into his hand; this turns the
//! moveset on for the fight. The wave is SpEffect 1908 applied to the WIELDER (provenance and the
//! equip-time measurement that killed the param-write approach: `er_logic::serpent_hunter`).
//!
//! ⭐ RULING REVERSED, 2026-08-21 (#345, Alaric). The first ruling made the waves global and said
//! so here: "'only in the vanilla arena' is not a behaviour worth preserving" -- because enemy
//! rando can move Rykard into any arena and ARENA-keyed gating would miss him (measured: a Rykard
//! healthbar in play region 68410). That reasoning stands; the conclusion changed because the
//! gate is FIGHT-keyed: `healthbar_shows(RYKARD_CHR_ID, ..)` follows Rykard wherever the
//! randomiser put him -- the same signal the grant already keys on (#594). With the spear a
//! randomized item any player can receive early, an always-on screen-wide wave attack was a
//! balance hole everywhere except the one fight it exists for. Consequences of the reversal:
//!   * the `EquipParamWeapon` resident-slot write is GONE (it enabled the waves at equip time,
//!     anywhere -- precisely the thing turned off). The row is left exactly as vanilla ships it.
//!   * [`ensure`] applies 1908 only while the fight is ON, and STRIPS it when the bar drops --
//!     victory, death and quit-out all close the window; walking out of a fight that despawned
//!     does too, because the bar is the window.
//!   * the decision is pure and lives in [`er_logic::serpent_hunter::wave_action`], including the
//!     two hard cases: a failed healthbar read freezes the hand (never strip on a blink), and
//!     nothing is EVER stripped mid-fight, even off an unreadable bag.
//!
//! THE PROBES stay, unchanged in spirit:
//!   * STATIC ([`probe_row`]) -- the row's six SpEffect fields as shipped, logged once per
//!     session, READ-ONLY now. If a player reports waves firing outside the fight, this line
//!     says whether some OTHER mod wrote the resident slot -- we no longer do.
//!   * LIVE ([`probe_fight`]) -- the player's whole active SpEffect list while Rykard's healthbar
//!     is up. `wave_speffect_1908_active=true` in-fight and absent out of it is #345's
//!     acceptance measurement (rule 11, the original report inverted).

use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{ChrInsExt, EquipParamWeapon, SoloParamRepository, WorldChrMan};
use er_logic::serpent_hunter::{SERPENT_HUNTER_ROW, WAVE_SPEFFECT, WaveAction, wave_action};
use fromsoftware_shared::FromStatic;

/// Has the STATIC half of the probe been logged this session? Its content cannot change.
static PROBED_ROW: AtomicBool = AtomicBool::new(false);
/// Has the "applied for the fight" line been said this session? The APPLY itself repeats by
/// design (it heals a cleared list every tick inside the window); the LINE must not.
static ANNOUNCED: AtomicBool = AtomicBool::new(false);
/// Have we dumped the player's SpEffect list for the CURRENT Rykard fight? Re-armed when the bar
/// drops, so a re-fight probes again instead of the first fight of a session being the only one.
static PROBED_FIGHT: AtomicBool = AtomicBool::new(false);

/// STATIC probe: log row 17030000's six SpEffect fields as the game currently has them. Once per
/// session, READ-ONLY -- #345 removed the write, so a non-vanilla triple here means some OTHER
/// mod edited the row, and that is exactly what this line exists to show a support thread.
pub fn probe_row() {
    if PROBED_ROW.load(Ordering::Relaxed) || !crate::flags::in_world() {
        return;
    }
    // SAFETY: FD4 singleton; read on the single-threaded FrameBegin tick.
    let Ok(repo) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    let row_id = SERPENT_HUNTER_ROW as u32;
    // #351: the probe is read-only, but upstream rows_mut still panics on a mid-restream
    // holder -- defer via param_guard (latch not set, re-runs next tick).
    let Some(rows) = crate::param_guard::rows_mut::<EquipParamWeapon>(repo, "serpent-hunter probe")
    else {
        return;
    };
    for (id, row) in rows {
        if id != row_id {
            continue;
        }
        PROBED_ROW.store(true, Ordering::Relaxed);
        log::info!(
            "serpent-hunter PROBE row {SERPENT_HUNTER_ROW}: resident={:?} behavior={:?} \
             (vanilla ships -1 in the resident triple; #345 removed our write, so anything else \
             here was another mod's hand)",
            [
                row.resident_sp_effect_id(),
                row.resident_sp_effect_id1(),
                row.resident_sp_effect_id2(),
            ],
            [
                row.sp_effect_behavior_id0(),
                row.sp_effect_behavior_id1(),
                row.sp_effect_behavior_id2(),
            ],
        );
        return;
    }
}

/// Per-tick wave keeper: 1908 ON the player while the Rykard fight is on and the spear is held;
/// OFF the player the moment the bar drops. The decision table (including "never strip on a
/// failed read" and "never strip mid-fight") is `er_logic::serpent_hunter::wave_action`.
pub fn ensure(fight_on: Option<bool>) {
    if !crate::flags::in_world() {
        return;
    }
    // `None` = the bag was unreadable this tick; that is "don't know", never "no". The pure half
    // treats it as apply-nothing, strip-regardless-outside-the-fight.
    let holds = crate::upgrades::held_weapon_row(SERPENT_HUNTER_ROW).map(|r| r.is_some());
    // SAFETY: FD4 singleton, mutated only on the single-threaded FrameBegin tick.
    let Ok(wcm) = (unsafe { WorldChrMan::instance_mut() }) else {
        return;
    };
    let Some(player) = wcm.main_player.as_mut() else {
        return;
    };
    // THE canonical predicate -- touching a dying character's SpEffect lists is where this
    // crate's CTD was first observed (`no_equip_load`'s module doc). Applies to REMOVE as much
    // as to apply.
    if er_logic::death_guard::lists_unsafe_to_touch(player.chr_ins.modules.data.hp) {
        return;
    }
    let chr = &mut player.chr_ins;
    let present = chr
        .special_effect
        .entries()
        .any(|e| e.param_id == WAVE_SPEFFECT);
    match wave_action(fight_on, holds, present) {
        WaveAction::Nothing => {}
        WaveAction::Apply => {
            chr.apply_speffect(WAVE_SPEFFECT, false);
            if !ANNOUNCED.swap(true, Ordering::Relaxed) {
                log::info!(
                    "serpent-hunter: wave SpEffect {WAVE_SPEFFECT} applied for the Rykard fight \
                     (fight-keyed, #345; it comes off when the bar drops)"
                );
            }
        }
        WaveAction::Remove => {
            chr.remove_speffect(WAVE_SPEFFECT);
            // One line per strip is one line per fight: after removal `present` is false and the
            // table goes quiet, so this cannot spam.
            log::info!(
                "serpent-hunter: Rykard's bar is down -- wave SpEffect {WAVE_SPEFFECT} removed \
                 (#345: the moveset belongs to the fight)"
            );
        }
    }
}

/// LIVE half of the probe: dump the player's active SpEffect list while Rykard's healthbar is up.
///
/// `fight_on` is `er_logic::boss_grants::healthbar_shows(...)` -- `None` is a FAILED READ and must
/// not re-arm the latch, or a blink mid-fight dumps a second time. Once per fight, re-armed when
/// the bar drops.
pub fn probe_fight(fight_on: Option<bool>) {
    match fight_on {
        Some(false) => {
            PROBED_FIGHT.store(false, Ordering::Relaxed);
            return;
        }
        None => return,
        Some(true) => {}
    }
    if PROBED_FIGHT.swap(true, Ordering::Relaxed) {
        return;
    }
    // SAFETY: FD4 singleton, read-only walk on the single-threaded FrameBegin tick.
    let Ok(wcm) = (unsafe { WorldChrMan::instance_mut() }) else {
        return;
    };
    let Some(player) = wcm.main_player.as_mut() else {
        return;
    };
    // THE canonical predicate, not a private copy -- touching the SpEffect lists of a dying
    // character is where this crate's CTD was first observed (`no_equip_load`'s module doc).
    if er_logic::death_guard::lists_unsafe_to_touch(player.chr_ins.modules.data.hp) {
        PROBED_FIGHT.store(false, Ordering::Relaxed); // not a real dump; let the next tick retry
        return;
    }
    let chr = &player.chr_ins;
    let ids: Vec<i32> = chr.special_effect.entries().map(|e| e.param_id).collect();
    let has_wave = ids.contains(&WAVE_SPEFFECT);
    log::info!(
        "serpent-hunter PROBE fight: Rykard's healthbar is up; player speffects={ids:?} \
         wave_speffect_{WAVE_SPEFFECT}_active={has_wave} -- #345's acceptance shape: true HERE, \
         absent once the bar is down"
    );
}
