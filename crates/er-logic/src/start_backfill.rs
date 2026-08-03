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

/// Continuous in-world dwell after which the inventory is trusted without a real pickup.
///
/// REHOMED from the deleted `start_grant_replay` (#267). The gate itself is unchanged and still
/// needed -- it guards start FLAGS and the unique start grants, and it is the settle the backfill
/// waits out before its first scan. Only the DRAIN it was written for is gone.
pub const START_ITEM_SETTLE_MS: u64 = 8_000;

/// The inventory is SETTLED once the game has driven a real `AddItem` (`real_pickup_seen` -- proof
/// the save / new-game bulk load replace is done) OR we have been continuously in-world at least
/// [`START_ITEM_SETTLE_MS`] (the fallback when the player triggers no pickup).
pub fn start_items_settled(real_pickup_seen: bool, in_world_ms: u64) -> bool {
    real_pickup_seen || in_world_ms >= START_ITEM_SETTLE_MS
}

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
    fn settle_needs_a_real_pickup_or_the_dwell() {
        assert!(!start_items_settled(false, 0));
        assert!(!start_items_settled(false, START_ITEM_SETTLE_MS - 1));
        assert!(start_items_settled(false, START_ITEM_SETTLE_MS));
        assert!(
            start_items_settled(true, 0),
            "a real pickup proves the bulk load is done -- no need to wait out the dwell"
        );
    }

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
    /// Which round each fid last burned an attempt in. `MAX_ATTEMPTS` is meant to bound the number
    /// of TICKS we keep asking, but `record` is called once per COPY, and repetition in `startItems`
    /// encodes quantity -- 10 Cracked Pots means ten calls in one tick. Counting per call let a
    /// 10-copy item burn its whole budget before the first tick ended, so the next tick would declare
    /// it FAILED after what was really a single round. Observed as a near miss in Alaric's
    /// 2026-08-01 smoke test: four consecutive rounds of ten grants each, saved only because they
    /// were all `NotReady` (which deliberately burns nothing) while the inventory pointer re-primed
    /// after a world edge. Had they been `Capped`, the pots would have been declared undeliverable.
    last_round: HashMap<i32, u64>,
    round: u64,
    prev_snapshot: Option<HashSet<u32>>,
    /// FullIDs whose grant came back [`GrantOutcome::Capped`] -- the call succeeded and the game
    /// added nothing. Recorded because the BAG CANNOT SHOW THIS: the scan is presence-based on
    /// purpose (see `present_stack_is_not_topped_up` -- counting quantity would re-grant a stack
    /// the player has simply used), so one arrived pot makes nine requested pots look satisfied.
    /// The cap outcome is the only evidence that the rest never landed.
    capped: Vec<i32>,
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

        self.round += 1;
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
        // ONE attempt per fid per ROUND, however many copies were asked for. See `last_round`.
        if self.last_round.get(&fid) == Some(&self.round) {
            return;
        }
        self.last_round.insert(fid, self.round);
        *self.attempts.entry(fid).or_insert(0) += 1;
        if outcome == GrantOutcome::Capped && !self.capped.contains(&fid) {
            self.capped.push(fid);
        }
    }

    /// Fold the NEXT snapshot into the delivered set: an item we attempted and can now SEE is
    /// delivered; one we attempted and still cannot see is not, whatever the call returned.
    pub fn confirm(&mut self, present: &HashSet<u32>) {
        for &fid in self.attempts.keys() {
            if present.contains(&(fid as u32)) && !self.confirmed.contains(&fid) {
                self.confirmed.push(fid);
            }
        }
    }

    /// FullIDs the game refused to add because a delivery cap was already met. Non-empty means the
    /// player is owed items that CANNOT arrive, and a convergence report that does not say so is a
    /// false success (#308).
    ///
    /// 🛑 Deliberately NOT folded into `Exhausted`. Exhausted means "we ran out of attempts and do
    /// not know why"; this means "the game told us it would never take them". Different facts, and
    /// the second one is actionable -- it points at the seed's start-item list, not at the client.
    pub fn capped_shortfall(&self) -> &[i32] {
        &self.capped
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

    /// THE MOTIVATING CASE (rule 11). Alaric, 2026-08-03, client 0.3.2 -- nine Hefty Cracked Pots
    /// requested, the first Placed, the rest eaten by the pot delivery cap, and the loop reported
    /// CONVERGED **in the same second**:
    ///
    ///   16:54:29 9/40 startItems absent -> attempting ["0x401ea99c" x9]
    ///   16:54:29 grant 0x401ea99c -> Placed
    ///   16:54:29 CONVERGED -- all 40 startItems present in bag. granted 1 this session
    ///   16:54:30 [WARN] pot-cap: grant of 1 CAPPED to 0 (held 10, cap 10)
    ///
    /// 🛑 The scan is right to say Converged: the bag DOES hold the id, and asking it for
    /// quantities instead would re-grant a stack the player has merely used
    /// (`present_stack_is_not_topped_up` pins that, and it is why the obvious fix is wrong).
    /// `GrantOutcome::Capped` is the only evidence the other eight never landed, so it has to be
    /// remembered rather than re-derived from a bag that cannot show it.
    #[test]
    fn a_capped_grant_is_remembered_even_though_the_bag_looks_satisfied() {
        let mut st = BackfillState::new();
        // one copy arrived, eight were capped -- all in the same round
        st.record(POT, GrantOutcome::Placed);
        st.round += 1; // `record` allows one attempt per fid per round
        st.record(POT, GrantOutcome::Capped);

        // the bag now shows the pot, so the scan converges -- correctly
        assert_eq!(
            st.observe(&snap(&[POT]), &[POT, POT]),
            ScanVerdict::Unsettled
        );
        assert_eq!(
            st.observe(&snap(&[POT]), &[POT, POT]),
            ScanVerdict::Converged
        );

        assert_eq!(
            st.capped_shortfall(),
            &[POT],
            "converging while a grant was capped is a false success -- the player is owed copies \
             that CANNOT arrive, and only the cap outcome knows it (#308)"
        );
    }

    /// ...and a clean run reports nothing, so the warn cannot become boilerplate.
    #[test]
    fn a_placed_grant_leaves_no_shortfall() {
        let mut st = BackfillState::new();
        st.record(POT, GrantOutcome::Placed);
        assert!(st.capped_shortfall().is_empty());
        // NotReady is not an attempt at all, so it cannot manufacture one either
        st.record(LANTERN, GrantOutcome::NotReady);
        assert!(st.capped_shortfall().is_empty());
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

    /// A repeated start item (10 Cracked Pots = ten calls in ONE tick) must burn ONE attempt, not
    /// ten. Counting per call gave a 10-copy item less than a single round of retries before it was
    /// declared FAILED. Alaric's 2026-08-01 smoke test came within one outcome of hitting this.
    #[test]
    fn quantity_does_not_burn_the_retry_budget() {
        let mut st = BackfillState::new();
        let bag = snap(&[LANTERN]);
        st.observe(&bag, &[POT; 10]);
        for round in 1..=MAX_ATTEMPTS {
            match st.observe(&bag, &[POT; 10]) {
                ScanVerdict::Grant(fids) => {
                    assert_eq!(
                        fids.len(),
                        10,
                        "all ten copies are still owed on round {round}"
                    );
                    for f in fids {
                        st.record(f, GrantOutcome::Capped); // accepted, never lands
                    }
                }
                other => panic!("round {round} must still be retryable, got {other:?}"),
            }
        }
        assert_eq!(
            st.observe(&bag, &[POT; 10]),
            ScanVerdict::Exhausted(vec![POT]),
            "and only AFTER MAX_ATTEMPTS full rounds is it declared failed"
        );
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
