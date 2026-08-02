//! A [`log::Log`] wrapper that collapses runs of identical records.
//!
//! # Why this exists
//!
//! Five logs uploaded by one player on 2026-08-02 held **612,842 `[ERROR]` lines**, and
//! essentially all of them were one alternating pair, two per frame:
//!
//! ```text
//! [ERROR] Insufficient display size: 0x0
//! [ERROR] Render error: Error { code: HRESULT(0xFFFFFFFF), message: "" }
//! ```
//!
//! Both are emitted by our rendering dependency, not by us. In `hudhook 0.9.0`:
//!
//! * `src/renderer/pipeline.rs:150` — `Pipeline::render` bails with `error!("Insufficient
//!   display size: {w}x{h}")` when `display_size * framebuffer_scale` is zero on either axis,
//!   which is what a **minimised** window reports, and
//! * `src/hooks/dx12.rs:235` — the `IDXGISwapChain::Present` hook logs `error!("Render error:
//!   {e:?}")` for the `HRESULT(-1)` that bail returns.
//!
//! Neither site has a rate limit and hudhook exposes no knob to quiet them, so the pair repeats
//! for as long as the player leaves the game minimised — for hours, at frame rate. Real content
//! drowns, and the log a reporter is asked to upload runs to hundreds of megabytes.
//!
//! # What this does, and what it deliberately does not do
//!
//! It does **not** know anything about those two strings. Matching on message text would rot the
//! moment the dependency rewords it, and would leave every other per-frame log line — ours
//! included — free to do the same thing again. This collapses the *class*: any record that
//! repeats is reported once, then counted.
//!
//! # Semantics
//!
//! A record's identity is `(level, target, formatted message)`. The collapser keeps up to
//! [`WINDOW`] identities "in flight" at once:
//!
//! * The **first** occurrence of an identity is always passed straight through. A novel error
//!   never waits.
//! * Subsequent occurrences of an in-flight identity are counted, not emitted.
//! * A count is reported every [`REPEAT_EVERY`] occurrences **or** every [`REPORT_INTERVAL`],
//!   whichever comes first, as `... repeated N times (T total): <message>` at the original level
//!   and target. So a condition that persists for six hours still leaves periodic evidence with a
//!   rising total, instead of going silent.
//! * An identity that has not recurred within [`RUN_IDLE_TIMEOUT`] stops being in flight; its
//!   tail count is reported and the next occurrence is news again.
//!
//! ⚠️ **`WINDOW` is the whole point of the fix, and it must stay > 1.** The reported flood is two
//! messages that *alternate*. A "same as the previous record?" comparator — the obvious
//! implementation — collapses exactly nothing here, because no two adjacent records are ever
//! equal. `the_real_alternating_pair_at_frame_rate_collapses` is the test that pins this.
//!
//! # Cost
//!
//! This sits on a path that runs twice per frame. Per record: one reused-buffer format (no
//! allocation once the buffer has grown), one 64-bit hash, and a linear scan of at most
//! [`WINDOW`] entries that rejects on the hash before it ever compares a string. The work it
//! replaces is a file write.
//!
//! # Threading
//!
//! hudhook renders on the game's render thread while the client's logic runs on its own worker,
//! so both call this concurrently. State is behind a `Mutex`, taken with `try_lock`: if the lock
//! is unavailable the record is passed through uncollapsed. That is deliberate rather than
//! merely convenient — `shared::handle_panics` logs from inside the panic hook, which runs
//! *before* unwinding releases anything, so a blocking lock here could deadlock the panic path.
//! Failing open costs a duplicate line; failing closed costs the crash report.

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use log::{Level, Log, Metadata, Record};

/// How many distinct message identities may be in flight at once.
///
/// Must be > 1 or interleaved floods do not collapse at all; see the module docs. 16 is enough
/// room for a per-frame flood to survive the client's own periodic chatter without being evicted.
pub const WINDOW: usize = 16;

/// `WINDOW == 1` is the naive "is this the same as the previous record?" collapser, and against
/// the flood this module exists for it collapses *nothing* -- 200,000 records out of 200,000 in,
/// because the two messages alternate and no two adjacent records are ever equal. The tests catch
/// that, but a constant this easy to "simplify" deserves to fail at compile time too.
const _: () = assert!(
    WINDOW > 1,
    "WINDOW must exceed 1 or interleaved floods never collapse"
);

