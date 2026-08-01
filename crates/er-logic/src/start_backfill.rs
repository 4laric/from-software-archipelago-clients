//! Inventory-verified start-item backfill (pure).
//!
//! `grants::drain_start_items` grants the start items once, gated by a persisted BOOLEAN
//! (`start_items_granted`). That boolean is set the first time a character connects, so a character
//! first played BEFORE an item was added to `startItems` never receives the new item -- the flag
//! says "already granted" even though the item was never in the bag. (Live 2026-07-18: a
//! Roundtable-hub character with the Flask of Crimson Tears in `startItems` but no healing flask;
//! the getItemFlag `60000` "obtained" was also set on the fresh save, so nothing noticed.)
//!
//! This is the backstop: given the held inventory, compute which `startItems` are NOT actually in
//! it -- verifying against the bag, not a boolean. Repetition in `startItems` encodes quantity (13
//! copies == grant 13x), and is preserved: an item present even once satisfies ALL its copies (we
//! don't top up a partly-used stack), an entirely-absent item yields all its copies back.
//!
//! Flask nuance: the Flask of Crimson Tears / Cerulean Tears each span a CONTIGUOUS goods-id range
//! covering every empty/charged pair for upgrade levels +0..+12 (Crimson 1000..=1025, Cerulean
//! 1050..=1075 -- verified against EquipParamGoods + GoodsName). ANY member (empty, charged, OR any
//! +N) counts as "have a flask", so the whole family is satisfied: never re-grant the base +0 flask
//! to a player who already holds an upgraded one (live 2026-07-20: a base Flask of Crimson Tears was
//! granted to a character already holding Flask of Crimson Tears +4, ids 1008/1009).

use std::collections::HashSet;

const CATEGORY_GOODS: u32 = 0x4000_0000;
const CATEGORY_MASK: u32 = 0xF000_0000;
const ROW_MASK: u32 = 0x0FFF_FFFF;

/// Goods-row ranges interchangeable for "do you have this flask": every empty/charged pair across
/// upgrade levels +0..+12. Crimson (HP) 1000..=1025, Cerulean (FP) 1050..=1075.
const FLASK_RANGES: &[std::ops::RangeInclusive<u32>] = &[1000..=1025, 1050..=1075];

fn flask_range(row: u32) -> Option<&'static std::ops::RangeInclusive<u32>> {
    FLASK_RANGES.iter().find(|fam| fam.contains(&row))
}

