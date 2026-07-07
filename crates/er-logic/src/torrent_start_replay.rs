//! `torrent_start_replay` — headless timeline replay for the TORRENT (mount) grant on a
//! region-lock / rolled (num_regions) start.
//!
//! Sibling of [`crate::start_grant_replay`] and [`crate::region_lock_replay`], for the next
//! start-sequencing bug. On a NORMAL (vanilla) start the player receives Torrent + the Spectral
//! Steed Whistle from the Melina hand-off at the first grace (the vanilla EMEVD reactor). On a
//! ROLLED / region-lock start (`randomStartAreaId` / num_regions) the player is warped out of the
//! tutorial and past the Chapel-of-Anticipation Melina scene, so that hand-off NEVER fires. When
//! the spectral steed BELL / whistle is ALSO randomized, `torrent_start=auto` (which normally
//! leans on the vanilla hand-off) misses it too — the player ends up with NO MOUNT and can't
//! summon Torrent. That is the 2026-07-05 "Torrent mountless on region-lock starts" bug
//! (er-torrent-regionlock-mountless; Pacificator66: "Melina pan, no Melina, no whistle").
//!
//! The fix is the same shape as the Torch clobber and the front-door grace latch: don't rely on a
//! bypassed vanilla path — on a region-lock / rolled start, GRANT the mount (Torrent full id)
//! directly, independent of the Melina hand-off. This module lifts the decision into pure,
//! host-tested code (`should_force_grant_torrent`) and replays the start timeline so the fix is
//! provable offline and stays a regression guard.

/// Torrent (the spectral steed) as a start-item FullID: `0x40000000 | 130` (the mount tag the
/// standalone `start_items` drain grants — see `eldenring-ap` `startgrants.rs`). The harness only
/// needs a stable token to track through the timeline.
pub const TORRENT_FULL_ID: i32 = 0x4000_0000 | 130;

/// Pure decision: should the client FORCE-grant the mount directly on this start?
///
/// A region-lock / rolled start warps the player past the Chapel-of-Anticipation Melina scene, so
/// the Melina hand-off that normally delivers Torrent (and, on a normal start, the Spectral Steed
/// Whistle) never fires. Force-grant exactly when this is a region-lock start AND the Melina
/// hand-off will NOT deliver the mount on its own (bell randomized / scene bypassed). On a normal
/// start the vanilla hand-off delivers it, so we must NOT force-grant (that would double-grant).
pub fn should_force_grant_torrent(
    is_region_lock_start: bool,
    melina_handoff_will_deliver: bool,
) -> bool {
    is_region_lock_start && !melina_handoff_will_deliver
}

#[cfg(test)]
mod replay {
    use super::*;
    use crate::hook::GameHook;
    use std::collections::HashMap;

    /// The Chapel-of-Anticipation grace flag whose reactor runs the Melina hand-off (illustrative;
    /// the real flag lives in the baked EMEVD). On a normal start the game sets it and the reactor
    /// delivers Torrent; a region-lock warp skips the scene so it never sets.
    const MELINA_HANDOFF_FLAG: u32 = 73100;

    /// A game model that records granted items + set flags and can model the Melina hand-off
    /// delivering Torrent on a NORMAL start (and its absence on a bypassed region-lock start).
    struct TorrentGame {
        flags: HashMap<u32, bool>,
        in_world: bool,
        /// Everything granted so far (mount + anything else), in order: (full_id, qty).
        granted: Vec<(i32, i32)>,
    }

    impl TorrentGame {
        fn new() -> Self {
            TorrentGame {
                flags: HashMap::new(),
                in_world: true,
                granted: Vec::new(),
            }
        }
        fn is_set(&self, flag: u32) -> bool {
            self.flags.get(&flag).copied().unwrap_or(false)
        }
        /// True once the mount (Torrent) has been granted through any path.
        fn holds_mount(&self) -> bool {
            self.granted.iter().any(|&(id, _)| id == TORRENT_FULL_ID)
        }
        /// How many times the mount was granted (to catch double-grants).
        fn mount_grant_count(&self) -> usize {
            self.granted.iter().filter(|&&(id, _)| id == TORRENT_FULL_ID).count()
        }
        /// The vanilla Melina hand-off reactor: sets its flag and delivers Torrent. Only runs on a
        /// NORMAL start (the region-lock warp skips the Chapel scene entirely).
        fn run_melina_handoff(&mut self) {
            self.flags.insert(MELINA_HANDOFF_FLAG, true);
            self.granted.push((TORRENT_FULL_ID, 1));
        }
    }

