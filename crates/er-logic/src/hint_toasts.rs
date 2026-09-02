//! Pure classification, deduplication and burst coalescing for streamed AP hint toasts.
//!
//! The server replays standing hints on connect. The first observed batch is therefore a
//! baseline, not news; later packets are deduplicated by their stable four-part identity. The
//! full standing ledger remains owned by `tracker::HintSet` and is never filtered here.

use std::collections::HashSet;

const MAX_LINE_CHARS: usize = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HintIdentity {
    pub item_id: i64,
    pub location_id: i64,
    pub finding_slot: i32,
    pub receiving_slot: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintNotice {
    pub identity: HintIdentity,
    pub item_name: String,
    pub location_name: String,
    pub finding_player: String,
    pub receiving_player: String,
}

#[derive(Clone, Debug, Default)]
pub struct HintToastFeed {
    primed: bool,
    seen: HashSet<HintIdentity>,
}

impl HintToastFeed {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume one newly-arrived packet batch. The first batch establishes the standing-hint
    /// baseline and is silent. Subsequent batches return at most three presentation lines.
    pub fn ingest(
        &mut self,
        local_slot: i32,
        batch: impl IntoIterator<Item = HintNotice>,
    ) -> Vec<String> {
        let mut eligible = Vec::new();
        for hint in batch {
            if !self.seen.insert(hint.identity) {
                continue;
            }
            if self.primed {
                if let Some(line) = format_notice(local_slot, &hint) {
                    eligible.push(line);
                }
            }
        }
        if !self.primed {
            self.primed = true;
            return Vec::new();
        }
        if eligible.len() <= 3 {
            return eligible;
        }
        let more = eligible.len() - 2;
        eligible.truncate(2);
        eligible.push(format!("...and {more} more hints (F6)"));
        eligible
    }
}

fn ascii(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_control() {
                ' '
            } else if c.is_ascii() {
                c
            } else {
                '?'
            }
        })
        .collect()
}

fn fit(prefix: &str, item: &str, suffix: &str) -> String {
    let prefix = ascii(prefix);
    let suffix = ascii(suffix);
    let item = ascii(item);
    let fixed = prefix.chars().count() + suffix.chars().count();
    if fixed >= MAX_LINE_CHARS {
        return format!("{prefix}{suffix}")
            .chars()
            .take(MAX_LINE_CHARS)
            .collect();
    }
    let room = MAX_LINE_CHARS - fixed;
    let item_len = item.chars().count();
    let item = if item_len <= room {
        item
    } else if room <= 3 {
        ".".repeat(room)
    } else {
        let mut shortened: String = item.chars().take(room - 3).collect();
        shortened.push_str("...");
        shortened
    };
    format!("{prefix}{item}{suffix}")
}

fn format_notice(local_slot: i32, hint: &HintNotice) -> Option<String> {
    if hint.identity.receiving_slot == local_slot {
        let suffix = format!(
            " at {}'s {}",
            ascii(&hint.finding_player),
            ascii(&hint.location_name)
        );
        Some(fit("Hint: ", &hint.item_name, &suffix))
    } else if hint.identity.finding_slot == local_slot {
        let suffix = format!(
            " for {} at {}",
            ascii(&hint.receiving_player),
            ascii(&hint.location_name)
        );
        Some(fit("Your world: ", &hint.item_name, &suffix))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(n: i64, finding: i32, receiving: i32) -> HintNotice {
        HintNotice {
            identity: HintIdentity {
                item_id: 100 + n,
                location_id: 200 + n,
                finding_slot: finding,
                receiving_slot: receiving,
            },
            item_name: format!("Item {n}"),
            location_name: format!("Location {n}"),
            finding_player: format!("Player {finding}"),
            receiving_player: format!("Player {receiving}"),
        }
    }

    #[test]
    fn standing_connect_batch_is_a_silent_baseline() {
        let mut feed = HintToastFeed::new();
        assert!(feed.ingest(1, [hint(1, 2, 1), hint(2, 1, 2)]).is_empty());
    }

    #[test]
    fn local_incoming_and_actionable_hints_are_classified() {
        let mut feed = HintToastFeed::new();
        feed.ingest(1, []);
        assert_eq!(
            feed.ingest(1, [hint(1, 2, 1), hint(2, 1, 2), hint(3, 2, 3)]),
            [
                "Hint: Item 1 at Player 2's Location 1",
                "Your world: Item 2 for Player 2 at Location 2",
            ]
        );
    }

    #[test]
    fn duplicates_and_found_transitions_do_not_retoast() {
        let mut feed = HintToastFeed::new();
        feed.ingest(1, []);
        let h = hint(1, 2, 1);
        assert_eq!(feed.ingest(1, [h.clone()]).len(), 1);
        assert!(feed.ingest(1, [h]).is_empty());
    }

    #[test]
    fn bursts_keep_two_and_summarize_the_rest() {
        let mut feed = HintToastFeed::new();
        feed.ingest(1, []);
        let lines = feed.ingest(1, (0..5).map(|n| hint(n, 2, 1)));
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2], "...and 3 more hints (F6)");
    }

    #[test]
    fn output_is_ascii_single_line_and_bounded() {
        let mut feed = HintToastFeed::new();
        feed.ingest(1, []);
        let mut h = hint(1, 2, 1);
        h.item_name = "Runebear's extraordinarily long progression widget — deluxe".into();
        h.location_name = "Café\nCatacombs".into();
        let line = &feed.ingest(1, [h])[0];
        assert!(line.is_ascii());
        assert!(!line.contains('\n'));
        assert!(line.chars().count() <= MAX_LINE_CHARS);
        assert!(line.contains("Catacombs"), "location is preserved: {line}");
    }
}
