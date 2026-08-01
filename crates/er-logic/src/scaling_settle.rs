//! When the enemy-scaling sweep is allowed to walk the game's `ChrIns` sets after a map transition.
//!
//! Pure, host-tested, no game and no clock of its own (callers pass `now_ms`). The production
//! caller is `eldenring-archipelago/src/scaling.rs::tick`; it must call THIS, not keep an inline
//! copy (CONTRIBUTING: "a green predicate with no production caller is not a fix -- it is a spec").
//!
//! # What this guards, and what it does NOT
//!
//! Walking a chr set while the game is tearing one map down and streaming the next in can
//! dereference a half-constructed `ChrIns` and crash the game NATIVELY -- no Rust panic, the game's
//! own memory (Siofra / the Eternal Cities, 2026-07-09).
//!
//! THIS GATE IS THE PRIMARY DEFENCE. (An earlier revision of this paragraph said the primary
//! defence was `scaling.rs::active_characters` filtering on `chr_load_status == Active`. That
//! filter shipped on 2026-07-27 and was REVERTED the same day: fightable, loaded enemies are not
//! in state `Active`, so it rejected ~99.5% of entries and no real enemy was ever scaled -- a
//! Mohgwyn/Snowfield start met vanilla endgame stats. The sweep is back to walking every non-null
//! entry; `chr_load_status` is only COUNTED into a histogram now, never filtered on, until live
//! logs say what states enemies actually occupy. See `scaling.rs::sweepable_characters`.)
//!
//! Alongside the timer, [`sweep_blocked_by_death`] closes the one transition edge no timer signal
//! covers from its start: the local player's death (hp <= 0 fires frames before any in-world edge
//! or region change is observable).
//!
//! # The window it replaces
//!
//! The previous guard was one 2500 ms timer, restarted on any observed region change, and BOTH the
//! region observation and the expiry check lived behind the sweep's 30-tick throttle. That produced
//! four terms, only one of which anybody chose:
//!
//! | term | cost @60fps | cost @30fps |
//! |---|---|---|
//! | clock restarted at the first throttled tick, discarding the arrival-edge arm | 0-500 ms | 0-1000 ms |
//! | the 2500 ms constant | 2500 ms | 2500 ms |
//! | expiry only *checked* on a throttled tick | 0-500 ms | 0-1000 ms |
//! | **every transient `play_region_id`: a full restart** | **+2500 ms each** | same |
//!
//! ~3.0 s typical, ~8 s when the region flapped. And because the region was sampled at 500 ms, a
//! flap SHORTER than that was never seen at all -- the guard was simultaneously too slow and too
//! blind. Both faults came from one line ordering.
//!
//! So: observe per tick (strictly MORE protective -- flaps that used to be invisible now hold the
//! gate), and split the single timer in two, because it was conflating two different questions:
//!
//! * `settle_ms` -- how long since a LOAD (warp request / in-world edge). Unchanged at 2500.
//! * `stable_ms` -- how long since the region last CHANGED. A flap now costs `stable_ms`, not a
//!   fresh `settle_ms`, so churn stops compounding.
//!
//! Lowering `settle_ms` is deliberately NOT part of this: it moves the crash boundary, and it should
//! be moved from measured data (`ReleaseDiag`, logged by the caller) rather than from a third
//! feel-based guess after 4000 and 2500.

/// Tunables. Split out so the two questions above can move independently, and so the replay tier can
/// drive the OLD behaviour through the same code path (`legacy_single_timer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettlePolicy {
    /// Minimum ms since the last load/transition signal before the sweep may run.
    pub settle_ms: u64,
    /// Minimum ms since the region last changed. Ignored when `legacy_single_timer`.
    pub stable_ms: u64,
    /// Model the pre-2026-07-27 guard: ONE timer, fully restarted by a region change, `stable_ms`
    /// unused. Only the replay tier sets this -- it is what makes the tests below a
    /// failing-without-the-fix / passing-with-it pair rather than a snapshot of current behaviour.
    pub legacy_single_timer: bool,
}