/// Report a running count at least this often, measured in occurrences.
pub const REPEAT_EVERY: u64 = 1000;

/// Report a running count at least this often, measured in wall time.
///
/// This is the bound that matters for a *slow* repeat — one line every few seconds still floods a
/// log over an evening, but would take hours to reach [`REPEAT_EVERY`].
pub const REPORT_INTERVAL: Duration = Duration::from_secs(60);

/// How long an identity may go unseen before its run is considered over.
///
/// A message that comes back after a long silence is news again, and is emitted in full.
pub const RUN_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// A message identity that is currently in flight.
struct Run {
    /// Fast-reject fingerprint of `(level, target, message)`.
    hash: u64,
    level: Level,
    target: String,
    message: String,
    /// Occurrences since this run started, including the one that was emitted in full.
    total: u64,
    /// Occurrences counted but not yet reported.
    pending: u64,
    last_seen: Instant,
    last_report: Instant,
}

impl Run {
    /// Reports and clears the pending count, if there is one.
    fn report(&mut self, now: Instant) -> Option<Emission> {
        if self.pending == 0 {
            return None;
        }
        let text = format!(
            "... repeated {} times ({} total): {}",
            self.pending, self.total, self.message
        );
        self.pending = 0;
        self.last_report = now;
        Some(Emission {
            level: self.level,
            target: self.target.clone(),
            text,
        })
    }
}

/// A synthesized record to hand to the inner logger once the state lock is released.
struct Emission {
    level: Level,
    target: String,
    text: String,
}

#[derive(Default)]
struct State {
    runs: Vec<Run>,
    /// Reused formatting buffer, so the hot path does not allocate.
    scratch: String,
}

impl State {
    /// Folds `record` into the in-flight set.
    ///
    /// Returns the summary lines this record caused and whether the record itself should be
    /// passed through.
    fn observe(&mut self, record: &Record<'_>, now: Instant) -> (Vec<Emission>, bool) {
        let mut emissions = Vec::new();

        self.scratch.clear();
        // Writing to a String is infallible; the Result is only there for the generic signature.
        let _ = write!(self.scratch, "{}", record.args());

        let State { runs, scratch } = self;
        let level = record.level();
        let target = record.target();
        let hash = fingerprint(level, target, scratch);

        // Retire runs that have gone quiet. Done before the lookup, so a message returning after
        // a silence is treated as new rather than as a continuation.
        let mut i = 0;
        while i < runs.len() {
            if now.saturating_duration_since(runs[i].last_seen) >= RUN_IDLE_TIMEOUT {
                let mut run = runs.remove(i);
                emissions.extend(run.report(now));
            } else {
                i += 1;
            }
        }

        let existing = runs.iter().position(|run| {
            run.hash == hash
                && run.level == level
                && run.target == target
                && run.message == *scratch
        });

        if let Some(index) = existing {
            let run = &mut runs[index];
            run.total += 1;
            run.pending += 1;
            run.last_seen = now;
            if run.pending >= REPEAT_EVERY
                || now.saturating_duration_since(run.last_report) >= REPORT_INTERVAL
            {
                emissions.extend(run.report(now));
            }
            return (emissions, false);
        }

        // Novel identity. Make room if we have to, remember it, then let it through untouched.
        if runs.len() >= WINDOW
            && let Some(index) = runs
                .iter()
                .enumerate()
                .min_by_key(|(_, run)| run.last_seen)
                .map(|(index, _)| index)
        {
            let mut evicted = runs.remove(index);
            emissions.extend(evicted.report(now));
        }
        runs.push(Run {
            hash,
            level,
            target: target.to_owned(),
            message: scratch.clone(),
            total: 1,
            pending: 0,
            last_seen: now,
            last_report: now,
        });

        (emissions, true)
    }

    /// Reports every outstanding count without ending the runs.
    fn drain(&mut self, now: Instant) -> Vec<Emission> {
        self.runs
            .iter_mut()
            .filter_map(|run| run.report(now))
            .collect()
    }
}

