//! Presentation-only names for local ER log locations; protocol text is untouched.
pub fn compact(name: &str, progression: bool, sweep_eligible: Option<bool>, swept: bool) -> String {
    let mut text = name;
    if let Some((head, tail)) = text.rsplit_once(" [f") {
        if tail
            .strip_suffix(']')
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        {
            text = head;
        }
    }
    if let Some((head, tail)) = text.rsplit_once(" (m") {
        if tail.strip_suffix(')').is_some_and(|tile| {
            let fields: Vec<_> = tile.split('_').collect();
            (2..=4).contains(&fields.len())
                && fields
                    .iter()
                    .all(|f| f.len() == 2 && f.bytes().all(|b| b.is_ascii_digit()))
        }) {
            text = head;
        }
    }
    let clause = [", may be sweep-granted by ", ", also granted by "]
        .into_iter()
        .filter_map(|opener| text.find(opener).map(|at| (at, opener.len())))
        .min_by_key(|&(at, _)| at);
    let body = match (clause, sweep_eligible) {
        (Some((at, len)), Some(true)) => format!("{}, sweep: {}", &text[..at], &text[at + len..]),
        (Some((at, _)), Some(false)) => text[..at].to_owned(),
        _ => text.to_owned(),
    };
    format!(
        "{}{}{}",
        if progression { "[P] " } else { "" },
        if swept { "[S] " } else { "" },
        body
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    const NAME: &str = "Limgrave :: Item, may be sweep-granted by Tree Sentinel (m60_42_42) [f123]";
    #[test]
    fn compact_seed_eligible_and_observed_grant_are_distinct() {
        assert_eq!(
            compact(NAME, true, Some(true), false),
            "[P] Limgrave :: Item, sweep: Tree Sentinel"
        );
        assert_eq!(
            compact(NAME, false, Some(true), true),
            "[S] Limgrave :: Item, sweep: Tree Sentinel"
        );
        assert_eq!(compact(NAME, false, Some(false), false), "Limgrave :: Item");
    }
    #[test]
    fn boss_parentheses_and_nonidentity_suffix_survive() {
        assert_eq!(
            compact(
                "Item, also granted by Cavalry (Glaive) (m60_48_55) [f9]",
                false,
                Some(true),
                false
            ),
            "Item, sweep: Cavalry (Glaive)"
        );
        assert_eq!(
            compact("Item (region unconfirmed) [friendly]", false, None, false),
            "Item (region unconfirmed) [friendly]"
        );
    }
    #[test]
    fn unknown_seed_retains_uncertainty_and_input_is_immutable() {
        let original = NAME.to_owned();
        assert_eq!(
            compact(&original, false, None, false),
            "Limgrave :: Item, may be sweep-granted by Tree Sentinel"
        );
        assert_eq!(original, NAME);
    }
    #[test]
    fn tile_shapes_only_and_unicode_preserved() {
        assert_eq!(
            compact("Épée (m10_00_00_00) [f999]", false, None, false),
            "Épée"
        );
        assert_eq!(
            compact("Épée (m10_bad) [fno]", false, None, false),
            "Épée (m10_bad) [fno]"
        );
    }
}

/// A member already discovered directly in this poll has ambiguous causality.
/// Do not label it as a sweep grant, even if the same sweep includes it.
pub fn sweep_report_evidence(
    id: i64,
    direct: &std::collections::HashSet<i64>,
    submitted_sorted: &[i64],
) -> bool {
    !direct.contains(&id) && submitted_sorted.binary_search(&id).is_ok()
}

#[cfg(test)]
mod provenance_tests {
    use super::*;
    #[test]
    fn direct_same_poll_and_withheld_checks_do_not_claim_sweep_origin() {
        let direct = [1].into_iter().collect();
        assert!(!sweep_report_evidence(1, &direct, &[1, 2]));
        assert!(sweep_report_evidence(2, &direct, &[1, 2]));
        assert!(!sweep_report_evidence(3, &direct, &[1, 2]));
    }
}
