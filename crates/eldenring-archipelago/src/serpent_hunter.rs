//! Force the Serpent-Hunter's wave moveset on, and PROBE the fight if that is not enough.
//!
//! boblerrr, 2026-08-07 15:23: "it equipped but weapon dont work" -- Alaric: "oh like it doesn't
//! have the special effect?" -- "exactly". #102 gets the spear into his hand; this is the other
//! half of the same report, and the two are independent.
//!
//! THE WRITE. `EquipParamWeapon` row 17030000 ships `-1` in all three RESIDENT SpEffect fields, and
//! the game turns the wave moveset on from somewhere else during the fight. Writing SpEffect 1908
//! into a free resident slot is the community's standing fix -- see
//! [`er_logic::serpent_hunter`] for the provenance and for what is NOT measured about it. The
//! decision (which slot, or none) lives there and is host-tested; this module is the I/O.
//!
//! 🛑 THIS IS NOT A `safe_speffect_rows` CLAIM, and the gate does not apply. That policy governs
//! REPURPOSING a no-op `SpEffectParam` row by rewriting its fields. Nothing here touches
//! `SpEffectParam` at all: 1908 is a vanilla row used AS-IS, and the only write is an `i32` into an
//! `EquipParamWeapon` slot that vanilla leaves at `-1`.
//!
//! ⚠️ CONSEQUENCE, STATED PLAINLY: a resident SpEffect is always-on while the weapon is equipped,
//! so this makes the waves fire ANYWHERE, not only against Rykard. On a randomiser where the spear
//! can be found anywhere and enemy rando can move Rykard into a DLC arena (bobler's 2026-08-07 log
//! has exactly that -- his Rykard healthbar came up in play region 68410), "only in the vanilla
//! arena" is not a behaviour worth preserving. It is deliberately NOT an option: an option is a
//! slot-data key, and a slot-data key is a contract change and a version band.
//!
//! 🛑🛑 THE PARAM WRITE ALONE IS NOT ENOUGH -- MEASURED, 2026-08-07 19:56:29. The resident slot is
//! read when the weapon is EQUIPPED and never re-evaluated, so a row edited under an
//! already-equipped weapon is INERT until it is re-equipped. The probe caught it the first session
//! it shipped:
//!
//! ```text
//! 19:23:15  auto_equip: slot 1 <- 0x0103db70 (param 17030000, ...)      <- spear goes in hand
//! 19:56:29  serpent-hunter PROBE row 17030000: resident=[-1,-1,-1] behavior=[-1,-1,-1]
//! 19:56:29  serpent-hunter: wave SpEffect 1908 -> resident slot 0 (read-back OK, now [1908,-1,-1])
//! 19:56:29  serpent-hunter PROBE fight: ... wave_speffect_1908_active=false
//! ```
//!
//! The write landed and read back, and 1908 was still not on the player -- the spear had been in
//! his hand for 33 minutes. bobler then swapped weapons, walked back in, and the waves worked.
//!
//! So [`ensure_applied`] puts 1908 on the player DIRECTLY, the way `no_equip_load` /
//! `no_fall_damage` / `scadu_blessing` / `scaling` already do, and re-applies it whenever it goes
//! missing. That is not belt-and-braces over the param write, it is the load-bearing half: a map
//! load restores the vanilla row AND the player keeps holding the spear across it, so the rewrite
//! cannot rebind and the waves would die on every load without this. The param write stays because
//! it is still correct for any FUTURE equip, and it costs one i32.
//!
//! THE PROBE, which is the point of shipping the two together. If the write does not fix it, the
//! log must say what to do next rather than leaving us to guess again. Two halves:
//!   * STATIC -- the row's six SpEffect fields as vanilla shipped them, logged once before we
//!     touch anything. Confirms the `-1`s, and shows the BEHAVIOR triple too, so if resident turns
//!     out to be the wrong triple the log already names the alternative.
//!   * LIVE -- the player's whole active SpEffect list while Rykard's healthbar is up. THAT is the
//!     measurement nothing has taken: it distinguishes "the arena applies 1908 to the player"
//!     (it will be in the list in a vanilla fight, absent in a broken one) from "the wave is gated
//!     on the TARGET", which the wikis lean toward and which a resident SpEffect cannot fix.

