//! Pure region-completion calculation for the optional goal-region gate.

use std::collections::{HashMap, HashSet};

pub fn incomplete_regions(
    surface: &HashSet<u64>,
    location_regions: &HashMap<u64, String>,
    goal_region: Option<&str>,
    checked: &HashSet<u64>,
    viewed_shops: &HashSet<i64>,
) -> Vec<String> {
    let mut by_region: HashMap<&str, Vec<u64>> = HashMap::new();
    for &location in surface {
        let Some(region) = location_regions.get(&location).map(String::as_str) else {
            continue;
        };
        if region == "Roundtable Hold" || Some(region) == goal_region {
            continue;
        }
        by_region.entry(region).or_default().push(location);
    }
    let mut incomplete: Vec<String> = by_region
        .into_iter()
        .filter(|(_, locations)| {
            locations.iter().any(|location| {
                !checked.contains(location) && !viewed_shops.contains(&(*location as i64))
            })
        })
        .map(|(region, _)| region.to_owned())
        .collect();
    incomplete.sort();
    incomplete
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(values: &[u64]) -> HashSet<u64> {
        values.iter().copied().collect()
    }

    #[test]
    fn every_surface_check_in_each_non_goal_region_is_required() {
        let regions = HashMap::from([
            (1, "Limgrave".into()),
            (2, "Limgrave".into()),
            (3, "Caelid".into()),
            (4, "Ashen Capital".into()),
        ]);
        assert_eq!(
            incomplete_regions(
                &set(&[1, 2, 3, 4]),
                &regions,
                Some("Ashen Capital"),
                &set(&[1]),
                &HashSet::new()
            ),
            vec!["Caelid", "Limgrave"]
        );
    }

    #[test]
    fn viewing_a_shop_satisfies_it_without_a_location_check() {
        let regions = HashMap::from([(10, "Stormveil".into())]);
        let viewed = HashSet::from([10_i64]);
        assert!(
            incomplete_regions(&set(&[10]), &regions, None, &HashSet::new(), &viewed).is_empty()
        );
    }

    #[test]
    fn non_surface_checks_do_not_count_and_starting_regions_are_not_special() {
        let regions = HashMap::from([(1, "Starting Region".into()), (2, "Starting Region".into())]);
        assert!(
            incomplete_regions(&set(&[1]), &regions, None, &set(&[1]), &HashSet::new()).is_empty()
        );
    }
}
