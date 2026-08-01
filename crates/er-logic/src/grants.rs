//! Once-per-save grant drains, extracted from `features.rs` `drain_notify_grants`.
//!
//! `drain_start_items` was DELETED 2026-08-01 (#267): its dedup was the persisted, character-less
//! `start_items_granted` boolean. Plain start items are now delivered by the reconciler ledger and
//! reconciled against the BAG by `crate::start_backfill`, whose dedup is possession.
//! The persisted guards live on a passed-in [`SaveState`] (the live code keeps them as `grant.rs`
//! statics) so tests get fresh state.

use crate::hook::GameHook;
use crate::save_state::SaveState;
use std::collections::VecDeque;

/// place yet.
pub fn drain_notify_grants(
    hook: &mut dyn GameHook,
    queue: &mut VecDeque<i32>,
    save: &mut SaveState,
) {
    let mut retry = VecDeque::new();
    while let Some(id) = queue.pop_front() {
        if save.notify_granted.contains(&id) {
            continue;
        }
        if hook.grant_full_id(id, 1) {
            save.notify_granted.insert(id);
        } else {
            retry.push_back(id);
        }
    }
    *queue = retry;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::fake::FakeGame;

    // The two start-item drain tests were deleted with the drain (#267). Their cases have honest
    // homes on the possession-dedup convergence loop, in `crate::start_backfill`:
    //   * "granted once, not re-granted on reconnect"  -> flask_dedup_survives_a_reload_via_the_bag_replay
    //   * "no inventory keeps the whole queue"         -> not_ready_does_not_consume_an_attempt
    // Both are stronger there: the bag cannot go stale, and a NotReady tick cannot burn an attempt.

    #[test]
    fn notify_granted_once_then_dedup() {
        let mut g = FakeGame::new();
        let mut save = SaveState::default();
        let rune = 191i32 | 0x4000_0000u32 as i32; // a restored great-rune notify FullID
        let mut q: VecDeque<i32> = [rune].into_iter().collect();

        drain_notify_grants(&mut g, &mut q, &mut save);
        assert_eq!(g.grants, vec![(rune, 1)]);
        assert!(save.notify_granted.contains(&rune));
        assert!(q.is_empty());

        let mut q2: VecDeque<i32> = [rune].into_iter().collect();
        drain_notify_grants(&mut g, &mut q2, &mut save);
        assert_eq!(g.grants, vec![(rune, 1)]); // unchanged
    }

    #[test]
    fn notify_no_inventory_requeues() {
        let mut g = FakeGame::new();
        g.set_inventory_ready(false);
        let mut save = SaveState::default();
        let mut q: VecDeque<i32> = [12345].into_iter().collect();
        drain_notify_grants(&mut g, &mut q, &mut save);
        assert!(g.grants.is_empty());
        assert!(!save.notify_granted.contains(&12345));
        assert_eq!(q.iter().copied().collect::<Vec<_>>(), vec![12345]);
    }
}
