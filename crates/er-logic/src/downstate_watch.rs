//! The apply/restore schedule for the down-palette probe, made symmetric (client#183).
//!
//! # Two defects in one reading
//!
//! bobler's 2026-08-12 log finally answered #110's question -- `maxHpRate` reads on the player, to
//! the integer, `414 * 0.25 = 103.5 -> 103`. But the probe that produced it has two faults, and
//! both are in the schedule rather than the measurement:
//!
//! ```text
//! 17:45:40  applied 20018004 to the PLAYER
//! 17:45:42  WATCH +1s (player): hp=414/414          <- labelled +1s, actually ~2s later
//! 17:45:44  WATCH +2s (player): hp=414/414
//! 17:45:46  WATCH +3s (player): hp=414/414
//! 17:45:47  WATCH +4s (player): hp=103/103          <- the apply took FOUR reads to show up
//! 17:45:47  removed 20018004 from the player
//! 17:45:47  RESTORED (player): hp=103/103           <- same tick. Still 103.
//! ```
//!
//! ## 1. The restore is dumped in the window the probe just proved is too early
//!
//! The apply needed four reads to appear. The restore is read on the **same tick** as the removal,
//! and unsurprisingly still shows the down value -- so **there is no probe evidence that max HP
//! ever came back.** What rescues the reading is a different instrument entirely: `boss-fight
//! START` at 17:52:15 reads `player 414/414`. That is luck, and it only exists because the
//! boss-fight probe happens to be default-on.
//!
//! A probe whose conclusion depends on another probe being enabled has not measured its own claim.
//! [`step`] gives the restore the same read count and interval as the apply, and latches only after
//! it.
//!
//! ## 2. `+4s` is not four seconds
//!
//! The label is `frames / 60`, which assumes the loop runs at 60 fps. The wall clock says those
//! reads are ~2s apart, so anyone reading `+4s` as four seconds of latency is out by ~1.75x --
//! and latency is precisely what this probe exists to measure.
//!
//! 🛑 THE FIX IS TO STOP DERIVING TIME FROM TICKS, not to retune the divisor. A tick count is a
//! tick count; the caller has a clock and passes real elapsed milliseconds for the label. The
//! schedule still runs on ticks (that is what the loop has), but nothing claims those ticks are
//! seconds.

/// Which half of the probe a read belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// After applying the row: watching max HP come DOWN.
    Apply,
    /// After removing it: watching max HP come BACK. This half did not exist.
    Restore,
}

/// What the probe should do this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Not a scheduled read tick.
    Wait,
    /// Emit read `index` (1-based) of this phase.
    Read { index: u32, phase: Phase },
    /// The apply watch is complete: remove the row, then start watching the restore.
    Remove,
    /// The restore watch is complete: latch.
    Latch,
}

/// The schedule, as a pure function of the two tick counters.
///
/// `applied_frames` counts ticks since the row was applied; `removed_frames` is `None` until the
/// row is removed, and counts ticks since then afterwards.
///
/// 🛑 SYMMETRIC BY CONSTRUCTION. Both halves use the same `interval` and `dumps`, because the
/// asymmetry IS the defect -- the apply got four reads and the restore got zero.
pub fn step(applied_frames: u32, removed_frames: Option<u32>, interval: u32, dumps: u32) -> Step {
    if interval == 0 {
        return Step::Wait; // a zero interval would divide by zero and read every tick
    }
    match removed_frames {
        None => {
            if !applied_frames.is_multiple_of(interval) || applied_frames == 0 {
                return Step::Wait;
            }
            let index = applied_frames / interval;
            if index < dumps {
                Step::Read {
                    index,
                    phase: Phase::Apply,
                }
            } else if index == dumps {
                // The last apply read and the removal share a tick, exactly as before -- what
                // changes is that the removal no longer ends the probe.
                Step::Remove
            } else {
                Step::Wait
            }
        }
        Some(since) => {
            if !since.is_multiple_of(interval) || since == 0 {
                return Step::Wait;
            }
            let index = since / interval;
            if index < dumps {
                Step::Read {
                    index,
                    phase: Phase::Restore,
                }
            } else if index == dumps {
                Step::Latch
            } else {
                Step::Wait
            }
        }
    }
}

/// The label for a read. ⚠️ Milliseconds from a real clock, never ticks/60 -- see the module docs
/// for the 1.75x the derived label was out by. ASCII (repo rule 10).
pub fn read_label(index: u32, phase: Phase, elapsed_ms: u64) -> String {
    let which = match phase {
        Phase::Apply => "WATCH",
        Phase::Restore => "RESTORE WATCH",
    };
    format!("{which} read {index} (+{elapsed_ms}ms)")
}

