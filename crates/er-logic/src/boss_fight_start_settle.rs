//! `boss_fight_start_settle` — hold `boss-fight START` until the region settles, and keep the
//! pre-settle read as its own marked line (client#195).
//!
//! # The reading this exists to reject
//!
//! Alaric's `archipelago-2026-08-13.log`, build `cf1f111305b7`. Royal Knight Loretta,
//! `npc_param 32520921`:
//!
//! ```text
//! 00:56:55  boss-fight START:  t=0.0s ... region 6200007 boss 4214/4214 (100%)
//! 00:56:55  boss-fight START carried: ... speffects [7060, 7460]
//! 00:56:56  boss-fight SAMPLE: t=1.0s ... region 6200010 boss 2122/2122 (100%)
//! ...       (~40 further samples, every one /2122)
//! ```
//!
//! `START` fires on the healthbar edge, inside the fog-load frame, before `play_region` resolves --
//! so **region, `max_hp` and the carried speffect list are all read one frame early, all three at
//! once.** The settled number is exact: vanilla `NpcParam.hp` 32520921 = 1860, x 1.141 (`7010`,
//! tier 0/19) = 2122.3 -> 2122. The START number, 4214, is 2.266x base and lands on no tier in the
//! band. And `7060` / `7460` appear in no `-> speffect` line anywhere in that log -- the only rungs
//! the session ever applied were `7010` and `7030`. A carried list citing an unissued row is the
//! tell.
//!
//! # Why the early read is KEPT rather than discarded
//!
//! ⭐ THE ISSUE ASKS FOR THIS EXPLICITLY AND THE REASON IS GOOD. `4214/2122 = 1.9859` and #189's
//! `2199/1099.6 = 1.99985` are both very nearly 2x, and that ratio is itself a reading about WHEN
//! `max_hp` recomputes after a `maxHpRate` speffect moves -- the open question in #188 and #183.
//! Gating the line silently would throw away the evidence that made three issues legible. So the
//! pre-settle read is emitted, marked, and never becomes the headline number.
//!
//! # Why this reuses the census's gate, minus one term
//!
//! [`crate::scaling_settle::SweepGate`] already observes region flaps per tick and is host-tested;
//! #195's own fix note is that "the enemy-scaling census already solved this for itself and the
//! fight probe does not use it". This uses it, with [`START_POLICY`].
//!
//! 🛑 `settle_ms` IS ZERO HERE, ON PURPOSE. In the census that term is a CRASH GUARD: walking chr
//! sets during a map teardown dereferences half-constructed `ChrIns` and takes the game down
//! natively. The boss-fight probe is read-only over the same walk and its module docs already claim
//! that exemption in writing ("Read-only. No param writes, no game state, no flags"). Borrowing the
//! crash term would delay every START by 2.5s to guard against a hazard this caller does not face.
//! What #195 is about is the REGION being stale, and that is `stable_ms`.

use crate::scaling_settle::{SettlePolicy, SweepGate};

/// How long `play_region` must hold one value before `START` is believed.
///
/// The census's `stable_ms`, unchanged and for the same reason: per-tick observation already makes
/// it stricter than the 2500 it replaced, and it should come down on measured `ReleaseDiag` data
/// rather than on a third feel-based guess. In the Loretta trace the region resolved inside one
/// second, so this is not the binding constraint on a normal fog-gate entry.
pub const REGION_STABLE_MS: u64 = 1_500;

/// Region-stability only. See the module docs for why the crash term is dropped.
pub const START_POLICY: SettlePolicy = SettlePolicy {
    settle_ms: 0,
    stable_ms: REGION_STABLE_MS,
    legacy_single_timer: false,
};

/// What the probe should write this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartEmit {
    /// The pre-settle read, marked as raw. Emitted once, at the healthbar edge.
    Raw,
    /// The settled read. THIS is the fight's `START`, and the line every downstream number is
    /// allowed to come from.
    Settled,
    /// Still settling, or already done. Nothing to write.
    Hold,
}

