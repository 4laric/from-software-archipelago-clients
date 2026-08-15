//! `pot_cap_tally` — count what the pot cap eats, per row, flushed at the world edge (world#692).
//!
//! # The line this replaces
//!
//! From bobler's 2026-08-15 log, 66 seconds after the first world edge:
//!
//! ```text
//! 09:30:21 [WARN] pot-cap: goods 0x40002526 grant of 1 CAPPED to 0 (held 9, cap 9) -- the
//!   remainder is reported delivered but never enters the inventory. Further caps on this row
//!   are silent.
//! ```
//!
//! The sentence is accurate and that is the problem: it announces its own blindness. One
//! `AtomicU32` announce-bit per capped row means the FIRST cap on a row is logged and every one
//! after it is dropped, so **the log's one line is a floor, not a count** — in that session and in
//! every session this project has recorded.
//!
//! ⭐ THAT IS WHY THIS LANDS BEFORE THE POLICY QUESTION, not after. #692 asks whether a capped
//! grant should be retried at the next world edge or dropped without an ack; both answers are
//! arguments about a rate nobody has ever measured. A one-off at the start loadout and a steady
//! drip across a 2h12m session want different fixes, and the current instrument cannot tell them
//! apart. This module makes the next log answer that.
//!
//! # Why the world edge is the flush point
//!
//! The cap frees up as the player consumes pots, so the world edge is where the number is both
//! stable and meaningful: it is the same boundary `detour::on_world_edge` already retires the
//! inventory pointer on, and the boundary any future retry would run at. A per-grant line would be
//! the noise the announce-bit was added to suppress; a per-session total would arrive only at
//! shutdown, which is exactly when a crash loses it.
//!
//! # 🛑 What this deliberately does NOT change
//!
//! The ack. `grant_full_id_outcome` returns `Capped`, `grant_full_id` maps that to `true`, and the
//! watermark advances. That conflation is documented in `detour.rs` as CORRECT for the ledger --
//! "a capped pot is as delivered as it will ever be, and the watermark must advance past it or the
//! stream stalls forever" -- and #692 proposes reversing it. Reversing a documented decision on an
//! unmeasured rate is how the next postmortem gets written. Measure first.

/// Rows tracked at once. `POT_DELIVERY_CAPS` carries four; the slack is so a new cap row cannot
/// silently fall off the end of the tally the way the announce-bit's 32 bits never could.
pub const MAX_TRACKED_ROWS: usize = 8;

/// One row's running loss since the last flush.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RowTally {
    /// The capped goods FullID, e.g. `0x4000_2526`.
    pub full_id: i32,
    /// How many grants were capped at all (the number the old announce-bit could only ever say 1).
    pub events: u32,
    /// How many individual items never entered the bag. `i64` because it is a running sum of
    /// `requested - allowed` and a seed handing out a large stack must not be able to wrap it.
    pub lost: i64,
}

/// Per-row capped-grant counters, drained at the world edge.
///
/// Fixed-size and `const`-constructible on purpose: it lives in a `static Mutex` beside the caps
/// table it shadows, and an allocation on the grant path is not something this crate does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PotCapTally {
    rows: [RowTally; MAX_TRACKED_ROWS],
    used: usize,
    /// Rows that arrived after the table was full. Stated rather than silently dropped -- an
    /// overflow that hides a loss is the same defect as the announce-bit.
    overflowed: u32,
}

impl Default for PotCapTally {
    fn default() -> Self {
        Self::new()
    }
}

impl PotCapTally {
    pub const fn new() -> Self {
        Self {
            rows: [RowTally {
                full_id: 0,
                events: 0,
                lost: 0,
            }; MAX_TRACKED_ROWS],
            used: 0,
            overflowed: 0,
        }
    }

    /// Record one capped grant. `allowed < requested` is the caller's precondition; anything else
    /// is not a cap and is ignored, so a caller that records unconditionally cannot inflate the
    /// count.
    pub fn record(&mut self, full_id: i32, requested: i32, allowed: i32) {
        if allowed >= requested {
            return;
        }
        let lost = (requested as i64) - (allowed.max(0) as i64);
        for row in self.rows[..self.used].iter_mut() {
            if row.full_id == full_id {
                row.events = row.events.saturating_add(1);
                row.lost = row.lost.saturating_add(lost);
                return;
            }
        }
        if self.used == MAX_TRACKED_ROWS {
            self.overflowed = self.overflowed.saturating_add(1);
            return;
        }
        self.rows[self.used] = RowTally {
            full_id,
            events: 1,
            lost,
        };
        self.used += 1;
    }

    pub fn is_empty(&self) -> bool {
        self.used == 0 && self.overflowed == 0
    }

    /// Rows recorded since the last flush, in first-capped order.
    pub fn rows(&self) -> &[RowTally] {
        &self.rows[..self.used]
    }

    /// The line to log, and reset. `None` when nothing was capped -- a quiet world edge stays
    /// quiet, which is what makes a line that DOES appear worth reading.
    pub fn flush(&mut self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let line = self.report();
        *self = Self::new();
        Some(line)
    }

    /// The line [`Self::flush`] would emit, without resetting. Split out so the formatting is
    /// testable without the side effect.
    pub fn report(&self) -> String {
        let total: i64 = self.rows().iter().map(|r| r.lost).sum();
        let mut per_row = String::new();
        for (i, r) in self.rows().iter().enumerate() {
            if i > 0 {
                per_row.push_str(", ");
            }
            // ASCII only, and the id in the same `{:#x}` shape the cap WARN prints, so the two
            // lines join on a grep.
            per_row.push_str(&format!(
                "goods {:#x} x{} ({} item(s))",
                r.full_id, r.events, r.lost
            ));
        }
        let tail = if self.overflowed > 0 {
            format!(
                " -- plus {} cap(s) on rows beyond the {} this tally holds, NOT counted above",
                self.overflowed, MAX_TRACKED_ROWS
            )
        } else {
            String::new()
        };
        format!(
            "pot-cap tally (world edge): {total} item(s) reported delivered and never placed, \
             across {} row(s): {per_row}{tail}. These are AP deliveries the watermark has already \
             advanced past (world#692)",
            self.rows().len()
        )
    }
}

