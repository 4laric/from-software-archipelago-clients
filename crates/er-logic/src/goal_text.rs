//! Player-facing description of held-item goal requirements (world#813).

/// Describe required Region Locks and the count-based Great Rune goal.
pub fn required_items_line(
    items: &[String],
    rune_items: &[String],
    runes_required: usize,
) -> Option<String> {
    let rune_count = runes_required.min(rune_items.len());
    if items.is_empty() && rune_count == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if !items.is_empty() {
        parts.push(format!(
            "you must HOLD these {} item(s): {}",
            items.len(),
            items.join(", ")
        ));
    }
    if rune_count != 0 {
        parts.push(format!(
            "hold any {} of these {} Great Runes: {}",
            rune_count,
            rune_items.len(),
            rune_items.join(", ")
        ));
    }
    Some(format!("goal: {}", parts.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn any_four_of_seven_is_explicit() {
        let runes = v(&[
            "Godrick's Great Rune",
            "Great Rune of the Unborn",
            "Malenia's Great Rune",
            "Mohg's Great Rune",
            "Morgott's Great Rune",
            "Radahn's Great Rune",
            "Rykard's Great Rune",
        ]);
        let line = required_items_line(&[], &runes, 4).unwrap();
        assert!(line.contains("any 4 of these 7 Great Runes"), "{line}");
        assert!(!line.contains("SPECIFIC"), "{line}");
    }

    #[test]
    fn locks_and_runes_are_described_separately() {
        let line = required_items_line(
            &v(&["Liurnia Lock", "Stormveil Lock"]),
            &v(&["Godrick's Great Rune", "Rykard's Great Rune"]),
            1,
        )
        .unwrap();
        assert!(line.contains("HOLD these 2 item(s)"), "{line}");
        assert!(line.contains("any 1 of these 2 Great Runes"), "{line}");
    }

    #[test]
    fn no_requirements_means_no_line() {
        assert_eq!(required_items_line(&[], &[], 0), None);
    }

    #[test]
    fn frame_is_ascii() {
        assert!(required_items_line(&v(&["A"]), &v(&["B"]), 1)
            .unwrap()
            .is_ascii());
    }
}
