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
//! ER_DOWNSTATE_PROBE_PLAYER=1          make the PLAYER the subject (see below)
//! ```
//!
//! ## Player-subject mode -- the TRAP-ITEMS question
//!
//! Everything measured so far applies a `20018xxx` row to an ENEMY. The trap-items design wants the
//! mirror image: on the PLAYER, `maxHpRate 0.25` is "your HP is quartered" and the attack rows are
//! "your damage is 30%". A whole graded family of traps falls out of work that is already
//! datamined, measured and merged -- **if these rows read on the player at all.**
//!
//! Nobody has ever checked. The player is a `ChrIns` and these rows leave their targeting flags at
//! 1, which is encouraging and is not evidence.
//!
//! Four deliberate differences from the enemy path:
//!
//! 1. **It applies `20018004` ONLY, not the pair.** `maxHpRate` is the sole effect of this block we
//!    can READ on a `ChrIns` -- the crate exposes no attack field, so `20018002` would add an
//!    unreadable variable and the one behavioural field in the block (`targetPriority`, whose
//!    meaning on the player is undefined). One row, one number, one conclusion.
//! 2. **It REMOVES the row when the watch finishes** and dumps again to prove it came off. These
//!    rows are `effectEndurance -1`; on an enemy that is harmless because the enemy dies, but on
//!    the player a permanent 0.25x max HP is a ruined session. Restore is a safety requirement
//!    first and a second reading second -- a `max_hp` that drops AND comes back is a far stronger
//!    result than one that merely drops.
//! 3. **A death guard on every access**, because quartering max HP makes dying likely and iterating
//!    `special_effect` at the death cam is this crate's original CTD (`er_logic::death_guard`).
//! 4. **It runs on its own `instance_mut()` path and returns before the enemy code**, so the
//!    shipped #346 instrument is not perturbed by the trap question sharing a file with it.
//!
//! 🛑 A pass here does NOT prove the attack half. It proves a `20018xxx` RATE COLUMN reaches the
//! player's derived stats. The attack rows are the same block, same `spCategory`, same shape, so
//! they very probably follow -- but "very probably" is the exact phrasing that broke enemy scaling
//! once. Damage dealt is the only observable for attack and this probe does not read it.
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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use eldenring::cs::{ChrIns, ChrInsExt, FieldInsHandle, WorldChrMan};
use fromsoftware_shared::FromStatic;

/// 0.25x HP, then 0.30x all-element attack. Applied in this order so that if the second call is the
/// one that misbehaves, the log still shows the first one's outcome in isolation.
const DOWNSTATE_PAIR: [i32; 2] = [20018004, 20018002];

/// The single row used in player-subject mode: `maxHpRate 0.25`. See the module docs for why the
/// PAIR is deliberately not used on the player.
const PLAYER_PROBE_ROW: i32 = 20018004;

/// One subject per session. A probe that WRITES must not be able to run twice by accident.
static DONE: AtomicBool = AtomicBool::new(false);
/// The subject we are waiting to find, once `last_hit_by` has named one.
static SUBJECT: Mutex<Option<FieldInsHandle>> = Mutex::new(None);

/// 🛑 v1 READ TOO EARLY, AND THAT IS THE WHOLE REASON FOR v2.
///
/// The 2026-08-05 run answered the question this probe was built for -- **both ids landed**:
///
/// ```text
/// BEFORE: npc_id=4480 chr_type=Npc hp=4162 speffects=[.., 7060, ..]
/// applied 20018004   AFTER: hp=4162 speffects=[.., 20018004]
/// applied 20018002   AFTER: hp=4162 speffects=[.., 20018004, 20018002]
/// ```
///
/// So the engine accepts a DLC-block `20018xxx` row on a base-game enemy and keeps it in the list.
/// But `hp` did not move -- and v1 could not tell you whether that means anything, for two reasons
/// it built in itself:
///
///   1. it dumped in the SAME tick as the apply, and the engine recalculates derived stats on a
///      later frame, so the read was simply too early; and
///   2. it logged `hp` -- the CURRENT value -- while `maxHpRate` changes `max_hp`. A full-health
///      enemy's current hp need not move at all until something re-clamps it.
///
/// v2 fixes both: it logs `max_hp` beside `hp`, reports whether each entry's `param_data` actually
/// resolved to a param row (a carried id with a NULL row would mean the id is not in this player's
/// table), and re-reads the subject on a schedule instead of once. Latching now happens when the
/// WATCH finishes, not when the apply returns.
static WATCH: Mutex<Option<Watch>> = Mutex::new(None);

