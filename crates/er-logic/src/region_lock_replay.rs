//! `region_lock_replay` — headless timeline replay for the region-lock BLOOM reconcile.
//!
//! Twin of [`crate::start_grant_replay`], for the next timing bug. Receiving a `<Region> Lock` item
//! should "bloom" the region — set its warp-unlock GRACES + the region open flag + reveal flags. The
//! Windows bloom pass (`eldenring-ap` `region.rs`, `bloom_regions`) latches on the OPEN FLAG alone:
//! `if flags::get_event_flag(open_flag) { continue; }` (region.rs:143). When a region's front-door
//! grace IS its open flag (Limgrave: 73100 is both), a save / new-game load that drops the interior
//! graces but keeps the persistent front-door flag is NEVER repaired — the pass sees the open flag
//! set, thinks the region is done, and the stranded interior graces stay lost. That is the
//! 2026-07-06 "region-unlock lights no graces" bug (gf-region-grace-loss-frontdoor-latch; Caelid
//! worked because its front-door grace != open flag; Limgrave lost its graces).
//!
//! The fix is the same shape as the Torch clobber: latch on OBSERVED state, not a proxy. A region is
//! bloom-SETTLED only when the open flag AND every grace read back set; until then, re-apply the
//! unset flags each tick (reconcile, don't dispatch). This module lifts that predicate into pure,
//! host-tested code the Windows pass should call, and replays the save-load that strands the graces.

#[cfg(test)]
mod replay {
    use crate::hook::GameHook;
    use crate::region_lock::region_bloom_settled;
    use std::collections::HashMap;

    // Limgrave, faithful to the pinned bug: the front-door grace IS the open flag (73100 = both),
    // plus two interior graces that a save-load strands. (gf-region-grace-loss-frontdoor-latch.)
    const OPEN_FLAG: u32 = 73100;
    const FRONT_DOOR_GRACE: u32 = 73100; // == OPEN_FLAG: this coupling is the whole bug
    const GRACES: [u32; 3] = [FRONT_DOOR_GRACE, 73104, 73105];

    /// A flag-holder game model that can replay a save / new-game load: it drops the volatile
    /// interior graces while the persistent front-door / open flag survives.
    struct RegionGame {
        flags: HashMap<u32, bool>,
        /// Steady-state flag-holder readiness (CSEventFlagMan). When false, `try_set` fails (retry).
        holder_ready: bool,
    }

    impl RegionGame {
        fn new() -> Self {
            RegionGame {
                flags: HashMap::new(),
                holder_ready: true,
            }
        }
        fn is_set(&self, flag: u32) -> bool {
            self.flags.get(&flag).copied().unwrap_or(false)
        }
        /// The save / new-game load lands: interior graces are dropped, the front-door / open flag
        /// (the region-entered flag) persists. This is the clobber that strands the interior graces.
        fn save_load_drops_interior_graces(&mut self) {
            for &g in &GRACES {
                if g != OPEN_FLAG {
                    self.flags.insert(g, false);
                }
            }
        }
    }

    impl GameHook for RegionGame {
        fn get_event_flag(&self, flag: u32) -> bool {
            self.is_set(flag)
        }
        fn set_event_flag(&mut self, flag: u32, on: bool) {
            self.flags.insert(flag, on);
        }
        fn try_set_event_flag(&mut self, flag: u32, on: bool) -> bool {
            if !self.holder_ready {
                return false; // holder not ready -> caller must retry
            }
            self.flags.insert(flag, on);
            true
        }
        fn in_world(&self) -> bool {
            true
        }
        fn play_region_id(&self) -> Option<i32> {
            None
        }
        fn grant_full_id(&mut self, _full_id: i32, _qty: i32) -> bool {
            true
        }
        fn player_hp(&self) -> Option<i32> {
            None
        }
        fn weapon_track_and_cap(&self, _base: i32) -> Option<(i32, bool)> {
            None
        }
        fn highest_held_level(&self, _somber: bool) -> Option<i32> {
            None
        }
        fn scadutree_blessing(&self) -> Option<i32> {
            None
        }
        fn set_scadutree_blessing(&mut self, _level: i32) {}
    }