impl SettlePolicy {
    /// Shipping values. `settle_ms` is the historical constant, deliberately unmoved.
    /// `stable_ms` starts CONSERVATIVE at 1500: per-tick observation already makes it stricter than
    /// the 2500 it replaces (which could miss a flap entirely), and it should only come down on the
    /// evidence in `ReleaseDiag`.
    pub const SHIPPING: Self = Self {
        settle_ms: 2500,
        stable_ms: 1500,
        legacy_single_timer: false,
    };

    /// The guard as it behaved before 2026-07-27. Replay only.
    pub const LEGACY: Self = Self {
        settle_ms: 2500,
        stable_ms: 0,
        legacy_single_timer: true,
    };
}

/// Why the gate released, for the one-shot log the caller emits. Diagnostic only -- but it is the
/// whole point of shipping this: the old guard returned early with NO log on either skip path, so a
/// window that stacked to ~8 s in play was invisible to everything except a human noticing that
/// enemies felt wrong (CONTRIBUTING: "tolerance requires telemetry").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseDiag {
    /// ms from the transition signal to release.
    pub since_transition_ms: u64,
    /// ms from the last region change to release.
    pub since_region_change_ms: u64,
    /// How many distinct region values were observed since the transition. >0 means the region
    /// flapped; under the old guard each of these cost a full `settle_ms`.
    pub flaps: u32,
}

/// Transition/region bookkeeping for the sweep gate.
///
/// `on_region` is intended to be called EVERY tick, before any throttle -- that is half the fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SweepGate {
    transition_at: Option<u64>,
    region: Option<i32>,
    region_changed_at: Option<u64>,
    flaps: u32,
}

impl SweepGate {
    pub const fn new() -> Self {
        Self {
            transition_at: None,
            region: None,
            region_changed_at: None,
            flaps: 0,
        }
    }

    /// A load edge: a warp was requested, or `in_world` went false->true. Restarts `settle_ms` and
    /// resets the flap tally so `ReleaseDiag` describes THIS transition.
    pub fn on_transition(&mut self, now_ms: u64) {
        self.transition_at = Some(now_ms);
        self.flaps = 0;
    }

    /// Observe the current play-region bucket. Call every tick. Returns whether it changed.
    ///
    /// Under `legacy_single_timer` a change restarts the whole settle timer, reproducing the old
    /// `REGION_ENTERED = now()` overwrite -- including the way it discarded the arrival-edge arm.
    pub fn on_region(&mut self, region: i32, now_ms: u64, policy: &SettlePolicy) -> bool {
        if self.region == Some(region) {
            return false;
        }
        let first = self.region.is_none();
        self.region = Some(region);
        self.region_changed_at = Some(now_ms);
        if !first {
            self.flaps = self.flaps.saturating_add(1);
        }
        if policy.legacy_single_timer {
            self.transition_at = Some(now_ms);
        }
        true
    }

    /// May the sweep walk the chr sets now? An unknown term is vacuously satisfied: with no
    /// transition ever signalled there is nothing to wait for.
    pub fn sweep_allowed(&self, now_ms: u64, policy: &SettlePolicy) -> bool {
        let settled = match self.transition_at {
            Some(t) => now_ms.saturating_sub(t) >= policy.settle_ms,
            None => true,
        };
        if policy.legacy_single_timer {
            return settled;
        }
        let stable = match self.region_changed_at {
            Some(t) => now_ms.saturating_sub(t) >= policy.stable_ms,
            None => true,
        };
        settled && stable
    }

    pub fn flaps(&self) -> u32 {
        self.flaps
    }

    pub fn release_diag(&self, now_ms: u64) -> ReleaseDiag {
        ReleaseDiag {
            since_transition_ms: self.transition_at.map_or(0, |t| now_ms.saturating_sub(t)),
            since_region_change_ms: self
                .region_changed_at
                .map_or(0, |t| now_ms.saturating_sub(t)),
            flaps: self.flaps,
        }
    }
}

