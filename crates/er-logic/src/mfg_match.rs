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
        (1 | 2, _) => true,
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
        for (table, row) in [(0, 123), (3, 123)] {
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
}