    /// One `bloom_regions` pass for a single region whose lock has been received.
    /// `latch_on_observed = false` reproduces today's region.rs:143 behavior (skip once the open flag
    /// is set); `true` uses [`region_bloom_settled`] and re-applies any flag not observed-set.
    fn bloom_pass(g: &mut RegionGame, latch_on_observed: bool) {
        let settled = if latch_on_observed {
            region_bloom_settled(OPEN_FLAG, &GRACES, &|f| g.is_set(f))
        } else {
            g.is_set(OPEN_FLAG) // region.rs:143 — the buggy proxy latch
        };
        if settled {
            return;
        }
        // Reconcile: set every grace not observed-set (holder-aware), then the open flag.
        for grace in GRACES {
            if !g.is_set(grace) {
                g.try_set_event_flag(grace, true);
            }
        }
        g.set_event_flag(OPEN_FLAG, true);
    }

    /// One frame of the session timeline.
    #[derive(Clone, Copy)]
    enum Ev {
        /// The `<Region> Lock` item arrives (natural-key trigger fires -> the region wants blooming).
        ReceiveRegionLock,
        /// A `bloom_regions` tick runs.
        Tick,
        /// Save / new-game load: interior graces dropped, front-door / open flag persists.
        SaveLoad,
        /// Flip flag-holder readiness (models CSEventFlagMan not ready right after a load).
        HolderReady(bool),
    }

    fn replay(events: &[Ev], latch_on_observed: bool) -> RegionGame {
        let mut g = RegionGame::new();
        let mut lock_received = false;
        for ev in events {
            match *ev {
                Ev::ReceiveRegionLock => lock_received = true,
                Ev::SaveLoad => g.save_load_drops_interior_graces(),
                Ev::HolderReady(v) => g.holder_ready = v,
                Ev::Tick => {}
            }
            if lock_received {
                bloom_pass(&mut g, latch_on_observed);
            }
        }
        g
    }

    fn all_graces_set(g: &RegionGame) -> bool {
        GRACES.iter().all(|&f| g.is_set(f))
    }

    #[test]
    fn interior_graces_are_stranded_by_the_open_flag_latch() {
        // Pre-fix: bloom sets everything on receipt; the save-load drops the interior graces but the
        // front-door / open flag persists, so the open-flag latch skips forever. Reproduces the bug.
        let timeline = [
            Ev::ReceiveRegionLock, // bloom: all graces + open flag set
            Ev::Tick,
            Ev::SaveLoad, // 73104 / 73105 dropped, 73100 persists
            Ev::Tick,
            Ev::Tick,
        ];
        let g = replay(&timeline, false);
        assert!(
            g.is_set(OPEN_FLAG),
            "the front-door / open flag persists across the load"
        );
        assert!(
            !all_graces_set(&g),
            "regression guard: the open-flag latch strands the interior graces (documents the bug)"
        );
    }

    #[test]
    fn all_graces_recover_when_latching_on_observed_state() {
        // Same timeline, latch on observed state: the post-load bloom sees interior graces unset and
        // re-applies them.
        let timeline = [Ev::ReceiveRegionLock, Ev::Tick, Ev::SaveLoad, Ev::Tick];
        let g = replay(&timeline, true);
        assert!(
            all_graces_set(&g),
            "latching on all-graces-set must recover the stranded interior graces"
        );
    }

    #[test]
    fn reconcile_survives_a_not_ready_flag_holder() {
        // After the load the flag holder is briefly not ready; the reconcile must keep retrying and
        // complete only once it's ready — never dropping a grace on a transient failure.
        // The holder must go down BEFORE the load, so the post-load bloom can't immediately
        // re-heal (the bloom runs every tick and heals the instant it's able to).
        let recovered = replay(
            &[
                Ev::ReceiveRegionLock,
                Ev::HolderReady(false), // holder down first...
                Ev::SaveLoad,           // ...so the post-load bloom can't re-heal yet
                Ev::Tick, // reconcile attempted but holder not ready -> graces stay unset
                Ev::HolderReady(true),
                Ev::Tick, // now it lands
            ],
            true,
        );
        assert!(
            all_graces_set(&recovered),
            "reconcile must recover once the flag holder is ready"
        );

        // And it was genuinely still unset while the holder was down.
        let mid = replay(
            &[
                Ev::ReceiveRegionLock,
                Ev::HolderReady(false),
                Ev::SaveLoad,
                Ev::Tick,
            ],
            true,
        );
        assert!(
            !all_graces_set(&mid),
            "graces must not appear set while the holder is not ready"
        );
    }

