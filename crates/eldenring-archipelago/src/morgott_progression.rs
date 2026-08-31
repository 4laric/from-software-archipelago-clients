//! Game-side repair for Morgott's post-boss golden progression seals.
//!
//! Vanilla m11_00 event 11002501 observes Morgott defeat, applies blocking SpEffects 4280/4282,
//! then waits for Erdtree-interaction flag 11000500 before applying 4281/4283 and setting final
//! seal state 11000501. A randomized arena boss can set the defeat flag without leaving the removed
//! Melina/interaction flow reachable. We synthesize only 11000500 and let vanilla finish its own
//! effect sequence.

use std::sync::atomic::{AtomicBool, Ordering};

use er_logic::morgott_progression::{
    ERDTREE_APPROACHED_FLAG, MORGOTT_DEFEAT_FLAG, MORGOTT_PROGRESSION_COMPLETE_FLAG,
    MorgottProgressionState, should_mark_erdtree_approached,
};

static WARNED: AtomicBool = AtomicBool::new(false);

pub fn tick() {
    if !crate::flags::in_world() {
        return;
    }

    let state = MorgottProgressionState {
        defeated: crate::flags::get_event_flag(MORGOTT_DEFEAT_FLAG),
        erdtree_approached: crate::flags::get_event_flag(ERDTREE_APPROACHED_FLAG),
        progression_complete: crate::flags::get_event_flag(MORGOTT_PROGRESSION_COMPLETE_FLAG),
    };
    if !should_mark_erdtree_approached(state) {
        WARNED.store(false, Ordering::Relaxed);
        return;
    }

    let accepted = crate::flags::try_set_event_flag(ERDTREE_APPROACHED_FLAG, true);
    if crate::flags::get_event_flag(ERDTREE_APPROACHED_FLAG) {
        WARNED.store(false, Ordering::Relaxed);
        log::info!(
            "Morgott progression: arena defeat {MORGOTT_DEFEAT_FLAG} set; repaired Erdtree approach \
             flag {ERDTREE_APPROACHED_FLAG}. Vanilla event 11002501 owns final seal transition \
             {MORGOTT_PROGRESSION_COMPLETE_FLAG}"
        );
    } else if !WARNED.swap(true, Ordering::Relaxed) {
        log::warn!(
            "Morgott progression: Erdtree approach flag {ERDTREE_APPROACHED_FLAG} did not stick \
             (write accepted={accepted}); retrying while in world"
        );
    }
}
