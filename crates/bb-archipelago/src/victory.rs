use std::fmt::Write as _;

use crate::ledger::VictoryRecord;

fn count(value: Option<u32>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
}

fn elapsed(value: Option<u64>) -> String {
    value.map_or_else(
        || "unknown".to_owned(),
        |seconds| {
            format!(
                "{:02}:{:02}:{:02}",
                seconds / 3_600,
                (seconds % 3_600) / 60,
                seconds % 60
            )
        },
    )
}

pub fn summary_text(record: &VictoryRecord) -> String {
    let checks = match (record.checks_completed, record.checks_total) {
        (Some(completed), Some(total)) => format!("{completed}/{total}"),
        _ => "unknown".to_owned(),
    };
    let mut text = String::new();
    let _ = writeln!(text, "Bloodborne Archipelago victory");
    let _ = writeln!(text, "Goal: {}", record.goal_name);
    let _ = writeln!(text, "Completion time: {}", elapsed(record.elapsed_seconds));
    let _ = writeln!(text, "Checks: {checks}");
    let _ = writeln!(text, "Received items: {}", count(record.received_items));
    let _ = writeln!(text, "Sent items: {}", count(record.sent_items));
    let _ = writeln!(text, "Deaths: {}", count(record.deaths));
    let _ = writeln!(text, "DeathLinks: {}", count(record.death_links));
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_counters_remain_unknown() {
        let text = summary_text(&VictoryRecord {
            goal_location: 1,
            goal_name: "Moon Presence".into(),
            completed_at_ms: 2,
            elapsed_seconds: None,
            checks_completed: Some(80),
            checks_total: None,
            received_items: None,
            sent_items: None,
            deaths: None,
            death_links: None,
        });
        assert!(text.contains("Completion time: unknown"));
        assert!(text.contains("Checks: unknown"));
        assert!(text.contains("Received items: unknown"));
        assert!(!text.contains("Deaths: 0"));
    }
}
