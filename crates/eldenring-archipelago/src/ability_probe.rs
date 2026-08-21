//! `ability_probe` -- LOG-ONLY probes for SPEC-ability-lock-mode (er-archipelago#945).
//!
//! The ability-lock spec gates seven of its eight abilities on per-button input masking, and
//! input masking is only safe if the client can tell "in world" from "in a menu" -- ER reuses
//! Circle/B (dodge) as menu-back and RB/LB (R1/L1) as menu tab-switchers, so a blind mask locks
//! the player out of their own inventory. This module buys the two measurements the spec's §4.2
//! names, with NO hooks at all: both probes are per-tick sampling of state the pinned crate
//! already maps, logged on change only.
//!
//! ## Probe 1 -- the menu-context predicate
//!
//! Every tick, sample a basket of candidate menu-state reads and log the basket ONLY when it
//! changes. The protocol (open/close each menu class in sequence) then tells us which signal --
//! if any -- tracks "a game menu is open":
//!
//!   * `CSMenuManImp.popup_menu.is_some()` -- the popup menu's presence pointer;
//!   * `player_menu_ctrl.chr_menu_flags` -- bit 3 is documented as `pause_menu_state` (set by
//!     TAE DISABLE_START_INPUTS, controls whether the pause menu CAN open; whether it tracks
//!     "is open" is exactly what the protocol measures);
//!   * `selected_goods_item` / `selected_magic_item` -- menu activity, already proven readable
//!     by hover_probe;
//!   * the player's `ChrDebugFlags` raw value -- does the game itself gate player actions while
//!     a menu is up?
//!
//! ## Probe 2 -- what Roundtable flips (the no-combat-zone mechanism)
//!
//! The same snapshot carries the player's `ChrDebugFlags` and the full active SpEffect list,
//! sampled continuously and logged on change plus on every play_region transition. `ChrDebugFlags`
//! bit 4 is `disabled_secondary_actions` -- the crate documents it as "Disables attack, jump,
//! crouch and any other actions except movement", which is the ability-lock semantic for seven
//! abilities in one bit. Walking into and out of the Roundtable Hold (play_region 11100) and
//! watching what the GAME sets answers the spec's second question: if vanilla's no-combat zone
//! is a flag or a SpEffect, enforcement may not need input masking at all. The continuous watch
//! also catches dialogue/cutscene flips, which are the menu predicate's false-positive hazards.
//!
//! ## Gate and safety
//!
//! **OFF by default.** `ER_ABILITY_PROBE=1` or `"probes": {"ability": true}` enables it; off is
//! one atomic load per tick. Every game read goes through the singletons' availability checks
//! plus `catch_unwind`, so a gameless host (CI) samples nothing and logs nothing. The module
//! writes NOTHING -- no detour, no state, no SpEffect.
//!
//! ## Protocol (posted with the PR)
//!
//! 1. Stand in the open world. Open and close, in order: pause menu, equipment tab (L1/R1
//!    through the tabs), the map, a merchant shop, a gesture menu. ~5s each.
//! 2. Talk to an NPC through one full dialogue.
//! 3. Walk into the Roundtable Hold, draw your weapon and TRY to swing (note in the log whether
//!    the swing happened), walk back out.
//! 4. Stop playing and post the log. The basket lines are the menu predicate's candidates; the
//!    region-transition dumps are Roundtable's answer.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use fromsoftware_shared::FromStatic;

/// Set by `install()` when the gate is on. The only thing `tick()` checks before doing work.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// The Roundtable Hold's play_region id (world repo `area_locks.py`: 11100 is the HUB, excluded
/// from kick geometry). The transition dump calls it out by name so the Roundtable answer is
/// findable in the log without cross-referencing ids.
const ROUNDTABLE_PLAY_REGION: i32 = 11100;

