//! Grace-flag flush with holder-not-ready retry, extracted from `features.rs::flush_grace_flags`.
//! The queue + the per-session "already set" set are passed in (the live code keeps them as module
//! statics) so each test gets fresh state.

use crate::hook::GameHook;
use std::collections::{HashSet, VecDeque};

/// Drain pending grace flags: skip ones already set this session; set each via `try_set_event_flag`;
/// flags whose holder isn't ready are retained for the next tick (never dropped).
pub fn flush_grace_flags(
    hook: &mut dyn GameHook,
    queue: &mut VecDeque<u32>,
    session: &mut HashSet<u32>,
) {
    let mut retry = VecDeque::new();
    while let Some(flag) = queue.pop_front() {
        if session.contains(&flag) {
            continue;
        }
        if hook.try_set_event_flag(flag, true) {
            session.insert(flag);
        } else {
            retry.push_back(flag);
        }
    }
    *queue = retry;
}

/// Find the grace entity id whose `BonfireWarpParam.eventflagId` is `unlock_flag`.
///
/// The runtime supplies `(event flag, entity id)` pairs from the live param repository. Keeping
/// the selection here makes the rescue-console join host-testable without baking a second grace
/// table into the client.
pub fn entity_for_unlock_flag(
    rows: impl IntoIterator<Item = (u32, u32)>,
    unlock_flag: u32,
) -> Option<u32> {
    rows.into_iter()
        .find_map(|(flag, entity)| (flag == unlock_flag).then_some(entity))
}

/// One `!grace` result line. A resolved row includes the literal command the player can paste
/// back into the console; an unresolved row says why the rescue route cannot be aimed yet.
pub fn console_grace_line(
    label: &str,
    unlock_flag: u32,
    is_unlocked: bool,
    entity_id: Option<u32>,
) -> String {
    match entity_id {
        Some(entity) => {
            format!("{label}: flag {unlock_flag} = {is_unlocked}; !warp {entity}")
        }
        None => format!(
            "{label}: flag {unlock_flag} = {is_unlocked}; warp unavailable (BonfireWarpParam not ready/no row)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::fake::FakeGame;

    #[test]
    fn holder_not_ready_retries_then_lands() {
        let mut g = FakeGame::new();
        g.set_flag_holder_ready(false);
        let mut q: VecDeque<u32> = [76971, 76972].into_iter().collect();
        let mut session = HashSet::new();

        flush_grace_flags(&mut g, &mut q, &mut session);
        assert!(g.set_flags().is_empty());
        assert_eq!(q.iter().copied().collect::<Vec<_>>(), vec![76971, 76972]);
        assert!(session.is_empty());

        g.set_flag_holder_ready(true);
        flush_grace_flags(&mut g, &mut q, &mut session);
        assert_eq!(g.set_flags(), vec![76971, 76972]);
        assert!(q.is_empty());
        assert!(session.contains(&76971) && session.contains(&76972));
    }

    #[test]
    fn partial_readiness_drains_only_successes() {
        let mut g = FakeGame::new();
        g.script_flag_holder_ready(vec![true, false]); // 1st lands, 2nd not ready
        let mut q: VecDeque<u32> = [76971, 76972].into_iter().collect();
        let mut session = HashSet::new();

        flush_grace_flags(&mut g, &mut q, &mut session);
        assert_eq!(g.set_flags(), vec![76971]);
        assert_eq!(q.iter().copied().collect::<Vec<_>>(), vec![76972]);
        assert!(session.contains(&76971) && !session.contains(&76972));
    }

    #[test]
    fn already_set_this_session_is_skipped() {
        let mut g = FakeGame::new();
        let mut q: VecDeque<u32> = [76971].into_iter().collect();
        let mut session: HashSet<u32> = [76971].into_iter().collect();
        flush_grace_flags(&mut g, &mut q, &mut session);
        assert!(g.set_flags().is_empty()); // skipped, nothing re-set
        assert!(q.is_empty());
    }

    #[test]
    fn unlock_flag_resolves_to_the_entity_the_warp_command_needs() {
        // BonfireWarpParam row 110000 in the committed 2.6.2.0 data: eventflagId 71100,
        // bonfireEntityId 11001950. The values witness the two id spaces without becoming a
        // production table -- production reads these fields from the live param repository.
        let rows = [(71000, 10001950), (71100, 11001950), (71101, 11011950)];
        assert_eq!(entity_for_unlock_flag(rows, 71100), Some(11001950));
        assert_eq!(entity_for_unlock_flag(rows, 79999), None);
    }

    #[test]
    fn grace_line_prints_a_command_that_can_be_pasted_straight_back() {
        assert_eq!(
            console_grace_line("Elden Throne", 71100, false, Some(11001950)),
            "Elden Throne: flag 71100 = false; !warp 11001950"
        );
        assert!(console_grace_line("Elden Throne", 71100, false, None).contains("warp unavailable"));
    }
}
