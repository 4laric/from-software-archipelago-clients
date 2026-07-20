//! DeathLink side-effect logic, extracted from `deathlink.rs`. Latches lifted into a passed-in
//! `DeathLatch` (the live code keeps them as module statics) so each test gets fresh state.

use crate::hook::GameHook;

/// Cross-tick DeathLink state (replaces the `static AtomicBool`s `KILL_PENDING`/`SEND_PENDING`/
/// `WAS_DEAD`).
#[derive(Default)]
pub struct DeathLatch {
    pub kill_pending: bool,
    pub send_pending: bool,
    pub was_dead: bool,
}

/// A foreign DeathLink event arrived: latch a kill — but ONLY when this slot's `death_link` option
/// is on (SWEEP H2: the DeathLink tag is advertised unconditionally for room visibility, so a slot
/// with `death_link: 0` still RECEIVES events; they must be dropped here). Returns whether latched.
pub fn latch_incoming(latch: &mut DeathLatch, enabled: bool) -> bool {
    if enabled {
        latch.kill_pending = true;
    }
    enabled
}

/// Apply an INCOMING DeathLink: once in-world, set the kill flag; clear the latch only on success
/// (retry next tick while the holder isn't ready). `enabled` is the slot's `death_link` option —
/// belt-and-braces (SWEEP H2 / R2): a stale latched kill must never fire once death_link is
/// known-disabled, even if it was latched before the option parsed (mirrors client `drive_kill`).
pub fn drive_incoming_kill(hook: &mut dyn GameHook, latch: &mut DeathLatch, enabled: bool) {
    if !enabled {
        return;
    }
    if latch.kill_pending && hook.in_world() && hook.kill_player() {
        latch.kill_pending = false;
    }
}

/// Detect an OUTGOING death edge (alive -> dead) and mark a send. Off-world clears the edge so a
/// respawn isn't read as a fresh death.
pub fn poll_outgoing_death(hook: &mut dyn GameHook, latch: &mut DeathLatch) {
    if !hook.in_world() {
        latch.was_dead = false;
        return;
    }
    let dead = hook.read_local_death();
    if dead && !latch.was_dead {
        latch.send_pending = true;
    }
    latch.was_dead = dead;
}

// ================================================================================================
// KEEP-RUNES on incoming DeathLink (2026-07-20)
// ================================================================================================
//
// A pure-runtime incoming kill is a direct `hp = 0` write (client `kill_local_player`) -- an
// ordinary death, so the engine banks the player's held runes into a bloodstain, at risk. The old
// baked path avoided this with `ForceCharacterDeath(_, true)` (keep-runes); the `eldenring` crate
// exposes no bloodstain API to replicate the "clear the drop" half, so we keep runes the
// timing-robust way instead:
//
//   1. read  `rune_count` BEFORE the kill                 (client IO)
//   2. write `rune_count = 0` right after the kill        (client IO) -> the death banks an EMPTY
//      bloodstain, so there is nothing to duplicate on restore
//   3. once the player has respawned (in-world AND hp > 0), write the snapshot back   (client IO)
//
// The restore is RE-ASSERTED for [`RESTORE_REASSERT_TICKS`] consecutive alive ticks rather than
// written once: a live probe showed `rune_count` zeroing "way after YOU DIED" (during the death /
// load while off-world), so a single write on the first alive frame already lands after the engine
// bank -- but if on some path the engine's zero lands a tick or two AFTER control returns, a
// one-shot restore would race and lose. Re-writing the snapshot across a short window (~5 ticks,
// well under the time it takes a just-respawned player to change their rune total at a grace)
// beats that race with no user-visible clobber.
//
// This struct owns ONLY the decision state (what is owed, and when to pay it). Every game read /
// write stays in the client, so er-logic keeps its "no eldenring/windows deps" contract and the
// state machine stays host-testable. If a probe ever shows the engine banks the bloodstain on the
// exact hp==0 frame (making step 2 too late to empty it), the fix is entirely client-side -- swap
// the "write 0" for the `GameDataMan::death_state` Sacrificial-Twig path; this struct is unaffected.

