//! Did the rung we wrote actually move `max_hp`? (client#188, #186, #183)
//!
//! # One mechanism, four issues
//!
//! The scaling cluster is a single question wearing four hats. Applying a rung is two separate
//! events -- **we write a speffect**, and **the engine recomputes `max_hp`** -- and the census has
//! only ever observed the first:
//!
//! * **#186** -- `npc_param 47500014` read `3826` to the fight probe and `6080` to the census.
//!   Settled: both were right. `7040` was cleared and `7010` applied, and `max_hp` stayed on the
//!   OLD tier's number. Two instruments, two moments, one un-recomputed enemy.
//! * **#188** -- the controlled pair. `504020088` (`ready`) followed the rung to `2212`;
//!   `504020188` (`unloaded`), same base, same scan, same write, kept `15209`. 7/7 loaded followed,
//!   0/6 unloaded did.
//! * **#189** -- CLOSED by the params, not by this: `34600913` carries `spEffectID28 = 4410`
//!   (`maxHpRate 2.0`) natively, so its true vanilla base is 1328 and the "2x" was never ours.
//!   Kept in this list because it is the control -- it proves the ladder and the census arithmetic
//!   are sound, which is what makes the other three readings trustworthy.
//! * **#183** -- the down-palette rows DO read on the player, and the RESTORE is unwitnessed. Same
//!   shape: we watch the write and not the effect.
//!
//! # 🛑 THE OPEN CONTRADICTION THIS MODULE EXISTS TO SETTLE
//!
//! #188's own note flags it: the controlled pair says `ready` recomputes, but **#186's data has two
//! `ready` samples that stayed stale for 56 seconds**. Different builds, so either the behaviour
//! changed, or -- and this is the live possibility -- **`ready` is necessary but not sufficient**,
//! and those two were sampled inside a recompute window nobody has measured.
//!
//! "Stale for one tick" and "stale indefinitely" are the same reading today. They are opposite
//! diagnoses: the first is a sampling artifact and needs no fix, the second is enemies standing at
//! the wrong HP for the rest of the session. [`RescaleWatch`] separates them by remembering what
//! was written and re-reading it on a schedule.
//!
//! # And the count has been over-reporting the whole time
//!
//! `(re)scaled 163 enemy(ies)` counts every write, including the ones to `unloaded` characters that
//! changed nothing -- #188 says so outright. That is the same defect this client has now had four
//! times over (`readback STUCK`, `0/4 re-keyed`, `sweep-flush 19/14`, `trap spawn 3 x c4150`): a
//! summary reporting the REQUEST rather than the RESULT. [`Verdict`] is the result.

/// How long a write may go unrecomputed before it stops being a sampling artifact.
///
/// One census tick is ~500 ms and #188's loaded pair recomputed within the same second, so a
/// second is generous for "the engine simply had not run yet". Past this a `ready` character that
/// has not followed its rung is the #186 shape and is worth a line.
pub const SETTLE_GRACE_MS: u64 = 1_000;

/// How long before a still-stale write is called permanent rather than slow. #186's readings held
/// for 56 s; 30 s is comfortably past any recompute window and still inside a single fight.
pub const STALE_VERDICT_MS: u64 = 30_000;

/// What happened to one write, once we looked again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// `max_hp` moved to the expected value. The write worked.
    Recomputed { after_ms: u64 },
    /// Still on the old number, and the character is `unloaded`. **This is #188's answer and it is
    /// EXPECTED** -- an unloaded chr accepts the speffect and does not recompute. It is not a
    /// defect in the write; it is a write that is not finished, and the fix is to re-apply when it
    /// loads rather than to count it done.
    StaleUnloaded { after_ms: u64 },
    /// 🛑 Still on the old number while LOADED, past the grace window. This is the reading #186
    /// saw and #188 could not explain, and it is the one that decides whether `ready` is
    /// sufficient. If this verdict never appears in a playtest, the loaded rule holds and #186 was
    /// a build that has since changed; if it does, `ready` is necessary and not sufficient.
    StaleLoaded { after_ms: u64 },
}

impl Verdict {
    /// Is this the reading that changes the diagnosis?
    pub fn is_anomaly(&self) -> bool {
        matches!(self, Verdict::StaleLoaded { .. })
    }
}