fn fingerprint(level: Level, target: &str, message: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    (level as usize).hash(&mut hasher);
    target.hash(&mut hasher);
    message.hash(&mut hasher);
    hasher.finish()
}

/// Wraps a logger so that repeated records are counted instead of re-emitted.
///
/// See the module docs for the semantics and for why this is keyed on the record rather than on
/// the message that prompted it.
pub struct CollapseDuplicates {
    inner: Box<dyn Log>,
    state: Mutex<State>,
    clock: Box<dyn Fn() -> Instant + Send + Sync>,
}

impl CollapseDuplicates {
    /// Wraps `inner`.
    pub fn new(inner: Box<dyn Log>) -> Self {
        Self::with_clock(inner, Box::new(Instant::now))
    }

    /// Wraps `inner`, reading the current time from `clock`.
    ///
    /// Only the time-based rules ([`REPORT_INTERVAL`], [`RUN_IDLE_TIMEOUT`]) consult it; this
    /// exists so tests can exercise them without sleeping.
    fn with_clock(inner: Box<dyn Log>, clock: Box<dyn Fn() -> Instant + Send + Sync>) -> Self {
        Self {
            inner,
            state: Mutex::new(State::default()),
            clock,
        }
    }

    fn emit(&self, emission: &Emission) {
        self.inner.log(
            &Record::builder()
                .level(emission.level)
                .target(&emission.target)
                .args(format_args!("{}", emission.text))
                .build(),
        );
    }
}

impl Log for CollapseDuplicates {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &Record<'_>) {
        if !self.inner.enabled(record.metadata()) {
            return;
        }

        // See the threading note in the module docs: failing open is the safe direction.
        let Ok(mut state) = self.state.try_lock() else {
            self.inner.log(record);
            return;
        };
        let now = (self.clock)();
        let (emissions, pass_through) = state.observe(record, now);
        drop(state);