    #[test]
    fn pure_settled_semantics() {
        let none = |_f: u32| false;
        let all = |_f: u32| true;
        assert!(!region_bloom_settled(OPEN_FLAG, &GRACES, &none));
        assert!(region_bloom_settled(OPEN_FLAG, &GRACES, &all));
        // Open flag set but an interior grace missing = NOT settled (the exact bug condition).
        let only_open = |f: u32| f == OPEN_FLAG;
        assert!(!region_bloom_settled(OPEN_FLAG, &GRACES, &only_open));
    }
}

// ---------------------------------------------------------------------------------------------
// Countdown-kick replay — headless timeline for the region-gate polish
// ([`crate::region_lock::KickCountdown`]). The hard gate (`kick_decision`) still decides *whether*
// the player is sealed; this machine only decides *when/how* the kick is announced: it warns for a
// grace window (banner "The seal of <Region> repels you... Ns") before teleporting. Time is
// injected (`now_ms`), so the arm/warn/kick/disarm timeline replays deterministically here — no
// real clock. (SPEC-gf-boss-lock-tracker.md "Region-gate polish — the countdown kick".)

#[cfg(test)]
mod countdown_replay {
    use crate::region_lock::{KickAction, KickCountdown, DEFAULT_KICK_GRACE_MS};

    const REGION: &str = "Caelid";
    const LOCK: &str = "Caelid Lock";

    /// One tick of the session timeline: the injected clock (ms) and whether the hard gate
    /// (`kick_decision`) reports the player sealed THIS tick.
    #[derive(Clone, Copy)]
    struct Frame {
        now_ms: u64,
        sealed: bool,
    }

    const fn sealed(now_ms: u64) -> Frame {
        Frame {
            now_ms,
            sealed: true,
        }
    }
    const fn open(now_ms: u64) -> Frame {
        Frame {
            now_ms,
            sealed: false,
        }
    }

    /// Replay a whole timeline through one `KickCountdown`, returning the per-frame actions.
    fn replay(frames: &[Frame]) -> Vec<KickAction> {
        let mut kc = KickCountdown::new();
        frames
            .iter()
            .map(|&f| kc.update(f.now_ms, f.sealed, REGION, LOCK))
            .collect()
    }

    fn secs_of(a: &KickAction) -> Option<u32> {
        match a {
            KickAction::Warn { secs_left, .. } => Some(*secs_left),
            _ => None,
        }
    }

    #[test]
    fn arm_on_enter_warns_not_kicks() {
        // The first sealed tick arms the countdown and WARNS (full grace); it must not kick.
        let out = replay(&[sealed(0)]);
        assert_eq!(
            out[0],
            KickAction::Warn {
                region: REGION.to_string(),
                secs_left: (DEFAULT_KICK_GRACE_MS / 1000) as u32, // 10s at arm
                lock_name: LOCK.to_string(),
            },
            "entering a sealed region warns (not kicks) and names the region + lock"
        );
    }

    #[test]
    fn kick_fires_only_after_grace_elapses() {
        // Warns right up to the boundary, kicks exactly when elapsed >= grace (10_000 ms).
        let out = replay(&[sealed(0), sealed(5_000), sealed(9_999), sealed(10_000)]);
        assert_eq!(secs_of(&out[0]), Some(10));
        assert_eq!(secs_of(&out[1]), Some(5));
        assert_eq!(
            secs_of(&out[2]),
            Some(1),
            "last whole second before the kick still warns"
        );
        assert_eq!(
            out[3],
            KickAction::Kick {
                region: REGION.to_string()
            },
            "the kick fires once the grace has fully elapsed"
        );
    }

    #[test]
    fn leaving_mid_countdown_disarms_and_reentry_restarts_full() {
        // Enter (warn 10s), leave before the grace elapses (None + disarm), then re-enter LATER:
        // the countdown restarts from full — the pre-leave elapsed must NOT carry over.
        let out = replay(&[
            sealed(0),      // arm at t=0, warn 10s
            open(8_000),    // left the region -> disarm, None
            sealed(8_000),  // re-enter -> re-arm at t=8_000, warn 10s again (not a kick)
            sealed(17_999), // 9_999 ms into the SECOND visit -> still warning (1s)
            sealed(18_000), // 10_000 ms into the second visit -> kick
        ]);
        assert_eq!(secs_of(&out[0]), Some(10));
        assert_eq!(out[1], KickAction::None, "leaving disarms the countdown");
        assert_eq!(
            secs_of(&out[2]),
            Some(10),
            "re-entry restarts from full grace (elapsed does not carry across a leave)"
        );
        assert_eq!(secs_of(&out[3]), Some(1));
        assert_eq!(
            out[4],
            KickAction::Kick {
                region: REGION.to_string()
            }
        );
    }