/// The `start_items` FullIDs NOT present in `present` (the set of held inventory item ids, encoded
/// identically to FullIDs: `(category<<28) | row`). Flask families are satisfied by ANY member.
/// Order- and repetition-preserving.
pub fn missing_start_items(present: &HashSet<u32>, start_items: &[i32]) -> Vec<i32> {
    start_items
        .iter()
        .copied()
        .filter(|&fid| {
            let id = fid as u32;
            if present.contains(&id) {
                return false;
            }
            if id & CATEGORY_MASK == CATEGORY_GOODS {
                if let Some(fam) = flask_range(id & ROW_MASK) {
                    if fam.clone().any(|r| present.contains(&(CATEGORY_GOODS | r))) {
                        return false;
                    }
                }
            }
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(ids: &[u32]) -> HashSet<u32> {
        ids.iter().copied().collect()
    }
    const FLASK_HP: i32 = (CATEGORY_GOODS | 1001) as i32; // charged crimson
    const FLASK_HP_EMPTY: u32 = CATEGORY_GOODS | 1000; // empty crimson
    const WEAPON_X: i32 = 0x0000_2710; // weapon row 10000

    #[test]
    fn absent_items_are_returned_present_ones_dropped() {
        let present = set(&[WEAPON_X as u32]);
        let start = [WEAPON_X, FLASK_HP];
        assert_eq!(missing_start_items(&present, &start), vec![FLASK_HP]);
    }

    #[test]
    fn empty_flask_satisfies_the_family() {
        // Player holds only the EMPTY crimson flask; the charged-id start item must NOT re-grant.
        let present = set(&[FLASK_HP_EMPTY]);
        assert!(missing_start_items(&present, &[FLASK_HP]).is_empty());
    }

    #[test]
    fn upgraded_flask_satisfies_the_family() {
        // Live 2026-07-20: holding Flask of Crimson Tears +4 (charged id 1009) must NOT re-grant the
        // base +0 (id 1001). Any level in the family range satisfies it.
        let present = set(&[CATEGORY_GOODS | 1009]);
        assert!(missing_start_items(&present, &[FLASK_HP]).is_empty());
        // ... and an upgraded Cerulean (id 1059 = +4 charged) satisfies the FP start item.
        let fp = (CATEGORY_GOODS | 1051) as i32;
        assert!(missing_start_items(&set(&[CATEGORY_GOODS | 1059]), &[fp]).is_empty());
    }

    #[test]
    fn no_flask_at_all_backfills_it() {
        let present = set(&[WEAPON_X as u32]);
        assert_eq!(missing_start_items(&present, &[FLASK_HP]), vec![FLASK_HP]);
    }

    #[test]
    fn repetition_is_preserved_for_absent_quantity() {
        let present: HashSet<u32> = HashSet::new();
        let start = [WEAPON_X, WEAPON_X, WEAPON_X];
        assert_eq!(missing_start_items(&present, &start), vec![WEAPON_X; 3]);
    }

    #[test]
    fn present_stack_is_not_topped_up() {
        // One copy held -> all copies considered satisfied (no over-grant of a partly-used stack).
        let present = set(&[WEAPON_X as u32]);
        assert!(missing_start_items(&present, &[WEAPON_X, WEAPON_X]).is_empty());
    }

    #[test]
    fn empty_start_list_is_empty() {
        assert!(missing_start_items(&HashSet::new(), &[]).is_empty());
    }
}

// =================================================================================================
// HONESTY + CONVERGENCE (#248, 2026-08-01)
// =================================================================================================
//
// The backfill reported items it had not delivered. Two layers, both by design elsewhere:
//
//   (a) `detour::grant_full_id` returns TRUE for a pot grant capped to zero ("At/over the cap we
//       report success"). That is CORRECT for the ledger watermark -- the item is as delivered as it
//       will ever be -- and catastrophic for a VERIFIER, which must not count it.
//   (b) the settle gate admits a PARTIALLY POPULATED inventory, so absences were read off a bag that
//       was still filling (the 17-id scan that declared 32/35 absent).
//
// Plus the caller latched DONE unconditionally, so hard failures were never retried.
//
// The property this module now enforces:
//
//   **The backfill never reports an item delivered unless a subsequent snapshot contains it.**
//
// Nothing here talks to the game; the client feeds it snapshots and outcomes.

use std::collections::HashMap;

/// What one grant attempt actually achieved — the distinction `grant_full_id`'s `bool` cannot carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantOutcome {
    /// The item entered the bag. The ONLY outcome that may be reported as delivered.
    Placed,
    /// The call succeeded but the game capped the quantity to zero (pot delivery caps). The ledger
    /// counts this as delivered; the verifier must NOT.
    Capped,
    /// Inventory pointer not ready — nothing happened, retry.
    NotReady,
}

/// How many times one FullID may be attempted before it is declared FAILED. Mirrors the grant
/// stall guard's `MAX_GRANT_ATTEMPTS`: bound the flood, never abandon silently.
pub const MAX_ATTEMPTS: u32 = 3;

/// What the caller should do this tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanVerdict {
    /// The snapshot is not trustworthy yet (empty, or it disagrees with the previous tick). Do
    /// nothing and do NOT latch — this is the guard against reading absences off a filling bag.
    Unsettled,
    /// Every start item is accounted for. Latch DONE and report converged.
    Converged,
    /// Attempt these FullIDs (repetition preserved = quantity).
    Grant(Vec<i32>),
    /// Nothing left that may be retried, and these never landed. Latch DONE and warn LOUDLY.
    Exhausted(Vec<i32>),
}