/// One outstanding write: what we applied, to whom, and what `max_hp` read at the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pending {
    /// Instance identity. `npc_param_id` alone is NOT an instance -- #186's row had ten sightings
    /// of what may or may not have been one character, and that ambiguity is half why it took two
    /// issues to settle.
    key: u64,
    npc_param_id: i32,
    /// `max_hp` as it read at the moment of the write.
    max_hp_before: i32,
    applied_ms: u64,
    /// Already reported, so a long-lived stale entry says its line once rather than per tick.
    reported: bool,
}

/// Remembers rung writes and re-reads them, so "we wrote it" stops standing in for "it took".
///
/// Fixed capacity: a sweep can touch ~340 entities and this must not grow without bound on a path
/// that already walks every character every tick. Oldest entries are dropped, and the drop is
/// COUNTED rather than silent -- an instrument that quietly forgets is how #186 happened.
#[derive(Debug, Clone, Default)]
pub struct RescaleWatch {
    pending: Vec<Pending>,
    dropped: u32,
}

/// Most writes tracked at once.
pub const WATCH_CAP: usize = 512;

impl RescaleWatch {
    pub const fn new() -> Self {
        Self {
            pending: Vec::new(),
            dropped: 0,
        }
    }

    /// Record that a rung was written to `key`, whose `max_hp` read `max_hp_before` at the time.
    ///
    /// Re-writing an entity already pending REPLACES its entry: the newest write is the one whose
    /// effect we are waiting on, and keeping the older one would time a window that has moved.
    pub fn note_applied(&mut self, key: u64, npc_param_id: i32, max_hp_before: i32, now_ms: u64) {
        if let Some(p) = self.pending.iter_mut().find(|p| p.key == key) {
            p.npc_param_id = npc_param_id;
            p.max_hp_before = max_hp_before;
            p.applied_ms = now_ms;
            p.reported = false;
            return;
        }
        if self.pending.len() >= WATCH_CAP {
            self.pending.remove(0);
            self.dropped = self.dropped.saturating_add(1);
        }
        self.pending.push(Pending {
            key,
            npc_param_id,
            max_hp_before,
            applied_ms: now_ms,
            reported: false,
        });
    }

    /// Re-read an entity we are waiting on. `Some(verdict)` the first time it is decidable.
    ///
    /// 🛑 A CHANGED `max_hp` IS THE ONLY PROOF OF A RECOMPUTE, and it is checked before the clock:
    /// an entity that moved inside the grace window is still a success, not an unknown.
    pub fn observe(
        &mut self,
        key: u64,
        max_hp_now: i32,
        loaded: bool,
        now_ms: u64,
    ) -> Option<Verdict> {
        let idx = self.pending.iter().position(|p| p.key == key)?;
        let p = self.pending[idx];
        let after_ms = now_ms.saturating_sub(p.applied_ms);
        if max_hp_now != p.max_hp_before {
            self.pending.remove(idx);
            return Some(Verdict::Recomputed { after_ms });
        }
        if after_ms < SETTLE_GRACE_MS {
            return None; // the engine may simply not have run yet
        }
        if self.pending[idx].reported {
            return None; // said once
        }
        self.pending[idx].reported = true;
        Some(if loaded {
            Verdict::StaleLoaded { after_ms }
        } else {
            Verdict::StaleUnloaded { after_ms }
        })
    }

    /// Entities still waiting, and writes forgotten to the cap.
    pub fn outstanding(&self) -> (usize, u32) {
        (self.pending.len(), self.dropped)
    }

    /// Forget everything. Called on the world edge: a load re-derives every character, so a write
    /// from the previous world is not something the next one owes.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Writes that have been waiting longer than [`STALE_VERDICT_MS`] -- the population that is
    /// stale INDEFINITELY rather than for a tick, which is the distinction #188 asks for.
    pub fn long_stale(&self, now_ms: u64) -> Vec<i32> {
        self.pending
            .iter()
            .filter(|p| now_ms.saturating_sub(p.applied_ms) >= STALE_VERDICT_MS)
            .map(|p| p.npc_param_id)
            .collect()
    }
}