        for emission in &emissions {
            self.emit(emission);
        }
        if pass_through {
            self.inner.log(record);
        }
    }

    fn flush(&self) {
        if let Ok(mut state) = self.state.try_lock() {
            let now = (self.clock)();
            let emissions = state.drain(now);
            drop(state);
            for emission in &emissions {
                self.emit(emission);
            }
        }
        self.inner.flush();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    use super::*;

    /// The exact pair from the 2026-08-02 log bundle, with the targets `log` derives from
    /// hudhook's module paths.
    const DISPLAY_SIZE: (&str, &str) = (
        "hudhook::renderer::pipeline",
        "Insufficient display size: 0x0",
    );
    const RENDER_ERROR: (&str, &str) = (
        "hudhook::hooks::dx12",
        r#"Render error: Error { code: HRESULT(0xFFFFFFFF), message: "" }"#,
    );

    /// A shared handle to a sink that records what actually reached the log file.
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<String>>>);

    impl Sink {
        fn lines(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
    }

    impl Log for Sink {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }

        fn log(&self, record: &Record<'_>) {
            self.0
                .lock()
                .unwrap()
                .push(format!("[{}] {}", record.level(), record.args()));
        }

        fn flush(&self) {}
    }

    /// A clock the test advances by hand.
    #[derive(Clone, Default)]
    struct FakeClock(Arc<AtomicU64>);

    impl FakeClock {
        fn advance(&self, by: Duration) {
            self.0.fetch_add(by.as_millis() as u64, Ordering::SeqCst);
        }

        fn source(&self) -> Box<dyn Fn() -> Instant + Send + Sync> {
            let base = Instant::now();
            let millis = self.0.clone();
            Box::new(move || base + Duration::from_millis(millis.load(Ordering::SeqCst)))
        }
    }

    fn harness() -> (Sink, FakeClock, CollapseDuplicates) {
        let sink = Sink::default();
        let clock = FakeClock::default();
        let logger = CollapseDuplicates::with_clock(Box::new(sink.clone()), clock.source());
        (sink, clock, logger)
    }

    fn say(logger: &dyn Log, level: Level, (target, message): (&str, &str)) {
        logger.log(
            &Record::builder()
                .level(level)
                .target(target)
                .args(format_args!("{message}"))
                .build(),
        );
    }

    /// Sums the `N` out of every `... repeated N times` line.
    fn reported(lines: &[String]) -> u64 {
        lines
            .iter()
            .filter_map(|line| {
                let rest = line.split_once("... repeated ")?.1;
                rest.split_once(" times")?.0.parse::<u64>().ok()
            })
            .sum()
    }

    /// The motivating case, at the scale it was actually reported (CONTRIBUTING rule 11).
    ///
    /// 100,000 frames of the alternating pair — 200,000 records, the shape of the 612,842 lines
    /// in the uploaded bundle. Every one of those records must be accounted for, and the log must
    /// not grow with the length of the flood.
    #[test]
    fn the_real_alternating_pair_at_frame_rate_collapses() {
        let (sink, _clock, logger) = harness();

        const FRAMES: u64 = 100_000;
        for _ in 0..FRAMES {
            say(&logger, Level::Error, DISPLAY_SIZE);
            say(&logger, Level::Error, RENDER_ERROR);
        }
        logger.flush();

        let lines = sink.lines();

        // Bounded, and bounded FAR below the input. Without the collapser this is 200,000, which
        // is also why the failure message shows a head rather than the whole sink.
        assert!(
            lines.len() <= 256,
            "{} records collapsed to {} lines, expected a small bounded number. First few:\n{}",
            2 * FRAMES,
            lines.len(),
            lines[..8.min(lines.len())].join("\n"),
        );

        // Nothing is lost: two records emitted in full, the rest counted.
        let first_occurrences = lines
            .iter()
            .filter(|line| !line.contains("... repeated "))
            .count() as u64;
        assert_eq!(first_occurrences, 2, "lines emitted in full: {lines:#?}");
        assert_eq!(
            first_occurrences + reported(&lines),
            2 * FRAMES,
            "counts must add up to every record that came in"
        );

        // Both members of the pair survive as greppable text at their original level.
        assert!(lines.iter().any(|line| line.contains(DISPLAY_SIZE.1)));
        assert!(lines.iter().any(|line| line.contains(RENDER_ERROR.1)));
        assert!(lines.iter().all(|line| line.starts_with("[ERROR]")));
    }

    /// The trap this fix exists to avoid: a "same as the last record?" comparator sees no two
    /// adjacent records that match, and collapses nothing.
    #[test]
    fn a_last_record_only_comparator_would_not_have_helped() {
        let (sink, _clock, logger) = harness();

        for _ in 0..1_000 {
            say(&logger, Level::Error, DISPLAY_SIZE);
            say(&logger, Level::Error, RENDER_ERROR);
        }
        logger.flush();

        // No two consecutive records are equal, yet this still collapses.
        assert!(sink.lines().len() < 10, "{:#?}", sink.lines());
    }

    #[test]
    fn the_first_occurrence_is_never_suppressed() {
        let (sink, _clock, logger) = harness();

        say(&logger, Level::Error, DISPLAY_SIZE);

        assert_eq!(sink.lines(), vec!["[ERROR] Insufficient display size: 0x0"]);
    }

    /// A novel message dropped into the middle of a long flood must appear at once, and must not
    /// be counted against, or delayed by, the flood.
    #[test]
    fn a_novel_message_inside_a_long_run_is_emitted_immediately() {
        let (sink, _clock, logger) = harness();

        for _ in 0..5_000 {
            say(&logger, Level::Error, DISPLAY_SIZE);
            say(&logger, Level::Error, RENDER_ERROR);
        }
        let before = sink.lines().len();

        say(
            &logger,
            Level::Warn,
            (
                "eldenring_archipelago::core",
                "Refused: wrong save for this room",
            ),
        );

        let lines = sink.lines();
        assert_eq!(
            lines.len(),
            before + 1,
            "the novel record should be the very next line out"
        );
        assert_eq!(
            lines.last().unwrap(),
            "[WARN] Refused: wrong save for this room"
        );

        // And the flood keeps collapsing around it rather than restarting.
        for _ in 0..5_000 {
            say(&logger, Level::Error, DISPLAY_SIZE);
            say(&logger, Level::Error, RENDER_ERROR);
        }
        assert!(sink.lines().len() < 32, "{:#?}", sink.lines());
    }

    /// Distinct messages are never collapsed into each other, however fast they arrive.
    #[test]
    fn distinct_messages_all_get_through() {
        let (sink, _clock, logger) = harness();

        for i in 0..500 {
            let message = format!("Received item {i}");
            say(
                &logger,
                Level::Info,
                ("eldenring_archipelago::core", &message),
            );
        }

        assert_eq!(sink.lines().len(), 500);
    }

    /// A run that persists must not go dark: even with too few occurrences to hit REPEAT_EVERY,
    /// wall time forces periodic evidence with a rising total.
    #[test]
    fn a_slow_repeat_still_reports_on_a_timer() {
        let (sink, clock, logger) = harness();

        // One occurrence every 10s for an hour: 360 records, never reaching REPEAT_EVERY.
        for _ in 0..360 {
            say(&logger, Level::Error, RENDER_ERROR);
            clock.advance(Duration::from_secs(10));
        }
        logger.flush();

        let lines = sink.lines();
        assert!(
            lines.iter().filter(|l| l.contains("... repeated ")).count() >= 5,
            "an hour-long repeat left no periodic evidence: {lines:#?}"
        );
        // Totals rise, so a reader can see the condition is ongoing rather than historical.
        assert!(
            lines.iter().any(|line| line.contains("(360 total)")),
            "{lines:#?}"
        );
    }

    /// After a silence, the same message is news again and is emitted in full.
    #[test]
    fn a_message_returning_after_a_silence_is_emitted_in_full() {
        let (sink, clock, logger) = harness();

        say(&logger, Level::Error, RENDER_ERROR);
        say(&logger, Level::Error, RENDER_ERROR);
        clock.advance(RUN_IDLE_TIMEOUT + Duration::from_secs(1));
        say(&logger, Level::Error, RENDER_ERROR);

        let lines = sink.lines();
        assert_eq!(lines.len(), 3, "{lines:#?}");
        assert!(lines[1].contains("... repeated 1 times"), "{lines:#?}");
        assert_eq!(lines[2], format!("[ERROR] {}", RENDER_ERROR.1));
    }

    /// Level and target are part of the identity: the same words at a different level are a
    /// different record and must not be folded together.
    #[test]
    fn level_and_target_are_part_of_the_identity() {
        let (sink, _clock, logger) = harness();

        say(&logger, Level::Error, RENDER_ERROR);
        say(&logger, Level::Warn, RENDER_ERROR);
        say(&logger, Level::Error, ("somewhere::else", RENDER_ERROR.1));

        assert_eq!(sink.lines().len(), 3, "{:#?}", sink.lines());
    }

    /// The window is finite, so more than WINDOW live identities push the oldest out — and the
    /// evicted run reports its tail rather than losing it.
    #[test]
    fn an_evicted_run_reports_its_tail() {
        let (sink, _clock, logger) = harness();

        say(&logger, Level::Error, RENDER_ERROR);
        say(&logger, Level::Error, RENDER_ERROR);
        for i in 0..WINDOW {
            let message = format!("filler {i}");
            say(&logger, Level::Info, ("filler", &message));
        }

        let lines = sink.lines();
        assert!(
            lines
                .iter()
                .any(|line| line.contains("... repeated 1 times") && line.contains(RENDER_ERROR.1)),
            "{lines:#?}"
        );
    }

    /// Levels the inner logger rejects cost nothing and are not counted.
    #[test]
    fn a_disabled_record_is_dropped_without_being_counted() {
        struct ErrorsOnly(Sink);
        impl Log for ErrorsOnly {
            fn enabled(&self, metadata: &Metadata<'_>) -> bool {
                metadata.level() <= Level::Error
            }
            fn log(&self, record: &Record<'_>) {
                if self.enabled(record.metadata()) {
                    self.0.log(record);
                }
            }
            fn flush(&self) {}
        }

        let sink = Sink::default();
        let logger = CollapseDuplicates::new(Box::new(ErrorsOnly(sink.clone())) as Box<dyn Log>);

        say(&logger, Level::Debug, ("noise", "chatty"));
        say(&logger, Level::Error, RENDER_ERROR);

        assert_eq!(sink.lines().len(), 1);
    }
}
