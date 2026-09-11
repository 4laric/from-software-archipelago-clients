//! Static source identities intersected with the active seed. This never proves
//! acquisition, pin position, or independent corroboration. Recorded hover is history.
#[path = "mfg_match_data.rs"]
mod data;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Candidate {
    pub ap_id: i64,
    pub original_flag: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchStatus {
    UnknownIdentity,
    InvalidIdentity,
    Unmatched,
    OutOfSeed,
    SingleCandidate,
    AmbiguousCandidates,
}

#[derive(Debug, Eq, PartialEq)]
pub struct MatchResult {
    pub status: MatchStatus,
    pub catalog_candidates: Vec<Candidate>,
    pub seed_candidates: Vec<Candidate>,
}

/// Original world catalog name, for exact comparison against the current server
/// name before presenting review actions. ID membership alone cannot detect ID reuse.
pub fn catalog_name(ap_id: i64) -> Option<&'static str> {
    data::NAMES
        .binary_search_by_key(&ap_id, |&(id, _)| id)
        .ok()
        .map(|index| data::NAMES[index].1)
}

/// Flag zero and the pair (table zero, row zero) mean unknown. A known lot is
/// always table-qualified. When both identities are known they must agree.
/// `in_seed` must include checked AND unchecked IDs from the current slot;
/// do not use name guesses, the full data package, or a previous connection.
pub fn resolve(
    original_flag: u32,
    lot_table: u32,
    lot_row: u32,
    in_seed: impl Fn(i64) -> bool,
) -> MatchResult {
    let mut result = MatchResult {
        status: MatchStatus::Unmatched,
        catalog_candidates: Vec::new(),
        seed_candidates: Vec::new(),
    };
    let known_lot = match (lot_table, lot_row) {
        (0, 0) => false,
        (1 | 2, 1..) => true,
        _ => {
            result.status = MatchStatus::InvalidIdentity;
            return result;
        }
    };
    if !known_lot && original_flag == 0 {
        result.status = MatchStatus::UnknownIdentity;
        return result;
    }
    if known_lot {
        let start = data::LOTS.partition_point(|&(t, r, _, _)| (t, r) < (lot_table, lot_row));
        for &(t, r, flag, ap_id) in &data::LOTS[start..] {
            if (t, r) != (lot_table, lot_row) {
                break;
            }
            if original_flag == 0 || original_flag == flag {
                result.catalog_candidates.push(Candidate {
                    ap_id,
                    original_flag: flag,
                });
            }
        }
    } else {
        let start = data::FLAGS.partition_point(|&(flag, _)| flag < original_flag);
        for &(flag, ap_id) in &data::FLAGS[start..] {
            if flag != original_flag {
                break;
            }
            result.catalog_candidates.push(Candidate {
                ap_id,
                original_flag: flag,
            });
        }
    }
    result.catalog_candidates.sort_unstable();
    result.catalog_candidates.dedup();
    result.seed_candidates = result
        .catalog_candidates
        .iter()
        .copied()
        .filter(|candidate| in_seed(candidate.ap_id))
        .collect();
    result.status = match (
        result.catalog_candidates.len(),
        result.seed_candidates.len(),
    ) {
        (0, _) => MatchStatus::Unmatched,
        (_, 0) => MatchStatus::OutOfSeed,
        (_, 1) => MatchStatus::SingleCandidate,
        _ => MatchStatus::AmbiguousCandidates,
    };
    result
}

/// Presentation only: eligibility is not the actual randomized item class.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LotStyleKind {
    ProgressionSurface = 1,
    Hint = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LotStyle {
    pub lot_table: u32,
    pub lot_row: u32,
    pub style: LotStyleKind,
}