/// Cross-tick bookkeeping for the convergence loop.
#[derive(Debug, Default)]
pub struct BackfillState {
    attempts: HashMap<i32, u32>,
    prev_snapshot: Option<HashSet<u32>>,
    /// Items delivered AND confirmed present by a later snapshot — the only ones we may report.
    confirmed: Vec<i32>,
}

impl BackfillState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide what to do given this tick's inventory snapshot.
    ///
    /// TWO-TICK AGREEMENT: a snapshot is only trusted once an identical, non-empty snapshot was seen
    /// on the previous tick. A bag that is still filling changes between ticks, so it can never
    /// satisfy this — which is precisely the 17-id false-absence class.
    pub fn observe(&mut self, present: &HashSet<u32>, start_items: &[i32]) -> ScanVerdict {
        let agreed = !present.is_empty() && self.prev_snapshot.as_ref() == Some(present);
        self.prev_snapshot = Some(present.clone());
        if !agreed {
            return ScanVerdict::Unsettled;
        }

        let missing = missing_start_items(present, start_items);
        if missing.is_empty() {
            return ScanVerdict::Converged;
        }
        let retryable: Vec<i32> = missing
            .iter()
            .copied()
            .filter(|f| self.attempts.get(f).copied().unwrap_or(0) < MAX_ATTEMPTS)
            .collect();
        if retryable.is_empty() {
            // Deduplicate for the warn: the player wants the ids, not the multiplicity.
            let mut failed = missing;
            failed.sort_unstable();
            failed.dedup();
            return ScanVerdict::Exhausted(failed);
        }
        ScanVerdict::Grant(retryable)
    }

    /// Record what an attempt achieved. `Capped` and `NotReady` are NOT deliveries; only a later
    /// snapshot showing the item present makes it one (see [`Self::confirm`]).
    pub fn record(&mut self, fid: i32, outcome: GrantOutcome) {
        if outcome == GrantOutcome::NotReady {
            return; // nothing was attempted -- do not burn an attempt on a pointer that wasn't up
        }
        *self.attempts.entry(fid).or_insert(0) += 1;
    }

    /// Fold the NEXT snapshot into the delivered set: an item we attempted and can now SEE is
    /// delivered; one we attempted and still cannot see is not, whatever the call returned.
    pub fn confirm(&mut self, present: &HashSet<u32>) {
        for (&fid, _) in self.attempts.iter() {
            if present.contains(&(fid as u32)) && !self.confirmed.contains(&fid) {
                self.confirmed.push(fid);
            }
        }
    }

    /// The count we are entitled to report as granted. Bag-verified, never call-verified.
    pub fn confirmed_count(&self) -> usize {
        self.confirmed.len()
    }

    /// FullIDs attempted at least once that no snapshot has ever confirmed.
    pub fn unconfirmed(&self) -> Vec<i32> {
        let mut v: Vec<i32> = self
            .attempts
            .keys()
            .copied()
            .filter(|f| !self.confirmed.contains(f))
            .collect();
        v.sort_unstable();
        v
    }
}

#[cfg(test)]
mod honesty_tests {
    use super::*;

    const POT: i32 = (CATEGORY_GOODS | 2000) as i32;
    const LANTERN: i32 = (CATEGORY_GOODS | 2001) as i32;

    fn snap(ids: &[i32]) -> HashSet<u32> {
        ids.iter().map(|&i| i as u32).collect()
    }

    /// A filling bag changes between ticks, so it never earns agreement -- the 17-id false-absence
    /// class cannot happen.
    #[test]
    fn a_changing_snapshot_is_never_acted_on() {
        let mut st = BackfillState::new();
        assert_eq!(
            st.observe(&snap(&[LANTERN]), &[POT]),
            ScanVerdict::Unsettled
        );
        assert_eq!(
            st.observe(&snap(&[LANTERN, POT]), &[POT]),
            ScanVerdict::Unsettled,
            "the bag changed -- still filling, still not trustworthy"
        );
        // Now it holds still.
        assert_eq!(
            st.observe(&snap(&[LANTERN, POT]), &[POT]),
            ScanVerdict::Converged
        );
    }