/// Post-apply observation state. `frames` counts ticks since the apply; `dumps` counts how many
/// scheduled re-reads have been emitted.
struct Watch {
    subject: FieldInsHandle,
    frames: u32,
    dumps: u32,
}

/// Re-read the subject every this many ticks (~1s at 60fps) ...
const WATCH_INTERVAL: u32 = 60;
/// ... this many times, then latch. Four reads over ~4s is long enough that "the engine had not
/// recalculated yet" stops being an available excuse for a flat number.
const WATCH_DUMPS: u32 = 4;

fn enabled() -> bool {
    std::env::var_os("ER_DOWNSTATE_PROBE").is_some()
}

fn armed() -> bool {
    std::env::var_os("ER_DOWNSTATE_PROBE_ARM").is_some()
}

/// Player-subject mode: the trap-items question, not the #346 one.
fn player_mode() -> bool {
    std::env::var_os("ER_DOWNSTATE_PROBE_PLAYER").is_some()
}

/// Ticks elapsed in player mode. Separate from [`Watch`] because the player needs no handle: it is
/// resolved fresh from `main_player` every read, so there is nothing to store and match.
static PLAYER_FRAMES: AtomicU32 = AtomicU32::new(0);
/// Set once the row has been applied, so the tick knows to watch rather than re-apply.
static PLAYER_ARMED: AtomicBool = AtomicBool::new(false);

/// Log every scaling-relevant fact we can read off one enemy, and its whole active SpEffect list.
///
/// The full list matters, not just "is our id there": if the pair lands but something else is also
/// present, the HP number stops being attributable and we need to see that in the same line rather
/// than infer it later.
fn dump(chr: &ChrIns, tag: &str) {
    let ids: Vec<i32> = chr.special_effect.entries().map(|e| e.param_id).collect();
    // Ids whose `param_data` is NULL: the entry exists but resolved to no param row. That is the
    // difference between "the game took our id" and "the game has that row", and v1 could not tell
    // them apart because it logged only the id.
    let unresolved: Vec<i32> = chr
        .special_effect
        .entries()
        .filter(|e| e.param_data.is_none())
        .map(|e| e.param_id)
        .collect();
    log::info!(
        "[downstate-probe] {tag}: npc_id={} chr_type={:?} team={} hp={}/{} speffects={:?} \
         unresolved={:?}",
        chr.npc_id,
        chr.chr_type,
        chr.team_type,
        chr.modules.data.hp,
        chr.modules.data.max_hp,
        ids,
        unresolved,
    );
}

/// Locate a subject in the sets the sweep covers.
///
/// Reusing `sweepable_characters` deliberately: it carries the `chr_load_status` history and the
/// "walk the ENTRY, never the `ChrIns` behind it" discipline that the 2026-07-27 CTD taught us, and
/// a probe is not the place to reinvent that.
fn find_subject(wcm: &eldenring::cs::WorldChrMan, target: FieldInsHandle) -> Option<&mut ChrIns> {
    for chr in crate::scaling::sweepable_characters(&wcm.open_field_chr_set.base) {
        if chr.field_ins_handle == target {
            return Some(chr);
        }
    }
    for slot in wcm.chr_sets.iter().flatten() {
        for chr in crate::scaling::sweepable_characters(slot) {
            if chr.field_ins_handle == target {
                return Some(chr);
            }
        }
    }
    None
}

/// The post-apply watch: re-read the subject on a schedule, then latch.
///
/// Returns true when it handled this tick (so `tick` must not fall through into the apply path).
/// A subject that has unloaded simply stops producing reads and the watch still latches on schedule
/// -- a probe that waited forever for a corpse would be worse than one that says what it saw.
fn watch_step(wcm: &eldenring::cs::WorldChrMan) -> bool {
    let Ok(mut guard) = WATCH.lock() else {
        return false;
    };
    let Some(w) = guard.as_mut() else {
        return false;
    };
    w.frames += 1;
    if !w.frames.is_multiple_of(WATCH_INTERVAL) {
        return true;
    }
    w.dumps += 1;
    let (subject, dumps, secs) = (w.subject, w.dumps, w.frames / WATCH_INTERVAL);
    let finished = dumps >= WATCH_DUMPS;
    if finished {
        *guard = None;
    }
    drop(guard);

    match find_subject(wcm, subject) {
        Some(chr) => dump(chr, &format!("WATCH +{secs}s")),
        None => log::info!("[downstate-probe] WATCH +{secs}s: subject not loaded this tick"),
    }
    if finished {
        DONE.store(true, Ordering::Relaxed);
        log::info!("[downstate-probe] watch complete -- latched for this session");
    }
    true
}