/// Is the enemy sweep blocked because the LOCAL PLAYER is dead or dying?
///
/// The death-cam transition tears down `chr_ins` + `special_effect` lists, and mutating a
/// SpEffect list mid-teardown is a NATIVE crash. Three client modules have carried an
/// `hp <= 0` guard for exactly this class since it was first observed (`no_fall_damage.rs`,
/// `no_equip_load.rs`, `deathlink.rs`) -- each protecting only the PLAYER's own list. The
/// scaling sweep mutates the same list type on up to ~340 entities per pass and had no such
/// guard: between the death edge (`hp <= 0`, observable immediately) and the respawn's
/// in-world false->true edge (which re-arms the settle gate, but only AFTER teardown has run),
/// the engine begins tearing the world down while the sweep keeps walking and mutating it.
/// Neither the region-stability half nor the transition half of [`SweepGate`] can see this
/// edge: a death in place changes no region and requests no warp.
///
/// Pure predicate so the timeline is host-testable; the production caller is
/// `scaling.rs::tick`, reading hp exactly as DeathLink's `read_local_hp` does (the read the
/// three existing guards already trust). Worst case of a false positive is the documented
/// degrade: enemies keep their current tier for the seconds the player is dead.
///
/// 🛑 Kept as a NAME, not a second implementation: the rule lives once in
/// [`crate::death_guard::lists_unsafe_to_touch`]. Five modules grew private copies of `hp <= 0`
/// before anyone counted them.
pub fn sweep_blocked_by_death(player_hp: i32) -> bool {
    crate::death_guard::lists_unsafe_to_touch(player_hp)
}

#[cfg(test)]
mod replay {
    //! Timeline harness (CONTRIBUTING: "regression by replay -- model the timeline, not a single
    //! tick"). Its own state model over the gate's seam; every test is a
    //! failing-under-`LEGACY` / passing-under-`SHIPPING` pair, and each is named after the BUG
    //! MECHANISM rather than the assertion.

    use super::*;

    const THROTTLE_TICKS: u32 = 30;

    #[test]
    fn a_death_in_place_blocks_the_sweep_until_respawn_replay() {
        // THE TIMELINE the timer gate cannot see: a settled, region-stable player dies in place.
        // No warp request, no region change, no in-world edge yet -- every timer says "sweep".
        // Pre-fix the sweep walks the tearing-down entries and mutates their SpEffect lists;
        // post-fix hp <= 0 blocks it from the FIRST death frame, and respawn (hp > 0 again,
        // plus the in-world edge re-arming the settle gate) restores service.
        let (mut gate, pol) = (SweepGate::new(), SettlePolicy::SHIPPING);
        gate.on_transition(0);
        gate.on_region(1042, 0, &pol);
        let settled = pol.settle_ms.max(pol.stable_ms) + 1;
        assert!(
            gate.sweep_allowed(settled, &pol),
            "settled and stable: the timer gate is fully open"
        );
        let hp_dying = 0;
        assert!(
            sweep_blocked_by_death(hp_dying),
            "the death edge blocks the sweep while every timer says go"
        );
        let hp_respawned = 450;
        assert!(
            !sweep_blocked_by_death(hp_respawned),
            "respawn restores the sweep"
        );
    }

    #[derive(Debug, Clone, Copy)]
    enum Ev {
        /// `lua_warp_detour` / the `in_world` false->true edge.
        Transition,
        /// The play-region bucket the game reports from this point on.
        Region(i32),
        /// Advance the clock by N frames, sweeping wherever the gate allows.
        Frames(u32),
    }

    struct Run {
        /// Absolute ms at which each permitted sweep happened.
        sweeps: Vec<u64>,
    }