/// Number of consecutive alive ticks the restore is re-asserted over (see module note above).
pub const RESTORE_REASSERT_TICKS: u8 = 5;

/// Decision state for keep-runes on an incoming DeathLink death. `owed = Some(n)` means a kill
/// zeroed the player's runes and `n` must be written back once they respawn; the restore is then
/// re-asserted for [`RESTORE_REASSERT_TICKS`] alive ticks to beat a late engine zero.
#[derive(Debug)]
pub struct KeepRunes {
    owed: Option<u32>,
    /// `true` once the player is back alive and we've begun paying the debt back.
    restoring: bool,
    /// Alive-tick re-assertions still to emit (only meaningful while `restoring`).
    reassert_left: u8,
}

impl Default for KeepRunes {
    fn default() -> Self {
        Self::new()
    }
}

impl KeepRunes {
    /// `const` ctor so the client can hold it in a `static Mutex<KeepRunes>`.
    pub const fn new() -> Self {
        Self {
            owed: None,
            restoring: false,
            reassert_left: 0,
        }
    }

    /// Arm a restore after an incoming kill landed. `snapshot` is the `rune_count` read BEFORE the
    /// client zeroed it, or `None` if GameDataMan was unresolvable -- in which case nothing was
    /// zeroed and nothing is owed (the death is a vanilla drop, exactly as before this feature).
    /// Re-arming (a second incoming kill) resets the window to the new snapshot.
    pub fn arm(&mut self, snapshot: Option<u32>) {
        if let Some(runes) = snapshot {
            self.owed = Some(runes);
            self.restoring = false;
            self.reassert_left = 0;
        }
    }

    /// Per-tick check. While a debt is owed AND the player is in-world and alive, returns
    /// `Some(runes)` on each of [`RESTORE_REASSERT_TICKS`] consecutive alive ticks for the client to
    /// write into `rune_count`, then disarms. Returns `None` while nothing is owed, or while the
    /// player is still dead / on a load screen -- a death mid-window just pauses it (the remaining
    /// re-assertions resume once alive again), so the debt is never dropped early.
    pub fn poll_restore(&mut self, in_world: bool, hp: Option<i32>) -> Option<u32> {
        let owed = self.owed?;
        if !(in_world && hp.is_some_and(|h| h > 0)) {
            return None; // dead or loading -> wait; a started window pauses here
        }
        if !self.restoring {
            self.restoring = true;
            self.reassert_left = RESTORE_REASSERT_TICKS;
        }
        self.reassert_left = self.reassert_left.saturating_sub(1);
        if self.reassert_left == 0 {
            self.owed = None;
            self.restoring = false;
        }
        Some(owed)
    }

