//! death_award_sweep.rs — fire checks whose EMEVD corpse-award the game missed (clients#385).
//!
//! The decision lives in er_logic::death_award_sweep (host-tested); this file is the arm. The
//! shipped `death_award_pairs.json` (game data, beside the dll like the check-lot table) names
//! every `value == 0` corpse-award as a (death flag, check flag) pair. Death UP with check DOWN
//! in the live save means the award was missed and is unrecoverable in-game — the reload branch
//! force-kills the corpse without re-offering the loot — so this pass SETS the check flag, and
//! the ordinary check detection pays the location. Retroactive on any seed, because the death
//! flag persists in the save and the server remembers what is unsent.
//!
//! ONE pass per connect, latched: a death that happens mid-session gets its award the normal way
//! (or becomes this pass's business on the NEXT connect, after the reload that makes the loss
//! real). Sweeping continuously would race a live corpse the player is walking toward.
//!
//! The table is OPTIONAL at runtime: absent or malformed degrades to no sweep, said once, loudly
//! — a silent degrade here is an unpayable check nobody ever hears about.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Pairs retained for THIS seed (table ∩ the seed's check flags). None = not configured yet.
static RETAINED: Mutex<Option<Vec<(u32, u32)>>> = Mutex::new(None);
static DONE: AtomicBool = AtomicBool::new(false);

/// Called at slot_data parse with the seed's check-flag set (the same `loc_flags` values every
/// shop module receives). Loads and intersects the table; re-arms the sweep for the new session.
pub fn configure(table_path: PathBuf, seed_check_flags: HashSet<u32>) {
    DONE.store(false, Ordering::Relaxed);
    let pairs = match std::fs::read_to_string(&table_path) {
        Ok(text) => match er_logic::death_award_sweep::parse_table(&text) {
            Ok(p) => p,
            Err(e) => {
                log::warn!(
                    "death-award sweep: {} is malformed ({e}) -- sweep INERT. A check whose \
                     corpse-award the game missed will stay unpaid until the table is fixed \
                     (regenerate: tools/gen_death_award_pairs.py).",
                    table_path.display()
                );
                *RETAINED.lock().unwrap() = Some(Vec::new());
                return;
            }
        },
        Err(_) => {
            log::info!(
                "death-award sweep: no {} beside the DLL -- sweep inert (bundle predates \
                 clients#385; harmless, but a missed corpse-award cannot self-heal)",
                table_path.display()
            );
            *RETAINED.lock().unwrap() = Some(Vec::new());
            return;
        }
    };
    let kept = er_logic::death_award_sweep::retained(&pairs, &seed_check_flags);
    log::info!(
        "death-award sweep: armed -- {} of {} corpse-award pair(s) belong to this seed; one \
         pass at connect will pay any whose death the game recorded without the award",
        kept.len(),
        pairs.len()
    );
    *RETAINED.lock().unwrap() = Some(kept);
}

/// The one pass. Called from the in-world tick (flags readable there by the same contract every
/// flag poll in that block relies on). Latches after running; `configure` re-arms.
pub fn run() {
    if DONE.load(Ordering::Relaxed) {
        return;
    }
    let pairs = match RETAINED.lock().unwrap().clone() {
        Some(p) => p,
        None => return, // not configured yet -- wait, do not latch
    };
    DONE.store(true, Ordering::Relaxed);
    if pairs.is_empty() {
        return;
    }
    let mut fired = 0u32;
    for (death, check) in pairs {
        let death_up = crate::flags::get_event_flag(death);
        let check_up = crate::flags::get_event_flag(check);
        if er_logic::death_award_sweep::missed(death_up, check_up) {
            crate::flags::set_event_flag(check, true);
            fired += 1;
            log::info!(
                "death-award sweep: death flag {death} is up with check flag {check} down -- \
                 the game recorded the kill and never paid the award (unrecoverable in-game); \
                 check flag set, the check pays now"
            );
        }
    }
    if fired > 0 {
        log::info!("death-award sweep: {fired} missed corpse-award(s) paid this connect");
    }
}
