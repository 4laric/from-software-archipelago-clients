//! The sentences the goal shows the PLAYER, as opposed to the ones it writes to the log
//! (world#656).
//!
//! # The motivating case
//!
//! **AHHHREPTAR**, v0.4.0, 2026-08-14: he finished a `great_runes` seed holding four Great Runes
//! and no victory was sent. His own diagnosis, arrived at by reading the spoiler log:
//!
//! > I think it expected me to get a very specific four great runes, rather than just any four of
//! > the six.
//!
//! Correct. `core._resolve_required_runes` is `sorted(avail)[:want]`, so at `goal_great_runes: 4`
//! the required set is a specific four, and four others complete nothing.
//!
//! # 🛑 THE DATA WAS NEVER MISSING. THE CHANNEL WAS.
//!
//! It is tempting to read #656 as "the required names are discarded at generation time". They are
//! not, and that matters for the size of the fix:
//!
//! * the apworld emits them -- `"great_rune_items": list(required)` is in slot_data;
//! * the contract declares them -- `contract_gen.rs` carries `great_rune_items`;
//! * the client parses them into `GoalConfig::item_goals` and **already logs the names**.
//!
//! Every half worked, and a player still had to open the spoiler. `log::info!` goes to
//! `archipelago-<date>.log`, which is the artifact we receive *after* someone reports a problem;
//! the player is looking at the game. So this is not a new contract key, not a three-repo change,
//! and not a generation fix -- it is the same string on the channel the player actually reads.
//!
//! # What the sentence has to say
//!
//! `release/EldenRing.yaml` promises "collect `goal_great_runes` Great Runes", which reads as **any
//! N**, and the yaml comment carefully heads off a *lesser* misreading (that killing the bearer
//! counts) while leaving the one that actually ends runs. So the line has to carry both facts:
//! HOLD them, and they are a SPECIFIC set. Saying only "you must hold N items" would reproduce the
//! template's own ambiguity in a second place.

/// One line naming every item the goal requires, or `None` when it requires none.
///
/// Deliberately says "a SPECIFIC set chosen for this seed" rather than listing a count alone: the
/// count is exactly what the yaml already promises, and the count is what misled the player this
/// exists for.
pub fn required_items_line(items: &[String]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    Some(format!(
        "goal: you must HOLD these {} item(s) -- killing the boss that vanilla-drops one is NOT \
         enough, and they are a SPECIFIC set chosen for this seed, not any {} of a kind: {}",
        items.len(),
        items.len(),
        items.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// ⭐ AHHHREPTAR's seed. He held four Great Runes and the run did not end; the line has to name
    /// WHICH four, and say they are a specific set rather than any four.
    #[test]
    fn the_line_names_the_runes_and_says_they_are_specific() {
        let line = required_items_line(&v(&[
            "Godrick's Great Rune",
            "Malenia's Great Rune",
            "Mohg's Great Rune",
            "Morgott's Great Rune",
        ]))
        .expect("a goal with items produces a line");
        for name in [
            "Godrick's Great Rune",
            "Malenia's Great Rune",
            "Mohg's Great Rune",
            "Morgott's Great Rune",
        ] {
            assert!(line.contains(name), "every required item is named: {line}");
        }
        assert!(
            line.contains("SPECIFIC"),
            "a count alone is what the yaml already promises, and what misled him: {line}"
        );
        assert!(
            line.contains("HOLD"),
            "the other half of the yaml's promise: {line}"
        );
    }

    /// 🛑 A REGION_LOCKS SEED GAINS NO BANNER. Most seeds require no items; a line saying so would
    /// be noise on every connect, and noise is how the existing log line got ignored.
    #[test]
    fn no_items_means_no_line() {
        assert_eq!(required_items_line(&[]), None);
    }

    /// The item list is not only Great Runes -- `goalRequiredItems` puts the kept Region Locks in
    /// the same bucket, and a great_runes seed needs the runes AND the locks. One line, all of it.
    #[test]
    fn region_locks_ride_the_same_line() {
        let line = required_items_line(&v(&[
            "Godrick's Great Rune",
            "Liurnia Lock",
            "Rauh Base Lock",
        ]))
        .unwrap();
        assert!(
            line.contains("Liurnia Lock") && line.contains("Rauh Base Lock"),
            "{line}"
        );
        assert!(line.contains("3 item(s)"), "{line}");
    }

    /// A single requirement still reads as a sentence rather than a fragment.
    #[test]
    fn one_item_is_still_a_sentence() {
        let line = required_items_line(&v(&["Godrick's Great Rune"])).unwrap();
        assert!(line.contains("1 item(s)"), "{line}");
        assert!(line.contains("Godrick's Great Rune"), "{line}");
    }

    /// In-game strings are ASCII-only (repo rule 10). Item names come off the wire, so the check is
    /// on the FRAME rather than on the whole line -- a foreign apworld may legitimately send
    /// non-ASCII names and that must not be this function's problem.
    #[test]
    fn the_frame_is_ascii() {
        let line = required_items_line(&v(&["A", "B"])).unwrap();
        assert!(line.is_ascii(), "{line}");
    }
}
