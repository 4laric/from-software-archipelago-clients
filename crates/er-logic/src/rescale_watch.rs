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

/// How many times a stale write may be RE-APPLIED before we stop and say so.
///
/// ⭐ BOUNDED ON PURPOSE. The retry exists because a write to an `unloaded` chr does not recompute
/// (#188: 0/6 followed), and re-applying when it loads is what finishes the job. But an entity that
/// never recomputes however many times we write must not be re-written every sweep forever -- that
/// is churn on a path walking ~340 entities a tick, and it would bury the finding.
///
/// 🛑 EXHAUSTING THE BUDGET ON A **LOADED** CHR IS THE ANSWER TO #186's OPEN QUESTION. If `ready`
/// were sufficient, a loaded retry always takes. `RetriesExhausted { loaded: true }` says it is not
/// -- in one line, from a real session, without anyone designing a second experiment.
pub const MAX_REAPPLY: u32 = 3;

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
    StaleUnloaded { after_ms: u64, unchanged_from: i32 },
    /// Re-applied [`MAX_REAPPLY`] times and `max_hp` still has not followed.
    ///
    /// On an `unloaded` entity this cannot happen (it is never retried). On a LOADED one it is
    /// #186's reading, reached by experiment rather than by comparing two logs.
    RetriesExhausted {
        after_ms: u64,
        loaded: bool,
        unchanged_from: i32,
    },
    /// 🛑 Still on the old number while LOADED, past the grace window. This is the reading #186
    /// saw and #188 could not explain, and it is the one that decides whether `ready` is
    /// sufficient. If this verdict never appears in a playtest, the loaded rule holds and #186 was
    /// a build that has since changed; if it does, `ready` is necessary and not sufficient.
    StaleLoaded { after_ms: u64, unchanged_from: i32 },
}