    impl GameHook for TorrentGame {
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
            self.in_world
        }
        fn play_region_id(&self) -> Option<i32> {
            None
        }
        fn grant_full_id(&mut self, full_id: i32, qty: i32) -> bool {
            self.granted.push((full_id, qty));
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

    /// One frame of the start timeline.
    #[derive(Clone, Copy)]
    enum Ev {
        /// A rolled / num_regions start: the player is warped past the Chapel Melina scene. The
        /// bell/whistle is randomized, so the vanilla hand-off will NOT deliver the mount.
        RegionLockStart,
        /// A vanilla start: the Chapel Melina scene plays and its reactor will deliver the mount.
        NormalStart,
        /// The Melina hand-off reactor fires (delivers Torrent). Only has an effect after a
        /// NormalStart — on a region-lock start the scene was skipped, so it's a no-op.
        MelinaHandoff,
        /// A client update tick (the force-grant is attempted here, as `update_live` would).
        Tick,
    }

    /// Replay a start timeline. `force_grant = false` reproduces today's pre-fix behavior (the
    /// mount arrives ONLY if the Melina hand-off delivers it); `true` applies the fix: on a
    /// region-lock start where the hand-off won't deliver, force-grant the mount directly (once).
    fn replay(events: &[Ev], force_grant: bool) -> TorrentGame {
        let mut g = TorrentGame::new();
        let mut is_region_lock_start = false;
        // On a region-lock start the Chapel scene is bypassed, so the hand-off never delivers.
        // On a normal start it does. This mirrors the live `torrent_start=auto` assumption.
        let mut melina_will_deliver = false;
        let mut forced = false; // once-only latch for the direct grant
        for &ev in events {
            match ev {
                Ev::RegionLockStart => {
                    is_region_lock_start = true;
                    melina_will_deliver = false; // scene skipped + bell randomized
                }
                Ev::NormalStart => {
                    is_region_lock_start = false;
                    melina_will_deliver = true;
                }
                Ev::MelinaHandoff => {
                    // The reactor only runs when the scene actually played (normal start).
                    if melina_will_deliver {
                        g.run_melina_handoff();
                    }
                }
                Ev::Tick => {}
            }
            if force_grant
                && !forced
                && should_force_grant_torrent(is_region_lock_start, melina_will_deliver)
            {
                g.grant_full_id(TORRENT_FULL_ID, 1);
                forced = true;
            }
        }
        g
    }

    #[test]
    fn no_mount_on_regionlock_start_relying_on_melina() {
        // Pre-fix: a rolled / region-lock start warps past the Chapel scene and the bell is
        // randomized, so the Melina hand-off never delivers Torrent. Relying on it alone leaves
        // the player with no mount. Reproduces er-torrent-regionlock-mountless.
        let timeline = [
            Ev::RegionLockStart,
            Ev::MelinaHandoff, // no-op: scene was skipped
            Ev::Tick,
            Ev::Tick,
        ];
        let g = replay(&timeline, false);
        assert!(
            !g.holds_mount(),
            "regression guard: on a region-lock start the bypassed Melina hand-off never delivers \
             the mount (documents the bug)"
        );
    }

    #[test]
    fn mount_granted_on_regionlock_start_when_forced() {
        // With the fix, the same bypassed region-lock start force-grants Torrent directly.
        let timeline = [
            Ev::RegionLockStart,
            Ev::MelinaHandoff, // still a no-op
            Ev::Tick,
        ];
        let g = replay(&timeline, true);
        assert!(
            g.holds_mount(),
            "the fix must force-grant the mount on a region-lock start"
        );
        assert_eq!(
            g.mount_grant_count(),
            1,
            "the direct grant must fire exactly once (latched), not every tick"
        );
    }

    #[test]
    fn normal_start_still_gets_mount_via_melina_without_double_grant() {
        // On a NORMAL start the vanilla hand-off delivers the mount; the fix must NOT also
        // force-grant it (that would be a double-grant / duplicate Torrent).
        let timeline = [
            Ev::NormalStart,
            Ev::MelinaHandoff, // delivers Torrent
            Ev::Tick,
            Ev::Tick,
        ];
        let g = replay(&timeline, true);
        assert!(g.holds_mount(), "a normal start must still receive the mount from Melina");
        assert_eq!(
            g.mount_grant_count(),
            1,
            "a normal start must not double-grant the mount (Melina only, no forced grant)"
        );
    }

    #[test]
    fn pure_force_grant_semantics() {
        // Region-lock start with the hand-off bypassed -> force-grant.
        assert!(should_force_grant_torrent(true, false));
        // Region-lock start but (hypothetically) the hand-off will still deliver -> don't.
        assert!(!should_force_grant_torrent(true, true));
        // Normal start (Melina delivers) -> never force-grant.
        assert!(!should_force_grant_torrent(false, true));
        // Not a region-lock start and nothing will deliver -> still not our job here.
        assert!(!should_force_grant_torrent(false, false));
    }
}