/// One tick's basket. Compared field-by-field against the previous tick; any difference logs
/// the WHOLE new basket (a diff of raw values is how an unnamed bit still shows up -- the crate
/// maps named bits, Roundtable may flip one nobody named).
#[derive(Clone, PartialEq)]
struct Snapshot {
    play_region: i32,
    /// `ChrDebugFlags` as its Debug string -- the bitfield's raw tuple field is private, and the
    /// Debug impl prints the raw value with named bits, which is exactly the change signal a
    /// probe needs. A string compare is once per tick; not a hot path.
    debug_flags: String,
    popup_menu: bool,
    /// `ChrMenuFlags` raw, same Debug-string treatment.
    chr_menu_flags: String,
    selected_goods: Option<u32>,
    selected_magic: Option<u32>,
    /// The player's full active SpEffect id list (the downstate-probe enumeration). Probe 2's
    /// primary suspect: a Roundtable "no combat" row would appear here on entry and leave on exit.
    speffects: Vec<i32>,
}

static LAST: Mutex<Option<Snapshot>> = Mutex::new(None);

/// One-time banner + protocol, called from the connect path next to `hover_probe::install`.
/// No hooks exist to build -- both probes are pure sampling -- so "install" is the gate check
/// and the instructions.
pub fn install() {
    if !shared::probes::enabled("ER_ABILITY_PROBE", "ability") {
        return;
    }
    ACTIVE.store(true, Ordering::Relaxed);
    log::info!(
        "ability-probe: ACTIVE -- menu-context + Roundtable state sampler (SPEC-ability-lock-mode, \
         er-archipelago#945). Protocol: (1) open/close pause menu, equipment (tab through with \
         the shoulder buttons), map, a merchant, gestures -- ~5s each; (2) one full NPC \
         dialogue; (3) walk into the Roundtable Hold, TRY to attack, walk out; (4) stop and post \
         the log. Log-only: this module writes nothing."
    );
}

/// Sample the basket. `None` on any singleton being down (main menu, mid-load, gameless CI
/// host) -- the probe says nothing rather than guess, the same discipline as hover_probe's
/// `sample_selected_goods`.
fn sample() -> Option<Snapshot> {
    std::panic::catch_unwind(|| {
        let play_region = crate::flags::play_region_id()?;
        let wcm = unsafe { eldenring::cs::WorldChrMan::instance() }.ok()?;
        let player = wcm.main_player.as_ref()?;
        let mm = unsafe { eldenring::cs::CSMenuManImp::instance() }.ok()?;
        Some(Snapshot {
            play_region,
            debug_flags: format!("{:?}", player.debug_flags),
            popup_menu: mm.popup_menu.is_some(),
            chr_menu_flags: format!("{:?}", mm.player_menu_ctrl.chr_menu_flags.flags),
            selected_goods: mm.player_menu_ctrl.selected_goods_item.param_id(),
            selected_magic: mm.player_menu_ctrl.selected_magic_item.param_id(),
            speffects: player
                .special_effect
                .entries()
                .map(|e| e.param_id)
                .collect(),
        })
    })
    .ok()
    .flatten()
}

/// Per-tick sampler, called next to `hover_probe::tick`. Logs the basket on change only; a
/// play_region transition gets its own louder line because that is probe 2's event.
pub fn tick() {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let Some(cur) = sample() else {
        return;
    };
    let mut last = match LAST.lock() {
        Ok(l) => l,
        Err(_) => return,
    };
    if last.as_ref() == Some(&cur) {
        return;
    }
    let region_changed = last
        .as_ref()
        .is_none_or(|l| l.play_region != cur.play_region);
    if region_changed {
        let tag = if cur.play_region == ROUNDTABLE_PLAY_REGION {
            " -- THE ROUNDTABLE HOLD: what just flipped is the no-combat mechanism"
        } else {
            ""
        };
        log::info!("ability-probe: PLAY_REGION -> {}{tag}", cur.play_region,);
    }
    log::info!(
        "ability-probe: pr={} popup={} chr_menu_flags={} sel_goods={:?} sel_magic={:?} \
         debug_flags={} speffects={:?}",
        cur.play_region,
        cur.popup_menu,
        cur.chr_menu_flags,
        cur.selected_goods,
        cur.selected_magic,
        cur.debug_flags,
        cur.speffects,
    );
    *last = Some(cur);
}

/// Session teardown (disconnect/reset), wired next to `hover_probe::reset`: drop the last
/// basket so a reconnect's first sample logs fresh instead of diffing against a dead session.
pub fn reset() {
    if let Ok(mut l) = LAST.lock() {
        *l = None;
    }
}