    #[test]
    fn an_empty_snapshot_is_never_acted_on() {
        let mut st = BackfillState::new();
        assert_eq!(st.observe(&HashSet::new(), &[POT]), ScanVerdict::Unsettled);
        assert_eq!(st.observe(&HashSet::new(), &[POT]), ScanVerdict::Unsettled);
    }

    /// THE #248 ACCEPTANCE TEST (Rule 11: the motivating case IS the test).
    ///
    /// A hook that ACCEPTS every call but never adds the item -- exactly what a capped-to-zero pot
    /// grant looks like -- must report ZERO granted and name the item as FAILED. The old code
    /// reported `granted 22/32` in this situation.
    #[test]
    fn a_grant_that_accepts_but_never_places_reports_zero_and_fails_loudly() {
        let mut st = BackfillState::new();
        let bag = snap(&[LANTERN]); // POT never arrives, no matter how often we ask

        let mut verdicts = Vec::new();
        for _ in 0..8 {
            let v = st.observe(&bag, &[POT]);
            if let ScanVerdict::Grant(ref fids) = v {
                for &f in fids {
                    // The game says "sure" and does nothing. This is the whole bug.
                    st.record(f, GrantOutcome::Capped);
                }
            }
            st.confirm(&bag);
            verdicts.push(v);
        }

        assert_eq!(
            st.confirmed_count(),
            0,
            "NOTHING was delivered -- reporting any non-zero count is the #248 lie"
        );
        assert_eq!(st.unconfirmed(), vec![POT]);
        assert!(
            verdicts.contains(&ScanVerdict::Exhausted(vec![POT])),
            "after MAX_ATTEMPTS the item must be declared FAILED, not silently dropped: {verdicts:?}"
        );
    }

    /// The honest success path: the item actually lands and IS reported.
    #[test]
    fn a_grant_that_places_is_confirmed_by_the_next_snapshot() {
        let mut st = BackfillState::new();
        let before = snap(&[LANTERN]);
        st.observe(&before, &[POT]);
        let v = st.observe(&before, &[POT]);
        assert_eq!(v, ScanVerdict::Grant(vec![POT]));
        st.record(POT, GrantOutcome::Placed);

        let after = snap(&[LANTERN, POT]);
        st.confirm(&after);
        assert_eq!(st.confirmed_count(), 1);
        assert!(st.unconfirmed().is_empty());
        st.observe(&after, &[POT]);
        assert_eq!(st.observe(&after, &[POT]), ScanVerdict::Converged);
    }

    /// A not-ready inventory pointer must not burn an attempt -- otherwise three load-screen ticks
    /// would exhaust an item that was never actually asked for.
    #[test]
    fn not_ready_does_not_consume_an_attempt() {
        let mut st = BackfillState::new();
        let bag = snap(&[LANTERN]);
        st.observe(&bag, &[POT]);
        for _ in 0..10 {
            if let ScanVerdict::Grant(fids) = st.observe(&bag, &[POT]) {
                for f in fids {
                    st.record(f, GrantOutcome::NotReady);
                }
            }
        }
        assert_eq!(
            st.observe(&bag, &[POT]),
            ScanVerdict::Grant(vec![POT]),
            "still retryable: no real attempt was ever made"
        );
    }

    // ---- MIGRATED from the drain's replay tier (#267) --------------------------------------
    //
    // The drain's dedup was `start_items_granted`, a PERSISTED boolean. Possession replaces it. These
    // two cases are the drain's motivating bugs, re-homed onto the convergence loop rather than
    // deleted with it -- and both are STRICTLY STRONGER here, because a bag cannot go stale the way
    // a boolean can.

