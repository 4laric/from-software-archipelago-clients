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

/// A region's bloom is SETTLED only when the open flag AND every warp-unlock grace read back set.
/// Replaces the Windows bloom pass's `get_event_flag(open_flag)` skip-latch (region.rs:143), which
/// conflates "front door open" with "all graces applied" and strands interior graces after a
/// save-load. See the module docs.
pub fn region_bloom_settled(open_flag: u32, graces: &[u32], get_flag: &dyn Fn(u32) -> bool) -> bool {
    get_flag(open_flag) && graces.iter().all(|&g| get_flag(g))
}

#[cfg(test)]
mod replay {
    use super::*;
    use crate::hook::GameHook;
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
        assert!(g.is_set(OPEN_FLAG), "the front-door / open flag persists across the load");
        assert!(
            !all_graces_set(&g),
            "regression guard: the open-flag latch strands the interior graces (documents the bug)"
        );
    }

    #[test]
    fn all_graces_recover_when_latching_on_observed_state() {
        // Same timeline, latch on observed state: the post-load bloom sees interior graces unset and
        // re-applies them.
        let timeline = [
            Ev::ReceiveRegionLock,
            Ev::Tick,
            Ev::SaveLoad,
            Ev::Tick,
        ];
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
                Ev::Tick,               // reconcile attempted but holder not ready -> graces stay unset
                Ev::HolderReady(true),
                Ev::Tick,               // now it lands
            ],
            true,
        );
        assert!(all_graces_set(&recovered), "reconcile must recover once the flag holder is ready");

        // And it was genuinely still unset while the holder was down.
        let mid = replay(
            &[Ev::ReceiveRegionLock, Ev::HolderReady(false), Ev::SaveLoad, Ev::Tick],
            true,
        );
        assert!(!all_graces_set(&mid), "graces must not appear set while the holder is not ready");
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