use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{ChrInsExt, EquipParamWeapon, SoloParamRepository, WorldChrMan};
use er_logic::serpent_hunter::{ResidentWrite, SERPENT_HUNTER_ROW, WAVE_SPEFFECT, resident_write};
use fromsoftware_shared::FromStatic;

static APPLIED: AtomicBool = AtomicBool::new(false);
/// Has the STATIC half of the probe been logged this session? Separate from `APPLIED` so a map
/// load re-arms the write without re-spamming a line whose content cannot change.
static PROBED_ROW: AtomicBool = AtomicBool::new(false);
/// Has the "applied directly" line been said this session? The APPLY itself repeats by design
/// (it heals a cleared list every tick); the LINE must not, or a load spams the log.
static ANNOUNCED: AtomicBool = AtomicBool::new(false);

/// Have we dumped the player's SpEffect list for the CURRENT Rykard fight? Re-armed when the bar
/// drops, so a re-fight probes again instead of the first fight of a session being the only one.
static PROBED_FIGHT: AtomicBool = AtomicBool::new(false);

/// Re-arm the once-per-session write. Called from core.rs's `in_world` false->true edge.
///
/// 🛑 A MAP LOAD STREAMS `EquipParamWeapon` BACK IN AND RESTORES THE VANILLA ROW -- this is the
/// `no_weapon_reqs` lesson verbatim, where the option worked until the player's first load and
/// then quietly stopped. bobler reloads constantly, so without this the fix would look flaky
/// rather than absent, which is strictly harder to diagnose.
pub fn reset() {
    APPLIED.store(false, Ordering::Relaxed);
}

/// Per-tick until applied: put the wave SpEffect in the spear's resident slot.
pub fn tick() {
    if APPLIED.load(Ordering::Relaxed) {
        return;
    }
    // MENU/BOOT GATE: the param repo's holders exist but are not settled before the world is up,
    // and `rows_mut` panics there. Same signal every other param writer in this crate gates on.
    if !crate::flags::in_world() {
        return;
    }
    // SAFETY: FD4 singleton; only mutated on the single-threaded FrameBegin tick.
    let Ok(repo) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    let row_id = SERPENT_HUNTER_ROW as u32;
    for (id, row) in repo.rows_mut::<EquipParamWeapon>() {
        if id != row_id {
            continue;
        }
        let resident = [
            row.resident_sp_effect_id(),
            row.resident_sp_effect_id1(),
            row.resident_sp_effect_id2(),
        ];
        if !PROBED_ROW.swap(true, Ordering::Relaxed) {
            log::info!(
                "serpent-hunter PROBE row {SERPENT_HUNTER_ROW}: resident={:?} behavior={:?} \
                 (vanilla ships -1 in the resident triple; behavior is logged so a wrong-triple \
                 diagnosis needs no second build)",
                resident,
                [
                    row.sp_effect_behavior_id0(),
                    row.sp_effect_behavior_id1(),
                    row.sp_effect_behavior_id2(),
                ],
            );
        }
        match resident_write(resident, WAVE_SPEFFECT) {
            ResidentWrite::AlreadyPresent(i) => {
                log::info!(
                    "serpent-hunter: wave SpEffect {WAVE_SPEFFECT} already in resident slot {i} -- \
                     nothing to do"
                );
            }
            ResidentWrite::Slot(i) => {
                match i {
                    0 => row.set_resident_sp_effect_id(WAVE_SPEFFECT),
                    1 => row.set_resident_sp_effect_id1(WAVE_SPEFFECT),
                    _ => row.set_resident_sp_effect_id2(WAVE_SPEFFECT),
                }
                // READ BACK. A write we did not confirm is a claim, not a fact -- the same reason
                // shop_sell reads its rows back. If this line ever disagrees, the row is not where
                // we think it is and every downstream conclusion is void.
                let after = [
                    row.resident_sp_effect_id(),
                    row.resident_sp_effect_id1(),
                    row.resident_sp_effect_id2(),
                ];
                if after[i] == WAVE_SPEFFECT {
                    log::info!(
                        "serpent-hunter: wave SpEffect {WAVE_SPEFFECT} -> resident slot {i} \
                         (read-back OK, now {after:?}); the spear keeps its Rykard moveset \
                         everywhere while equipped"
                    );
                } else {
                    log::warn!(
                        "serpent-hunter: WROTE {WAVE_SPEFFECT} to resident slot {i} but read back \
                         {after:?} -- the write did not stick, so the waves will NOT fire"
                    );
                }
            }
            ResidentWrite::NoFreeSlot => {
                // On the vanilla row this cannot happen. If it does, something edited 17030000
                // first, and THAT is the finding -- not a reason to clobber it.
                log::warn!(
                    "serpent-hunter: all three resident slots on row {SERPENT_HUNTER_ROW} are \
                     occupied ({resident:?}) -- refusing to clobber; the waves will NOT fire"
                );
            }
        }
        APPLIED.store(true, Ordering::Relaxed);
        return;
    }
    // Row absent: the table is up but 17030000 is not in it. Retry next tick rather than latch --
    // that is the same "not ready yet" shape the rest of this crate treats as transient.
}

