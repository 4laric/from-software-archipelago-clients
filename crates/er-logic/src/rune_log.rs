//! `rune_log` -- the wording of every rune-count line the client emits (world issue #259).
//!
//! ## Why this exists
//!
//! 2026-08-01, client `7400666` 0.3.0: after an Alt-F4 and reconnect Alaric reported that "the
//! mechanism maybe gave me some runes." That report was **unfalsifiable in either direction**,
//! because nothing anywhere logged the player's rune count. Not "hard to check" -- *impossible*,
//! from any artefact we produce. The leading hypothesis (vanilla Alt-F4 rollback to the last
//! autosave) was a guess, and stayed one.
//!
//! > *"When a number looks wrong, do not reason about it. Instrument it."* -- CONTRIBUTING,
//! > "The tell".
//!
//! ## What gets logged, and what deliberately does not
//!
//! Three sites, all of them **edges**:
//!
//! * once at connect ([`Sample::Connect`]),
//! * once per in-world false->true edge ([`Sample::WorldEdge`], carrying the world epoch),
//! * before and after **every** client-side write ([`report_write`]), naming the caller.
//!
//! Never per tick. Rune count moves constantly in normal play, and a per-tick line is noise that
//! gets filtered out and takes the useful lines with it.
//!
//! The edges are the point: a rollback, a keep-runes restore, a save-load clobber and a legitimate
//! boss payout are **identical in a single sample**. They are only distinguishable as a *pair* of
//! readings either side of an edge, which is exactly what the report above lacked.
//!
//! ## Not-knowing is louder than knowing
//!
//! `GameDataMan` is down at the main menu and during parts of a load, so both the read and the
//! write can fail to answer. Every one of those cases gets its own line saying so
//! ([`describe_sample`] with `None`, [`WriteReport::landed`] false) rather than silence -- a write
//! that changes the player's rune count and does not say so, or fails to and does not say so, is
//! the "polite false" class this repo keeps paying for.
//!
//! ## The read-back
//!
//! [`report_write`] takes the value observed AFTER the write, not the value we asked for. A write
//! we did not verify landed is not evidence that it landed (CONTRIBUTING, *Runtime visibility* --
//! "reconcile, don't dispatch"), and a log line that prints the requested value as though it were
//! the outcome would be a confident wrong answer in the one artefact built to prevent them.
//!
//! Pure: values in, strings out, no game, no I/O. `eldenring-archipelago::runes` is the caller.

/// A point at which the rune count is sampled. Both are edges; neither fires per tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sample {
    /// Once per connect, from the slot_data parse, beside the connect banner.
    Connect,
    /// Once per in-world false->true edge, carrying the world epoch `detour::on_world_edge`
    /// just bumped -- so two readings can be told apart even when the numbers are equal.
    WorldEdge { epoch: u64 },
}

impl Sample {
    fn site(&self) -> String {
        match self {
            Sample::Connect => "at connect".to_string(),
            Sample::WorldEdge { epoch } => format!("at world edge (epoch {epoch})"),
        }
    }
}

/// One rune-count reading, rendered.
///
/// `runes` is `None` when `GameDataMan` was down -- at the main menu, or mid-load. That is a real
/// answer ("we could not see it"), and it is written down as one rather than skipped, so a gap in
/// the readings is never ambiguous between "we did not look" and "we looked and could not tell".
pub fn describe_sample(sample: Sample, runes: Option<u32>) -> String {
    match runes {
        Some(n) => format!("runes: {n} held {}", sample.site()),
        None => format!(
            "runes: UNREADABLE {} -- GameDataMan is down (main menu or mid-load)",
            sample.site()
        ),
    }
}

/// The rendered result of one client-side rune write.
pub struct WriteReport {
    /// The line to log.
    pub line: String,
    /// Did the read-back agree with what we asked for? `false` means the write did NOT take
    /// effect -- log it at WARN, and do not let a caller treat it as done.
    pub landed: bool,
}

/// Render a client-side write of the rune count, before -> after, with the caller named.
///
/// * `cause` -- who asked for the write, in words a log reader can act on ("keep-runes restore").
/// * `before` -- the reading taken immediately before, or `None` if unreadable.
/// * `requested` -- the value we asked the game for.
/// * `after` -- the value read BACK after the write, or `None` if the write was refused (or the
///   read-back could not be taken).
///
/// Four outcomes, four distinguishable lines. The `after != requested` case is the one worth the
/// extra branch: it is the only shape that says "we wrote and the game did not keep it", which is
/// otherwise indistinguishable from a successful write in every artefact we have.
pub fn report_write(
    cause: &str,
    before: Option<u32>,
    requested: u32,
    after: Option<u32>,
) -> WriteReport {
    let from = match before {
        Some(n) => n.to_string(),
        None => "UNREADABLE".to_string(),
    };
    match after {
        Some(observed) if observed == requested => WriteReport {
            line: format!("runes: write {from} -> {observed} ({cause})"),
            landed: true,
        },
        Some(observed) => WriteReport {
            line: format!(
                "runes: write {from} -> {requested} requested, read-back says {observed} \
                 -- WRITE DID NOT LAND ({cause})"
            ),
            landed: false,
        },
        None => WriteReport {
            line: format!(
                "runes: write {from} -> {requested} REFUSED -- GameDataMan is down, rune count \
                 UNCHANGED ({cause})"
            ),
            landed: false,
        },
    }
}

