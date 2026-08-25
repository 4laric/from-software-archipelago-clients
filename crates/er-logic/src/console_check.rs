//! `!check <name>` console lookup helpers (#1008): resolve a location's acquisition flag by name so
//! a player can `!setflag` an unsent check. Steve, on Discord: "can I somehow check for the ids
//! myself? cuz I have a bunch of these that didn't fire." The flag-poll already reports a check when
//! its acquisition flag is SET, so setting the flag sends the check on the next poll -- the manual
//! sibling of #1000 (auto-releasing enemy-drop checks whose flag can't fire under enemy rando).
//!
//! Pure formatting/filtering; the client resolves flag-poll location ids to display names and hands
//! them in.

/// One console line for a matched check: name, its acquisition flag, whether the flag is set now,
/// and a ready-to-paste `!setflag`. Mirrors [`crate::grace::console_grace_line`].
pub fn console_check_line(name: &str, flag: u32, set: bool) -> String {
    format!("{name}: flag {flag} = {set}; !setflag {flag} 1")
}

/// The one-line caveat printed under the matches: setting a check's acquisition flag also lets its
/// VANILLA item leak locally (the suppressor keys the same flag), so it is for checks that never
/// fired, not for double-dipping a normal pickup.
pub const CHECK_CAVEAT: &str =
    "note: !setflag on a check also drops its vanilla item locally -- use it for checks that never fired.";

/// Filter `(name, flag, set)` entries by a lowercase-substring `query`, sort by name, cap the count.
/// Returns the formatted lines plus how many matches the cap dropped. Pure -- unit-tested.
pub fn matched_lines(
    entries: impl IntoIterator<Item = (String, u32, bool)>,
    query: &str,
    cap: usize,
) -> (Vec<String>, usize) {
    let mut hits: Vec<(String, u32, bool)> = entries
        .into_iter()
        .filter(|(n, _, _)| n.to_lowercase().contains(query))
        .collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0));
    let total = hits.len();
    let lines: Vec<String> = hits
        .into_iter()
        .take(cap)
        .map(|(n, f, s)| console_check_line(&n, f, s))
        .collect();
    let dropped = total - lines.len();
    (lines, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_carries_flag_state_and_a_ready_setflag() {
        assert_eq!(
            console_check_line("Ainsel River :: Larval Tear", 12017965, false),
            "Ainsel River :: Larval Tear: flag 12017965 = false; !setflag 12017965 1"
        );
    }

    #[test]
    fn matched_lines_filters_sorts_and_caps() {
        let entries = vec![
            ("Zed Tear".to_string(), 3, false),
            ("Alpha Larval Tear".to_string(), 1, true),
            ("Beta Larval Tear".to_string(), 2, false),
            ("Unrelated Rune".to_string(), 9, false),
        ];
        // substring match on "tear", sorted by name, cap 2 -> 2 shown, 1 dropped (Zed).
        let (lines, dropped) = matched_lines(entries, "tear", 2);
        assert_eq!(lines.len(), 2);
        assert_eq!(dropped, 1);
        assert!(
            lines[0].starts_with("Alpha Larval Tear:"),
            "sorted by name: {:?}",
            lines
        );
        assert!(lines[1].starts_with("Beta Larval Tear:"));
        // the non-matching Rune is excluded.
        assert!(!lines.iter().any(|l| l.contains("Rune")));
    }

    #[test]
    fn no_match_yields_no_lines() {
        let (lines, dropped) = matched_lines(std::iter::empty(), "anything", 10);
        assert!(lines.is_empty());
        assert_eq!(dropped, 0);
    }
}
