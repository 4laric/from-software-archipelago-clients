//! `sweep_watch` -- say WHEN a boss-sweep trigger flag flips, and WHICH flag it was.
//!
//! MOTIVATING CASE (rule 11), 2026-08-07. bobler killed the boss in the Scadutree Avatar's arena
//! at 13:01:02 and the region's 49-check sweep did not land until 13:03:47 -- 2m45s and one warp
//! later. From his seat that is indistinguishable from a broken sweep, and he reported it as one
//! ("i killed metyr and got nothing", "i think something is wrong lol").
//!
//! Two separate holes made a three-minute gap unreadable from the log, and BOTH were places where
//! the answer was computed and then discarded:
//!
//! 1. **The banner dropped the flag.** `Boss sweep ({region}): {boss}` falls back to
//!    `Boss sweep ({region})` when the boss name cannot be resolved -- and on a seed with
//!    `0 boss-lock def(s)` it can NEVER be resolved, so every sweep loses its only identifier. The
//!    `(None, None)` arm one line below prints `flag {flag}` and would have been the useful one.
//!    🛑 The lesson is narrow and worth keeping: a fallback chain that degrades to the PRETTIEST
//!    remaining label instead of the most IDENTIFYING one is backwards for a diagnostic.
//!
//! 2. **Nothing ever logged a trigger flag that was still false.** The sweep is logged when it
//!    fires and never while it waits, so "armed and waiting" and "not armed at all" print the same
//!    thing: nothing. Absence of a line was carrying two meanings.
//!
//! This module owns the decision of what to SAY; the caller does the flag reads. Emitting only on
//! CHANGE is the whole design -- a per-poll dump would be thousands of lines over a session
//! (hudhook's every-frame ERROR pair once put 612,842 lines in one player's log), while a
//! transition log is silent until something happens and then timestamps it exactly.

use std::collections::BTreeMap;

/// One sweep group as the caller observed it this poll: `(trigger flag, member count, flag set?)`.
pub type GroupObservation = (u32, usize, bool);

/// Remembers the last observed state of every sweep trigger flag, so only CHANGES are reported.
#[derive(Debug, Default, Clone)]
pub struct SweepWatch {
    /// BTreeMap, not HashMap: the census line is read by a human comparing two runs, and a stable
    /// flag order is what makes that diff-able.
    seen: BTreeMap<u32, bool>,
    censused: bool,
}

impl SweepWatch {
    pub fn new() -> Self {
        Self {
            seen: BTreeMap::new(),
            censused: false,
        }
    }

    /// Forget everything. Call on seed change / reconnect, so the next poll re-censuses.
    pub fn reset(&mut self) {
        self.seen.clear();
        self.censused = false;
    }

