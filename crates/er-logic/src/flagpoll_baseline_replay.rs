//! `flagpoll_baseline_replay` — headless timeline replay for the flag-poll CONNECT BASELINE.
//!
//! Sibling of [`crate::start_grant_replay`] / [`crate::region_lock_replay`], for the next timing
//! bug. The client's flag-poll fires a location CHECK whenever a watched acquisition flag reads
//! SET — i.e. on a LEVEL, not a TRANSITION. On a FRESH save some acquisition flags are already set
//! by default (Flask of Crimson Tears = 60000, Wondrous Physick = 60020, the Third-Church Sacred
//! Tears), so the very first poll after connect sees them set and silently auto-checks those
//! locations — false positives the player never earned — and the false checks then feed the
//! vanilla suppressor, leaking vanilla items. That is the 2026-07-06
//! gf-flagpoll-newsave-default-flags bug (same class as FLAG_POLL_FALSE_POSITIVES).
//!
//! The fix (`patch_flag_poll_baseline.py`) is the same shape as the other replay tiers: decide on
//! OBSERVED TRANSITIONS, not raw levels. At connect, snapshot every watched flag that is already
//! set as a BASELINE; a poll may only fire a check for a flag that transitions unset -> set
//! RELATIVE TO that baseline. Flags set at baseline are inert forever (for this session) — a
//! genuinely-earned flag set after connect still fires. This module lifts the two pure decisions
//! ([`snapshot_baseline`], [`newly_set_since_baseline`]) into host-tested code the Windows poll
//! should call, and replays the fresh-save connect that produced the false checks.

use std::collections::HashSet;

/// At CONNECT: snapshot every watched flag that already reads set. These are the fresh-save /
/// pre-session defaults the poll must never convert into checks. Pure home for the connect-time
/// half of `patch_flag_poll_baseline.py`.
pub fn snapshot_baseline(watched: &[u32], get_flag: &dyn Fn(u32) -> bool) -> HashSet<u32> {
    watched.iter().copied().filter(|&f| get_flag(f)).collect()
}

/// At each POLL: the watched flags that read set NOW but were NOT set at the connect baseline —
/// i.e. genuine unset -> set transitions relative to the baseline. Only these may fire checks.
/// Preserves `watched` order. Pure home for the poll-time half of `patch_flag_poll_baseline.py`.
pub fn newly_set_since_baseline(
    watched: &[u32],
    baseline: &HashSet<u32>,
    get_flag: &dyn Fn(u32) -> bool,
) -> Vec<u32> {
    watched
        .iter()
        .copied()
        .filter(|&f| get_flag(f) && !baseline.contains(&f))
        .collect()
}

/// At CONNECT, choose the baseline the poll measures transitions against. If the save carries a
/// PERSISTED baseline (captured once, at the first fresh-save connect), reuse it verbatim; only a
/// save with NO persisted baseline (a genuinely fresh session) snapshots the currently-set flags.
/// This is the reconnect fix: re-snapshotting at every connect folds mid-playthrough pickups into
/// the baseline, making their checks inert forever ("picked it up, got nothing"). Persisting the
/// baseline once keeps post-baseline pickups firing across reconnects. Pure home for the
/// connect-time half of `patch_flagpoll_baseline_persist.py`.
pub fn effective_baseline(
    persisted: Option<&HashSet<u32>>,
    watched: &[u32],
    get_flag: &dyn Fn(u32) -> bool,
) -> HashSet<u32> {
    match persisted {
        Some(b) => b.clone(),
        None => snapshot_baseline(watched, get_flag),
    }
}

#[cfg(test)]
mod replay {
    use super::*;
    use crate::hook::GameHook;
    use std::collections::HashMap;

    // Faithful to the pinned bug: these acquisition flags are ALREADY SET on a fresh save.
    const FLASK_OF_CRIMSON_TEARS: u32 = 60000;
    const WONDROUS_PHYSICK: u32 = 60020;
    /// Illustrative watched flag for a check the player genuinely earns AFTER connect. The live
    /// client resolves real ids from the detection table; the harness only needs a stable token.
    const GENUINE_PICKUP: u32 = 65_100;

    /// Everything the poll watches in this scenario (fresh-save defaults + one genuine location).
    const WATCHED: [u32; 3] = [FLASK_OF_CRIMSON_TEARS, WONDROUS_PHYSICK, GENUINE_PICKUP];

    /// A flag-holder game model whose fresh save comes up with default acquisition flags already
    /// set — the state the false-positive bug actually lives in.
    struct FreshSaveGame {
        flags: HashMap<u32, bool>,
    }