    /// Drive the gate over a timeline.
    ///
    /// `per_tick_observation == false` reproduces the OLD ordering, where the 30-tick throttle was
    /// checked BEFORE the region was read, so both the observation and the expiry check happened
    /// only on every 30th frame.
    fn replay(events: &[Ev], policy: SettlePolicy, fps: u64, per_tick_observation: bool) -> Run {
        let frame_ms = 1000 / fps;
        let mut gate = SweepGate::new();
        let mut now: u64 = 0;
        let mut region: i32 = 0;
        let mut tick: u32 = 0;
        let mut last_sweep_tick: Option<u32> = None;
        let mut sweeps = Vec::new();

        for ev in events {
            match *ev {
                Ev::Transition => gate.on_transition(now),
                Ev::Region(r) => region = r,
                Ev::Frames(n) => {
                    for _ in 0..n {
                        let throttled_tick = tick.is_multiple_of(THROTTLE_TICKS);
                        if per_tick_observation {
                            gate.on_region(region, now, &policy);
                            let due = last_sweep_tick
                                .is_none_or(|t| tick.saturating_sub(t) >= THROTTLE_TICKS);
                            if due && gate.sweep_allowed(now, &policy) {
                                sweeps.push(now);
                                last_sweep_tick = Some(tick);
                            }
                        } else if throttled_tick {
                            // OLD: throttle first, so a region change is both observed late and
                            // -- because observing it returns early -- costs an extra throttle
                            // period before the timer even starts running.
                            if !gate.on_region(region, now, &policy)
                                && gate.sweep_allowed(now, &policy)
                            {
                                sweeps.push(now);
                            }
                        }
                        tick += 1;
                        now += frame_ms;
                    }
                }
            }
        }
        Run { sweeps }
    }

    /// A warp, then the region settles immediately. Baseline for the two below.
    fn warp_then_quiet(region: i32, frames: u32) -> Vec<Ev> {
        vec![Ev::Region(region), Ev::Transition, Ev::Frames(frames)]
    }

    #[test]
    fn sweep_released_while_chrins_still_stream_between_throttle_samples() {
        // THE CTD MECHANISM. The region flaps to a transient bucket at ~2400 ms and back well
        // inside one 500 ms throttle period, while the map is still streaming. The old ordering
        // samples the region only every 30 frames, so it never SEES the flap, and its timer expires
        // at 2500 ms -- releasing a chr-set walk mid-stream. Per-tick observation sees it and holds.
        let mut evs = vec![Ev::Region(100), Ev::Transition, Ev::Frames(143)]; // ~2383 ms @60
        evs.push(Ev::Region(999)); // transient bucket, lasts ~100 ms
        evs.push(Ev::Frames(6));
        evs.push(Ev::Region(100));
        evs.push(Ev::Frames(120));

        // The map is still streaming until 3000 ms. The old guard cannot release before ~2880 ms
        // (2500 rounded up to its next 30-tick sample), so this window is what separates
        // "released late by quantization" from "released while the chr set was still being built".
        let streaming_until_ms = 3000;

        let old = replay(&evs, SettlePolicy::LEGACY, 60, false);
        let first_old = old.sweeps.first().copied().expect("old policy never swept");
        assert!(
            first_old < streaming_until_ms,
            "the old guard was expected to release DURING streaming (that is the bug being fixed); \
             it released at {first_old}ms, after streaming ended at {streaming_until_ms}ms. \
             Re-derive this timeline rather than deleting the test."
        );

        let new = replay(&evs, SettlePolicy::SHIPPING, 60, true);
        let first_new = new.sweeps.first().copied().expect("new policy never swept");
        assert!(
            first_new >= streaming_until_ms,
            "per-tick observation released at {first_new}ms, still inside the streaming window \
             ending {streaming_until_ms}ms -- the sub-throttle flap was missed again."
        );
    }

