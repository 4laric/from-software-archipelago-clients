//! Map action targets differ from the seed progression-placement surface.
use std::collections::HashSet;

pub struct SweepTargetGroup {
    pub bosses: Vec<i64>,
    pub members: Vec<i64>,
}

pub struct Targets {
    pub locations: HashSet<i64>,
    pub unresolved_groups: usize,
}

pub fn progression_targets(
    surface: &HashSet<i64>,
    valid: &HashSet<i64>,
    groups: &[SweepTargetGroup],
) -> Targets {
    let mut locations: HashSet<_> = surface.intersection(valid).copied().collect();
    let mut bosses = HashSet::new();
    let mut unresolved_groups = 0;
    for group in groups {
        let relevant = group
            .members
            .iter()
            .any(|id| surface.contains(id) && valid.contains(id));
        for id in &group.members {
            locations.remove(id);
        }
        if relevant {
            let matching: Vec<_> = group
                .bosses
                .iter()
                .filter(|id| valid.contains(id))
                .copied()
                .collect();
            if matching.is_empty() {
                unresolved_groups += 1;
            }
            bosses.extend(matching);
        }
    }
    // A granting boss may itself occur as a member of another group. It is still a target.
    locations.extend(bosses);
    Targets {
        locations,
        unresolved_groups,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn set(ids: &[i64]) -> HashSet<i64> {
        ids.iter().copied().collect()
    }
    #[test]
    fn scarab_surface_moves_to_granting_boss_only_on_enabled_group() {
        let surface = set(&[7772308, 3]);
        let valid = set(&[7772308, 2, 3]);
        assert_eq!(
            progression_targets(&surface, &valid, &[]).locations,
            surface
        );
        let result = progression_targets(
            &surface,
            &valid,
            &[SweepTargetGroup {
                bosses: vec![2],
                members: vec![7772308],
            }],
        );
        assert_eq!(result.locations, set(&[2, 3]));
        assert_eq!(surface, set(&[7772308, 3]));
    }
    #[test]
    fn shared_groups_and_already_surface_boss_deduplicate() {
        let result = progression_targets(
            &set(&[1, 2]),
            &set(&[1, 2, 3]),
            &[
                SweepTargetGroup {
                    bosses: vec![2],
                    members: vec![1],
                },
                SweepTargetGroup {
                    bosses: vec![3],
                    members: vec![1, 2],
                },
            ],
        );
        assert_eq!(result.locations, set(&[2, 3]));
    }
    #[test]
    fn unknown_boss_and_foreign_ids_never_invent_targets() {
        let result = progression_targets(
            &set(&[1, 9]),
            &set(&[1, 2]),
            &[
                SweepTargetGroup {
                    bosses: vec![9],
                    members: vec![1],
                },
                SweepTargetGroup {
                    bosses: vec![2],
                    members: vec![9],
                },
            ],
        );
        assert!(result.locations.is_empty());
        assert_eq!(result.unresolved_groups, 1);
    }
    #[test]
    fn nonprogression_members_do_not_highlight_boss() {
        let result = progression_targets(
            &set(&[3]),
            &set(&[1, 2, 3]),
            &[SweepTargetGroup {
                bosses: vec![2],
                members: vec![1],
            }],
        );
        assert_eq!(result.locations, set(&[3]));
    }
}

#[cfg(test)]
mod multiple_rewards {
    use super::*;
    #[test]
    fn every_seed_boss_reward_for_one_event_is_targeted() {
        let valid = [1, 2, 3].into_iter().collect();
        let surface = [1].into_iter().collect();
        let result = progression_targets(
            &surface,
            &valid,
            &[SweepTargetGroup {
                bosses: vec![2, 3],
                members: vec![1],
            }],
        );
        assert_eq!(result.locations, [2, 3].into_iter().collect());
    }
}

/// Exact seed boss-event identities; kind 3 is negotiated separately from lot kinds.
pub fn boss_check_states(
    bosses: &std::collections::HashMap<u32, Vec<i64>>,
    seed_names: &std::collections::HashMap<i64, String>,
    targets: &HashSet<i64>,
    in_logic: &HashSet<i64>,
) -> Vec<crate::mfg_match::LotCheckState> {
    use crate::mfg_match::{LotCheckState, CHECK, IN_LOGIC, PROGRESSION, PROGRESSION_IN_LOGIC};
    let mut result = Vec::new();
    for (&flag, ids) in bosses {
        if flag == 0 {
            continue;
        }
        let mut flags = 0;
        for id in ids.iter().filter(|id| seed_names.contains_key(id)) {
            flags |= CHECK;
            let progression = targets.contains(id);
            let reachable = in_logic.contains(id);
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
        if flags != 0 {
            result.push(LotCheckState {
                lot_table: 3,
                lot_row: flag,
                flags,
            });
        }
    }
    result.sort_unstable_by_key(|s| s.lot_row);
    result
}

#[cfg(test)]
mod boss_state_tests {
    use super::*;
    use crate::mfg_match::*;
    #[test]
    fn tree_sentinel_and_godrick_use_exact_seed_event_identity() {
        let mapping = [(1042360800, vec![1]), (10000800, vec![2])]
            .into_iter()
            .collect();
        let names = [
            (1, "seed boss one".to_owned()),
            (2, "seed boss two".to_owned()),
        ]
        .into_iter()
        .collect();
        let states = boss_check_states(
            &mapping,
            &names,
            &[1].into_iter().collect(),
            &[1, 2].into_iter().collect(),
        );
        assert_eq!(
            states,
            vec![
                LotCheckState {
                    lot_table: 3,
                    lot_row: 10000800,
                    flags: CHECK | IN_LOGIC
                },
                LotCheckState {
                    lot_table: 3,
                    lot_row: 1042360800,
                    flags: CHECK | PROGRESSION | IN_LOGIC | PROGRESSION_IN_LOGIC
                }
            ]
        );
    }
    #[test]
    fn missing_seed_mapping_and_cross_witness_conjunction_are_not_invented() {
        let mapping = [(1, vec![10, 11]), (2, vec![12])].into_iter().collect();
        let names = [(10, "a".to_owned()), (11, "b".to_owned())]
            .into_iter()
            .collect();
        let states = boss_check_states(
            &mapping,
            &names,
            &[10].into_iter().collect(),
            &[11].into_iter().collect(),
        );
        assert_eq!(
            states,
            vec![LotCheckState {
                lot_table: 3,
                lot_row: 1,
                flags: CHECK | PROGRESSION | IN_LOGIC
            }]
        );
        assert!(boss_check_states(
            &std::collections::HashMap::new(),
            &names,
            &HashSet::new(),
            &HashSet::new()
        )
        .is_empty());
    }
}
