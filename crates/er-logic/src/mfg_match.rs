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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(data::FLAGS.len(), 4925);
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