/// Emit a deterministic full replacement snapshot. Checked candidates remain in
/// agreement as neutral, rather than making a sibling appear uniquely actionable.
pub fn color_styles(
    seed_names: &std::collections::HashMap<i64, String>,
    checked: &std::collections::HashSet<i64>,
    hinted: &std::collections::HashSet<i64>,
    progression_surface: &std::collections::HashSet<i64>,
) -> Vec<LotStyle> {
    let mut output = Vec::new();
    let mut previous = None;
    for &(table, row, _, _) in data::LOTS {
        if previous == Some((table, row)) {
            continue;
        }
        previous = Some((table, row));
        let matched = resolve(0, table, row, |id| seed_names.contains_key(&id));
        if matched.seed_candidates.is_empty()
            || matched.seed_candidates.iter().any(|candidate| {
                catalog_name(candidate.ap_id)
                    != seed_names.get(&candidate.ap_id).map(String::as_str)
            })
        {
            continue;
        }
        let style_for = |candidate: &Candidate| {
            if checked.contains(&candidate.ap_id) {
                None
            } else if hinted.contains(&candidate.ap_id) {
                Some(LotStyleKind::Hint)
            } else if progression_surface.contains(&candidate.ap_id) {
                Some(LotStyleKind::ProgressionSurface)
            } else {
                None
            }
        };
        let Some(style) = style_for(&matched.seed_candidates[0]) else {
            continue;
        };
        if matched
            .seed_candidates
            .iter()
            .all(|candidate| style_for(candidate) == Some(style))
        {
            output.push(LotStyle {
                lot_table: table,
                lot_row: row,
                style,
            });
        }
    }
    output
}

/// State flags describe check eligibility, never randomized item contents.
pub const CHECK: u32 = 1;
pub const PROGRESSION: u32 = 2;
pub const IN_LOGIC: u32 = 4;
pub const PROGRESSION_IN_LOGIC: u32 = 8;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LotCheckState {
    pub lot_table: u32,
    pub lot_row: u32,
    pub flags: u32,
}

/// Complete current-seed snapshot, including neutral and checked checks. Shared
/// lots union membership but conjunction requires a single check as witness.
pub fn check_states(
    seed_names: &std::collections::HashMap<i64, String>,
    progression_surface: &std::collections::HashSet<i64>,
    in_logic: &std::collections::HashSet<i64>,
) -> Vec<LotCheckState> {
    let mut output = Vec::new();
    let mut previous = None;
    for &(table, row, _, _) in data::LOTS {
        if previous == Some((table, row)) {
            continue;
        }
        previous = Some((table, row));
        let matched = resolve(0, table, row, |id| seed_names.contains_key(&id));
        if matched.seed_candidates.is_empty()
            || matched
                .seed_candidates
                .iter()
                .any(|c| catalog_name(c.ap_id) != seed_names.get(&c.ap_id).map(String::as_str))
        {
            continue;
        }
        let mut flags = CHECK;
        for candidate in matched.seed_candidates {
            let progression = progression_surface.contains(&candidate.ap_id);
            let reachable = in_logic.contains(&candidate.ap_id);
            if progression {
                flags |= PROGRESSION;
            }
            if reachable {
                flags |= IN_LOGIC;
            }
            if progression && reachable {
                flags |= PROGRESSION_IN_LOGIC;
            }
        }
        output.push(LotCheckState {
            lot_table: table,
            lot_row: row,
            flags,
        });
    }
    output
}

/// Join stable acquisition flags to THIS seed's AP IDs. Baked AP IDs and display
/// names can shift between compatible world releases and must not determine
/// the region/progression state of a current-seed check.
pub fn seed_check_states(
    location_flags: &std::collections::HashMap<i64, u32>,
    seed_names: &std::collections::HashMap<i64, String>,
    progression: &std::collections::HashSet<i64>,
    in_logic: &std::collections::HashSet<i64>,
) -> Vec<LotCheckState> {
    let mut by_flag = std::collections::HashMap::<u32, u32>::new();
    for (&id, &flag) in location_flags {
        if flag == 0 || !seed_names.contains_key(&id) {
            continue;
        }
        let mut state = CHECK;
        if progression.contains(&id) {
            state |= PROGRESSION;
        }
        if in_logic.contains(&id) {
            state |= IN_LOGIC;
        }
        if progression.contains(&id) && in_logic.contains(&id) {
            state |= PROGRESSION_IN_LOGIC;
        }
        *by_flag.entry(flag).or_default() |= state;
    }
    let mut lots = std::collections::BTreeMap::<(u32, u32), u32>::new();
    for &(table, row, flag, _) in data::LOTS {
        if let Some(&state) = by_flag.get(&flag) {
            *lots.entry((table, row)).or_default() |= state;
        }
    }
    lots.into_iter()
        .map(|((lot_table, lot_row), flags)| LotCheckState {
            lot_table,
            lot_row,
            flags,
        })
        .collect()
}