/// Holds `START` until `play_region` has stopped moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartGate {
    gate: SweepGate,
    raw_done: bool,
    settled_done: bool,
}

impl Default for StartGate {
    fn default() -> Self {
        Self::new()
    }
}

impl StartGate {
    pub const fn new() -> Self {
        Self {
            gate: SweepGate::new(),
            raw_done: false,
            settled_done: false,
        }
    }

    /// A boss healthbar just came up. Re-arms from scratch: a re-fight must not inherit the
    /// previous fight's settled region, which is exactly the state a second attempt at the same
    /// boss arrives in.
    pub fn open(&mut self, now_ms: u64) {
        *self = Self::new();
        self.gate.on_transition(now_ms);
    }

    /// Has the settled `START` been written? Until it has, no SAMPLE should be.
    pub fn settled(&self) -> bool {
        self.settled_done
    }

    /// Observe this tick's region and decide what to write.
    ///
    /// 🛑 `None` HOLDS. An unreadable region is don't-know, and [`crate::boss_fight_sample`]'s own
    /// doctrine is that don't-know is never an answer -- releasing on it would reintroduce exactly
    /// the stale read this module exists to stop, on the one tick most likely to be mid-load.
    pub fn poll(&mut self, now_ms: u64, region: Option<i32>) -> StartEmit {
        if !self.raw_done {
            self.raw_done = true;
            if let Some(r) = region {
                self.gate.on_region(r, now_ms, &START_POLICY);
            }
            return StartEmit::Raw;
        }
        if self.settled_done {
            return StartEmit::Hold;
        }
        let Some(r) = region else {
            return StartEmit::Hold;
        };
        self.gate.on_region(r, now_ms, &START_POLICY);
        if self.gate.sweep_allowed(now_ms, &START_POLICY) {
            self.settled_done = true;
            return StartEmit::Settled;
        }
        StartEmit::Hold
    }

    /// How many distinct region values were seen while settling. Rides on the settled line so a
    /// fog-gate entry that flapped is visible rather than inferred.
    pub fn flaps(&self) -> u32 {
        self.gate.flaps()
    }
}

#[cfg(test)]
mod replay {
    use super::*;

    /// Loretta's third attempt, verbatim: the fog-load frame reads `6200007`, and the region
    /// resolves to `6200010` by the first sample a second later.
    const PRE_SETTLE_REGION: i32 = 6_200_007;
    const SETTLED_REGION: i32 = 6_200_010;

    /// Drive the gate at ~60fps from `from` to `to`, collecting everything it asked to write.
    fn run(gate: &mut StartGate, from: u64, to: u64, region: Option<i32>) -> Vec<(u64, StartEmit)> {
        let mut out = Vec::new();
        let mut t = from;
        while t <= to {
            match gate.poll(t, region) {
                StartEmit::Hold => {}
                e => out.push((t, e)),
            }
            t += 16;
        }
        out
    }

    /// ⭐ THE ACCEPTANCE TEST FROM THE ISSUE. The fight's `START` must carry the SETTLED region,
    /// and the pre-settle read must be reachable only as `Raw`.
    #[test]
    fn the_loretta_trace_reports_the_settled_region_as_start() {
        let mut gate = StartGate::new();
        gate.open(0);

        // The fog-load frame: region reads 6200007.
        let edge = run(&mut gate, 0, 0, Some(PRE_SETTLE_REGION));
        assert_eq!(
            edge,
            vec![(0, StartEmit::Raw)],
            "the healthbar edge writes the raw read and nothing else"
        );

        // It resolves ~1s later and then holds.
        let settle = run(&mut gate, 16, 1_000, Some(PRE_SETTLE_REGION));
        assert!(
            settle.is_empty(),
            "START must not be written while the region is still moving: {settle:?}"
        );
        let after = run(&mut gate, 1_008, 4_000, Some(SETTLED_REGION));
        assert_eq!(
            after.len(),
            1,
            "exactly one settled START per fight: {after:?}"
        );
        let (t, e) = after[0];
        assert_eq!(e, StartEmit::Settled);
        assert!(
            t >= 1_008 + REGION_STABLE_MS,
            "the settled line waits for the region to hold, got t={t}"
        );
        assert!(gate.settled());
        assert_eq!(gate.flaps(), 1, "6200007 -> 6200010 is one flap");
    }