impl Verdict {
    /// Is this the reading that changes the diagnosis?
    pub fn is_anomaly(&self) -> bool {
        matches!(
            self,
            Verdict::StaleLoaded { .. } | Verdict::RetriesExhausted { loaded: true, .. }
        )
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
    /// How many times this write has been RE-APPLIED (client#188).
    retries: u32,
}

/// What the sweep should do about one watched entity this tick (client#188).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing: not watched, or still inside the grace window.
    Wait,
    /// ⭐ RE-APPLY THE RUNG. The entity is LOADED and its `max_hp` still has not followed, so the
    /// write is unfinished -- re-applying is what makes the engine recompute.
    ///
    /// 🛑 THE SWEEP CANNOT REACH THIS ON ITS OWN. `settled_on_target` returns early for a chr
    /// carrying the target rung and nothing else -- exactly what an unloaded write leaves behind --
    /// so the ordinary path skips it forever. That early return is right for every other purpose;
    /// this is the one case that has to go around it.
    Reapply,
    /// A decidable observation, for the log.
    Report(Verdict),
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
            retries: 0,
        });
    }

    /// Decide this entity's fate: re-apply, report, or wait (client#188).
    ///
    /// # Why the retry is driven from here rather than from the sweep
    ///
    /// The sweep does not know which entities carry an unfinished write -- it sees a chr holding
    /// the target rung and correctly leaves it alone. This watch is the only thing that knows a
    /// write was made and did not take, so it is the only thing that can ask for exactly those
    /// entities to be re-applied. Self-limiting by construction: an entity nobody wrote to is never
    /// returned.
    pub fn poll(&mut self, key: u64, max_hp_now: i32, loaded: bool, now_ms: u64) -> Action {
        let Some(idx) = self.pending.iter().position(|p| p.key == key) else {
            return Action::Wait;
        };
        let p = self.pending[idx];
        let after_ms = now_ms.saturating_sub(p.applied_ms);
        if max_hp_now != p.max_hp_before {
            self.pending.remove(idx);
            return Action::Report(Verdict::Recomputed { after_ms });
        }
        if after_ms < SETTLE_GRACE_MS {
            return Action::Wait; // the engine may simply not have run yet
        }
        // An unloaded chr cannot recompute, so there is nothing to retry -- wait for it to load.
        if !loaded {
            if self.pending[idx].reported {
                return Action::Wait;
            }
            self.pending[idx].reported = true;
            return Action::Report(Verdict::StaleUnloaded {
                after_ms,
                unchanged_from: p.max_hp_before,
            });
        }
        if p.retries >= MAX_REAPPLY {
            if self.pending[idx].reported {
                return Action::Wait;
            }
            self.pending[idx].reported = true;
            return Action::Report(Verdict::RetriesExhausted {
                after_ms,
                loaded,
                unchanged_from: p.max_hp_before,
            });
        }
        self.pending[idx].retries += 1;
        self.pending[idx].applied_ms = now_ms; // the retry restarts the window it is measured over
        Action::Reapply
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
///
/// 🛑 THERE IS NO `expected` ARGUMENT, and there never should have been one. It used to take an
/// `expected: i32` that the single caller passed as a literal `0`, so 13,487 warnings in one session
/// read `(expected 0)` -- a number that looks like a target `max_hp` and is not one. We do not KNOW
/// the target `max_hp`; the engine computes it from the rung. The only thing we know, and the only
/// thing the watch tests, is that it should have CHANGED. So the line says what it is unchanged
/// from.
pub fn verdict_line(npc_param_id: i32, observed: i32, v: Verdict) -> String {
    match v {
        Verdict::Recomputed { after_ms } => format!(
            "enemy-scaling recompute: npc_param {npc_param_id} max_hp followed the rung after \
             {after_ms}ms (now {observed})"
        ),
        Verdict::StaleUnloaded {
            after_ms,
            unchanged_from,
        } => format!(
            "enemy-scaling recompute: npc_param {npc_param_id} still {observed} (unchanged from \
             {unchanged_from}) after {after_ms}ms -- UNLOADED, so the write is not finished. \
             Re-apply when it loads; do not count it as scaled (client#188)"
        ),
        Verdict::RetriesExhausted {
            after_ms,
            loaded,
            unchanged_from,
        } if loaded => format!(
            "enemy-scaling recompute: npc_param {npc_param_id} still {observed} (unchanged from \
             {unchanged_from}) after {MAX_REAPPLY} re-applications and {after_ms}ms while LOADED \
             -- `ready` is NOT sufficient for a recompute, which is #186's reading reproduced by \
             experiment (client#188)"
        ),
        Verdict::RetriesExhausted {
            after_ms,
            unchanged_from,
            ..
        } => format!(
            "enemy-scaling recompute: npc_param {npc_param_id} still {observed} (unchanged from \
             {unchanged_from}) after {after_ms}ms -- unloaded throughout, never retried \
             (client#188)"
        ),
        Verdict::StaleLoaded {
            after_ms,
            unchanged_from,
        } => format!(
            "enemy-scaling recompute: npc_param {npc_param_id} still {observed} (unchanged from \
             {unchanged_from}) after {after_ms}ms while LOADED -- `ready` is NOT sufficient for a \
             recompute. This is the #186 reading, and it decides the cluster (client#188)"
        ),
    }
}

#[cfg(test)]
mod replay {
    use super::*;

    /// ⭐ THE RED-FIRST ASSERTION FOR THE FIX (client#188). The write to the UNLOADED half of the
    /// controlled pair never recomputed -- 0/6 followed. When that chr finally loads, the sweep
    /// skips it (it carries the rung, so `settled_on_target` returns early), so the ONLY thing that
    /// can finish the job is a re-apply, and the watch is the only thing that knows to ask.
    #[test]
    fn a_stale_write_is_re_applied_once_the_chr_loads() {
        let mut w = RescaleWatch::new();
        w.note_applied(UNLOADED_KEY, 504_020_188, STALE_HP, 0);

        // While unloaded: reported once, never retried -- it cannot recompute, so a retry is churn.
        assert_eq!(
            w.poll(UNLOADED_KEY, STALE_HP, false, 2_000),
            Action::Report(Verdict::StaleUnloaded {
                after_ms: 2_000,
                unchanged_from: STALE_HP,
            })
        );
        assert_eq!(w.poll(UNLOADED_KEY, STALE_HP, false, 3_000), Action::Wait);

        // It loads, still stale -> re-apply.
        assert_eq!(w.poll(UNLOADED_KEY, STALE_HP, true, 4_000), Action::Reapply);
    }

    /// And the retry is what proves itself: after the re-apply, a moved `max_hp` retires the write.
    #[test]
    fn a_successful_retry_reports_recomputed_and_retires() {
        let mut w = RescaleWatch::new();
        w.note_applied(1, 1, STALE_HP, 0);
        assert_eq!(w.poll(1, STALE_HP, true, 2_000), Action::Reapply);
        assert_eq!(
            w.poll(1, RECOMPUTED_HP, true, 2_500),
            Action::Report(Verdict::Recomputed { after_ms: 500 }),
            "the window restarts at the retry, so this measures the RETRY's latency"
        );
        assert_eq!(w.outstanding().0, 0);
    }

    /// 🛑 BOUNDED. An entity that never follows must not be re-written every sweep for the session
    /// -- that is churn on a path walking ~340 entities a tick.
    #[test]
    fn retries_are_bounded_and_then_stated() {
        let mut w = RescaleWatch::new();
        w.note_applied(1, 47_500_014, 6080, 0);
        let mut t = 2_000;
        for _ in 0..MAX_REAPPLY {
            assert_eq!(w.poll(1, 6080, true, t), Action::Reapply);
            t += 2_000;
        }
        let a = w.poll(1, 6080, true, t);
        assert_eq!(
            a,
            Action::Report(Verdict::RetriesExhausted {
                after_ms: 2_000,
                loaded: true,
                unchanged_from: 6080
            })
        );
        assert_eq!(w.poll(1, 6080, true, t + 5_000), Action::Wait, "said once");
    }

    /// ⭐ AND THAT EXHAUSTION IS THE ANSWER TO #186's OPEN QUESTION. If `ready` were sufficient a
    /// loaded retry always takes; an exhausted budget on a LOADED chr says it is not, and it is
    /// flagged as the anomaly so it reaches the log as a WARN.
    #[test]
    fn loaded_exhaustion_is_the_anomaly_that_settles_186() {
        let v = Verdict::RetriesExhausted {
            after_ms: 8_000,
            loaded: true,
            unchanged_from: 6080,
        };
        assert!(
            v.is_anomaly(),
            "this is the reading that decides the cluster"
        );
        let line = verdict_line(47_500_014, 6080, v);
        assert!(line.contains("NOT sufficient"), "{line}");
        assert!(line.contains("client#188"), "{line}");

        // The unloaded flavour is ordinary and must NOT be an anomaly.
        assert!(!Verdict::RetriesExhausted {
            after_ms: 8_000,
            loaded: false,
            unchanged_from: 6080
        }
        .is_anomaly());
    }

    /// 🛑 AN ENTITY WE NEVER WROTE TO IS NEVER RE-APPLIED. The retry is self-limiting: the sweep
    /// asks about every character it walks, and only the ones carrying an unfinished write come
    /// back Reapply.
    #[test]
    fn an_unwatched_entity_is_never_re_applied() {
        let mut w = RescaleWatch::new();
        assert_eq!(w.poll(999, 1, true, 60_000), Action::Wait);
    }

    /// Inside the grace window nothing is re-applied -- the engine may simply not have run, and a
    /// retry there would measure our own impatience.
    #[test]
    fn no_retry_inside_the_grace_window() {
        let mut w = RescaleWatch::new();
        w.note_applied(1, 1, STALE_HP, 0);
        assert_eq!(w.poll(1, STALE_HP, true, SETTLE_GRACE_MS - 1), Action::Wait);
        assert_eq!(w.poll(1, STALE_HP, true, SETTLE_GRACE_MS), Action::Reapply);
    }

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
            w.poll(READY_KEY, RECOMPUTED_HP, true, 900),
            Action::Report(Verdict::Recomputed { after_ms: 900 }),
            "the loaded one followed its rung"
        );
        assert_eq!(
            w.poll(UNLOADED_KEY, STALE_HP, false, 1_500),
            Action::Report(Verdict::StaleUnloaded {
                after_ms: 1_500,
                unchanged_from: STALE_HP,
            }),
            "the unloaded one did not, and that is the expected answer -- not a defect"
        );
    }

    /// 🛑 THE ANOMALY THAT DECIDES THE CLUSTER. #186's instance was `ready` and stayed stale; if
    /// that reproduces, `ready` is necessary and not sufficient.
    #[test]
    fn a_loaded_entity_that_never_follows_is_flagged_as_the_anomaly() {
        let mut w = RescaleWatch::new();
        w.note_applied(1, 47_500_014, 6080, 0);
        // Past the grace window and LOADED, the watch now RETRIES rather than merely reporting --
        // the report comes when the budget is spent (`retries_are_bounded_and_then_stated`).
        assert_eq!(w.poll(1, 6080, true, 56_000), Action::Reapply);
        let v = Verdict::StaleLoaded {
            after_ms: 56_000,
            unchanged_from: 6080,
        };
        assert!(v.is_anomaly(), "this is the reading worth a playtest");
        let line = verdict_line(47_500_014, 6080, v);
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
            w.poll(1, RECOMPUTED_HP, true, 16),
            Action::Report(Verdict::Recomputed { after_ms: 16 })
        );
    }

    /// Inside the grace window with nothing moved is DON'T KNOW, not a defect: one census tick is
    /// ~500ms and the engine may simply not have run.
    #[test]
    fn unchanged_inside_the_grace_window_says_nothing() {
        let mut w = RescaleWatch::new();
        w.note_applied(1, 1, STALE_HP, 0);
        assert_eq!(w.poll(1, STALE_HP, true, SETTLE_GRACE_MS - 1), Action::Wait);
        assert_ne!(w.poll(1, STALE_HP, true, SETTLE_GRACE_MS), Action::Wait);
    }

    /// Said once. The sweep re-walks every character every tick; a stale entity must not log at
    /// 2 Hz for the rest of the session.
    #[test]
    fn a_stale_verdict_is_stated_once() {
        let mut w = RescaleWatch::new();
        w.note_applied(1, 1, STALE_HP, 0);
        assert!(matches!(
            w.poll(1, STALE_HP, false, 2_000),
            Action::Report(_)
        ));
        for t in 3..500 {
            assert_eq!(w.poll(1, STALE_HP, false, t * 1_000), Action::Wait);
        }
    }

    /// A recompute RETIRES the entry, so a later sweep of a settled enemy is not a second verdict.
    #[test]
    fn a_recompute_retires_the_write() {
        let mut w = RescaleWatch::new();
        w.note_applied(1, 1, STALE_HP, 0);
        assert!(matches!(
            w.poll(1, RECOMPUTED_HP, true, 500),
            Action::Report(Verdict::Recomputed { .. })
        ));
        assert_eq!(w.poll(1, RECOMPUTED_HP, true, 900), Action::Wait, "retired");
        assert_eq!(w.outstanding().0, 0);
    }

    /// Re-writing an entity restarts its window: the newest write is the one being waited on, and
    /// timing the older one would measure a window that has moved.
    #[test]
    fn a_second_write_restarts_the_window() {
        let mut w = RescaleWatch::new();
        w.note_applied(1, 1, 100, 0);
        assert!(
            w.poll(1, 100, true, 5_000) != Action::Wait,
            "first is decidable"
        );
        w.note_applied(1, 1, 100, 10_000);
        assert_eq!(
            w.poll(1, 100, true, 10_500),
            Action::Wait,
            "the new window has not elapsed"
        );
        assert_eq!(w.outstanding().0, 1, "still one entity, not two");
    }

    /// 🛑 AN ENTITY WE NEVER WROTE TO IS NOT OUR BUSINESS. The sweep samples settled enemies too,
    /// and a verdict on one we did not touch would be a fabricated observation.
    #[test]
    fn an_unwatched_entity_yields_nothing() {
        let mut w = RescaleWatch::new();
        assert_eq!(w.poll(99, 1, true, 10_000), Action::Wait);
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
        assert_eq!(w.poll(1, 2, true, 5_000), Action::Wait);
    }

    /// 🛑 NO LINE MAY SAY `expected 0`.
    ///
    /// `verdict_line` used to take an `expected: i32` that its ONE caller passed as the literal
    /// `0`, so every warning read `(expected 0)` -- a number shaped exactly like a target `max_hp`
    /// and not one. 13,487 of those in a single session. We do not know the target `max_hp` (the
    /// engine derives it from the rung); the only thing the watch tests is that it should have
    /// MOVED, so the line says what it failed to move FROM.
    #[test]
    fn no_verdict_claims_an_expected_value_it_does_not_have() {
        for v in [
            Verdict::StaleUnloaded {
                after_ms: 2_000,
                unchanged_from: 6080,
            },
            Verdict::StaleLoaded {
                after_ms: 56_000,
                unchanged_from: 6080,
            },
            Verdict::RetriesExhausted {
                after_ms: 8_000,
                loaded: true,
                unchanged_from: 6080,
            },
            Verdict::RetriesExhausted {
                after_ms: 8_000,
                loaded: false,
                unchanged_from: 6080,
            },
        ] {
            let line = verdict_line(47_500_014, 6080, v);
            assert!(
                !line.contains("expected"),
                "no invented expectation: {line}"
            );
            assert!(
                line.contains("unchanged from 6080"),
                "a non-move must say what it did not move from: {line}"
            );
        }
    }

    /// The success verdict is the one the census now COUNTS, and it must stay distinguishable from
    /// the failures by pattern rather than by reading its text -- the caller branches on it to
    /// decide between a tally bump and a log line.
    #[test]
    fn a_confirmed_recompute_is_neither_an_anomaly_nor_a_non_move() {
        let v = Verdict::Recomputed { after_ms: 900 };
        assert!(!v.is_anomaly());
        let line = verdict_line(47_500_014, 7141, v);
        assert!(line.contains("followed the rung"), "{line}");
        assert!(!line.contains("unchanged from"), "{line}");
    }

    /// Every line is ASCII (repo rule 10).
    #[test]
    fn the_lines_are_ascii() {
        for v in [
            Verdict::Recomputed { after_ms: 1 },
            Verdict::StaleUnloaded {
                after_ms: 2_000,
                unchanged_from: 6080,
            },
            Verdict::StaleLoaded {
                after_ms: 56_000,
                unchanged_from: 6080,
            },
            Verdict::RetriesExhausted {
                after_ms: 8_000,
                loaded: true,
                unchanged_from: 6080,
            },
            Verdict::RetriesExhausted {
                after_ms: 8_000,
                loaded: false,
                unchanged_from: 6080,
            },
        ] {
            assert!(verdict_line(47_500_014, 6080, v).is_ascii());
        }
    }
}
