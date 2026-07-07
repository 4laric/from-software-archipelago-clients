//! `grace_flush_replay` — headless timeline replay for the grace-flush SESSION-SET reconcile.
//!
//! Twin of [`crate::region_lock_replay`] (the closest cousin — both are grace reconcile), for the
//! flaw baked into [`crate::grace::flush_grace_flags`]. That drain keeps a per-session "already set"
//! `HashSet`: once a grace is inserted into `session`, every later flush SKIPS it forever
//! (`grace.rs:17` — `if session.contains(&flag) { continue; }`). The set is fire-and-forget — it
//! records "we set this once", NOT "the flag is currently set in-game". So if a save / new-game load
//! DROPS a grace's flag, the flush will not re-set it: the session set still says "done", the queue
//! entry is skipped, and the grace is stranded lost.
//!
//! This is the same class as the region-lock front-door latch (gf-region-grace-loss-frontdoor-latch)
//! and the bundle-lock grace-reconcile gap (er-bundle-lock-grace-reconcile-gap): the reconcile keys
//! on a session-local "already did it" proxy instead of the OBSERVED flag. The fix is the same shape
//! as the region-bloom settle predicate: latch on read-back state — a grace set is SETTLED only once
//! its flag reads back set, and any grace that reads back false must be re-queued each tick.
//!
//! A session-set strand is invisible to slot_data / shape validation AND to the single-tick
//! `FakeGame` (which never replays a later save-load drop). This module models the game-state
//! TIMELINE — queue graces, a save-load that drops one, holder-not-ready windows — so the fix is
//! provable offline, on any host, and stays a regression. It drives the REAL `flush_grace_flags`
//! (with its persistent session set) on the pre-fix path to reproduce the strand; the only new
//! production logic is the pure [`grace_flush_settled`] gate the Windows reconcile should latch on.

/// A grace set is SETTLED only when EVERY grace flag reads back set. Mirrors
/// [`crate::region_lock_replay::region_bloom_settled`]. Replaces `flush_grace_flags`'s session-set
/// skip (`grace.rs:17`), which records "set once" rather than "currently set" and so strands a grace
/// dropped by a save / new-game load. The reconcile should re-queue any grace this reports unset.
pub fn grace_flush_settled(graces: &[u32], get_flag: &dyn Fn(u32) -> bool) -> bool {
    graces.iter().all(|&g| get_flag(g))
}

#[cfg(test)]
mod replay {
    use super::*;
    use crate::grace::flush_grace_flags;
    use crate::hook::GameHook;
    use std::collections::{HashMap, HashSet, VecDeque};

    // A grace set faithful to the strand: one persistent grace and one that a save / new-game load
    // drops. Real grace flags live in the 76xxx / 73xxx bands (see grace.rs tests + region_lock).
    const PERSISTENT_GRACE: u32 = 76971;
    const VOLATILE_GRACE: u32 = 76972;
    const GRACES: [u32; 2] = [PERSISTENT_GRACE, VOLATILE_GRACE];

    /// A flag-holder game model that can replay a save / new-game load: it drops the volatile grace
    /// while the persistent grace survives — the drop the session set is blind to.
    struct GraceGame {
        flags: HashMap<u32, bool>,
        /// Steady-state flag-holder readiness (CSEventFlagMan). When false, `try_set` fails (retry).
        holder_ready: bool,
    }

    impl GraceGame {
        fn new() -> Self {
            GraceGame {
                flags: HashMap::new(),
                holder_ready: true,
            }
        }
        fn is_set(&self, flag: u32) -> bool {
            self.flags.get(&flag).copied().unwrap_or(false)
        }
        /// The save / new-game load lands: the volatile grace's flag is dropped, the persistent
        /// grace survives. This is the drop the fire-and-forget session set never notices.
        fn save_load_drops(&mut self, flag: u32) {
            self.flags.insert(flag, false);
        }
    }

    impl GameHook for GraceGame {
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

    /// One frame of the session timeline.
    #[derive(Clone, Copy)]
    enum Ev {
        /// The grace set arrives (a region-lock / bundle-lock item queues its warp-unlock graces).
        QueueGraces,
        /// A grace-flush tick runs.
        Tick,
        /// Save / new-game load: the volatile grace's flag is dropped, the persistent one survives.
        SaveLoad,
        /// Flip flag-holder readiness (models CSEventFlagMan not ready right after a load).
        HolderReady(bool),
    }

