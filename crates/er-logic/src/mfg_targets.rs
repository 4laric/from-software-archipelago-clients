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

#[path = "mfg_boss_flags_data.rs"]
mod boss_flags;

/// Distinct acquisition and defeat identities are joined only through datamined records.
pub fn native_boss_flag(flag: u32) -> Option<u32> {
    let rows = boss_flags::ACQUISITION_DEFEAT;
    if let Ok(index) = rows.binary_search_by_key(&flag, |&(acquisition, _)| acquisition) {
        return Some(rows[index].1);
    }
    rows.iter()
        .any(|&(_, defeat)| defeat == flag)
        .then_some(flag)
}

pub fn seed_boss_map(
    locations: &std::collections::HashMap<i64, u32>,
    valid: &HashSet<i64>,
) -> std::collections::HashMap<u32, Vec<i64>> {
    let mut output: std::collections::HashMap<u32, Vec<i64>> = std::collections::HashMap::new();
    for (&id, &acquisition) in locations {
        if !valid.contains(&id) {
            continue;
        }
        if let Some(defeat) = native_boss_flag(acquisition) {
            output.entry(defeat).or_default().push(id);
        }
    }
    for ids in output.values_mut() {
        ids.sort_unstable();
        ids.dedup();
    }
    output
}

#[cfg(test)]
mod real_seed_identity_tests {
    use super::*;
    #[test]
    fn real_acquisition_flags_resolve_godrick_and_tree_sentinel() {
        let input = [(7770653, 510010), (7770692, 530100), (7772308, 34107110)]
            .into_iter()
            .collect();
        let mapping = seed_boss_map(&input, &[7770653, 7770692, 7772308].into_iter().collect());
        assert_eq!(mapping.get(&10000800), Some(&vec![7770653]));
        assert_eq!(mapping.get(&1042360800), Some(&vec![7770692]));
        assert!(!mapping.contains_key(&510010));
        assert!(!mapping.contains_key(&530100));
        assert!(!mapping.values().flatten().any(|&id| id == 7772308));
        assert!(seed_boss_map(&input, &HashSet::new()).is_empty());
    }
    #[test]
    fn illustrative_surface_redirect_uses_real_tree_sentinel_identity() {
        let valid = [7770692, 7772308].into_iter().collect();
        let mapping = seed_boss_map(
            &[(7770692, 530100), (7772308, 34107110)]
                .into_iter()
                .collect(),
            &valid,
        );
        let defeat = native_boss_flag(530100).unwrap();
        let targets = progression_targets(
            &[7772308].into_iter().collect(),
            &valid,
            &[SweepTargetGroup {
                bosses: mapping[&defeat].clone(),
                members: vec![7772308],
            }],
        );
        assert_eq!(targets.locations, [7770692].into_iter().collect());
        let states = boss_check_states(
            &mapping,
            &[(7770692, "Golden Halberd".to_owned())]
                .into_iter()
                .collect(),
            &targets.locations,
            &[7770692].into_iter().collect(),
        );
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].lot_table, 3);
        assert_eq!(states[0].lot_row, 1042360800);
        assert_eq!(states[0].flags, 15);
    }
    #[test]
    fn lookup_sorted_unique_and_unknown_flag_fails_closed() {
        assert!(boss_flags::ACQUISITION_DEFEAT
            .windows(2)
            .all(|w| w[0].0 < w[1].0));
        assert_eq!(native_boss_flag(1042360800), Some(1042360800));
        assert_eq!(native_boss_flag(0), None);
    }
}

/// Add native sweep targets that have no individual boss check in this seed.
/// Each exact enabled defeat flag represents its remaining valid member checks.
/// Separate witnesses never manufacture the progression-and-in-logic conjunction.
pub fn append_sweep_trigger_states(
    states: &mut Vec<crate::mfg_match::LotCheckState>,
    groups: &std::collections::HashMap<u32, Vec<i64>>,
    bosses: &std::collections::HashMap<u32, Vec<i64>>,
    remaining: &HashSet<i64>,
    surface: &HashSet<i64>,
    in_logic: &HashSet<i64>,
) {
    use crate::mfg_match::{LotCheckState, CHECK, IN_LOGIC, PROGRESSION, PROGRESSION_IN_LOGIC};
    for (&flag, members) in groups {
        if flag == 0 || bosses.get(&flag).is_some_and(|ids| !ids.is_empty()) {
            continue;
        }
        let mut flags = 0;
        for id in members.iter().filter(|id| remaining.contains(id)) {
            flags |= CHECK;
            let progression = surface.contains(id);
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
            states.push(LotCheckState {
                lot_table: 3,
                lot_row: flag,
                flags,
            });
        }
    }
    states.sort_unstable_by_key(|state| (state.lot_table, state.lot_row));
}

#[cfg(test)]
mod sweep_trigger_tests {
    use super::*;
    use std::collections::HashMap;
    #[test]
    fn actual_native_trigger_without_boss_check_is_highlighted_until_checked() {
        // Actual AP_14089154938208861744: native trigger 1043370800 has
        // surface member 7772824 but no boss reward check in locationFlags.
        let groups = [(1043370800, vec![7772824])].into_iter().collect();
        let remaining = [7772824].into_iter().collect();
        let mut states = Vec::new();
        append_sweep_trigger_states(
            &mut states,
            &groups,
            &HashMap::new(),
            &remaining,
            &remaining,
            &remaining,
        );
        assert_eq!(states.len(), 1);
        assert_eq!(
            (states[0].lot_table, states[0].lot_row, states[0].flags),
            (3, 1043370800, 15)
        );
        states.clear();
        append_sweep_trigger_states(
            &mut states,
            &groups,
            &HashMap::new(),
            &HashSet::new(),
            &remaining,
            &remaining,
        );
        assert!(states.is_empty());
    }
    #[test]
    fn actual_scarab_phantom_trigger_never_fabricates_a_boss_ap_check() {
        let remaining = [7772308].into_iter().collect();
        let bosses = seed_boss_map(&[(7772308, 34107110)].into_iter().collect(), &remaining);
        assert!(bosses.is_empty());
        let mut states = Vec::new();
        append_sweep_trigger_states(
            &mut states,
            &[(34100800, vec![7772308])].into_iter().collect(),
            &bosses,
            &remaining,
            &remaining,
            &remaining,
        );
        assert_eq!(states[0].lot_row, 34100800);
        // No native marker exists for this phantom trigger: publishing its exact
        // identity cannot redirect to an unrelated boss or manufacture a pin.
        assert_eq!(states[0].lot_table, 3);
    }
    #[test]
    fn split_witnesses_do_not_claim_reachable_progression_and_unknown_members_are_excluded() {
        let groups = [(34100800, vec![1, 2, 3])].into_iter().collect();
        let mut states = Vec::new();
        append_sweep_trigger_states(
            &mut states,
            &groups,
            &HashMap::new(),
            &[1, 2].into_iter().collect(),
            &[1, 3].into_iter().collect(),
            &[2, 3].into_iter().collect(),
        );
        assert_eq!(states[0].flags, 7);
        states.clear();
        append_sweep_trigger_states(
            &mut states,
            &groups,
            &[(34100800, vec![4])].into_iter().collect(),
            &[1, 2].into_iter().collect(),
            &[1].into_iter().collect(),
            &[1].into_iter().collect(),
        );
        assert!(
            states.is_empty(),
            "existing boss states must not be duplicated"
        );
    }
}