/// The line one decidable verdict logs. ASCII (repo rule 10).
pub fn verdict_line(npc_param_id: i32, expected: i32, observed: i32, v: Verdict) -> String {
    match v {
        Verdict::Recomputed { after_ms } => format!(
            "enemy-scaling recompute: npc_param {npc_param_id} max_hp followed the rung after \
             {after_ms}ms (now {observed})"
        ),
        Verdict::StaleUnloaded { after_ms } => format!(
            "enemy-scaling recompute: npc_param {npc_param_id} still {observed} after {after_ms}ms \
             -- UNLOADED, so the write is not finished (expected {expected}). Re-apply when it \
             loads; do not count it as scaled (client#188)"
        ),
        Verdict::StaleLoaded { after_ms } => format!(
            "enemy-scaling recompute: npc_param {npc_param_id} still {observed} after {after_ms}ms \
             while LOADED (expected {expected}) -- `ready` is NOT sufficient for a recompute. This \
             is the #186 reading, and it decides the cluster (client#188)"
        ),
    }
}

#[cfg(test)]
mod replay {
    use super::*;

    /// #188's controlled pair: same base 1939, same scan, same write, differing only in load
    /// status. `504020088` was `ready` and followed to 2212; `504020188` was `unloaded` and kept
    /// 15209.
    const READY_KEY: u64 = 504_020_088;
    const UNLOADED_KEY: u64 = 504_020_188;
    const STALE_HP: i32 = 15209;
    const RECOMPUTED_HP: i32 = 2212;

    /// ⭐ THE RED-FIRST ASSERTION. The two halves of #188's pair must reach OPPOSITE verdicts from
    /// the same write -- which is exactly what `(re)scaled 163` cannot say.
    #[test]
    fn the_controlled_pair_reaches_opposite_verdicts() {
        let mut w = RescaleWatch::new();
        w.note_applied(READY_KEY, 504_020_088, STALE_HP, 0);
        w.note_applied(UNLOADED_KEY, 504_020_188, STALE_HP, 0);

        assert_eq!(
            w.observe(READY_KEY, RECOMPUTED_HP, true, 900),
            Some(Verdict::Recomputed { after_ms: 900 }),
            "the loaded one followed its rung"
        );
        assert_eq!(
            w.observe(UNLOADED_KEY, STALE_HP, false, 1_500),
            Some(Verdict::StaleUnloaded { after_ms: 1_500 }),
            "the unloaded one did not, and that is the expected answer -- not a defect"
        );
    }

    /// 🛑 THE ANOMALY THAT DECIDES THE CLUSTER. #186's instance was `ready` and stayed stale; if
    /// that reproduces, `ready` is necessary and not sufficient.
    #[test]
    fn a_loaded_entity_that_never_follows_is_flagged_as_the_anomaly() {
        let mut w = RescaleWatch::new();
        w.note_applied(1, 47_500_014, 6080, 0);
        let v = w
            .observe(1, 6080, true, 56_000)
            .expect("past the grace window this is decidable");
        assert_eq!(v, Verdict::StaleLoaded { after_ms: 56_000 });
        assert!(v.is_anomaly(), "this is the reading worth a playtest");
        let line = verdict_line(47_500_014, 3826, 6080, v);
        assert!(line.contains("NOT sufficient"), "{line}");
        assert!(line.contains("client#188"), "{line}");
    }

    /// 🛑 A CHANGED VALUE BEATS THE CLOCK. An entity that recomputes inside the grace window is a
    /// success, not an unknown -- checking the clock first would report a defect on a working write.
    #[test]
    fn recompute_inside_the_grace_window_is_still_a_success() {
        let mut w = RescaleWatch::new();
        w.note_applied(1, 504_020_088, STALE_HP, 0);
        assert_eq!(
            w.observe(1, RECOMPUTED_HP, true, 16),
            Some(Verdict::Recomputed { after_ms: 16 })
        );
    }

    /// Inside the grace window with nothing moved is DON'T KNOW, not a defect: one census tick is
    /// ~500ms and the engine may simply not have run.
    #[test]
    fn unchanged_inside_the_grace_window_says_nothing() {
        let mut w = RescaleWatch::new();
        w.note_applied(1, 1, STALE_HP, 0);
        assert_eq!(w.observe(1, STALE_HP, true, SETTLE_GRACE_MS - 1), None);
        assert!(w.observe(1, STALE_HP, true, SETTLE_GRACE_MS).is_some());
    }

