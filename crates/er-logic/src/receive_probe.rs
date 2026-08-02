//! `receive_probe` -- the one throttled line that tells "sends work, receives dead" apart.
//!
//! ## Why this exists (rule 11: the motivating case IS the acceptance test)
//!
//! Two live investigations were blocked on the same missing line.
//!
//! **Issue #293 (Hazel).** A player's receive cursor sat at 172 for three days across seven
//! sessions while checks kept sending. The client has exactly THREE ways to produce that, and on
//! the build she was running **all three print an identical log**, so the bundle could not
//! discriminate them:
//!
//!  - **F1** -- `can_grant` was false all session. `can_grant = has_inventory() && in_world() &&
//!    !is_refused()`, but the flag poll that drives SENDS needs only `locations_loaded &&
//!    in_world()`. That asymmetry is the "sends work, receives dead" generator, and which of the
//!    three inputs is false says which bug it is (no inventory pointer => a foreign
//!    `AddItemFunc` hook; refused => the marker guard).
//!  - **F2** -- the cursor is AHEAD of the stream (`received_through > received_items().len()`).
//!    Every arriving item lands below the cursor and is swallowed as `AlreadyPushed`. Nothing
//!    clamps `received_through` down, so this NEVER self-heals. See [`RecvState::f2_cursor_ahead`].
//!  - **F3** -- the H3 hold: one item that will not place holds the watermark forever (the caller
//!    logs `failed to place -- receive watermark held for retry`, so F3 is the one already
//!    distinguishable from an existing bundle -- by its presence, not by this line).
//!
//! `client.received_items().len()` was logged NOWHERE. With the stream length beside the cursor
//! and the three `can_grant` inputs broken out, one line separates all three on first read:
//! stream climbing + cursor frozen + `can_grant=false` is F1 (and names which input); cursor above
//! the stream is F2; everything true and both numbers frozen with the H3 warn present is F3.
//!
//! **Issue #296 (evergaol).** Delivery reported `converged=true` while owing ~14 items, sat dead
//! for 18 seconds, then applied all 14 the instant the player crossed a world edge. Whether those
//! items were IN THE STREAM during the dead window is the whole question, and nothing timestamped
//! when the stream GREW. The change-triggered emission below does exactly that.
//!
//! ## Instrument only
//!
//! 🛑 Nothing in this module changes delivery behaviour. In particular F2 is REPORTED and never
//! repaired: clamping `received_through` down to the stream length would re-grant every item from
//! the stream length onward (~172 items in #293), which is a product decision, not a diagnostic
//! one. That call is Alaric's.

/// The receive tick's whole observable state, in one comparable value.
///
/// `PartialEq` is the throttle: a tick whose state equals the last emitted one is silent (bar the
/// heartbeat). That is why every field is here and why they are all cheap scalars -- the caller
/// builds one of these on the hot path, every frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecvState {
    /// `client.received_items().len()` -- how many items the ROOM has sent us this connection.
    pub stream_len: usize,
    /// `Core::received_through` -- how far the client believes it has delivered.
    pub cursor: usize,
    /// `detour::has_inventory()`
    pub has_inventory: bool,
    /// `flags::in_world()`
    pub in_world: bool,
    /// `reconcile_io::is_refused()`
    pub refused: bool,
}

impl RecvState {
    /// The exact predicate `core.rs` gates the receive step on. Kept here so the log line cannot
    /// drift from the thing it claims to be reporting.
    pub fn can_grant(&self) -> bool {
        self.has_inventory && self.in_world && !self.refused
    }

    /// **F2**: the cursor is ahead of the stream.
    ///
    /// Strictly greater. `cursor == stream_len` is the HEALTHY steady state (everything delivered),
    /// and `cursor < stream_len` is an ordinary backlog the next tick works through.
    pub fn f2_cursor_ahead(&self) -> bool {
        self.cursor > self.stream_len
    }

