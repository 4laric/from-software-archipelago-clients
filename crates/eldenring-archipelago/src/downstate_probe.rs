//! downstate_probe -- the ONE live measurement #346 phase 1b is blocked on. Hard no-op unless
//! `ER_DOWNSTATE_PROBE` is set, and read-only unless `ER_DOWNSTATE_PROBE_ARM` is ALSO set.
//!
//! ## The question, and why nothing on a desk can answer it
//!
//! The scaling ladder has no rung below 1.0, so a hand-tuned enemy can be scaled up and never down
//! ([`crate::scaling`], issue #346). The full 11,325-row `SpEffectParam` datamine says no SINGLE row
//! in the game scales HP *and* attack below 1.0 -- 20 rows are under 1.0 HP, 25 are under 1.0 attack,
//! zero are both. What does exist is a PAIR from the DLC ally-tuning block, and because these are
//! `spCategory 0` they stack (that is the same property that forces the sweep to clear the `70xx`
//! before applying its own):
//!
//! | id | effect | fields differing from the identity row `7000` |
//! |---|---|---|
//! | `20018004` | 0.25x HP | 8 -- `maxHpRate`, `targetPriority 0 -> 1`, 6x `regist*ChangeRate` |
//! | `20018002` | 0.30x all-element attack | 12 -- 5 attack rates, `targetPriority 0 -> -0.5`, 6x `regist*` |
//!
//! Both `effectEndurance -1` (infinite), `conditionHp -1` (unconditional), no `stateInfo`, no
//! `vfxId`, no icon. So on paper it is clean. On paper is the problem: **no one has ever applied a
//! `20018xxx` row to a base-game enemy.** Whether the engine honours it there is a RUNTIME fact, and
//! the datamine is structurally unable to settle it.
//!
//! 🛑 This file has already shipped one change that inferred a runtime meaning from a name, flagged
//! it "UNVERIFIED IN-GAME", and broke enemy scaling for every player -- see the `chr_load_status`
//! postmortem on [`crate::scaling::sweepable_characters`]. Not twice.
//!
//! ## Why this is in the client and not in Cheat Engine
//!
//! It was tried in CE first (2026-08-05) and never landed a single effect. Structure READS were
//! perfect -- every offset derived from the crate matched the live game, confirmed against two
//! independent vtable RVAs -- but calling `ChrIns::apply_speffect` from a CE-created thread did
//! nothing via `executeCodeEx` and faulted via an assembled stub. The control settles what that
//! means: `7200`, a rung the sweep applies successfully every tick, ALSO failed to land. So it was
//! the call mechanism, not the ids.
//!
//! The bytes at the RVA start `lea rcx, [rip+...]` -- a function whose first act is to clobber the
//! `this` register -- and the shared crate carries an `arxan` module, so ER's code is protected and
//! `module_base + rva` is not a safe assumption from outside. In here we go through the crate, on
//! the same code path the sweep uses, inside the game's own update loop. That is not a workaround
//! for the CE failure; it is the more faithful test.
//!
//! ## Using it
//!
//! ```text
//! ER_DOWNSTATE_PROBE=1                 observe only: name a subject and dump its state
//! ER_DOWNSTATE_PROBE=1 ER_DOWNSTATE_PROBE_ARM=1    also apply the pair, once, to one subject
//! ```
//!
//! **The subject is whatever last hit you.** Let one trash enemy land a hit and it becomes the
//! subject; the probe latches after one subject per session so it cannot cascade across a fight.
//! Pick something with a long health bar and hit it a few times first, so a 0.25x clamp is obvious.
//!
//! ## Reading the result
//!
//! * `hp` collapsing to roughly a quarter -> `maxHpRate` from a `20018xxx` row is honoured on a
//!   base-game enemy, and phase 1b's three down-states are buildable.
//! * ids present in the dump but `hp` unmoved -> the row applies but its rates do not reach an
//!   enemy. Phase 1b is dead in this form.
//! * ids ABSENT from the dump after the apply -> the row is not in the loaded param table at all
//!   (it ships only in DLC param data). Phase 1b is dead outright.
//!
//! Any of the three is a result. Attach the log to #346.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{ChrIns, ChrInsExt, FieldInsHandle, WorldChrMan};
use fromsoftware_shared::FromStatic;

/// 0.25x HP, then 0.30x all-element attack. Applied in this order so that if the second call is the
/// one that misbehaves, the log still shows the first one's outcome in isolation.
const DOWNSTATE_PAIR: [i32; 2] = [20018004, 20018002];