#[cfg(test)]
mod replay {
    use super::*;

    const INTERVAL: u32 = 60;
    const DUMPS: u32 = 4;

    fn drive(total_frames: u32) -> Vec<Step> {
        let mut out = Vec::new();
        let mut removed: Option<u32> = None;
        for f in 1..=total_frames {
            let applied = f;
            let s = step(applied, removed, INTERVAL, DUMPS);
            if let Some(r) = removed.as_mut() {
                *r += 1;
            }
            match s {
                Step::Wait => {}
                Step::Remove => {
                    removed = Some(0);
                    out.push(s);
                }
                other => out.push(other),
            }
            if matches!(s, Step::Latch) {
                break;
            }
        }
        out
    }

    /// ⭐ THE RED-FIRST ASSERTION (client#183). The restore must get the SAME number of reads as
    /// the apply -- today it gets zero, dumped on the removal tick, in the window the apply just
    /// proved is too early.
    #[test]
    fn the_restore_gets_as_many_reads_as_the_apply() {
        let steps = drive(INTERVAL * (DUMPS * 2 + 2));
        let applies = steps
            .iter()
            .filter(|s| {
                matches!(
                    s,
                    Step::Read {
                        phase: Phase::Apply,
                        ..
                    }
                )
            })
            .count();
        let restores = steps
            .iter()
            .filter(|s| {
                matches!(
                    s,
                    Step::Read {
                        phase: Phase::Restore,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            applies, 3,
            "reads 1..3, with read 4's tick being the removal"
        );
        assert_eq!(
            restores, 3,
            "the restore half must be symmetric -- it had ZERO reads before this"
        );
        assert!(steps.contains(&Step::Remove));
        assert!(steps.contains(&Step::Latch), "and it still ends");
    }

    /// 🛑 THE REMOVAL STILL HAPPENS, AND ON THE SAME TICK IT ALWAYS DID. The row is
    /// `effectEndurance -1`, so a schedule change that delayed or skipped the removal would leave
    /// the player at 0.25x max HP for the session. That is a worse bug than the one being fixed.
    #[test]
    fn the_removal_is_not_delayed_or_skipped() {
        let steps = drive(INTERVAL * 20);
        let remove_at = steps.iter().position(|s| matches!(s, Step::Remove));
        assert_eq!(remove_at, Some(3), "immediately after the third read");
        assert_eq!(
            steps.iter().filter(|s| matches!(s, Step::Remove)).count(),
            1,
            "removed exactly once"
        );
    }

    /// Latch happens once, after the restore watch, and nothing follows it.
    #[test]
    fn it_latches_exactly_once_at_the_end() {
        let steps = drive(INTERVAL * 20);
        assert_eq!(steps.last(), Some(&Step::Latch));
        assert_eq!(steps.iter().filter(|s| matches!(s, Step::Latch)).count(), 1);
    }

    /// Non-interval ticks do nothing, in both phases.
    #[test]
    fn only_interval_ticks_read() {
        assert_eq!(step(1, None, INTERVAL, DUMPS), Step::Wait);
        assert_eq!(step(59, None, INTERVAL, DUMPS), Step::Wait);
        assert_eq!(
            step(0, None, INTERVAL, DUMPS),
            Step::Wait,
            "tick 0 is not a read"
        );
        assert_eq!(step(100, Some(1), INTERVAL, DUMPS), Step::Wait);
        assert_eq!(step(100, Some(0), INTERVAL, DUMPS), Step::Wait);
    }

    /// A zero interval must not divide by zero.
    #[test]
    fn a_zero_interval_is_inert() {
        assert_eq!(step(60, None, 0, DUMPS), Step::Wait);
        assert_eq!(step(60, Some(60), 0, DUMPS), Step::Wait);
    }

    /// ⚠️ THE LABEL CARRIES REAL MILLISECONDS, never ticks/60. `+4s` was out by ~1.75x, and
    /// latency is the thing this probe measures.
    #[test]
    fn the_label_reports_measured_time_not_derived_time() {
        let line = read_label(4, Phase::Apply, 7_000);
        assert!(line.contains("+7000ms"), "{line}");
        assert!(
            !line.contains("+4s"),
            "the derived label is what lied: {line}"
        );
        assert!(line.is_ascii());

        let r = read_label(1, Phase::Restore, 1_800);
        assert!(r.starts_with("RESTORE WATCH"), "{r}");
        assert!(r.contains("+1800ms"), "{r}");
    }
}