    /// How many future arrivals F2 will swallow: indices `stream_len .. cursor` are already below
    /// the cursor, so each one is skipped as `AlreadyPushed` the moment it lands.
    pub fn f2_stranded(&self) -> usize {
        self.cursor.saturating_sub(self.stream_len)
    }

    /// The state line. Shape is load-bearing -- it is what a log bundle gets grepped for.
    pub fn line(&self) -> String {
        format!(
            "recv: stream={} cursor={} can_grant={} (has_inv={} in_world={} refused={})",
            self.stream_len,
            self.cursor,
            self.can_grant(),
            self.has_inventory,
            self.in_world,
            self.refused
        )
    }
}

/// How long a state may go unchanged before the probe says so anyway.
///
/// ⭐ The heartbeat is the point of the whole instrument. A FROZEN cursor is precisely the case
/// where nothing changes, so a change-only throttle would go silent exactly when the bug is
/// happening and the bundle would again say nothing. 30s is ~1800 frames at 60fps -- unmeasurable
/// on the hot path, and dense enough that a three-day freeze leaves thousands of dated lines.
pub const DEFAULT_HEARTBEAT_MS: u64 = 30_000;

/// How long F2 must HOLD before it is believed.
///
/// 🛑 Not optional. On every single connect the client loads `received_through` from the save
/// (172, say) while `received_items()` is still empty, and AP then replays the stream. So
/// `cursor > stream_len` is the NORMAL state for the first seconds of every session, and an
/// ungated F2 warning would fire on every launch and be trained away inside a week -- the exact
/// fate of a gate that reports nothing. The dwell restarts whenever `stream_len` moves, so a
/// replay in progress keeps resetting it and only a stream that has STOPPED short trips the alarm.
pub const DEFAULT_F2_DWELL_MS: u64 = 30_000;

/// What the caller should do with this tick's observation. Every field is `None` on a quiet tick.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The throttled state line -> `log::info!`.
    pub line: Option<String>,
    /// The F2 alarm -> `log::warn!`. Emitted once per distinct (cursor, stream_len) pair.
    pub warning: Option<String>,
    /// The F2 on-screen notice -> `Deck::push`. Emitted EVERY tick while F2 holds; the deck
    /// refreshes identical text instead of stacking, so re-pushing is free (the same contract the
    /// refusal toast relies on).
    pub toast: Option<String>,
}

/// Change-plus-heartbeat throttle over [`RecvState`], and the F2 alarm.
#[derive(Debug)]
pub struct Probe {
    heartbeat_ms: u64,
    f2_dwell_ms: u64,
    last: Option<RecvState>,
    /// When [`Probe::observe`] last returned a line.
    last_emit_ms: u64,
    /// When the state last actually CHANGED -- reported on heartbeat lines as `held=`, which is
    /// both the freeze duration (#293) and the "nothing arrived in this window" measure (#296).
    last_change_ms: u64,
    /// Start of the current uninterrupted F2 condition, and the `stream_len` it started at. The
    /// pair is dropped whenever `stream_len` moves (a growing stream is a replay, not a fault).
    f2_since: Option<(u64, usize)>,
    /// The (cursor, stream_len) pair already warned about, so the warn is once per STATE.
    f2_warned: Option<(usize, usize)>,
}

impl Default for Probe {
    fn default() -> Self {
        Self::new(DEFAULT_HEARTBEAT_MS, DEFAULT_F2_DWELL_MS)
    }
}

impl Probe {
    pub fn new(heartbeat_ms: u64, f2_dwell_ms: u64) -> Self {
        Probe {
            heartbeat_ms,
            f2_dwell_ms,
            last: None,
            last_emit_ms: 0,
            last_change_ms: 0,
            f2_since: None,
            f2_warned: None,
        }
    }