    /// Replay a timeline, running a grace-flush each tick as the live client does.
    ///
    /// `reconcile_on_observed = false` drives the REAL [`flush_grace_flags`] with a PERSISTENT
    /// `session` set (the live module-static), reproducing the strand: once a grace is in `session`
    /// it is skipped forever, so the save-load drop is never repaired. `true` reconciles against
    /// OBSERVED flags instead — re-queueing (via a fresh session set) any grace whose flag reads back
    /// false — so a dropped grace is re-set. Returns the final game.
    fn replay(events: &[Ev], reconcile_on_observed: bool) -> GraceGame {
        let mut g = GraceGame::new();
        // The persistent session set the live flush_grace_flags keeps as a module static — this
        // persistence across ticks is exactly what strands a dropped grace on the pre-fix path.
        let mut session: HashSet<u32> = HashSet::new();
        let mut queue: VecDeque<u32> = VecDeque::new();
        let mut queued = false;
        for ev in events {
            match *ev {
                Ev::QueueGraces => {
                    queue = GRACES.into_iter().collect();
                    queued = true;
                }
                Ev::SaveLoad => g.save_load_drops(VOLATILE_GRACE),
                Ev::HolderReady(v) => g.holder_ready = v,
                Ev::Tick => {}
            }
            if !queued {
                continue;
            }
            if reconcile_on_observed {
                // THE FIX: latch on read-back state. Re-queue every grace not observed-set, drop the
                // stale session set (start fresh each reconcile) so a dropped grace is re-attempted.
                if !grace_flush_settled(&GRACES, &|f| g.is_set(f)) {
                    for grace in GRACES {
                        if !g.is_set(grace) {
                            queue.push_back(grace);
                        }
                    }
                    let mut fresh = HashSet::new();
                    flush_grace_flags(&mut g, &mut queue, &mut fresh);
                }
            } else {
                // PRE-FIX: the real flush with its persistent session set — a grace already in
                // `session` is skipped forever, even after the save-load drops its flag.
                flush_grace_flags(&mut g, &mut queue, &mut session);
            }
        }
        g
    }

    fn all_graces_set(g: &GraceGame) -> bool {
        GRACES.iter().all(|&f| g.is_set(f))
    }

    #[test]
    fn session_set_strands_grace_after_save_load() {
        // Pre-fix: the first flush sets both graces and marks them in `session`; the save-load drops
        // the volatile grace, but every later flush skips it (session says "done"). Strand. This
        // drives the REAL flush_grace_flags with a persistent session set. Documents the bug.
        let timeline = [
            Ev::QueueGraces, // flush sets both graces, both enter the session set
            Ev::Tick,
            Ev::SaveLoad, // 76972 dropped in-game; session set still says "done"
            Ev::Tick,
            Ev::Tick,
        ];
        let g = replay(&timeline, false);
        assert!(
            g.is_set(PERSISTENT_GRACE),
            "the persistent grace survives the load"
        );
        assert!(
            !all_graces_set(&g),
            "regression guard: the fire-and-forget session set strands the dropped grace (the bug)"
        );
    }

    #[test]
    fn observed_reconcile_recovers_dropped_grace() {
        // Same timeline, reconcile on observed flags: the post-load flush reads the volatile grace
        // back false and re-queues + re-sets it. Recovers.
        let timeline = [
            Ev::QueueGraces,
            Ev::Tick,
            Ev::SaveLoad,
            Ev::Tick,
        ];
        let g = replay(&timeline, true);
        assert!(
            all_graces_set(&g),
            "reconciling on observed flag state must recover the stranded grace"
        );
    }

    #[test]
    fn reconcile_survives_a_not_ready_flag_holder() {
        // After the load the flag holder is briefly not ready; the reconcile must keep retrying and
        // complete only once it's ready — never dropping the re-queued grace on a transient failure.
        // The holder must go down BEFORE the load, so the post-load reconcile can't immediately
        // re-heal (the flush runs every tick and heals the instant it's able to).
        let recovered = replay(
            &[
                Ev::QueueGraces,
                Ev::HolderReady(false), // holder down first...
                Ev::SaveLoad,           // ...so the post-load reconcile can't re-heal yet
                Ev::Tick,               // reconcile re-queues but holder not ready -> stays unset
                Ev::HolderReady(true),
                Ev::Tick,               // now it lands
            ],
            true,
        );
        assert!(
            all_graces_set(&recovered),
            "reconcile must recover once the flag holder is ready"
        );

        // And it was genuinely still unset while the holder was down.
        let mid = replay(
            &[Ev::QueueGraces, Ev::HolderReady(false), Ev::SaveLoad, Ev::Tick],
            true,
        );
        assert!(
            !all_graces_set(&mid),
            "the dropped grace must not appear set while the holder is not ready"
        );
    }

    #[test]
    fn pure_settled_semantics() {
        let none = |_f: u32| false;
        let all = |_f: u32| true;
        assert!(!grace_flush_settled(&GRACES, &none));
        assert!(grace_flush_settled(&GRACES, &all));
        // The persistent grace set but the volatile one missing = NOT settled (the exact strand).
        let only_persistent = |f: u32| f == PERSISTENT_GRACE;
        assert!(!grace_flush_settled(&GRACES, &only_persistent));
    }
}