    /// FROM `flask_grant_replay` (er-flask-double-grant-reconnect, 2026-07-05). A fresh Grafted Scion
    /// dies in the tutorial and the game reloads; the drain re-ran and turned a 3+1 flask start into
    /// 6+2. Its fix was "latch on PERSISTED state, not a session proxy a reload wipes".
    ///
    /// Under possession there is no latch to wipe: `BackfillState` is per-LAUNCH and starts empty,
    /// but the BAG persists, so a relaunch sees the flasks and converges. The dedup survives a reload
    /// because it never depended on our bookkeeping at all.
    #[test]
    fn flask_dedup_survives_a_reload_via_the_bag_replay() {
        let hp = (CATEGORY_GOODS | 1001) as i32;
        let fp = (CATEGORY_GOODS | 1051) as i32;
        let start = [hp, hp, hp, fp]; // 3 heal + 1 FP

        // First launch: empty bag, both flasks granted.
        let mut st = BackfillState::new();
        let empty = snap(&[]);
        st.observe(&empty, &start);
        assert_eq!(
            st.observe(&empty, &start),
            ScanVerdict::Unsettled,
            "empty bag is never trusted"
        );

        let bag = snap(&[hp, fp]);
        st.observe(&bag, &start);
        assert_eq!(st.observe(&bag, &start), ScanVerdict::Converged);

        // RELOAD: a brand-new state (session latches are gone), same bag.
        let mut after_reload = BackfillState::new();
        after_reload.observe(&bag, &start);
        assert_eq!(
            after_reload.observe(&bag, &start),
            ScanVerdict::Converged,
            "post-reload must grant NOTHING -- 3+1 must not become 6+2"
        );
        assert_eq!(after_reload.confirmed_count(), 0);
    }

    /// FROM `start_grant_replay` (Torch clobber, 2026-07-06). The grant fired during the load screen,
    /// the bulk inventory load then REPLACED the bag and wiped the Torch, and because the grant had
    /// latched with no read-back it never retried -- the Torch was gone forever.
    ///
    /// The old fix AVOIDED the clobber with a timer. The convergence loop DETECTS and heals it: the
    /// post-clobber snapshot shows the item absent and it is granted again. That covers a clobber
    /// landing after any timer would have expired, which the timer never could.
    #[test]
    fn a_clobbered_start_item_is_re_granted_not_lost_replay() {
        let torch = (CATEGORY_GOODS | 2000) as i32;
        let other = (CATEGORY_GOODS | 2001) as i32;
        let start = [torch];

        let mut st = BackfillState::new();
        let pre = snap(&[other]);
        st.observe(&pre, &start);
        assert_eq!(st.observe(&pre, &start), ScanVerdict::Grant(vec![torch]));
        st.record(torch, GrantOutcome::Placed);

        let landed = snap(&[other, torch]);
        st.confirm(&landed);
        assert_eq!(st.confirmed_count(), 1);

        // BULK LOAD REPLACES THE BAG: the Torch is gone and nothing told us.
        let clobbered = snap(&[other]);
        st.confirm(&clobbered);
        st.observe(&clobbered, &start);
        assert_eq!(
            st.observe(&clobbered, &start),
            ScanVerdict::Grant(vec![torch]),
            "the clobber must be SEEN and re-granted -- the old latch lost the Torch forever"
        );
    }

    /// A healthy established save: everything already present, so the loop converges on the first
    /// agreed snapshot and reports zero granted (no re-delivery).
    #[test]
    fn healthy_save_converges_with_nothing_granted() {
        let mut st = BackfillState::new();
        let bag = snap(&[LANTERN, POT]);
        st.observe(&bag, &[POT, LANTERN]);
        assert_eq!(st.observe(&bag, &[POT, LANTERN]), ScanVerdict::Converged);
        assert_eq!(st.confirmed_count(), 0);
        assert!(st.unconfirmed().is_empty());
    }
}
