//! Player-facing names for Archipelago items and locations.
//!
//! The client window used to print `Received AP item 12255536 (index 1)`. That line is a datapackage
//! lookup away from `Received Saw Cleaver`, and the lookup belongs on this side of the UI seam:
//! `client-ui` deliberately never sees a game, an item id, or a datapackage, so a renderer has
//! nothing to resolve a name *with*. Resolution happens in the worker, and only resolved text
//! crosses the bridge.
//!
//! These functions take names that a caller has already looked up, so the formatting policy -- what
//! an unresolvable id looks like, how many names fit on one line -- is testable without a server.

/// How many location names one feed line will spell out before it summarises.
const NAMED_LOCATIONS: usize = 3;

/// One item, named if the datapackage knew it.
///
/// The `item #id` fallback is deliberately kept: an id that the datapackage cannot name is rare and
/// is itself a diagnostic, so replacing it with "unknown item" would destroy the only handle a
/// player could quote in a bug report.
pub fn item_label(name: Option<&str>, ap_item_id: i64) -> String {
    match name {
        Some(name) if !name.trim().is_empty() => name.to_owned(),
        _ => format!("item #{ap_item_id}"),
    }
}

/// The feed line for a batch of location checks that just went out.
///
/// A release flood sends hundreds at once, so the line names the first few and counts the rest;
/// the old `Sent 214 location check(s)` named none of them and the new one must not be 214 names
/// long either.
pub fn location_check_line(names: &[String]) -> String {
    match names.len() {
        0 => "Sent no location checks".to_owned(),
        1 => format!("Sent check: {}", names[0]),
        count if count <= NAMED_LOCATIONS => format!("Sent {count} checks: {}", names.join(", ")),
        count => format!(
            "Sent {count} checks: {} and {} more",
            names[..NAMED_LOCATIONS].join(", "),
            count - NAMED_LOCATIONS
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_item_never_shows_its_id() {
        assert_eq!(item_label(Some("Saw Cleaver"), 12_255_536), "Saw Cleaver");
        // Whitespace-only is not a name: the datapackage answered, but not usefully.
        assert_eq!(item_label(Some("  "), 12_255_536), "item #12255536");
        assert_eq!(item_label(None, 12_255_536), "item #12255536");
        // The debug-formatted shape the window used to show is gone.
        assert!(!item_label(Some("Saw Cleaver"), 1).contains("AP item"));
    }

    #[test]
    fn a_check_line_names_what_it_can_and_counts_the_rest() {
        let names = |count: usize| {
            (0..count)
                .map(|index| format!("Location {index}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(location_check_line(&names(0)), "Sent no location checks");
        assert_eq!(location_check_line(&names(1)), "Sent check: Location 0");
        assert_eq!(
            location_check_line(&names(3)),
            "Sent 3 checks: Location 0, Location 1, Location 2"
        );
        assert_eq!(
            location_check_line(&names(7)),
            "Sent 7 checks: Location 0, Location 1, Location 2 and 4 more"
        );
    }

    #[test]
    fn a_flood_of_checks_stays_one_readable_line() {
        let names = (0..214)
            .map(|index| format!("Location {index}"))
            .collect::<Vec<_>>();
        let line = location_check_line(&names);
        assert!(line.starts_with("Sent 214 checks: "));
        assert!(line.ends_with("and 211 more"));
        assert!(line.len() < 120, "one line, not a wall: {line}");
    }
}