    #[test]
    fn unscaled_window_stacks_a_full_settle_per_region_flap() {
        // THE GAMEPLAY DEFECT (Alaric: "it's actually not impossible to get into a fight within
        // 2.5s of fast travel"). Three transients over ~1.5s after arrival. Under one restarting
        // timer each costs a fresh 2500 ms and the window compounds toward the ~8 s seen in play;
        // splitting settle from stability caps it near settle_ms.
        let mut evs = vec![Ev::Region(100), Ev::Transition];
        for r in [900, 901, 902] {
            evs.push(Ev::Frames(30));
            evs.push(Ev::Region(r));
        }
        evs.push(Ev::Frames(30));
        evs.push(Ev::Region(100));
        evs.push(Ev::Frames(600));

        let old = replay(&evs, SettlePolicy::LEGACY, 60, false);
        let new = replay(&evs, SettlePolicy::SHIPPING, 60, true);
        let (first_old, first_new) = (old.sweeps[0], new.sweeps[0]);

        assert!(
            first_old >= 4000,
            "the old guard was expected to STACK past 4000ms on four region changes; it released \
             at {first_old}ms. The premise of this test has changed -- re-derive it."
        );
        assert!(
            first_new <= 3500,
            "flap handling still compounds: first sweep at {first_new}ms (old: {first_old}ms)"
        );
        // ...and it must still never release within stable_ms of the last change. The last region
        // change is observed at frame 120 (4 x 30 frames @ 16 ms) = 1920 ms.
        let last_change_ms = 1920;
        assert!(
            first_new >= last_change_ms + SettlePolicy::SHIPPING.stable_ms,
            "released {first_new}ms, less than stable_ms ({}) after the last flap at \
             {last_change_ms}ms -- the gate got FASTER by getting less safe, which is not the trade",
            SettlePolicy::SHIPPING.stable_ms
        );
    }

    #[test]
    fn first_sweep_waits_a_whole_throttle_period_after_the_gate_already_opened() {
        // Pure latency, no safety change: the old code checked the throttle BEFORE the settle, so
        // once the window expired the sweep still waited for the next multiple of 30. Worst at low
        // fps -- which is exactly post-load. Asserted at 30fps, where the term is a full second.
        let evs = warp_then_quiet(100, 400);
        let old = replay(&evs, SettlePolicy::LEGACY, 30, false);
        let new = replay(&evs, SettlePolicy::SHIPPING, 30, true);

        let (first_old, first_new) = (old.sweeps[0], new.sweeps[0]);
        // The gate opens at settle_ms = 2500 either way; the only difference is when the sweep
        // NOTICES. Assert the delta rather than two magic constants -- the claim is "a whole
        // throttle period of dead time", and at 30fps that period is ~990ms.
        assert!(
            first_new <= 2600,
            "first sweep should land within a frame of the gate opening (2500ms); got {first_new}ms"
        );
        assert!(
            first_old.saturating_sub(first_new) >= 400,
            "expected the old ordering to lose most of a throttle period at 30fps; old \
             {first_old}ms vs new {first_new}ms is only {}ms",
            first_old.saturating_sub(first_new)
        );
    }

    #[test]
    fn an_unknown_transition_or_region_never_blocks_forever() {
        // Fail OPEN, not closed: a gate that never releases is a feature that silently does
        // nothing, which is the failure mode this project treats as worse than a crash.
        let gate = SweepGate::new();
        assert!(gate.sweep_allowed(0, &SettlePolicy::SHIPPING));
        assert!(gate.sweep_allowed(0, &SettlePolicy::LEGACY));
    }

    #[test]
    fn release_diag_reports_the_flaps_that_drove_the_wait() {
        let p = SettlePolicy::SHIPPING;
        let mut g = SweepGate::new();
        g.on_region(100, 0, &p);
        g.on_transition(10);
        g.on_region(900, 100, &p);
        g.on_region(100, 200, &p);
        let d = g.release_diag(3000);
        assert_eq!(
            d.flaps, 2,
            "two distinct region values after the transition"
        );
        assert_eq!(d.since_transition_ms, 2990);
        assert_eq!(d.since_region_change_ms, 2800);
    }

    #[test]
    fn a_transition_resets_the_flap_tally_so_the_log_describes_one_load() {
        let p = SettlePolicy::SHIPPING;
        let mut g = SweepGate::new();
        g.on_region(100, 0, &p);
        g.on_region(900, 10, &p);
        assert_eq!(g.flaps(), 1);
        g.on_transition(20);
        assert_eq!(g.flaps(), 0, "a new load starts a new tally");
    }
}
