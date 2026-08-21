//! `collect_cue` — WHEN the multiworld-pickup sound fires (client#336). Never WHAT plays: the
//! sound itself is platform glue (`eldenring_archipelago::sound_cue`), so the whole discipline the
//! issue's acceptance criteria are about lives here, host-tested.
//!
//! The criteria, as decisions:
//!
//! * **Fire only for a NEW local pickup/check.** The caller feeds every staged check id through
//!   here; an id that was ever cued (or silenced) before contributes nothing. Replay paths never
//!   reach here at all — receive-side replay is not staged as checks, and the sweep/shop stage
//!   sites already dedup against the server's checked set — but a stage site is a callsite, and a
//!   callsite is where regressions re-add things. The seen-set is the belt to their suspenders.
//! * **A sweep is ONE cue.** A boss/dungeon sweep stages dozens of ids in a single call; the batch
//!   collapses to one sound, not dozens overlapping.
//! * **Bursts are rate-limited.** A pickup inside the cooldown of the last cue is SILENT — and
//!   remembered as cued, so it cannot fire a stale cue after the cooldown ends (a sound 10 seconds
//!   after its pickup reads as a bug, not a confirmation). One intelligible cue per transaction,
//!   never a drip of catch-up sounds.
//! * **The limiter is telemetered.** A silenced pickup is invisible from the player's chair; the
//!   suppressed count is reported the next time a cue does play, so "I picked something up and
//!   heard nothing" has a log line to grep.

use std::collections::HashSet;

/// Minimum spacing between cues. Long enough that a sweep plus its stragglers (echo checks landing
/// a tick or two later) collapse into one sound; short enough that a pickup a breath after another
/// still confirms. Matches the order of magnitude of the toast deck's refresh, not of a frame.
pub const CUE_COOLDOWN_MS: u64 = 750;

#[derive(Default)]
pub struct CollectCue {
    /// Every check id that has already had its chance to sound — played OR silenced by the
    /// cooldown. Session-scoped; the owner resets it on seed change (check ids are seed-scoped).
    cued: HashSet<i64>,
    last_cue_ms: Option<u64>,
    /// Cues the cooldown has silenced since the last PLAYED cue. Read out by `take_suppressed` so
    /// the next audible cue can carry the count in its log line.
    suppressed: u64,
}

pub enum CueAction {
    /// Play the cue now. Carries how many pickups were silenced since the last played cue, for the
    /// log line (`0` is the common case and logs nothing extra).
    Play {
        suppressed_since_last: u64,
    },
    Silent,
}

impl CollectCue {
    /// Decide for one staged batch. `staged` is the check ids the caller just accepted for sending
    /// (one or many — a sweep arrives as one call).
    pub fn cue_action(&mut self, staged: &[i64], now_ms: u64) -> CueAction {
        let new: Vec<i64> = staged
            .iter()
            .copied()
            .filter(|id| !self.cued.contains(id))
            .collect();
        if new.is_empty() {
            return CueAction::Silent; // re-stage, echo, or replay residue: nothing new happened
        }
        // Every new id is remembered EITHER WAY: a silenced pickup must not fire a stale cue when
        // the cooldown ends.
        self.cued.extend(new);
        let in_cooldown = self
            .last_cue_ms
            .is_some_and(|t| now_ms.saturating_sub(t) < CUE_COOLDOWN_MS);
        if in_cooldown {
            self.suppressed += 1;
            return CueAction::Silent;
        }
        self.last_cue_ms = Some(now_ms);
        CueAction::Play {
            suppressed_since_last: std::mem::take(&mut self.suppressed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE MOTIVATING CASE (CONTRIBUTING rule 11): the issue's first acceptance criterion. One
    /// pickup, one sound; the same check re-staged (a retry, an echo) stays silent.
    #[test]
    fn a_new_pickup_cues_exactly_once() {
        let mut c = CollectCue::default();
        assert!(matches!(
            c.cue_action(&[1001], 1_000),
            CueAction::Play { .. }
        ));
        assert!(matches!(c.cue_action(&[1001], 5_000), CueAction::Silent));
    }

    /// THE MOTIVATING CASE: a boss sweep stages dozens of checks in ONE call. That is one cue,
    /// not dozens overlapping (the issue's third acceptance criterion).
    #[test]
    fn a_sweep_batch_is_one_cue() {
        let mut c = CollectCue::default();
        let sweep: Vec<i64> = (2000..2040).collect();
        assert!(matches!(
            c.cue_action(&sweep, 1_000),
            CueAction::Play { .. }
        ));
        // ...and the sweep's stragglers landing a tick later add nothing.
        assert!(matches!(c.cue_action(&[2005], 1_100), CueAction::Silent));
    }

    /// A pickup inside the cooldown is silent AND never fires a stale cue after the cooldown
    /// ends -- a sound ten seconds after its pickup reads as a bug, not a confirmation.
    #[test]
    fn a_cooldown_pickup_is_silent_now_and_stays_silent() {
        let mut c = CollectCue::default();
        assert!(matches!(c.cue_action(&[1], 1_000), CueAction::Play { .. }));
        assert!(matches!(c.cue_action(&[2], 1_500), CueAction::Silent));
        // The cooldown has long ended; id 2 must still not cue, and the NEXT genuinely new pickup
        // reports the silenced one in its line.
        assert!(matches!(c.cue_action(&[2], 60_000), CueAction::Silent));
        match c.cue_action(&[3], 60_000) {
            CueAction::Play {
                suppressed_since_last,
            } => assert_eq!(suppressed_since_last, 1),
            CueAction::Silent => panic!("a post-cooldown new pickup must cue"),
        }
    }

    /// The limiter must not eat a DISTINCT later transaction: a pickup after the cooldown cues.
    #[test]
    fn a_pickup_after_the_cooldown_cues_again() {
        let mut c = CollectCue::default();
        assert!(matches!(c.cue_action(&[1], 1_000), CueAction::Play { .. }));
        assert!(matches!(
            c.cue_action(&[2], 1_000 + CUE_COOLDOWN_MS),
            CueAction::Play { .. }
        ));
    }

    /// A mixed batch (one new id among already-cued ones) still cues for the new one.
    #[test]
    fn a_mixed_batch_cues_for_the_new_id() {
        let mut c = CollectCue::default();
        assert!(matches!(c.cue_action(&[1], 1_000), CueAction::Play { .. }));
        assert!(matches!(
            c.cue_action(&[1, 2], 5_000),
            CueAction::Play { .. }
        ));
    }
}