/// Keep SpEffect 1908 on the player while they own the Serpent-Hunter.
///
/// WHY DIRECTLY ON THE PLAYER, not left to the weapon row: the resident slot binds at EQUIP time
/// (module doc, measured). This is the only path that survives "already equipped" and "still
/// equipped across a map load", which between them cover almost every real session.
///
/// GATED ON POSSESSION, not applied unconditionally. What SpEffect 1908 does to a character NOT
/// holding the spear is UNMEASURED, and an always-on effect whose blast radius nobody has read is
/// how you ship a second bug while fixing the first. Owning the spear is a cheap, honest proxy for
/// "this player is here for the waves"; `holds_weapon_base` is the same bag read `boss_grants`
/// already makes each tick.
///
/// Self-correcting rather than latched: it re-applies the moment the id leaves the list, so a load,
/// a death or anything else that clears the player's effects heals on the next tick.
pub fn ensure_applied() {
    if !crate::flags::in_world() {
        return;
    }
    // `None` = the bag was unreadable this tick; that is "don't know", never "no".
    if crate::upgrades::holds_weapon_base(SERPENT_HUNTER_ROW) != Some(true) {
        return;
    }
    // SAFETY: FD4 singleton, mutated only on the single-threaded FrameBegin tick.
    let Ok(wcm) = (unsafe { WorldChrMan::instance_mut() }) else {
        return;
    };
    let Some(player) = wcm.main_player.as_mut() else {
        return;
    };
    // THE canonical predicate -- touching a dying character's SpEffect lists is where this crate's
    // CTD was first observed (`no_equip_load`'s module doc).
    if er_logic::death_guard::lists_unsafe_to_touch(player.chr_ins.modules.data.hp) {
        return;
    }
    let chr = &mut player.chr_ins;
    if chr
        .special_effect
        .entries()
        .any(|e| e.param_id == WAVE_SPEFFECT)
    {
        return;
    }
    chr.apply_speffect(WAVE_SPEFFECT, false);
    if !ANNOUNCED.swap(true, Ordering::Relaxed) {
        log::info!(
            "serpent-hunter: applied wave SpEffect {WAVE_SPEFFECT} to the player directly -- the \
             resident slot binds at EQUIP time, so the param write alone does nothing while the \
             spear is already in hand"
        );
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
         wave_speffect_{WAVE_SPEFFECT}_active={has_wave} -- if the waves did not fire AND this \
         says true, the effect is not what gates them and the next read is the TARGET's list"
    );
}