    /// 🛑 NO SAMPLE BEFORE THE SETTLED START. `settled()` is what the probe gates SAMPLE on, so a
    /// fight cannot report readings under a headline that has not been written yet.
    #[test]
    fn nothing_is_settled_until_the_region_holds() {
        let mut gate = StartGate::new();
        gate.open(0);
        gate.poll(0, Some(PRE_SETTLE_REGION));
        assert!(!gate.settled());
        run(&mut gate, 16, REGION_STABLE_MS - 1, Some(SETTLED_REGION));
        assert!(!gate.settled(), "one frame short is not settled");
    }

    /// An unreadable region holds forever rather than releasing on don't-know -- the tick most
    /// likely to read `None` is the mid-load tick this module exists to distrust.
    #[test]
    fn an_unreadable_region_holds() {
        let mut gate = StartGate::new();
        gate.open(0);
        assert_eq!(
            gate.poll(0, None),
            StartEmit::Raw,
            "the raw line still goes"
        );
        let out = run(&mut gate, 16, 60_000, None);
        assert!(out.is_empty(), "don't-know is never a release: {out:?}");
        assert!(!gate.settled());
    }

    /// A region that keeps flapping keeps the gate shut, and the flap count rides on the line so a
    /// slow fog-gate entry is visible rather than inferred.
    #[test]
    fn a_flapping_region_holds_and_is_counted() {
        let mut gate = StartGate::new();
        gate.open(0);
        gate.poll(0, Some(PRE_SETTLE_REGION));
        let mut t = 16;
        for i in 0..6 {
            // Flap every 500ms: never stable for REGION_STABLE_MS.
            let r = if i % 2 == 0 {
                SETTLED_REGION
            } else {
                PRE_SETTLE_REGION
            };
            let out = run(&mut gate, t, t + 480, Some(r));
            assert!(out.is_empty(), "a flapping region cannot settle: {out:?}");
            t += 496;
        }
        assert!(!gate.settled());
        assert!(
            gate.flaps() >= 6,
            "every flap is counted, got {}",
            gate.flaps()
        );
    }

    /// 🛑 A RE-FIGHT RE-ARMS. bobler fought the same boss twice in four minutes; the second
    /// attempt arrives with the region already settled from the first, and must still take its own
    /// raw read rather than inheriting one.
    #[test]
    fn a_refight_takes_its_own_reading() {
        let mut gate = StartGate::new();
        gate.open(0);
        run(&mut gate, 0, 10_000, Some(SETTLED_REGION));
        assert!(gate.settled());

        gate.open(200_000);
        assert!(!gate.settled(), "a new fight has written no START yet");
        assert_eq!(gate.poll(200_000, Some(PRE_SETTLE_REGION)), StartEmit::Raw);
        assert_eq!(gate.flaps(), 0, "the flap tally describes THIS fight");
    }

    /// Exactly one raw and one settled line per fight, however long it runs.
    #[test]
    fn one_raw_and_one_settled_per_fight() {
        let mut gate = StartGate::new();
        gate.open(0);
        let all = run(&mut gate, 0, 120_000, Some(SETTLED_REGION));
        let raws = all.iter().filter(|(_, e)| *e == StartEmit::Raw).count();
        let settleds = all.iter().filter(|(_, e)| *e == StartEmit::Settled).count();
        assert_eq!((raws, settleds), (1, 1), "{all:?}");
    }
}