    /// The lines to log for this observation. Empty when nothing changed.
    ///
    /// The FIRST call emits a census naming every group and its flag -- which is the line that
    /// maps a 49-member sweep back to its trigger without needing `dungeonSweepFlags` out of
    /// slot_data (that echo is itself truncated in the log, so it cannot be relied on).
    /// Every later call reports only flags whose state changed.
    ///
    /// ⭐ A group appearing for the first time AFTER the census is reported as an addition rather
    /// than silently folded in: sweep groups arrive with slot_data, so a new one mid-session means
    /// a reconnect or a config reload, and that is worth seeing.
    pub fn observe(&mut self, groups: &[GroupObservation]) -> Vec<String> {
        let mut out = Vec::new();

        if !self.censused {
            self.censused = true;
            let mut parts: Vec<String> = groups
                .iter()
                .map(|&(flag, members, set)| {
                    format!("{flag}({members}){}", if set { "=SET" } else { "" })
                })
                .collect();
            parts.sort();
            out.push(format!(
                "sweep-watch: census -- {} group(s), {} already set: [{}]",
                groups.len(),
                groups.iter().filter(|g| g.2).count(),
                parts.join(", ")
            ));
            for &(flag, _, set) in groups {
                self.seen.insert(flag, set);
            }
            return out;
        }

        for &(flag, members, set) in groups {
            match self.seen.insert(flag, set) {
                Some(prev) if prev == set => {}
                Some(_) => out.push(format!(
                    "sweep-watch: trigger flag {flag} -> {} ({members} member(s) in its group)",
                    if set { "SET" } else { "CLEARED" }
                )),
                None => out.push(format!(
                    "sweep-watch: NEW group, trigger flag {flag} = {} ({members} member(s)) -- \
                     groups arrive with slot_data, so this means a reconnect or a config reload",
                    if set { "SET" } else { "clear" }
                )),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_observation_censuses_every_group_and_names_its_flag() {
        // The census exists because slot_data's own `dungeonSweepFlags` echo was TRUNCATED in
        // bobler's log, so the 49-member group could not be mapped back to a trigger at all.
        let mut w = SweepWatch::new();
        let out = w.observe(&[(20000800, 49, false), (20001000, 3, true)]);
        assert_eq!(
            out.len(),
            1,
            "the census is ONE line, not one per group: {out:?}"
        );
        assert!(out[0].contains("20000800(49)"), "{}", out[0]);
        assert!(out[0].contains("20001000(3)=SET"), "{}", out[0]);
        assert!(out[0].contains("2 group(s), 1 already set"), "{}", out[0]);
    }

    #[test]
    fn an_unchanged_poll_says_nothing() {
        // 🛑 THE PROPERTY THAT KEEPS THIS SHIPPABLE. The poll runs continuously for a whole
        // session; anything that logs per-poll rather than per-CHANGE would bury the log.
        let mut w = SweepWatch::new();
        w.observe(&[(20000800, 49, false)]);
        for _ in 0..1000 {
            assert!(w.observe(&[(20000800, 49, false)]).is_empty());
        }
    }

    #[test]
    fn the_flip_is_timestamped_by_being_the_only_line() {
        // MOTIVATING CASE: this is the line that would have dated bobler's 2m45s gap. The kill is
        // already in the log as a check send; this supplies the other end.
        let mut w = SweepWatch::new();
        w.observe(&[(20000800, 49, false)]);
        assert!(w.observe(&[(20000800, 49, false)]).is_empty());
        let out = w.observe(&[(20000800, 49, true)]);
        assert_eq!(out.len(), 1);
        assert!(
            out[0].contains("20000800"),
            "the FLAG is the point: {}",
            out[0]
        );
        assert!(out[0].contains("SET"), "{}", out[0]);
        // and it does not repeat once reported
        assert!(w.observe(&[(20000800, 49, true)]).is_empty());
    }

    #[test]
    fn a_flag_going_back_to_false_is_reported_too() {
        // Not expected in play, but a sweep flag that CLEARS would be a real finding (save-scum,
        // NG+, a foreign write), and a watcher that only ever reports one direction would hide it.
        let mut w = SweepWatch::new();
        w.observe(&[(20000800, 49, true)]);
        let out = w.observe(&[(20000800, 49, false)]);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("CLEARED"), "{}", out[0]);
    }

    #[test]
    fn a_group_arriving_after_the_census_is_called_out_not_folded_in() {
        let mut w = SweepWatch::new();
        w.observe(&[(20000800, 49, false)]);
        let out = w.observe(&[(20000800, 49, false), (20001000, 7, false)]);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("NEW group"), "{}", out[0]);
        assert!(out[0].contains("20001000"), "{}", out[0]);
    }

    #[test]
    fn reset_re_censuses() {
        let mut w = SweepWatch::new();
        w.observe(&[(20000800, 49, false)]);
        w.reset();
        let out = w.observe(&[(20000800, 49, false)]);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("census"), "{}", out[0]);
    }

    #[test]
    fn every_line_is_ascii() {
        // These go to the log file, not the game font, so this is weaker than the toast rule --
        // but users paste log lines into Discord and a mojibake line costs a round trip.
        let mut w = SweepWatch::new();
        let mut all = w.observe(&[(20000800, 49, false), (20001000, 3, true)]);
        all.extend(w.observe(&[(20000800, 49, true), (20001000, 3, true), (7, 1, false)]));
        for s in all {
            assert!(s.is_ascii(), "{s:?}");
        }
    }
}