/// Should a RE-ASSERTED rune write actually fire?
///
/// DeathLink's keep-runes restore re-asserts the same value for
/// [`crate::deathlink::RESTORE_REASSERT_TICKS`] consecutive alive ticks, to beat an engine zero
/// that lands a tick or two after control returns. Routing all five through an unconditional
/// logging write would put five identical lines in the log for one restore -- which is the
/// per-tick noise this module exists to avoid, and it would bury the one line that matters.
///
/// So a re-assertion writes only when the game does NOT already hold the value. The interesting
/// case survives intact: if the engine zeroes late, tick N+2 reads 0, disagrees, writes, and gets
/// its line -- and that line is now *evidence of the late zero* rather than one of five duplicates.
///
/// `current == None` (GameDataMan down) returns true on purpose. "We could not read it" is not
/// "it is already correct"; attempt the write and let the refusal be logged.
pub fn needs_write(current: Option<u32>, requested: u32) -> bool {
    current != Some(requested)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------------------
    // The motivating case (CONTRIBUTING rule 11): the Alt-F4/reconnect report. Two readings
    // either side of the edge is the whole artefact that was missing, so it is the first test.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn a_pair_of_readings_brackets_an_alt_f4_reconnect() {
        // Last thing the pre-crash session said, then the first thing the new one says.
        let before_crash = describe_sample(Sample::WorldEdge { epoch: 4 }, Some(120_450));
        let after_reconnect = describe_sample(Sample::Connect, Some(138_900));

        assert_eq!(before_crash, "runes: 120450 held at world edge (epoch 4)");
        assert_eq!(after_reconnect, "runes: 138900 held at connect");

        // The point of the pair: the discontinuity is READABLE off the log, by a human, with no
        // debugger. 18450 runes appeared across the gap and no write line sits between them, so
        // it was not us -- which is the sentence the 2026-08-01 report could not be given.
        assert!(before_crash.contains("120450") && after_reconnect.contains("138900"));
    }

    #[test]
    fn connect_and_world_edge_are_distinguishable_and_the_epoch_is_carried() {
        assert_eq!(
            describe_sample(Sample::Connect, Some(7)),
            "runes: 7 held at connect"
        );
        assert_eq!(
            describe_sample(Sample::WorldEdge { epoch: 0 }, Some(7)),
            "runes: 7 held at world edge (epoch 0)"
        );
        // Two edges with the SAME count must still be tellable apart, or a log with a stable rune
        // count reads as one edge repeated. The epoch is what does that.
        assert_ne!(
            describe_sample(Sample::WorldEdge { epoch: 1 }, Some(7)),
            describe_sample(Sample::WorldEdge { epoch: 2 }, Some(7))
        );
    }

    #[test]
    fn an_unreadable_sample_says_so_instead_of_going_quiet() {
        let line = describe_sample(Sample::Connect, None);
        assert!(line.starts_with("runes: UNREADABLE at connect"), "{line}");
        // It must not be mistakable for a reading of zero -- "0 runes at connect" and "we could
        // not see the rune count" are opposite facts about a rollback report.
        assert!(!line.contains(" 0 "), "{line}");
    }

    #[test]
    fn a_zero_reading_is_a_reading_not_an_absence() {
        assert_eq!(
            describe_sample(Sample::Connect, Some(0)),
            "runes: 0 held at connect"
        );
    }

    // ---------------------------------------------------------------------------------------
    // Writes. Acceptance: "force a write of a known value and confirm the log shows exactly
    // that transition".
    // ---------------------------------------------------------------------------------------

    #[test]
    fn a_write_shows_old_new_and_cause() {
        let r = report_write("keep-runes restore", Some(12345), 67890, Some(67890));
        assert_eq!(r.line, "runes: write 12345 -> 67890 (keep-runes restore)");
        assert!(r.landed);
    }

    #[test]
    fn the_keep_runes_withhold_write_is_reported_too() {
        // The zeroing half of DeathLink keep-runes is a client write of the rune count, so it
        // reports like any other. It was previously visible only as a side clause of the kill
        // line ("N runes withheld"), which is a different claim from "we set the count to 0".
        let r = report_write(
            "keep-runes withhold (incoming DeathLink kill)",
            Some(4200),
            0,
            Some(0),
        );
        assert_eq!(
            r.line,
            "runes: write 4200 -> 0 (keep-runes withhold (incoming DeathLink kill))"
        );
        assert!(r.landed);
    }

    #[test]
    fn a_write_that_did_not_land_is_not_reported_as_a_write() {
        // Rule 7, "verify the fix by breaking it": this is the fix broken on purpose. The game
        // kept the old value, and the line must say that in words rather than print the number
        // we hoped for.
        let r = report_write("keep-runes restore", Some(12345), 67890, Some(12345));
        assert!(!r.landed);
        assert!(r.line.contains("WRITE DID NOT LAND"), "{}", r.line);
        assert!(r.line.contains("read-back says 12345"), "{}", r.line);
        // And it must not read as a completed 12345 -> 67890 transition.
        assert!(!r.line.contains("-> 67890 (keep"), "{}", r.line);
    }

    #[test]
    fn a_refused_write_names_the_reason_and_says_the_count_is_unchanged() {
        let r = report_write("keep-runes restore", Some(12345), 67890, None);
        assert!(!r.landed);
        assert!(r.line.contains("REFUSED"), "{}", r.line);
        assert!(r.line.contains("UNCHANGED"), "{}", r.line);
    }

    #[test]
    fn a_write_with_an_unreadable_before_still_reports() {
        // Belt and braces: if the pre-read failed but the write took, we still say what happened
        // rather than dropping the only record of a rune-count change.
        let r = report_write("keep-runes restore", None, 500, Some(500));
        assert_eq!(
            r.line,
            "runes: write UNREADABLE -> 500 (keep-runes restore)"
        );
        assert!(r.landed);
    }

    // ---------------------------------------------------------------------------------------
    // The re-assert window: five ticks of the same value must not become five log lines.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn a_reassertion_of_a_value_the_game_already_holds_does_not_write() {
        assert!(!needs_write(Some(4200), 4200));
    }

    #[test]
    fn a_reassertion_fires_when_the_engine_zeroed_us_late() {
        // The whole reason RESTORE_REASSERT_TICKS exists. Tick 1 wrote 4200; the engine's death
        // bank landed afterwards and set 0; tick 3 must notice and write again -- and that write
        // is the one line in the window worth having.
        assert!(needs_write(Some(0), 4200));
    }

    #[test]
    fn an_unreadable_count_is_not_treated_as_already_correct() {
        assert!(needs_write(None, 4200));
    }

    #[test]
    fn a_full_reassert_window_produces_exactly_one_line_when_nothing_disturbs_it() {
        // Model the client loop: poll_restore yields the same owed value on 5 consecutive alive
        // ticks. Nothing else touches the count, so exactly one write -- and one line -- happens.
        let owed = 4200u32;
        let mut game = Some(0u32); // zeroed by the kill
        let mut lines = Vec::new();
        for _ in 0..crate::deathlink::RESTORE_REASSERT_TICKS {
            if needs_write(game, owed) {
                let before = game;
                game = Some(owed);
                lines.push(report_write("keep-runes restore", before, owed, game).line);
            }
        }
        assert_eq!(
            lines,
            vec!["runes: write 0 -> 4200 (keep-runes restore)".to_string()]
        );
        assert_eq!(game, Some(owed));
    }

    #[test]
    fn a_late_engine_zero_inside_the_window_gets_its_own_line() {
        // Same window, but the engine banks the death on tick 3. Two lines, and the second one is
        // the artefact that tells you the late zero happened at all.
        let owed = 4200u32;
        let mut game = Some(0u32);
        let mut lines = Vec::new();
        for tick in 0..crate::deathlink::RESTORE_REASSERT_TICKS {
            if tick == 2 {
                game = Some(0); // the engine's late bank
            }
            if needs_write(game, owed) {
                let before = game;
                game = Some(owed);
                lines.push(report_write("keep-runes restore", before, owed, game).line);
            }
        }
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines
            .iter()
            .all(|l| l == "runes: write 0 -> 4200 (keep-runes restore)"));
    }

    #[test]
    fn every_line_is_greppable_by_one_prefix() {
        // One prefix pulls the whole rune story out of a session log. If a line ever stops
        // starting with it, reconstructing a run means knowing all the wordings in advance.
        let lines = [
            describe_sample(Sample::Connect, Some(1)),
            describe_sample(Sample::Connect, None),
            describe_sample(Sample::WorldEdge { epoch: 9 }, Some(1)),
            describe_sample(Sample::WorldEdge { epoch: 9 }, None),
            report_write("c", Some(1), 2, Some(2)).line,
            report_write("c", Some(1), 2, Some(1)).line,
            report_write("c", Some(1), 2, None).line,
            report_write("c", None, 2, Some(2)).line,
        ];
        for line in lines {
            assert!(line.starts_with("runes: "), "{line}");
            // In-game strings are ASCII-only in this project; log lines are held to the same bar
            // so a log can be pasted anywhere without mojibake.
            assert!(line.is_ascii(), "{line}");
        }
    }
}