    impl FreshSaveGame {
        fn new() -> Self {
            FreshSaveGame {
                flags: HashMap::new(),
            }
        }
        fn is_set(&self, flag: u32) -> bool {
            self.flags.get(&flag).copied().unwrap_or(false)
        }
        /// The fresh save finishes loading: the game itself sets the default acquisition flags
        /// (starting flask + physick) before the player has picked up anything.
        fn fresh_save_defaults(&mut self) {
            self.flags.insert(FLASK_OF_CRIMSON_TEARS, true);
            self.flags.insert(WONDROUS_PHYSICK, true);
        }
    }

    impl GameHook for FreshSaveGame {
        fn get_event_flag(&self, flag: u32) -> bool {
            self.is_set(flag)
        }
        fn set_event_flag(&mut self, flag: u32, on: bool) {
            self.flags.insert(flag, on);
        }
        fn try_set_event_flag(&mut self, flag: u32, on: bool) -> bool {
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
        /// The fresh save finishes loading -> default acquisition flags (60000/60020) come up SET.
        FreshSaveDefaults,
        /// The AP session connects. The fixed client snapshots the baseline HERE.
        Connect,
        /// The game sets a watched flag (a genuine pickup / acquisition after connect).
        SetFlag(u32),
        /// A flag-poll tick runs; any check it decides to fire is recorded.
        Poll,
    }

    /// Replay a timeline, recording every location-check the poll fires (as watched flag ids, in
    /// fire order). `use_baseline = false` reproduces today's level-triggered poll (fires on any
    /// watched flag reading set); `true` snapshots a baseline at Connect and fires only on
    /// [`newly_set_since_baseline`]. Both sides dedup already-fired checks, mirroring the live
    /// client's checked-set — the bug is the FIRST fire, not a repeat.
    fn replay(events: &[Ev], use_baseline: bool) -> Vec<u32> {
        let mut g = FreshSaveGame::new();
        let mut connected = false;
        let mut baseline: HashSet<u32> = HashSet::new();
        let mut fired: Vec<u32> = Vec::new();
        for &ev in events {
            match ev {
                Ev::FreshSaveDefaults => g.fresh_save_defaults(),
                Ev::Connect => {
                    connected = true;
                    if use_baseline {
                        baseline = snapshot_baseline(&WATCHED, &|f| g.get_event_flag(f));
                    }
                }
                Ev::SetFlag(flag) => g.set_event_flag(flag, true),
                Ev::Poll => {
                    if !connected {
                        continue; // the poll only runs on a connected session
                    }
                    let candidates: Vec<u32> = if use_baseline {
                        newly_set_since_baseline(&WATCHED, &baseline, &|f| g.get_event_flag(f))
                    } else {
                        // Today's poll: raw level check — set == check (the bug).
                        WATCHED
                            .iter()
                            .copied()
                            .filter(|&f| g.get_event_flag(f))
                            .collect()
                    };
                    for f in candidates {
                        if !fired.contains(&f) {
                            fired.push(f);
                        }
                    }
                }
            }
        }
        fired
    }

    #[test]
    fn fresh_save_defaults_falsely_check_without_baseline() {
        // Pre-fix: the first poll after connect sees the fresh-save default flags SET and fires
        // their checks — silent false positives. Reproduces the 2026-07-06 bug.
        let timeline = [Ev::FreshSaveDefaults, Ev::Connect, Ev::Poll];
        let fired = replay(&timeline, false);
        assert!(
            fired.contains(&FLASK_OF_CRIMSON_TEARS) && fired.contains(&WONDROUS_PHYSICK),
            "regression guard: without the baseline the fresh-save defaults auto-check \
             (documents the bug), fired={fired:?}"
        );
    }

    #[test]
    fn baseline_snapshot_suppresses_fresh_save_defaults() {
        // Fixed: the Connect snapshot captures 60000/60020 as baseline; polls — first and later —
        // must never fire them.
        let timeline = [
            Ev::FreshSaveDefaults,
            Ev::Connect,
            Ev::Poll,
            Ev::Poll,
            Ev::Poll,
        ];
        let fired = replay(&timeline, true);
        assert!(
            fired.is_empty(),
            "flags set at baseline must never fire a check, fired={fired:?}"
        );
    }