    #[test]
    fn secs_left_counts_down_each_second() {
        // A sealed tick every second yields a strictly decreasing 10..=1 banner countdown.
        let frames: Vec<Frame> = (0..10).map(|s| sealed(s * 1_000)).collect();
        let got: Vec<u32> = replay(&frames)
            .iter()
            .map(|a| secs_of(a).unwrap())
            .collect();
        assert_eq!(got, vec![10, 9, 8, 7, 6, 5, 4, 3, 2, 1]);
    }

    #[test]
    fn region_and_lock_name_propagate_into_the_action_and_banner() {
        let out = replay(&[sealed(0), sealed(10_000)]);
        match &out[0] {
            KickAction::Warn {
                region, lock_name, ..
            } => {
                assert_eq!(region, REGION);
                assert_eq!(
                    lock_name, LOCK,
                    "the missing <Region> Lock name is carried for the banner"
                );
            }
            other => panic!("expected Warn, got {other:?}"),
        }
        assert_eq!(
            out[0].banner().as_deref(),
            Some("The seal of Caelid repels you... 10s"),
            "the warning banner matches the SPEC wording"
        );
        assert_eq!(
            out[1],
            KickAction::Kick {
                region: REGION.to_string()
            }
        );
        assert_eq!(out[1].banner(), None, "a Kick has no warning banner");
    }

    #[test]
    fn no_rekick_spam_while_still_sealed_then_reentry_rewarns() {
        // After the kick fires, staying reported-sealed must stay QUIET (no per-tick re-kick). Once
        // the player actually leaves and re-enters, the countdown re-arms and re-warns — kicks are
        // throttled per visit but never permanently suppressed.
        let out = replay(&[
            sealed(0),      // warn
            sealed(10_000), // kick
            sealed(10_016), // still sealed right after -> quiet (no re-kick)
            sealed(20_000), // still sealed much later -> still quiet
            open(21_000),   // finally left -> disarm
            sealed(21_000), // re-enter -> re-warn from full
        ]);
        assert!(matches!(out[0], KickAction::Warn { .. }));
        assert_eq!(
            out[1],
            KickAction::Kick {
                region: REGION.to_string()
            }
        );
        assert_eq!(
            out[2],
            KickAction::None,
            "no re-kick while still reported sealed"
        );
        assert_eq!(out[3], KickAction::None, "still no re-kick");
        assert_eq!(
            out[4],
            KickAction::None,
            "leaving is a no-op action but disarms"
        );
        assert_eq!(
            secs_of(&out[5]),
            Some(10),
            "re-entry re-warns from full (not suppressed)"
        );
    }

    #[test]
    fn clock_reset_backwards_does_not_fire_a_spurious_kick() {
        // Arm at t=0, run 9s into the window (1s left), then a load resets the injected clock
        // backwards. `saturating_sub` means the backwards `now_ms` only re-lengthens the current
        // window; it must never elapse the grace early.
        let out = replay(&[sealed(0), sealed(9_000), sealed(0)]);
        assert_eq!(secs_of(&out[0]), Some(10), "arm on enter -> full window");
        assert_eq!(secs_of(&out[1]), Some(1), "9s in -> 1s left");
        assert_eq!(
            secs_of(&out[2]),
            Some(10),
            "a backwards clock re-warns full, never kicks early"
        );
    }

    #[test]
    fn custom_grace_window_is_honoured() {
        // The grace is configurable; a 3s window kicks at t=3_000.
        let mut kc = KickCountdown::with_grace_ms(3_000);
        assert_eq!(kc.grace_ms(), 3_000);
        assert_eq!(secs_of(&kc.update(0, true, REGION, LOCK)), Some(3));
        assert_eq!(secs_of(&kc.update(2_999, true, REGION, LOCK)), Some(1));
        assert_eq!(
            kc.update(3_000, true, REGION, LOCK),
            KickAction::Kick {
                region: REGION.to_string()
            }
        );
    }
}
