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