    #[test]
    fn genuine_transition_still_fires() {
        // The fix must not over-suppress: a watched flag set AFTER the baseline is a genuine
        // unset -> set transition and must fire — exactly once — while the baseline flags stay
        // inert on every poll around it.
        let timeline = [
            Ev::FreshSaveDefaults,
            Ev::Connect,
            Ev::Poll,                    // nothing new yet
            Ev::SetFlag(GENUINE_PICKUP), // the player genuinely earns a check
            Ev::Poll,                    // fires it
            Ev::Poll,                    // checked-set dedup: no repeat
        ];
        let fired = replay(&timeline, true);
        assert_eq!(
            fired,
            vec![GENUINE_PICKUP],
            "a post-baseline transition must fire exactly once, and only it"
        );
    }

    #[test]
    fn pure_baseline_semantics() {
        // snapshot_baseline captures exactly the watched flags that read set at connect.
        let at_connect = |f: u32| f == FLASK_OF_CRIMSON_TEARS || f == WONDROUS_PHYSICK;
        let baseline = snapshot_baseline(&WATCHED, &at_connect);
        assert!(baseline.contains(&FLASK_OF_CRIMSON_TEARS));
        assert!(baseline.contains(&WONDROUS_PHYSICK));
        assert!(!baseline.contains(&GENUINE_PICKUP));

        // newly_set_since_baseline: set-now && !in-baseline, watched order preserved.
        let now_all_set = |_f: u32| true;
        assert_eq!(
            newly_set_since_baseline(&WATCHED, &baseline, &now_all_set),
            vec![GENUINE_PICKUP],
            "baseline flags are inert even while they still read set"
        );

        // A flag neither set nor in the baseline stays absent; unwatched flags are never reported.
        let nothing_set = |_f: u32| false;
        assert!(newly_set_since_baseline(&WATCHED, &baseline, &nothing_set).is_empty());

        // Empty baseline degrades to the raw level check — every set watched flag reports.
        let empty: HashSet<u32> = HashSet::new();
        assert_eq!(
            newly_set_since_baseline(&WATCHED, &empty, &now_all_set),
            WATCHED.to_vec()
        );
    }

    #[test]
    fn persisted_baseline_survives_a_reconnect() {
        // The Sacred Tear reconnect bug (gf-flagpoll-newsave-default-flags / "picked it up, got
        // nothing"): a mid-playthrough reconnect RE-SNAPSHOTS the baseline. Because the player has
        // since earned pickups, those now-set flags fold into the fresh baseline and their checks go
        // inert forever. The fix persists the fresh-save baseline ONCE (SaveState.flag_poll_baseline)
        // and reuses it on every reconnect via `effective_baseline`, so a post-baseline pickup still
        // fires after the reconnect. Models the exact fresh-connect -> pickup -> reconnect timeline
        // as a regression pair.
        let mut g = FreshSaveGame::new();
        g.fresh_save_defaults(); // 60000 / 60020 up on the fresh save

        // FIRST connect (no persisted baseline yet): snapshot the fresh-save defaults; this is what
        // the fixed client persists into SaveState.flag_poll_baseline.
        let persisted = effective_baseline(None, &WATCHED, &|f| g.get_event_flag(f));
        assert!(
            persisted.contains(&FLASK_OF_CRIMSON_TEARS) && persisted.contains(&WONDROUS_PHYSICK),
            "the fresh-save baseline captures the default acquisition flags"
        );

        // The player genuinely earns a check mid-session (the Church of Pilgrimage Sacred Tear).
        g.set_event_flag(GENUINE_PICKUP, true);

        // RECONNECT, buggy path (persisted = None -> re-snapshot): the mid-session pickup is now set,
        // so it folds into the baseline and can NEVER fire again — reproduces "got nothing".
        let rebaseline_buggy = effective_baseline(None, &WATCHED, &|f| g.get_event_flag(f));
        assert!(
            newly_set_since_baseline(&WATCHED, &rebaseline_buggy, &|f| g.get_event_flag(f))
                .is_empty(),
            "regression guard: re-snapshotting at reconnect strands the mid-session pickup (the bug)"
        );

        // RECONNECT, fixed path: reuse the PERSISTED fresh-save baseline. The pickup is a genuine
        // post-baseline transition, so it still fires after the reconnect.
        let rebaseline_fixed =
            effective_baseline(Some(&persisted), &WATCHED, &|f| g.get_event_flag(f));
        assert_eq!(
            newly_set_since_baseline(&WATCHED, &rebaseline_fixed, &|f| g.get_event_flag(f)),
            vec![GENUINE_PICKUP],
            "the persisted baseline lets a post-baseline pickup fire across a reconnect"
        );
    }
}
