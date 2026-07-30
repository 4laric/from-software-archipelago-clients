//! `summon_pools` — decide WHICH summoning-pool flags are safe for us to set.
//!
//! ## Why this is not "just set the flags"
//!
//! Activating every summoning pool is a one-line cheat in a CE table: walk `SignPuddleParam` and set
//! an event flag whose id IS the param ROW id. That works in a vanilla game. It is unsafe here,
//! because **ER reuses flag id-space across corpora and some of those ids are OUR check flags.**
//!
//! Pool rows live in the `670xxx` band, and so do shop `eventFlag_forStock` values. `670100` is
//! simultaneously the Witchbane Ruins pool row AND the purchase flag of shop row `100692` (Blue
//! Cloth Vest) — a derived AP shop check. `670000` / `670200` / `670300` collide the same way with
//! `100691` / `100693` / `100694`, and two Twin Maiden rows sit in the band as well. Setting those
//! blind would falsely RELEASE checks and mark shop slots holding placed AP items as sold out.
//!
//! This is the [[er-version-cannot-identify-the-build]] whetblade bug wearing a different hat: a
//! vanilla flag doing double duty as an AP check. The rule that came out of that one — never write a
//! flag the poll is watching — is what this module enforces.
//!
//! So the decision is a set difference, and it is pure: given the pool row ids the game actually has
//! and the flag universe the client is watching for checks, partition into "safe to set" and
//! "skipped". The client supplies both inputs and does the writing.

use std::collections::BTreeSet;

/// The outcome of the safety partition. Both halves are sorted and de-duplicated so the log line is
/// stable and diffable between runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PoolPlan {
    /// Pool flags we may set: no AP check is keyed on them.
    pub to_set: Vec<u32>,
    /// Pool flags withheld because the check poll watches them. Non-empty is EXPECTED, not an error.
    pub skipped: Vec<u32>,
}

/// Partition `row_ids` against the `protected` flag universe.
///
/// `protected` must be every flag the client could interpret as a check firing — the values of the
/// location-flag map AND the keys of the sweep map. Passing an incomplete set silently re-opens the
/// false-release hole, so the caller assembles it from the merged table, never from a literal.
pub fn plan(row_ids: impl IntoIterator<Item = u32>, protected: &BTreeSet<u32>) -> PoolPlan {
    let mut to_set = BTreeSet::new();
    let mut skipped = BTreeSet::new();
    for id in row_ids {
        // Row id 0 is not a flag. Treat it as absent rather than writing flag 0.
        if id == 0 {
            continue;
        }
        if protected.contains(&id) {
            skipped.insert(id);
        } else {
            to_set.insert(id);
        }
    }
    PoolPlan {
        to_set: to_set.into_iter().collect(),
        skipped: skipped.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protected(ids: &[u32]) -> BTreeSet<u32> {
        ids.iter().copied().collect()
    }

    /// THE MOTIVATING CASE: 670100 is both the Witchbane Ruins pool row and the Blue Cloth Vest
    /// shop-stock flag. It must be withheld, and the pools that do NOT collide must still fire.
    #[test]
    fn shop_stock_collision_is_withheld() {
        let rows = [670000, 670100, 670200, 670300, 671500];
        let plan = plan(rows, &protected(&[670000, 670100, 670200, 670300]));
        assert_eq!(plan.to_set, vec![671500]);
        assert_eq!(plan.skipped, vec![670000, 670100, 670200, 670300]);
    }

    /// A sweep flag is watched as a KEY, not a value — it protects just the same.
    #[test]
    fn sweep_keys_protect_too() {
        let plan = plan([671000, 671001], &protected(&[671001]));
        assert_eq!(plan.to_set, vec![671000]);
        assert_eq!(plan.skipped, vec![671001]);
    }

    /// Nothing watched: every pool is fair game. This is the vanilla-ish case and must not regress
    /// into withholding everything.
    #[test]
    fn empty_protected_sets_everything() {
        let plan = plan([670100, 671500], &BTreeSet::new());
        assert_eq!(plan.to_set, vec![670100, 671500]);
        assert!(plan.skipped.is_empty());
    }

    /// EVERY pool watched: we must produce an empty write list, not fall back to writing them.
    #[test]
    fn fully_protected_writes_nothing() {
        let plan = plan([670100, 671500], &protected(&[670100, 671500]));
        assert!(plan.to_set.is_empty());
        assert_eq!(plan.skipped, vec![670100, 671500]);
    }

    /// Flag 0 is not a flag. A malformed/blank row must never make us write it.
    #[test]
    fn row_zero_is_dropped_not_written() {
        let plan = plan([0, 671500], &BTreeSet::new());
        assert_eq!(plan.to_set, vec![671500]);
        assert!(plan.skipped.is_empty());
    }

    /// Output is sorted and de-duplicated regardless of input order or repeats.
    #[test]
    fn output_is_stable_and_deduped() {
        let plan = plan([671500, 670100, 671500, 670100], &protected(&[670100]));
        assert_eq!(plan.to_set, vec![671500]);
        assert_eq!(plan.skipped, vec![670100]);
    }
}