    /// Feed one tick's state. Call ONLY while connected -- with no client there is no stream length
    /// to compare against, and an invented 0 would look exactly like F2.
    pub fn observe(&mut self, state: RecvState, now_ms: u64) -> Report {
        let mut report = Report::default();

        // ---- the throttle -------------------------------------------------------------------
        let changed = self.last != Some(state);
        if changed {
            self.last_change_ms = now_ms;
        }
        let first = self.last.is_none();
        let due = now_ms.saturating_sub(self.last_emit_ms) >= self.heartbeat_ms;
        if changed || first || due {
            let mut line = state.line();
            if !changed && !first {
                // Heartbeat: say HOW LONG it has been like this. Two reasons this suffix earns its
                // bytes -- the held time IS the #293 evidence, and it makes each heartbeat line
                // textually distinct, so a downstream duplicate-record collapser cannot fold the
                // proof-of-freeze into one line. (This module does not depend on such a collapser
                // existing either way.)
                let held = now_ms.saturating_sub(self.last_change_ms);
                line.push_str(&format!(" held={held}ms"));
            }
            report.line = Some(line);
            self.last_emit_ms = now_ms;
        }
        self.last = Some(state);

        // ---- F2 -----------------------------------------------------------------------------
        if !state.f2_cursor_ahead() {
            self.f2_since = None;
            self.f2_warned = None;
            return report;
        }
        let since = match self.f2_since {
            // A stream that MOVED means the connect replay is still running: restart the dwell
            // rather than accusing a healthy session.
            Some((_, len)) if len != state.stream_len => {
                self.f2_warned = None;
                now_ms
            }
            Some((since, _)) => since,
            None => now_ms,
        };
        self.f2_since = Some((since, state.stream_len));
        if now_ms.saturating_sub(since) < self.f2_dwell_ms {
            return report;
        }
        let key = (state.cursor, state.stream_len);
        if self.f2_warned != Some(key) {
            self.f2_warned = Some(key);
            report.warning = Some(f2_warning(&state));
        }
        report.toast = Some(f2_toast(&state));
        report
    }
}

/// The F2 log alarm. Names BOTH numbers, what it means, and that we deliberately did not fix it.
pub fn f2_warning(state: &RecvState) -> String {
    format!(
        "recv: CURSOR AHEAD OF STREAM -- received_through={} > received_items().len()={}. \
         This is failure mode F2: the next {} item(s) the room sends land BELOW the cursor and are \
         swallowed as AlreadyPushed, so receives are stranded permanently while sends keep working, \
         and it never self-heals. NOT clamped here on purpose -- lowering the cursor to {} would \
         re-grant every item from there on. (issue #293)",
        state.cursor,
        state.stream_len,
        state.f2_stranded(),
        state.stream_len
    )
}