    /// Said once. The sweep re-walks every character every tick; a stale entity must not log at
    /// 2 Hz for the rest of the session.
    #[test]
    fn a_stale_verdict_is_stated_once() {
        let mut w = RescaleWatch::new();
        w.note_applied(1, 1, STALE_HP, 0);
        assert!(w.observe(1, STALE_HP, false, 2_000).is_some());
        for t in 3..500 {
            assert_eq!(w.observe(1, STALE_HP, false, t * 1_000), None);
        }
    }

    /// A recompute RETIRES the entry, so a later sweep of a settled enemy is not a second verdict.
    #[test]
    fn a_recompute_retires_the_write() {
        let mut w = RescaleWatch::new();
        w.note_applied(1, 1, STALE_HP, 0);
        assert!(w.observe(1, RECOMPUTED_HP, true, 500).is_some());
        assert_eq!(w.observe(1, RECOMPUTED_HP, true, 900), None, "retired");
        assert_eq!(w.outstanding().0, 0);
    }

    /// Re-writing an entity restarts its window: the newest write is the one being waited on, and
    /// timing the older one would measure a window that has moved.
    #[test]
    fn a_second_write_restarts_the_window() {
        let mut w = RescaleWatch::new();
        w.note_applied(1, 1, 100, 0);
        assert!(
            w.observe(1, 100, true, 5_000).is_some(),
            "first is decidable"
        );
        w.note_applied(1, 1, 100, 10_000);
        assert_eq!(
            w.observe(1, 100, true, 10_500),
            None,
            "the new window has not elapsed"
        );
        assert_eq!(w.outstanding().0, 1, "still one entity, not two");
    }

    /// 🛑 AN ENTITY WE NEVER WROTE TO IS NOT OUR BUSINESS. The sweep samples settled enemies too,
    /// and a verdict on one we did not touch would be a fabricated observation.
    #[test]
    fn an_unwatched_entity_yields_nothing() {
        let mut w = RescaleWatch::new();
        assert_eq!(w.observe(99, 1, true, 10_000), None);
    }

    /// The cap holds and the drop is COUNTED. An instrument that quietly forgets is how #186
    /// happened in the first place.
    #[test]
    fn the_cap_holds_and_drops_are_counted() {
        let mut w = RescaleWatch::new();
        for k in 0..(WATCH_CAP as u64 + 10) {
            w.note_applied(k, k as i32, 1, 0);
        }
        let (outstanding, dropped) = w.outstanding();
        assert_eq!(outstanding, WATCH_CAP);
        assert_eq!(dropped, 10, "forgetting is stated, never silent");
    }

    /// `long_stale` is the "indefinitely, not for a tick" population #188 asks to separate.
    #[test]
    fn long_stale_separates_indefinite_from_transient() {
        let mut w = RescaleWatch::new();
        w.note_applied(1, 111, 1, 0);
        w.note_applied(2, 222, 1, STALE_VERDICT_MS);
        assert_eq!(
            w.long_stale(STALE_VERDICT_MS),
            vec![111],
            "only the old one"
        );
        assert_eq!(w.long_stale(0), Vec::<i32>::new());
    }

    /// A world edge re-derives every character, so writes from the previous world are not owed.
    #[test]
    fn the_world_edge_clears_the_watch() {
        let mut w = RescaleWatch::new();
        w.note_applied(1, 1, 1, 0);
        w.reset();
        assert_eq!(w.outstanding(), (0, 0));
        assert_eq!(w.observe(1, 2, true, 5_000), None);
    }

    /// Every line is ASCII (repo rule 10).
    #[test]
    fn the_lines_are_ascii() {
        for v in [
            Verdict::Recomputed { after_ms: 1 },
            Verdict::StaleUnloaded { after_ms: 2_000 },
            Verdict::StaleLoaded { after_ms: 56_000 },
        ] {
            assert!(verdict_line(47_500_014, 3826, 6080, v).is_ascii());
        }
    }
}
