//! `add_item_probe` -- remember what `AddItemFunc` actually RETURNED, per good.
//!
//! # Why this exists
//!
//! `detour::grant_item` called the game's `AddItemFunc` and dropped its `u64` on the floor, so
//! `grant_full_id_outcome` returned `GrantOutcome::Placed` for every call it dispatched -- whether
//! the item entered the bag or the game refused it and dropped it at the player's feet. Compose
//! that with `reconcile::diff` re-emitting `GrantUnique` for any desired good a snapshot cannot
//! see, and a REFUSED good is re-granted until [`crate::reconcile::MAX_GRANT_ATTEMPTS`] parks it,
//! then re-granted again after every world edge (`rearm_grant_stalls`) for the rest of the save.
//!
//! Live capture, bobler 2026-08-04, client 0.3.3 -- `goods 0x4000230c` (8972, Sanctified Whetblade)
//! stalling once per world edge across a 27-epoch session, with three copies hitting the floor each
//! time. The stall log RULED OUT both of `reconcile_io`'s ranked candidates (`key_accessor` was on
//! the single-player list; `in_storage=false`; nothing full), so the cause is not a blind read --
//! the game refused the add and we scored it as accepted.
//!
//! # What this module does NOT do
//!
//! It does not INTERPRET the return value. Nobody has RE'd what `AddItemFunc`'s `u64` means, and
//! guessing a "refused" predicate here would bake an unverified root cause into the reconciler.
//! This records the raw datum and hands it to the stall log. Turning it into a
//! `GrantOutcome::Refused` is a SECOND change, gated on a log that shows what a refusal returns.
//!
//! # The attribution rule (what the tests are about)
//!
//! A stall log naming the wrong call's return is worse than no log: it reads like evidence and is
//! not. Grants are dispatched for many goods per tick, so the probe is keyed BY GOOD -- a stall on
//! good A reports A's own last return or nothing at all, never whatever B did afterwards. That
//! asymmetry is the whole design, and [`AddItemProbe::last_for`] is where it lives.

use std::collections::HashMap;

/// One dispatched `AddItemFunc` call and what it returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddItemCall {
    /// The `full_id` (item id | category nibble) the call was made for.
    pub full_id: i32,
    /// The raw `u64` `AddItemFunc` returned. UNINTERPRETED -- see the module doc.
    pub ret: u64,
    /// Monotonic dispatch counter, so the reader can tell a fresh record from a stale one and
    /// order two goods' calls without a clock.
    pub seq: u64,
}

/// Per-good record of the most recent dispatched `AddItemFunc` call.
///
/// Only calls that were actually DISPATCHED are recorded. A grant that never reached the game (no
/// hook installed, no usable inventory pointer, a quantity capped to zero) records nothing, because
/// "we did not ask" and "we asked and it said this" are different facts and the stall log must not
/// blur them.
#[derive(Debug, Default)]
pub struct AddItemProbe {
    calls: HashMap<i32, AddItemCall>,
    seq: u64,
}

impl AddItemProbe {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a DISPATCHED call's raw return. Overwrites any earlier record for the same good --
    /// the stall cares about the most recent attempt, not the first.
    pub fn record(&mut self, full_id: i32, ret: u64) -> AddItemCall {
        self.seq += 1;
        let call = AddItemCall {
            full_id,
            ret,
            seq: self.seq,
        };
        self.calls.insert(full_id, call);
        call
    }

    /// The last dispatched call FOR THIS GOOD, or `None` if we never dispatched one.
    ///
    /// `None` is a real answer and must be rendered as such: it means the grant never reached
    /// `AddItemFunc`, which points at the inventory pointer / hook / pot cap, not at a refusal.
    pub fn last_for(&self, full_id: i32) -> Option<AddItemCall> {
        self.calls.get(&full_id).copied()
    }