    /// True while a restore is pending (a kill zeroed runes we have not fully paid back yet).
    pub fn is_armed(&self) -> bool {
        self.owed.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::{fake::FakeGame, DEATHLINK_KILL_FLAG};

    #[test]
    fn incoming_kill_sets_dedicated_flag_not_hp() {
        let mut g = FakeGame::new();
        g.set_in_world(true);
        g.set_hp(Some(1000)); // alive; HP must be untouched
        let mut latch = DeathLatch {
            kill_pending: true,
            ..Default::default()
        };
        drive_incoming_kill(&mut g, &mut latch, true);
        assert_eq!(g.set_flags(), vec![DEATHLINK_KILL_FLAG]);
        assert_eq!(g.player_hp(), Some(1000));
        assert!(!latch.kill_pending);
    }

    #[test]
    fn incoming_kill_retries_until_holder_ready() {
        let mut g = FakeGame::new();
        g.set_in_world(true);
        g.set_flag_holder_ready(false);
        let mut latch = DeathLatch {
            kill_pending: true,
            ..Default::default()
        };
        drive_incoming_kill(&mut g, &mut latch, true);
        assert!(latch.kill_pending);
        assert!(g.set_flags().is_empty());

        g.set_flag_holder_ready(true);
        drive_incoming_kill(&mut g, &mut latch, true);
        assert!(!latch.kill_pending);
        assert_eq!(g.set_flags(), vec![DEATHLINK_KILL_FLAG]);
    }

    #[test]
    fn incoming_kill_not_attempted_off_world() {
        let mut g = FakeGame::new();
        g.set_in_world(false);
        let mut latch = DeathLatch {
            kill_pending: true,
            ..Default::default()
        };
        drive_incoming_kill(&mut g, &mut latch, true);
        assert!(latch.kill_pending);
        assert!(g.set_flags().is_empty());
    }

    // --- SWEEP H2: death_link:0 slots must never be killed by other players' deaths ---

    #[test]
    fn disabled_slot_refuses_to_latch_incoming_death() {
        let mut latch = DeathLatch::default();
        assert!(!latch_incoming(&mut latch, false));
        assert!(!latch.kill_pending);
        assert!(latch_incoming(&mut latch, true));
        assert!(latch.kill_pending);
    }

    #[test]
    fn stale_latched_kill_never_fires_when_disabled() {
        // H2 belt-and-braces: an event that slipped in before the option parsed (or a bug in the
        // latch site) leaves kill_pending set — drive must STILL refuse to kill a death_link:0 slot.
        let mut g = FakeGame::new();
        g.set_in_world(true);
        g.set_hp(Some(1000));
        let mut latch = DeathLatch {
            kill_pending: true,
            ..Default::default()
        };
        drive_incoming_kill(&mut g, &mut latch, false);
        assert!(
            g.set_flags().is_empty(),
            "disabled slot was killed (SWEEP H2 regression)"
        );
        assert_eq!(g.player_hp(), Some(1000));
        // Latch intentionally NOT cleared: enabling later on the same session is the operator's
        // explicit choice; the invariant here is only that nothing fires while disabled.
        assert!(latch.kill_pending);
    }

    #[test]
    fn read_local_death_true_only_when_hp_le_zero_in_world() {
        let mut g = FakeGame::new();
        g.set_in_world(true);
        g.set_hp(Some(0));
        assert!(g.read_local_death());
        g.set_hp(Some(-5));
        assert!(g.read_local_death());
        g.set_hp(Some(1));
        assert!(!g.read_local_death());
    }

    #[test]
    fn read_local_death_false_off_world_even_if_hp_zero() {
        let mut g = FakeGame::new();
        g.set_in_world(false);
        g.set_hp(Some(0));
        assert!(!g.read_local_death());
    }

    #[test]
    fn outgoing_death_reported_once_per_death() {
        let mut g = FakeGame::new();
        g.set_in_world(true);
        g.set_hp(Some(0)); // dead, sustained
        let mut latch = DeathLatch::default();
        poll_outgoing_death(&mut g, &mut latch);
        assert!(latch.send_pending);
        latch.send_pending = false; // net thread drained it
        poll_outgoing_death(&mut g, &mut latch);
        assert!(!latch.send_pending); // no second send while death screen up
    }

    #[test]
    fn respawn_then_redeath_sends_again() {
        let mut g = FakeGame::new();
        g.set_in_world(true);
        let mut latch = DeathLatch::default();
        g.set_hp(Some(0));
        poll_outgoing_death(&mut g, &mut latch);
        assert!(latch.send_pending);
        latch.send_pending = false;
        g.set_hp(Some(1000)); // respawn
        poll_outgoing_death(&mut g, &mut latch);
        assert!(!latch.send_pending);
        g.set_hp(Some(0)); // new death
        poll_outgoing_death(&mut g, &mut latch);
        assert!(latch.send_pending);
    }

    #[test]
    fn off_world_clears_edge() {
        let mut g = FakeGame::new();
        g.set_in_world(true);
        g.set_hp(Some(0));
        let mut latch = DeathLatch::default();
        poll_outgoing_death(&mut g, &mut latch);
        latch.send_pending = false;
        g.set_in_world(false);
        poll_outgoing_death(&mut g, &mut latch);
        assert!(!latch.was_dead);
        assert!(!latch.send_pending);
    }
}

#[cfg(test)]
mod keep_runes_tests {
    use super::{KeepRunes, RESTORE_REASSERT_TICKS};