/// The local tracker uses coarse region access. Missing mapping is unknown for
/// map filtering, not an affirmative reachable claim. Empty means known ungated.
pub fn known_in_logic(
    id: u64,
    coarse: &std::collections::HashMap<u64, String>,
    open: &std::collections::HashSet<String>,
) -> bool {
    coarse
        .get(&id)
        .is_some_and(|region| region.is_empty() || open.contains(region))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn farum_seed_id_shift_preserves_region_logic() {
        // Current world tables/data.py: f13007000 is 7771400; the older
        // mfg_match_data catalogue calls it 7771401 (now a different check).
        let names = [(7771400, "Farum seed check".to_string())]
            .into_iter()
            .collect();
        let flags = [(7771400, 13007000)].into_iter().collect();
        let coarse = [(7771400, "Farum Azula".to_string())].into_iter().collect();
        let open = ["Farum Azula".to_string()].into_iter().collect();
        let reachable = [7771400]
            .into_iter()
            .filter(|&id| known_in_logic(id as u64, &coarse, &open))
            .collect();
        let none = std::collections::HashSet::new();
        assert!(!check_states(&names, &none, &reachable)
            .iter()
            .any(|s| s.lot_table == 1 && s.lot_row == 13000000));
        let states = seed_check_states(&flags, &names, &none, &reachable);
        assert_eq!(
            states
                .iter()
                .find(|s| s.lot_table == 1 && s.lot_row == 13000000)
                .unwrap()
                .flags,
            CHECK | IN_LOGIC
        );
        let closed = seed_check_states(&flags, &names, &none, &none);
        assert_eq!(
            closed
                .iter()
                .find(|s| s.lot_table == 1 && s.lot_row == 13000000)
                .unwrap()
                .flags,
            CHECK
        );
        assert!(
            seed_check_states(&flags, &std::collections::HashMap::new(), &none, &reachable)
                .is_empty()
        );
    }

    #[test]
    fn seed_flag_join_keeps_split_witnesses_and_ignores_reused_baked_id() {
        let names = [
            (1, "a".into()),
            (2, "b".into()),
            (7771401, "unrelated".into()),
        ]
        .into_iter()
        .collect();
        let flags = [(1, 13007000), (2, 13007000), (7771401, 0)]
            .into_iter()
            .collect();
        let states = seed_check_states(
            &flags,
            &names,
            &[1].into_iter().collect(),
            &[2, 7771401].into_iter().collect(),
        );
        let row = states
            .iter()
            .find(|s| s.lot_table == 1 && s.lot_row == 13000000)
            .unwrap();
        assert_eq!(row.flags, CHECK | PROGRESSION | IN_LOGIC);
        assert_eq!(
            states
                .iter()
                .filter(|s| s.lot_table == 1 && s.lot_row == 13000000)
                .count(),
            1
        );
    }

    #[test]
    fn check_state_shared_lot_conjunction_needs_same_witness() {
        let ids = [7770007, 7900004];
        let names = ids
            .into_iter()
            .map(|id| (id, catalog_name(id).unwrap().to_owned()))
            .collect();
        let prog = [ids[0]].into_iter().collect();
        let reachable = [ids[1]].into_iter().collect();
        let states = check_states(&names, &prog, &reachable);
        let state = states
            .iter()
            .find(|s| s.lot_table == 1 && s.lot_row == 10180)
            .unwrap();
        assert_eq!(state.flags, CHECK | PROGRESSION | IN_LOGIC);
        let both = check_states(&names, &prog, &prog);
        assert_eq!(
            both.iter()
                .find(|s| s.lot_table == 1 && s.lot_row == 10180)
                .unwrap()
                .flags,
            CHECK | PROGRESSION | IN_LOGIC | PROGRESSION_IN_LOGIC
        );
    }

    #[test]
    fn neutral_checks_included_and_stale_names_refused() {
        let mut names = [(7772256, catalog_name(7772256).unwrap().to_owned())]
            .into_iter()
            .collect();
        let empty = std::collections::HashSet::new();
        let states = check_states(&names, &empty, &empty);
        assert!(states
            .iter()
            .any(|s| s.lot_row == 32010040 && s.flags == CHECK));
        names.insert(7772256, "old seed name".to_owned());
        assert!(check_states(&names, &empty, &empty).is_empty());
        assert!(check_states(&std::collections::HashMap::new(), &empty, &empty).is_empty());
    }

    #[test]
    fn missing_region_is_not_reachable_but_known_ungated_is() {
        let coarse = [(1, "".to_owned()), (2, "Limgrave".to_owned())]
            .into_iter()
            .collect();
        let empty = std::collections::HashSet::new();
        assert!(known_in_logic(1, &coarse, &empty));
        assert!(!known_in_logic(2, &coarse, &empty));
        assert!(!known_in_logic(3, &coarse, &empty));
        assert!(known_in_logic(
            2,
            &coarse,
            &["Limgrave".to_owned()].into_iter().collect()
        ));
    }

    #[test]
    fn recovered_rewards_match_the_live_lot_and_active_seed() {
        for (table, lot, flag, ap_id) in [
            (1, 101621, 400162, 7774254),
            (1, 2046400001, 2046407001, 7774636),
            (1, 2046400002, 2046407002, 7774637),
            (1, 2046400003, 2046407003, 7774638),
            (1, 2046400004, 2046407004, 7774639),
            (1, 2047440901, 2047447901, 7774640),
            (2, 438100012, 1038457500, 7774641),
            (1, 30861, 530861, 7774642),
            (1, 40424, 540424, 7774643),
            (1, 40428, 540428, 7774644),
            (1, 40912, 540912, 7774645),
            (1, 40914, 540914, 7774646),
            (1, 40920, 540920, 7774647),
            (1, 40922, 540922, 7774648),
            (1, 102926, 400295, 7774649),
            (1, 30200900, 30207900, 7774650),
            (1, 104512, 400452, 7774651),
        ] {
            let result = resolve(flag, table, lot, |id| id == ap_id);
            assert_eq!(result.status, MatchStatus::SingleCandidate);
            assert_eq!(
                result.seed_candidates,
                vec![Candidate {
                    ap_id,
                    original_flag: flag
                }]
            );
            assert_eq!(
                resolve(flag, table, lot, |_| false).status,
                MatchStatus::OutOfSeed
            );
        }
        assert_eq!(
            resolve(1039527700, 1, 1039520700, |_| true).status,
            MatchStatus::Unmatched
        );
        // An enemy actor can award a map lot. Do not accept a conflicting namespace.
        assert_eq!(
            resolve(400452, 2, 104512, |_| true).status,
            MatchStatus::Unmatched
        );
    }

    #[test]
    fn observed_and_source_reference_lots_identify_expected_checks() {
        for (lot, ap_id) in [
            (32010040, 7772256),
            (942370060, 7772821),
            (942370070, 7772822),
        ] {
            let result = resolve(0, 1, lot, |id| id == ap_id);
            assert_eq!(result.status, MatchStatus::SingleCandidate);
            assert_eq!(result.seed_candidates[0].ap_id, ap_id);
            assert_eq!(result.catalog_candidates.len(), 1);
        }
    }

    #[test]
    fn shared_siblings_survive_and_seed_filter_is_explicit() {
        let all = resolve(197, 1, 10180, |_| true);
        assert_eq!(all.status, MatchStatus::AmbiguousCandidates);
        assert_eq!(
            all.seed_candidates
                .iter()
                .map(|c| c.ap_id)
                .collect::<Vec<_>>(),
            [7770007, 7900004]
        );
        let one = resolve(197, 1, 10180, |id| id == 7900004);
        assert_eq!(one.status, MatchStatus::SingleCandidate);
        assert_eq!(one.catalog_candidates, all.catalog_candidates);
        assert_eq!(one.seed_candidates.len(), 1);
        assert_eq!(
            resolve(197, 1, 10180, |_| false).status,
            MatchStatus::OutOfSeed
        );
    }

    #[test]
    fn conflicts_and_unknowns_never_fall_back_to_looser_matching() {
        assert_eq!(
            resolve(0, 0, 0, |_| true).status,
            MatchStatus::UnknownIdentity
        );
        for (table, row) in [(0, 123), (1, 0), (2, 0), (3, 123)] {
            assert_eq!(
                resolve(0, table, row, |_| true).status,
                MatchStatus::InvalidIdentity
            );
        }
        assert_eq!(
            resolve(197, 1, 942370060, |_| true).status,
            MatchStatus::Unmatched
        );
        assert_eq!(
            resolve(0, 2, 942370060, |_| true).status,
            MatchStatus::Unmatched
        );
        assert_eq!(
            resolve(0, 1, u32::MAX, |_| true).status,
            MatchStatus::Unmatched
        );
        assert_eq!(resolve(197, 0, 0, |_| true).seed_candidates.len(), 2);
    }

    #[test]
    fn generated_indexes_are_sorted_unique_and_consistent() {
        assert!(data::LOTS.len() > 3000);
        assert_eq!(data::NAMES.len(), data::FLAGS.len());
        assert!(data::NAMES.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(catalog_name(7772821).unwrap().contains("Flail"));
        assert!(catalog_name(7772256).unwrap().contains("Glintstone Scrap"));
        assert_eq!(catalog_name(-1), None);
        assert_eq!(data::FLAGS.len(), 4941);
        assert!(data::LOTS.windows(2).all(|w| w[0] < w[1]));
        assert!(data::FLAGS.windows(2).all(|w| w[0] < w[1]));
        for &(table, _, flag, id) in data::LOTS {
            assert!(matches!(table, 1 | 2));
            assert!(catalog_name(id).is_some());
            assert!(data::FLAGS.binary_search(&(flag, id)).is_ok());
        }
    }
    #[test]
    fn colors_use_eligibility_hint_priority_and_exact_seed_names() {
        use std::collections::{HashMap, HashSet};
        let id = 7772821;
        let mut names = HashMap::from([(id, catalog_name(id).unwrap().to_string())]);
        let none = HashSet::new();
        let surface = HashSet::from([id]);
        let style = |entries: Vec<LotStyle>| {
            entries
                .into_iter()
                .find(|entry| (entry.lot_table, entry.lot_row) == (1, 942370060))
                .map(|entry| entry.style)
        };
        assert_eq!(
            style(color_styles(&names, &none, &none, &surface)),
            Some(LotStyleKind::ProgressionSurface)
        );
        assert_eq!(
            style(color_styles(&names, &none, &surface, &surface)),
            Some(LotStyleKind::Hint)
        );
        assert_eq!(
            style(color_styles(&names, &surface, &surface, &surface)),
            None
        );
        assert_eq!(style(color_styles(&names, &none, &none, &none)), None);
        names.insert(id, "different catalog".to_string());
        assert_eq!(style(color_styles(&names, &none, &surface, &surface)), None);
        assert!(color_styles(&HashMap::new(), &none, &surface, &surface).is_empty());
    }

    #[test]
    fn shared_pin_requires_unanimous_seed_candidate_colors() {
        use std::collections::{HashMap, HashSet};
        let ids: Vec<_> = resolve(197, 1, 10180, |_| true)
            .seed_candidates
            .into_iter()
            .map(|candidate| candidate.ap_id)
            .collect();
        assert!(ids.len() > 1);
        let mut names: HashMap<_, _> = ids
            .iter()
            .map(|&id| (id, catalog_name(id).unwrap().to_string()))
            .collect();
        let all: HashSet<_> = ids.iter().copied().collect();
        let one = HashSet::from([ids[0]]);
        let none = HashSet::new();
        let style = |entries: Vec<LotStyle>| {
            entries
                .into_iter()
                .find(|entry| (entry.lot_table, entry.lot_row) == (1, 10180))
                .map(|entry| entry.style)
        };
        assert_eq!(
            style(color_styles(&names, &none, &none, &all)),
            Some(LotStyleKind::ProgressionSurface)
        );
        assert_eq!(style(color_styles(&names, &none, &one, &all)), None);
        assert_eq!(style(color_styles(&names, &one, &none, &all)), None);
        assert_eq!(style(color_styles(&names, &none, &none, &one)), None);
        assert_eq!(
            style(color_styles(&names, &none, &all, &all)),
            Some(LotStyleKind::Hint)
        );
        names.insert(ids[0], "mismatched sibling".to_string());
        assert_eq!(style(color_styles(&names, &none, &all, &all)), None);
        // Only actual seed candidates participate; foreign catalog siblings do not.
        names = HashMap::from([(ids[0], catalog_name(ids[0]).unwrap().to_string())]);
        assert_eq!(
            style(color_styles(&names, &none, &one, &all)),
            Some(LotStyleKind::Hint)
        );
    }
}