#[cfg(test)]
mod replay {
    use super::*;

    /// bobler's row, from the 2026-08-15 log.
    const CRACKED_POT: i32 = 0x4000_2526;
    const RITUAL_POT: i32 = 0x4000_251D;

    /// ⭐ THE RED-FIRST ASSERTION. The defect is not that the first cap goes unreported -- it is
    /// reported. It is that the second through Nth are dropped, so the log floors at 1 and the
    /// rate is unmeasurable. Driven against the announce-bit's behaviour this fails at `1 != 5`.
    #[test]
    fn every_cap_on_a_row_is_counted_not_just_the_first() {
        let mut t = PotCapTally::new();
        for _ in 0..5 {
            t.record(CRACKED_POT, 1, 0);
        }
        assert_eq!(
            t.rows().len(),
            1,
            "five caps on one row is one row, not five"
        );
        assert_eq!(
            t.rows()[0].events,
            5,
            "the announce-bit could only ever say 1; that is the whole bug"
        );
        assert_eq!(t.rows()[0].lost, 5, "five items never entered the bag");
    }

    /// A partial cap loses the remainder, not the whole grant. `grant of 3 CAPPED to 1` is two
    /// items lost -- getting this wrong would overstate the loss and argue for the wrong fix.
    #[test]
    fn a_partial_cap_loses_only_the_remainder() {
        let mut t = PotCapTally::new();
        t.record(CRACKED_POT, 3, 1);
        assert_eq!(t.rows()[0].lost, 2);
        assert_eq!(t.rows()[0].events, 1);
    }

    /// 🛑 A GRANT THAT WAS NOT CAPPED IS NOT A LOSS. The client calls `record` from inside the
    /// cap branch, but a future caller that records unconditionally must not inflate the number
    /// the policy decision will be made on.
    #[test]
    fn an_uncapped_grant_records_nothing() {
        let mut t = PotCapTally::new();
        t.record(CRACKED_POT, 1, 1);
        t.record(CRACKED_POT, 5, 9);
        assert!(t.is_empty(), "no cap, no tally");
        assert_eq!(t.flush(), None);
    }

    /// Rows are kept apart, and reported in the order they first capped.
    #[test]
    fn rows_are_counted_separately() {
        let mut t = PotCapTally::new();
        t.record(RITUAL_POT, 1, 0);
        t.record(CRACKED_POT, 2, 0);
        t.record(RITUAL_POT, 1, 0);
        assert_eq!(t.rows().len(), 2);
        assert_eq!(t.rows()[0].full_id, RITUAL_POT);
        assert_eq!(t.rows()[0].events, 2);
        assert_eq!(t.rows()[1].full_id, CRACKED_POT);
        assert_eq!(t.rows()[1].lost, 2);
    }

    /// The flush is a drain: the next world edge starts from zero, so two edges never
    /// double-count the same loss.
    #[test]
    fn flush_drains_and_a_quiet_edge_says_nothing() {
        let mut t = PotCapTally::new();
        t.record(CRACKED_POT, 1, 0);
        let first = t.flush().expect("a capped edge reports");
        assert!(first.contains("1 item(s)"), "{first}");
        assert!(t.is_empty(), "flush drains");
        assert_eq!(t.flush(), None, "a quiet world edge stays quiet");
    }

    /// The report names the total, the row count, and each row in the same `{:#x}` shape the cap
    /// WARN prints -- so the two lines join on a grep.
    #[test]
    fn the_report_joins_the_cap_warn_on_a_grep() {
        let mut t = PotCapTally::new();
        t.record(CRACKED_POT, 1, 0);
        t.record(CRACKED_POT, 2, 0);
        t.record(RITUAL_POT, 1, 0);
        let line = t.report();
        assert!(line.contains("0x40002526 x2"), "{line}");
        assert!(line.contains("0x4000251d x1"), "{line}");
        assert!(line.contains("4 item(s) reported delivered"), "{line}");
        assert!(line.contains("across 2 row(s)"), "{line}");
        assert!(line.contains("world#692"), "{line}");
    }

    /// 🛑 AN OVERFLOW IS STATED, NEVER SWALLOWED. A tally that silently stopped counting past its
    /// capacity would be the announce-bit defect with a bigger number in front of it.
    #[test]
    fn overflow_is_reported_rather_than_dropped() {
        let mut t = PotCapTally::new();
        for i in 0..(MAX_TRACKED_ROWS as i32 + 3) {
            t.record(0x4000_0000 | i, 1, 0);
        }
        assert_eq!(t.rows().len(), MAX_TRACKED_ROWS);
        let line = t.report();
        assert!(line.contains("3 cap(s) on rows beyond"), "{line}");
        assert!(line.contains("NOT counted above"), "{line}");
    }

    /// In-game and log strings stay ASCII (repo rule 10).
    #[test]
    fn the_report_is_ascii() {
        let mut t = PotCapTally::new();
        t.record(CRACKED_POT, 3, 1);
        assert!(t.report().is_ascii());
        for i in 0..(MAX_TRACKED_ROWS as i32 + 1) {
            t.record(0x4000_0000 | i, 1, 0);
        }
        assert!(t.report().is_ascii());
    }
}
