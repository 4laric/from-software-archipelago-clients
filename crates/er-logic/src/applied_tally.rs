//! `N of M` summaries that can tell "nothing to do" from "nothing done".
//!
//! # Why this is a shared type and not a sixth one-off
//!
//! This client has now had the same defect **six times**, each found separately and each fixed in
//! place:
//!
//! | line | what it counted | what a reader took it for |
//! |---|---|---|
//! | `trap spawn: 3 x c4150 requested` | `spec.count` -- what was ASKED | three basilisks (world#689) |
//! | `readback STUCK` | the write took | "jammed" -- the opposite (#200) |
//! | `capital release re-key: 0/4` | writes performed | four rows that failed (#200) |
//! | `sweep-flush: 14 ... (0 still owed)` | flags asserted | five flags lost (world#697) |
//! | `(re)scaled 163 enemy(ies)` | speffect writes | 163 enemies rescaled (#188) |
//! | `pot-cap: ... CAPPED` | the FIRST cap only | the only cap (world#692) |
//!
//! Every one is the same sentence: **a summary reporting the REQUEST rather than the RESULT.**
//! Five were benign and cost triage time anyway -- two of them (`readback STUCK`, `0/4 re-keyed`)
//! sent a maintainer at the healthy branch in writing. One was a real bug.
//!
//! 🛑 THE READER'S PROBLEM IS ALWAYS THE SAME. `0 of 534` is indistinguishable between "all 534
//! were already correct", "534 were absent", and "the table was not loaded and we walked nothing".
//! Those are a clean idempotent re-run, a data drift, and a silent failure. A count alone cannot
//! separate them, and the separation is the whole diagnostic value.
//!
//! ⭐ `spell_slot_length` is the model this generalises: it guards `rows == 0` before latching and
//! branches its line on whether the count matched the vanilla expectation, with a sentence saying
//! a stacked data mod legitimately changes it. That function is the only one of the six that a
//! reader could act on without opening the source.

/// One pass over a set of rows: what happened, not what was attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AppliedTally {
    /// Rows actually walked. 🛑 `0` here is the case that must never latch DONE -- it means the
    /// param table was not populated, not that there was nothing to do.
    pub walked: u32,
    /// Rows this pass changed.
    pub changed: u32,
    /// Rows already holding the desired value (a clean idempotent re-run).
    pub already: u32,
    /// Rows expected and not found -- data drift, a stacked mod, or a stale table.
    pub absent: u32,
}

impl AppliedTally {
    pub const fn new() -> Self {
        Self {
            walked: 0,
            changed: 0,
            already: 0,
            absent: 0,
        }
    }

    /// Did this pass see a populated table at all?
    ///
    /// The caller must not latch a one-shot DONE when this is false -- `shop_flags` states the rule
    /// outright ("if the clamp could not run, RETRY -- never latch DONE over it") and
    /// `flatten_regular_upgrades` did not follow it.
    pub fn ran(&self) -> bool {
        self.walked > 0
    }

    /// `true` when the pass had genuinely nothing to do -- rows were walked and every one was
    /// already correct. Distinct from [`Self::ran`] returning false, which is a failure.
    pub fn was_noop(&self) -> bool {
        self.ran() && self.changed == 0 && self.absent == 0
    }

    /// The summary clause. ASCII (repo rule 10).
    ///
    /// Names every bucket even at zero, so a reader never has to wonder which one was omitted --
    /// the omission is exactly what made `0/4` and `0/534` unreadable.
    pub fn summary(&self, what: &str) -> String {
        if !self.ran() {
            return format!(
                "{what}: NOTHING WALKED -- the param table was not populated, so this pass did \
                 not run and must not be latched as done"
            );
        }
        format!(
            "{what}: {} changed, {} already correct, {} absent, of {} row(s) walked",
            self.changed, self.already, self.absent, self.walked
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⭐ THE THREE READINGS `0 of 534` CANNOT TELL APART, now three different sentences.
    #[test]
    fn zero_changed_has_three_distinct_meanings() {
        let clean = AppliedTally {
            walked: 534,
            already: 534,
            ..AppliedTally::new()
        };
        let drifted = AppliedTally {
            walked: 534,
            absent: 534,
            ..AppliedTally::new()
        };
        let never_ran = AppliedTally::new();

        assert!(clean.was_noop(), "all already correct is a clean re-run");
        assert!(!drifted.was_noop(), "534 absent is not a clean re-run");
        assert!(
            !never_ran.ran(),
            "walking nothing is a failure, not a no-op"
        );

        let (a, b, c) = (
            clean.summary("shop-flags"),
            drifted.summary("shop-flags"),
            never_ran.summary("shop-flags"),
        );
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    /// 🛑 THE LATCH RULE. `ran() == false` is what must stop a one-shot from marking itself done --
    /// the bug `flatten_regular_upgrades` had and `spell_slot_length` guards against.
    #[test]
    fn an_unpopulated_table_says_do_not_latch() {
        let t = AppliedTally::new();
        assert!(!t.ran());
        let line = t.summary("flatten_regular_upgrades");
        assert!(line.contains("NOTHING WALKED"), "{line}");
        assert!(line.contains("must not be latched"), "{line}");
    }

    /// A real pass names every bucket, including the zeroes.
    #[test]
    fn a_real_pass_names_every_bucket() {
        let t = AppliedTally {
            walked: 100,
            changed: 7,
            already: 90,
            absent: 3,
        };
        let line = t.summary("shop-flags");
        for want in [
            "7 changed",
            "90 already correct",
            "3 absent",
            "100 row(s) walked",
        ] {
            assert!(line.contains(want), "{want} missing from: {line}");
        }
        assert!(!t.was_noop(), "7 changed is not a no-op");
    }

    /// Every line is ASCII (repo rule 10).
    #[test]
    fn the_summaries_are_ascii() {
        for t in [
            AppliedTally::new(),
            AppliedTally {
                walked: 4,
                already: 4,
                ..AppliedTally::new()
            },
        ] {
            assert!(t.summary("capital release re-key").is_ascii());
        }
    }
}