/// The F2 on-screen notice.
///
/// 🛑 ASCII ONLY -- this is drawn by the game's font, which has no glyph for an em-dash and draws a
/// literal `?` (v0.2.18 shipped exactly that). Pinned by `f2_toast_is_ascii`.
///
/// It names the ACTION, per the rule the REFUSED guard was fixed under: a player cannot act on
/// "cursor ahead of stream", and a log line is invisible from the player's chair -- which is how a
/// refused session went unnoticed for ~55 minutes of boblerrr's playtime. The advice is
/// deliberately conservative: reconnecting to the room the save was started in is the fix for the
/// cause we know (a re-hosted room / changed port), and the save-file surgery that would "fix" a
/// room that is truly gone re-grants the whole run, so it is not advice to put on screen.
pub fn f2_toast(state: &RecvState) -> String {
    format!(
        "AP: items cannot arrive. This save has received {} items but the room has only sent {}. \
         Reconnect to the room this save was started in, or report this with your client log.",
        state.cursor, state.stream_len
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> RecvState {
        RecvState {
            stream_len: 172,
            cursor: 172,
            has_inventory: true,
            in_world: true,
            refused: false,
        }
    }

    // ---------------------------------------------------------------------------------------
    // F2 predicate
    // ---------------------------------------------------------------------------------------

    /// The predicate fires EXACTLY when cursor > stream length. The two neighbours matter: equality
    /// is the healthy steady state and `cursor < len` is an ordinary backlog, so an off-by-one in
    /// either direction would make the alarm useless (always on, or on for every normal delivery).
    #[test]
    fn f2_fires_exactly_when_the_cursor_is_above_the_stream() {
        for (stream_len, cursor, want) in [
            (172usize, 172usize, false), // delivered up to date
            (172, 171, false),           // one still owed -- normal
            (172, 173, true),            // one past the end -- F2
            (0, 0, false),               // fresh room, fresh save
            (0, 172, true),              // #293's shape: save resumed against an empty stream
            (40, 172, true),
            (172, 40, false),
        ] {
            let s = RecvState {
                stream_len,
                cursor,
                ..healthy()
            };
            assert_eq!(
                s.f2_cursor_ahead(),
                want,
                "stream={stream_len} cursor={cursor}"
            );
        }
    }

    #[test]
    fn stranded_count_is_the_gap_and_never_underflows() {
        let s = RecvState {
            stream_len: 40,
            cursor: 172,
            ..healthy()
        };
        assert_eq!(s.f2_stranded(), 132);
        let ok = RecvState {
            stream_len: 172,
            cursor: 40,
            ..healthy()
        };
        assert_eq!(ok.f2_stranded(), 0, "no underflow on the healthy side");
    }

    /// `can_grant` in the line must be the AND of the three inputs beside it -- the whole point is
    /// that the reader can see WHICH input is false, so the three must not be able to disagree.
    #[test]
    fn can_grant_is_the_conjunction_the_client_gates_on() {
        for (has_inventory, in_world, refused, want) in [
            (true, true, false, true),
            (false, true, false, false),
            (true, false, false, false),
            (true, true, true, false),
        ] {
            let s = RecvState {
                has_inventory,
                in_world,
                refused,
                ..healthy()
            };
            assert_eq!(s.can_grant(), want);
            let line = s.line();
            assert!(
                line.contains(&format!("can_grant={want}")),
                "line must agree with the predicate: {line}"
            );
        }
    }

    /// The shape is the contract -- log bundles get grepped for it, and #293's whole complaint was
    /// that `received_items().len()` appeared in no line anywhere.
    #[test]
    fn the_line_names_the_stream_the_cursor_and_all_three_can_grant_inputs() {
        let s = RecvState {
            stream_len: 40,
            cursor: 172,
            has_inventory: false,
            in_world: true,
            refused: false,
        };
        assert_eq!(
            s.line(),
            "recv: stream=40 cursor=172 can_grant=false (has_inv=false in_world=true refused=false)"
        );
    }

    // ---------------------------------------------------------------------------------------
    // throttle
    // ---------------------------------------------------------------------------------------

    #[test]
    fn the_first_observation_always_emits() {
        let mut p = Probe::default();
        assert!(p.observe(healthy(), 0).line.is_some());
    }

    #[test]
    fn an_unchanged_state_is_silent_between_heartbeats() {
        let mut p = Probe::default();
        p.observe(healthy(), 0);
        for t in [1, 16, 500, 29_999] {
            assert_eq!(
                p.observe(healthy(), t).line,
                None,
                "no line at {t}ms -- the hot path must stay quiet"
            );
        }
    }

    /// Every field is a throttle key. A cursor that moves is a delivery; a stream that grows is an
    /// arrival (#296 asks for exactly that timestamp); an input that flips is F1 turning on or off.
    #[test]
    fn any_field_changing_emits_immediately() {
        let base = healthy();
        for mutate in [
            (|s: &mut RecvState| s.stream_len += 1) as fn(&mut RecvState),
            |s: &mut RecvState| s.cursor += 1,
            |s: &mut RecvState| s.has_inventory = false,
            |s: &mut RecvState| s.in_world = false,
            |s: &mut RecvState| s.refused = true,
        ] {
            let mut p = Probe::default();
            p.observe(base, 0);
            let mut next = base;
            mutate(&mut next);
            assert!(
                p.observe(next, 1).line.is_some(),
                "a changed field must emit on the very next tick: {next:?}"
            );
        }
    }

    /// ⭐ THE IMPORTANT ONE. A frozen state is the #293 case, and a change-only throttle would go
    /// silent exactly there -- the bundle would once again contain nothing. Assert the heartbeat
    /// keeps producing dated evidence, and that each line carries the growing `held=` so a reader
    /// can measure the freeze without diffing timestamps.
    #[test]
    fn a_frozen_state_still_emits_periodically() {
        let mut p = Probe::default();
        let stuck = RecvState {
            stream_len: 200,
            cursor: 172, // sends fine, receives dead: the stream grew, the cursor did not
            has_inventory: false,
            in_world: true,
            refused: false,
        };
        assert!(p.observe(stuck, 0).line.is_some(), "priming line");

        let mut emitted = Vec::new();
        // Three simulated minutes at 60fps, nothing ever changing.
        for frame in 1..=10_800u64 {
            let now = frame * 1000 / 60;
            if let Some(line) = p.observe(stuck, now).line {
                emitted.push((now, line));
            }
        }
        assert_eq!(
            emitted.len(),
            6,
            "3 minutes frozen must leave 6 heartbeats at 30s, got {emitted:?}"
        );
        for (now, line) in &emitted {
            assert!(
                line.contains("stream=200 cursor=172"),
                "heartbeat must carry the numbers: {line}"
            );
            assert!(
                line.contains("has_inv=false"),
                "and the input that says WHICH failure this is (F1): {line}"
            );
            assert!(
                line.contains("held="),
                "and how long it has been stuck ({now}ms): {line}"
            );
        }
        assert!(
            emitted.last().unwrap().1.contains("held=180000ms"),
            "held must be the freeze duration, not the heartbeat period: {:?}",
            emitted.last()
        );
    }

    /// A heartbeat is not a change: after one fires, the next is a full period away, and the state
    /// line must not start free-running every frame.
    #[test]
    fn heartbeats_do_not_compound() {
        let mut p = Probe::default();
        p.observe(healthy(), 0);
        assert!(p.observe(healthy(), 30_000).line.is_some());
        assert!(p.observe(healthy(), 30_016).line.is_none());
        assert!(p.observe(healthy(), 59_999).line.is_none());
        assert!(p.observe(healthy(), 60_000).line.is_some());
    }

    // ---------------------------------------------------------------------------------------
    // F2 alarm
    // ---------------------------------------------------------------------------------------

    /// The connect replay: cursor 172 loaded from the save, stream filling from 0. This is EVERY
    /// session's first seconds, so it must be silent -- and it must still trip once the stream
    /// stops short.
    #[test]
    fn the_connect_replay_does_not_trip_f2_but_a_stream_that_stops_short_does() {
        let mut p = Probe::default();
        let mut now = 0u64;
        for stream_len in 0..=40usize {
            let s = RecvState {
                stream_len,
                cursor: 172,
                ..healthy()
            };
            let r = p.observe(s, now);
            assert_eq!(r.warning, None, "replay at len={stream_len} must be quiet");
            assert_eq!(r.toast, None);
            now += 5_000; // a slow replay: 5s per item, far past the 30s dwell if it were ungated
        }
        // The room has nothing more to send. The dwell runs from the LAST stream movement.
        let last_growth = now - 5_000;
        let stalled = RecvState {
            stream_len: 40,
            cursor: 172,
            ..healthy()
        };
        assert_eq!(
            p.observe(stalled, last_growth + 29_999).warning,
            None,
            "not yet"
        );
        let r = p.observe(stalled, last_growth + 30_000);
        let w = r.warning.expect("F2 must fire once the stream stops short");
        assert!(w.contains("received_through=172"), "{w}");
        assert!(w.contains("received_items().len()=40"), "{w}");
        assert!(w.contains("132"), "names the stranded count: {w}");
        assert!(r.toast.is_some(), "and the player is told");
    }

    #[test]
    fn f2_never_fires_while_the_cursor_is_at_or_below_the_stream() {
        let mut p = Probe::default();
        for t in 0..200u64 {
            let s = RecvState {
                stream_len: 172,
                cursor: 172 - (t % 2) as usize, // 172 then 171, forever
                ..healthy()
            };
            let r = p.observe(s, t * 1_000);
            assert_eq!(r.warning, None, "healthy at t={t}");
            assert_eq!(r.toast, None, "healthy at t={t}");
        }
    }

    /// The warn is once per STATE (loud, not spammy); the TOAST re-pushes every tick because the
    /// deck refreshes identical text and an expired toast is an unwarned player.
    #[test]
    fn the_warning_is_once_per_state_but_the_toast_persists() {
        let mut p = Probe::new(DEFAULT_HEARTBEAT_MS, 0);
        let s = RecvState {
            stream_len: 40,
            cursor: 172,
            ..healthy()
        };
        assert!(p.observe(s, 0).warning.is_some(), "first is loud");
        let mut warns = 0;
        let mut toasts = 0;
        for t in 1..=600u64 {
            let r = p.observe(s, t * 16);
            warns += usize::from(r.warning.is_some());
            toasts += usize::from(r.toast.is_some());
        }
        assert_eq!(warns, 0, "same state must not re-warn");
        assert_eq!(toasts, 600, "but the notice must stay on screen");

        // A NEW state (the room sent one more, still short) is a new fact: warn again.
        let moved = RecvState {
            stream_len: 41,
            ..s
        };
        assert!(
            p.observe(moved, 10_000).warning.is_some(),
            "a new (cursor, stream) pair is a new fact"
        );
    }

    #[test]
    fn f2_state_clears_when_the_stream_catches_up() {
        let mut p = Probe::new(DEFAULT_HEARTBEAT_MS, 0);
        let bad = RecvState {
            stream_len: 40,
            cursor: 172,
            ..healthy()
        };
        assert!(p.observe(bad, 0).warning.is_some());
        assert_eq!(p.observe(healthy(), 1).warning, None);
        assert_eq!(p.observe(healthy(), 2).toast, None, "notice comes down");
        // ... and a later relapse is a fresh fact, not a suppressed duplicate.
        assert!(p.observe(bad, 3).warning.is_some());
    }

    // ---------------------------------------------------------------------------------------
    // the player-facing string
    // ---------------------------------------------------------------------------------------

    /// 🛑 In-game text is drawn by the game's font: ASCII only. Swept over a range rather than
    /// asserted on one string, because the v0.2.18 em-dash defect lived in the CONSTANT part of a
    /// format and a single-case assertion would have matched the broken text just as happily.
    #[test]
    fn f2_toast_is_ascii() {
        for (stream_len, cursor) in [(0usize, 1usize), (40, 172), (0, 999_999), (1, 2)] {
            let t = f2_toast(&RecvState {
                stream_len,
                cursor,
                ..healthy()
            });
            assert!(t.is_ascii(), "toast must be ASCII (FMG path): {t:?}");
        }
    }

    /// The rule the REFUSED guard was fixed under: say the CONSEQUENCE and name an ACTION. A
    /// player cannot act on "cursor ahead of stream".
    #[test]
    fn the_f2_toast_names_the_consequence_and_an_action() {
        let t = f2_toast(&RecvState {
            stream_len: 40,
            cursor: 172,
            ..healthy()
        });
        assert!(t.starts_with("AP: "), "{t}");
        assert!(t.contains("cannot arrive"), "the consequence: {t}");
        assert!(t.to_lowercase().contains("reconnect"), "an action: {t}");
        assert!(t.contains("172") && t.contains("40"), "the numbers: {t}");
    }
}