/// One subject per session. A probe that WRITES must not be able to run twice by accident.
static DONE: AtomicBool = AtomicBool::new(false);
/// The subject we are waiting to find, once `last_hit_by` has named one.
static SUBJECT: Mutex<Option<FieldInsHandle>> = Mutex::new(None);

fn enabled() -> bool {
    std::env::var_os("ER_DOWNSTATE_PROBE").is_some()
}

fn armed() -> bool {
    std::env::var_os("ER_DOWNSTATE_PROBE_ARM").is_some()
}

/// Log every scaling-relevant fact we can read off one enemy, and its whole active SpEffect list.
///
/// The full list matters, not just "is our id there": if the pair lands but something else is also
/// present, the HP number stops being attributable and we need to see that in the same line rather
/// than infer it later.
fn dump(chr: &ChrIns, tag: &str) {
    let ids: Vec<i32> = chr.special_effect.entries().map(|e| e.param_id).collect();
    log::info!(
        "[downstate-probe] {tag}: npc_id={} chr_type={:?} team={} hp={} speffects={:?}",
        chr.npc_id,
        chr.chr_type,
        chr.team_type,
        chr.modules.data.hp,
        ids,
    );
}

/// Per-tick entry point. Call from `update_live` while in-world; self-latching and env-gated.
pub fn tick() {
    if !enabled() || DONE.load(Ordering::Relaxed) {
        return;
    }
    let Ok(wcm) = (unsafe { WorldChrMan::instance() }) else {
        return;
    };
    let Some(player) = wcm.main_player.as_ref() else {
        return;
    };

    // Pick the subject: the last thing that hit us. Cheap, unambiguous, and it needs no lock-on
    // plumbing -- the crate exposes a target POSITION but no target handle, and `last_hit_by` is a
    // real `FieldInsHandle` we can match against the sets the sweep already walks.
    {
        let Ok(mut subject) = SUBJECT.lock() else {
            return;
        };
        let hit_by = player.chr_ins.last_hit_by;
        if subject.is_none() {
            if hit_by.is_empty() {
                return;
            }
            log::info!("[downstate-probe] subject selected from last_hit_by: {hit_by:?}");
            *subject = Some(hit_by);
        }
    }
    let Some(target) = SUBJECT.lock().ok().and_then(|s| *s) else {
        return;
    };

    // Find it in the sets the sweep covers. Reusing `sweepable_characters` deliberately: it carries
    // the chr_load_status history and the "walk the ENTRY, never the ChrIns behind it" discipline
    // that the 2026-07-27 CTD taught us, and a probe is not the place to reinvent that.
    let player_handle = player.field_ins_handle;
    let mut found: Option<&mut ChrIns> = None;
    for chr in crate::scaling::sweepable_characters(&wcm.open_field_chr_set.base) {
        if chr.field_ins_handle == target {
            found = Some(chr);
            break;
        }
    }
    if found.is_none() {
        for slot in wcm.chr_sets.iter().flatten() {
            for chr in crate::scaling::sweepable_characters(slot) {
                if chr.field_ins_handle == target {
                    found = Some(chr);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
    }
    let Some(chr) = found else {
        return; // not loaded this tick; try again next one
    };
    if chr.field_ins_handle == player_handle {
        log::info!("[downstate-probe] subject resolved to the PLAYER -- refusing");
        DONE.store(true, Ordering::Relaxed);
        return;
    }

    dump(chr, "BEFORE");

    if !armed() {
        log::info!(
            "[downstate-probe] observe-only (set ER_DOWNSTATE_PROBE_ARM=1 to apply {:?})",
            DOWNSTATE_PAIR
        );
        DONE.store(true, Ordering::Relaxed);
        return;
    }

    for id in DOWNSTATE_PAIR {
        chr.apply_speffect(id, false);
        log::info!("[downstate-probe] applied {id}");
        dump(chr, "AFTER");
    }

    // 🛑 Latch BEFORE anything else can re-enter. The sweep re-derives an enemy's state from what it
    // currently carries and will happily strip these on its next pass (they are outside its clear
    // range, so it will not -- but that is a property of today's ranges, not a guarantee), so a probe
    // that could fire twice would make its own result unreadable.
    DONE.store(true, Ordering::Relaxed);
    log::info!("[downstate-probe] done -- latched for this session");
}