/// Player-subject mode, self-contained: its own `instance_mut()`, its own counter, its own restore.
///
/// Kept entirely separate from the enemy path rather than threaded through it. The enemy probe is a
/// SHIPPED instrument for #346 whose readings are already on the record, and a trap-items question
/// arriving later has no business changing how it selects, applies or latches.
fn player_tick() {
    // SAFETY: FD4 singleton, mutated only on the single-threaded tick -- the same contract
    // `serpent_hunter` and `no_equip_load` rely on for their player writes.
    let Ok(wcm) = (unsafe { WorldChrMan::instance_mut() }) else {
        return;
    };
    let Some(player) = wcm.main_player.as_mut() else {
        return;
    };
    // DEATH GUARD before ANY `special_effect` access, including the first dump. Not a failure --
    // just try again next tick, once the player is alive and settled.
    if er_logic::death_guard::lists_unsafe_to_touch(player.chr_ins.modules.data.hp) {
        return;
    }
    let chr = &mut player.chr_ins;

    if !PLAYER_ARMED.load(Ordering::Relaxed) {
        dump(chr, "BEFORE (player)");
        if !armed() {
            log::info!(
                "[downstate-probe] player mode, observe-only (set ER_DOWNSTATE_PROBE_ARM=1 to \
                 apply {PLAYER_PROBE_ROW})"
            );
            DONE.store(true, Ordering::Relaxed);
            return;
        }
        chr.apply_speffect(PLAYER_PROBE_ROW, false);
        log::info!("[downstate-probe] applied {PLAYER_PROBE_ROW} to the PLAYER");
        dump(chr, "AFTER (player, same tick -- too early to mean anything, see WATCH)");
        PLAYER_ARMED.store(true, Ordering::Relaxed);
        log::info!(
            "[downstate-probe] watching the player's max_hp for {WATCH_DUMPS} reads at \
             {WATCH_INTERVAL}-tick intervals, then removing {PLAYER_PROBE_ROW}"
        );
        return;
    }

    let frames = PLAYER_FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    if !frames.is_multiple_of(WATCH_INTERVAL) {
        return;
    }
    let dumps = frames / WATCH_INTERVAL;
    dump(chr, &format!("WATCH +{dumps}s (player)"));
    if dumps < WATCH_DUMPS {
        return;
    }

    // 🛑 RESTORE IS MANDATORY, not politeness. `PLAYER_PROBE_ROW` is `effectEndurance -1`, so
    // without this the player carries 0.25x max HP for the rest of the session.
    chr.remove_speffect(PLAYER_PROBE_ROW);
    log::info!("[downstate-probe] removed {PLAYER_PROBE_ROW} from the player");
    dump(chr, "RESTORED (player)");
    DONE.store(true, Ordering::Relaxed);
    log::info!("[downstate-probe] player watch complete -- latched for this session");
}

/// Per-tick entry point. Call from `update_live` while in-world; self-latching and env-gated.
pub fn tick() {
    if !enabled() || DONE.load(Ordering::Relaxed) {
        return;
    }
    // Player mode is a different question with a different subject; it owns the tick outright and
    // never reaches the enemy selection below.
    if player_mode() {
        player_tick();
        return;
    }
    let Ok(wcm) = (unsafe { WorldChrMan::instance() }) else {
        return;
    };
    // The watch owns the tick once an apply has happened -- see WATCH for why v1's single
    // same-tick read could not answer its own question.
    if watch_step(wcm) {
        return;
    }
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

    let player_handle = player.field_ins_handle;
    let Some(chr) = find_subject(wcm, target) else {
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
        dump(
            chr,
            "AFTER (same tick -- too early to mean anything, see WATCH)",
        );
    }

    // Do NOT latch here. v1 did, and it threw away the only reads that could have shown an effect:
    // the engine recalculates derived stats on a later frame. Hand off to the watch instead.
    if let Ok(mut w) = WATCH.lock() {
        *w = Some(Watch {
            subject: target,
            frames: 0,
            dumps: 0,
        });
    }
    log::info!(
        "[downstate-probe] applied both -- watching max_hp for {} reads at {}-tick intervals",
        WATCH_DUMPS,
        WATCH_INTERVAL
    );
}