    #[test]
    fn unresolvable_snapshot_owes_nothing() {
        // GameDataMan down at kill time -> no runes were zeroed -> nothing to pay back (vanilla).
        let mut k = KeepRunes::new();
        k.arm(None);
        assert!(!k.is_armed());
        assert_eq!(k.poll_restore(true, Some(1000)), None);
    }

    #[test]
    fn restores_across_the_window_then_stops() {
        let mut k = KeepRunes::new();
        k.arm(Some(54_321));
        assert!(k.is_armed());
        assert_eq!(k.poll_restore(true, Some(0)), None, "still dead -> wait");
        assert_eq!(k.poll_restore(false, None), None, "load screen -> wait");
        // Re-asserted on each of RESTORE_REASSERT_TICKS consecutive alive ticks.
        for i in 0..RESTORE_REASSERT_TICKS {
            assert_eq!(
                k.poll_restore(true, Some(1)),
                Some(54_321),
                "alive tick {i} must re-assert the restore"
            );
        }
        assert!(!k.is_armed(), "window exhausted -> disarmed");
        assert_eq!(
            k.poll_restore(true, Some(1)),
            None,
            "never after the window"
        );
    }

    #[test]
    fn window_pauses_when_player_dies_again_mid_restore() {
        // A death during the re-assert window must not drop the remaining debt.
        let mut k = KeepRunes::new();
        k.arm(Some(777));
        assert_eq!(k.poll_restore(true, Some(1)), Some(777)); // 1st assertion
        assert_eq!(k.poll_restore(true, Some(0)), None, "died again -> pause");
        assert_eq!(k.poll_restore(false, None), None, "loading -> still paused");
        let mut emitted = 1;
        while k.is_armed() {
            assert_eq!(k.poll_restore(true, Some(1)), Some(777));
            emitted += 1;
        }
        assert_eq!(
            emitted, RESTORE_REASSERT_TICKS,
            "total assertions across the pause == the full window"
        );
    }

    #[test]
    fn does_not_restore_while_hp_unreadable_in_world() {
        // Transient HP resolve failure must not be read as "alive" and start the window early.
        let mut k = KeepRunes::new();
        k.arm(Some(10));
        assert_eq!(k.poll_restore(true, None), None);
        assert!(k.is_armed());
    }

    #[test]
    fn zero_runes_snapshot_still_pays_back_zero() {
        // Dying with 0 runes: Some(0) is "owed 0" (distinct from None "GameDataMan was down").
        let mut k = KeepRunes::new();
        k.arm(Some(0));
        assert!(k.is_armed());
        assert_eq!(k.poll_restore(true, Some(1)), Some(0), "first window tick");
        while k.is_armed() {
            assert_eq!(k.poll_restore(true, Some(1)), Some(0));
        }
        assert!(!k.is_armed());
    }

    #[test]
    fn re_arm_after_restore_handles_a_second_deathlink() {
        let mut k = KeepRunes::new();
        k.arm(Some(100));
        while k.is_armed() {
            assert_eq!(k.poll_restore(true, Some(1)), Some(100));
        }
        k.arm(Some(250)); // a second incoming kill later in the run resets the window
        assert!(k.is_armed());
        assert_eq!(k.poll_restore(true, Some(1)), Some(250));
        while k.is_armed() {
            assert_eq!(k.poll_restore(true, Some(1)), Some(250));
        }
        assert!(!k.is_armed());
    }
}
