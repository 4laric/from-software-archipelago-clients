//! Grace-flag flush with holder-not-ready retry, extracted from `features.rs::flush_grace_flags`.
//! The queue + the per-session "already set" set are passed in (the live code keeps them as module
//! statics) so each test gets fresh state.

use crate::hook::GameHook;
use std::collections::{HashSet, VecDeque};

/// A named Site of Grace from the game's `BonfireWarpParam -> PlaceName` join.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraceEntry {
    pub unlock_flag: u32,
    pub name: &'static str,
}

/// Complete named-grace table for the game version targeted by this client.
///
/// The TSV is generated in the world repository by `tools/datamine_grace_names.py`; `include_str!`
/// embeds it in the DLL, so rescue lookup remains available even when slot data omits a grace.
const GRACE_NAMES_TSV: &str = include_str!("grace_names.tsv");

pub fn grace_catalog() -> impl Iterator<Item = GraceEntry> {
    GRACE_NAMES_TSV.lines().filter_map(|line| {
        let (flag, name) = line.split_once('\t')?;
        Some(GraceEntry {
            unlock_flag: flag.parse().ok()?,
            name,
        })
    })
}

/// Case-insensitive substring search across every named grace, independent of AP slot data.
pub fn find_graces(query: &str) -> Vec<GraceEntry> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    grace_catalog()
        .filter(|entry| entry.name.to_lowercase().contains(&query))
        .collect()
}

/// Resolve an explicit rescue target. A numeric input addresses the unlock flag directly; a name
/// is accepted only when its substring has exactly one match, so broad commands cannot mutate many
/// flags accidentally.
pub fn resolve_grace_target(input: &str) -> Result<GraceEntry, Vec<GraceEntry>> {
    let input = input.trim();
    let matches = if let Ok(flag) = input.parse::<u32>() {
        grace_catalog()
            .filter(|entry| entry.unlock_flag == flag)
            .collect()
    } else {
        find_graces(input)
    };
    if matches.len() == 1 {
        Ok(matches[0])
    } else {
        Err(matches)
    }
}

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

/// Whether a LuaWarp target names a row in the live `BonfireWarpParam` table.
///
/// Menu travel supplies the full bonfire entity id; client-issued travel supplies that id minus
/// `warp_arg_delta`. Accept exactly those two spaces. Shape alone is not enough: an arbitrary
/// numeric `!warp` argument must not be allowed to drive state that is written before the load.
pub fn warp_target_resolves(
    entities: impl IntoIterator<Item = u32>,
    target: u32,
    warp_arg_delta: u32,
) -> bool {
    entities.into_iter().any(|entity| {
        entity == target
            || target
                .checked_add(warp_arg_delta)
                .is_some_and(|full| full == entity)
    })
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
        Some(entity) => format!(
            "{label}: flag {unlock_flag} = {is_unlocked}; !unlockgrace {unlock_flag}; !warp {entity}"
        ),
        None => format!(
            "{label}: flag {unlock_flag} = {is_unlocked}; !unlockgrace {unlock_flag}; warp unavailable (BonfireWarpParam not ready/no row)"
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
    fn warp_target_must_match_a_live_entity_in_one_of_the_two_argument_spaces() {
        let entities = [11_001_950, 11_051_954, 1_046_360_950];
        assert!(warp_target_resolves(entities, 11_051_954, 1000));
        assert!(warp_target_resolves(entities, 11_050_954, 1000));
        assert!(warp_target_resolves(entities, 1_046_359_950, 1000));
        assert!(!warp_target_resolves(entities, 75_227, 1000));
        assert!(!warp_target_resolves(entities, u32::MAX, 1000));
    }

    #[test]
    fn grace_line_prints_a_command_that_can_be_pasted_straight_back() {
        assert_eq!(
            console_grace_line("Elden Throne", 71100, false, Some(11001950)),
            "Elden Throne: flag 71100 = false; !unlockgrace 71100; !warp 11001950"
        );
        assert!(console_grace_line("Elden Throne", 71100, false, None).contains("warp unavailable"));
    }

    #[test]
    fn full_catalog_finds_graces_absent_from_typical_slot_data() {
        assert_eq!(
            find_graces("AINSEL river MAIN"),
            vec![GraceEntry {
                unlock_flag: 71214,
                name: "Ainsel River Main",
            }]
        );
        assert_eq!(
            resolve_grace_target("71214").unwrap().name,
            "Ainsel River Main"
        );
    }

    #[test]
    fn generated_catalog_has_unique_flags_and_no_blank_names() {
        let entries: Vec<_> = grace_catalog().collect();
        let flags: HashSet<_> = entries.iter().map(|entry| entry.unlock_flag).collect();
        assert_eq!(entries.len(), 419);
        assert_eq!(flags.len(), entries.len());
        assert!(entries.iter().all(|entry| !entry.name.trim().is_empty()));
    }

    #[test]
    fn mutation_target_must_be_unambiguous() {
        let matches = resolve_grace_target("church").unwrap_err();
        assert!(matches.len() > 1);
        assert!(resolve_grace_target("not a real grace")
            .unwrap_err()
            .is_empty());
    }
}
