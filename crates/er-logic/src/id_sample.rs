//! A capped sample that says how much it is NOT showing.
//!
//! # The bug this exists to stop
//!
//! The scaling census prints a count and a list side by side:
//!
//! ```text
//! unrunged 258 (... left vanilla 258, npc_param_ids [44501020, 41800010, ...])
//! ```
//!
//! The list is capped at twelve and carried **no marker**, so 63 of 76 lines in one session claimed
//! more than they printed and nothing said so. A reader takes the list as the population, because
//! it reads exactly like one -- and on 2026-08-16 that cost a real conclusion: a "72 of 73 disjoint"
//! correlation between the recompute failures and the `left vanilla` set, computed against a
//! 12-element sample of a 258-element one, and worth nothing (clients#235).
//!
//! ⭐ It is the same shape as `area_tiers.tsv`'s `parts` column in world#688: a number that answers
//! a NARROWER question than the one it looks like it answers. The fix is the same in both cases --
//! make the narrowing visible at the point of reading.
//!
//! # Why a count beside the list is not enough
//!
//! `unrunged` counts EVENTS and the id list is DEDUPLICATED, so "258 minus 12" is not the number of
//! ids withheld and printing it as one would replace a silent lie with a loud one. This tracks the
//! DISTINCT population separately, which is the only number that makes `+N more` true.

use std::collections::HashSet;

/// Ids kept for display, plus the size of the population they were drawn from.
#[derive(Debug, Default, Clone)]
pub struct IdSample {
    kept: Vec<i32>,
    seen: HashSet<i32>,
    cap: usize,
}

impl IdSample {
    pub fn new(cap: usize) -> Self {
        Self {
            kept: Vec::new(),
            seen: HashSet::new(),
            cap,
        }
    }

    /// Record one id. De-duplicated: a repeat is counted once and never re-printed.
    pub fn note(&mut self, id: i32) {
        if !self.seen.insert(id) {
            return;
        }
        if self.kept.len() < self.cap {
            self.kept.push(id);
        }
    }

    /// Distinct ids seen, whether or not they were kept.
    pub fn distinct(&self) -> usize {
        self.seen.len()
    }

    /// Distinct ids the cap withheld.
    pub fn withheld(&self) -> usize {
        self.seen.len().saturating_sub(self.kept.len())
    }

    pub fn kept(&self) -> &[i32] {
        &self.kept
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// The display form. Complete lists render exactly as a slice does, so nothing changes for the
    /// lines that were already honest; a truncated one names what it is hiding AND the population
    /// it was drawn from, because `+N more` alone still leaves the reader computing the total.
    pub fn render(&self) -> String {
        let ids = self
            .kept
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        if self.withheld() == 0 {
            format!("[{ids}]")
        } else {
            format!(
                "[{ids}, +{} more of {} distinct]",
                self.withheld(),
                self.distinct()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE MOTIVATING CASE (clients#235): `left vanilla 258` printing twelve ids and reading like a
    /// population.
    #[test]
    fn a_truncated_sample_says_so() {
        let mut s = IdSample::new(12);
        for id in 0..258 {
            s.note(id);
        }
        let out = s.render();
        assert!(
            out.contains("+246 more of 258 distinct"),
            "a truncated list must name what it withheld: {out}"
        );
        assert_eq!(s.kept().len(), 12, "the cap still holds");
        assert_eq!(s.distinct(), 258);
    }

    /// The lines that were already honest must not change shape, or every existing log-reading
    /// habit (and every grep) breaks for no gain.
    #[test]
    fn a_complete_sample_renders_exactly_like_a_slice() {
        let mut s = IdSample::new(12);
        for id in [7, 8, 9] {
            s.note(id);
        }
        assert_eq!(s.render(), "[7, 8, 9]");
        assert_eq!(s.withheld(), 0);
    }

    /// Repeats are the reason a count and a list disagree in the first place. 258 EVENTS over 3
    /// distinct ids is a complete list, not a truncated one, and must not claim otherwise.
    #[test]
    fn repeats_do_not_inflate_the_population() {
        let mut s = IdSample::new(12);
        for _ in 0..258 {
            for id in [11, 22, 33] {
                s.note(id);
            }
        }
        assert_eq!(s.distinct(), 3);
        assert_eq!(s.render(), "[11, 22, 33]");
    }

    /// Exactly at the cap is the off-by-one that would put a false `+0 more` on an honest line.
    #[test]
    fn exactly_at_the_cap_is_not_truncated() {
        let mut s = IdSample::new(3);
        for id in [1, 2, 3] {
            s.note(id);
        }
        assert_eq!(s.withheld(), 0);
        assert!(!s.render().contains("more"), "{}", s.render());
        s.note(4);
        assert_eq!(s.withheld(), 1);
        assert!(
            s.render().contains("+1 more of 4 distinct"),
            "{}",
            s.render()
        );
    }

    #[test]
    fn an_empty_sample_is_empty_and_renders_as_such() {
        let s = IdSample::new(12);
        assert!(s.is_empty());
        assert_eq!(s.render(), "[]");
        assert_eq!(s.withheld(), 0);
    }
}