    /// How many distinct goods have a record. Bounds check for the log, not a business rule.
    pub fn len(&self) -> usize {
        self.calls.len()
    }

    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// Drop every record. Called on a world edge alongside `rearm_grant_stalls`, so a stall on the
    /// far side of a load never quotes a return value from before it.
    pub fn clear(&mut self) {
        self.calls.clear();
    }
}

/// Render one good's probe state for the stall log. Kept here (not in the client) so the exact
/// wording is covered by the tests below and cannot drift into implying more than we know.
pub fn describe(probe: &AddItemProbe, full_id: i32) -> String {
    match probe.last_for(full_id) {
        Some(c) => format!("add_item_ret={:#x} (seq {})", c.ret, c.seq),
        None => "add_item_ret=NEVER DISPATCHED".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Illustrative token: the good from the live 2026-08-04 stall (Sanctified Whetblade).
    const WHETBLADE: i32 = 0x4000_230C;
    const OTHER: i32 = 0x4000_1F40;

    #[test]
    fn a_dispatched_call_is_remembered_against_its_own_good() {
        let mut p = AddItemProbe::new();
        p.record(WHETBLADE, 0x1);
        let c = p.last_for(WHETBLADE).expect("recorded");
        assert_eq!(c.full_id, WHETBLADE);
        assert_eq!(c.ret, 0x1);
    }

    /// THE MOTIVATING CASE. The reconciler dispatches grants for several goods per tick, and the
    /// stall log is written for ONE of them. If the probe were a single "last call" slot, a stall
    /// on the whetblade would quote whatever good happened to be granted after it -- a number that
    /// reads like evidence and is not. Keyed by good, the wrong good's return is unreachable.
    #[test]
    fn a_stalled_good_never_reports_another_goods_return() {
        let mut p = AddItemProbe::new();
        p.record(WHETBLADE, 0xDEAD);
        p.record(OTHER, 0xBEEF); // a later, unrelated grant in the same tick
        assert_eq!(
            p.last_for(WHETBLADE).map(|c| c.ret),
            Some(0xDEAD),
            "the whetblade must report its OWN return, not the grant that followed it"
        );
        assert_eq!(p.last_for(OTHER).map(|c| c.ret), Some(0xBEEF));
    }

    /// "We never asked" is not "we asked and it said 0". A grant that never reached AddItemFunc
    /// (no hook, no usable inventory pointer, quantity capped to zero) must be distinguishable
    /// from a dispatched call that returned zero, because they indict different subsystems.
    #[test]
    fn never_dispatched_is_distinct_from_a_zero_return() {
        let mut p = AddItemProbe::new();
        assert_eq!(p.last_for(WHETBLADE), None);
        assert_eq!(describe(&p, WHETBLADE), "add_item_ret=NEVER DISPATCHED");
        p.record(WHETBLADE, 0);
        assert_eq!(p.last_for(WHETBLADE).map(|c| c.ret), Some(0));
        assert_eq!(describe(&p, WHETBLADE), "add_item_ret=0x0 (seq 1)");
    }

    /// The stall cares about the most recent attempt: three attempts then a park, and the log must
    /// quote the third, not the first.
    #[test]
    fn the_latest_attempt_wins_and_seq_advances() {
        let mut p = AddItemProbe::new();
        p.record(WHETBLADE, 0xA);
        p.record(WHETBLADE, 0xB);
        let c = p.record(WHETBLADE, 0xC);
        assert_eq!(p.last_for(WHETBLADE), Some(c));
        assert_eq!(c.ret, 0xC);
        assert_eq!(
            c.seq, 3,
            "seq counts DISPATCHES, including repeats of one good"
        );
    }

    /// World-edge hygiene: `rearm_grant_stalls` gives every parked good a fresh attempt allowance,
    /// so the probe must not carry a pre-load return into a post-load stall.
    #[test]
    fn clear_drops_records_across_a_world_edge() {
        let mut p = AddItemProbe::new();
        p.record(WHETBLADE, 0xA);
        assert_eq!(p.len(), 1);
        p.clear();
        assert!(p.is_empty());
        assert_eq!(p.last_for(WHETBLADE), None);
    }
}
