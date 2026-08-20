//! Why the capital reconciler declined to write, and when a POSITION may set the burn flag ON
//! (client#200).
//!
//! # What bobler's log showed
//!
//! `archipelago-2026-08-15.log`, 20,754 lines, two sessions, 66 warps. The reconciler was armed,
//! the LuaWarp hook was installed and firing, and `capital warp intercept:` appears **zero times**.
//! There is exactly one write in the whole log:
//!
//! ```text
//! 14:02:58  kick-watch: play_region 6301090 -> 6301000 (sub 63010)
//! 14:03:01  kick-watch: play_region 6301000 -> 1105091 (sub 11050)
//! 14:03:01  capital reconcile: flag 9116 -> ON (play_region Some(1105091)); readback STUCK
//! ```
//!
//! No warp between 13:58:41 and 14:03:01 -- **he walked into Leyndell from Altus and arrived in
//! m11_05 while 9116 was OFF.** The latch then observed bucket 11050 and set 9116 ON, and the
//! readback confirmed it. `sub 11000` never appears in the log, so from that moment no position
//! existed from which the latch could write OFF.
//!
//! # The two problems this module fixes, and the one it does not
//!
//! ## 1. Three different declines were one silence
//!
//! [`crate::capital::reconcile_write`] returns a bare `None` for *not armed*, *unresolvable
//! target* and *already correct*, and both call sites only log inside `if let Some(w)`. So 66
//! warps produced no evidence at all, and the absence of a line was consistent with the reconciler
//! being inert, the target being unreadable, or everything being fine. It was the third. Nobody
//! could tell without the source and the timeline. [`Decision`] names which.
//!
//! ## 2. ON-from-position converts a symptom into save state
//!
//! ⭐ THE DIRECTION IS THE WHOLE ASYMMETRY. Writing 9116 **OFF** from position is self-correcting:
//! if it is wrong, the next load puts you somewhere that says so and the latch converges. Writing
//! it **ON** from position is not: it takes you somewhere strictly more Ashen, where `desired` is
//! ON again, so the write justifies itself forever. That is the trap, and it is why the one write
//! in bobler's log is the one that mattered.
//!
//! 🛑 AND A WARP TARGET IS NOT A POSITION. Deciding ON from a warp TARGET is explicit player
//! intent -- they chose the Ashen grace, before the load, which is exactly the seam
//! `capital_warp_intercept` exists to be. That path is deliberately left alone here: the finale
//! must still work. Only the per-tick latch, which infers from where the player *ended up*, has to
//! justify an ON.
//!
//! ## What it does NOT fix
//!
//! Why the game loaded m11_05 with 9116 OFF in the first place. That is upstream of everything
//! here and still unidentified. `capitalWorldBurnFlag` is now the corroborator that prevents a
//! position-only ON write, while `capitalPreBurnFlag` participates in the full-state reconciler;
//! neither explains the original contradictory load. This module states that contradiction and
//! makes the log carry the world-burn value at the moment it happens.

/// Why no write happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decline {
    /// The burn-done latch is clear: pre-burn / mid-burn, inert by design.
    NotArmed,
    /// Neither a capital bucket nor a resolvable warp target. Nothing is claimed about the flag.
    Unresolvable,
    /// The flag already holds the desired value. The overwhelmingly common case, and the reason
    /// the log was empty.
    AlreadyCorrect(bool),
    /// 🛑 The position says Ashen, the world-burn corroborator says the world is NOT burnt, and
    /// this is the per-tick latch rather than a warp target. Refused; see the module docs.
    ContradictedOn,
}

impl Decline {
    /// One clause, for the log line. ASCII (repo rule 10).
    pub fn reason(&self) -> &'static str {
        match self {
            Decline::NotArmed => {
                "burn-done latch clear -- the reconciler is INERT and cannot write, by design"
            }
            Decline::Unresolvable => {
                "neither a capital bucket nor a resolvable warp target -- nothing claimed"
            }
            Decline::AlreadyCorrect(_) => "flag already holds the desired value -- nothing to do",
            Decline::ContradictedOn => {
                "REFUSED: standing in an Ashen bucket but the world-burn flag says the world is \
                 NOT burnt. Writing ON from position alone would make a wrong load permanent and \
                 unrecoverable -- there is no position from which the latch could write OFF again \
                 (client#200). Warping to an Ashen grace still sets it: that is explicit intent"
            }
        }
    }
}

/// What the reconciler should do with one observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Write(bool),
    Declined(Decline),
}

impl Decision {
    /// The value to write, if any. Keeps [`crate::capital::reconcile_write`]'s old contract
    /// expressible as a one-liner over this, so there is ONE decision and not a drifting twin.
    pub fn write(&self) -> Option<bool> {
        match self {
            Decision::Write(w) => Some(*w),
            Decision::Declined(_) => None,
        }
    }
}

/// The reconcile decision, with the reason when it declines. Identical in outcome to the original
/// `reconcile_write`; it only says more.
pub fn decide(burn_done: bool, desired: Option<bool>, current: bool) -> Decision {
    if !burn_done {
        return Decision::Declined(Decline::NotArmed);
    }
    match desired {
        Some(want) if want != current => Decision::Write(want),
        Some(want) => Decision::Declined(Decline::AlreadyCorrect(want)),
        None => Decision::Declined(Decline::Unresolvable),
    }
}

/// [`decide`], plus the ON-from-position guard.
///
/// `world_burnt` is the corroborator (`capitalWorldBurnFlag`): `Some(true)` = the world really is
/// burnt, `Some(false)` = it is not, `None` = not configured or not readable.
///
/// 🛑 REFUSE ONLY ON A CONTRADICTION, never on an absence. `None` behaves exactly as today, so an
/// apworld that does not emit `capitalWorldBurnFlag` -- every seed generated before this key
/// existed -- keeps the behaviour it shipped with. A guard that changed those seeds on a datum it
/// could not read would be trading a rare trap for a common one.
pub fn decide_from_position(
    burn_done: bool,
    desired: Option<bool>,
    current: bool,
    world_burnt: Option<bool>,
) -> Decision {
    match decide(burn_done, desired, current) {
        Decision::Write(true) if world_burnt == Some(false) => {
            Decision::Declined(Decline::ContradictedOn)
        }
        other => other,
    }
}

/// Keep a warp-target decision authoritative while the position reader still reports the source
/// region. The game's warp is asynchronous: the hook runs before the load, and without this seam
/// the next frame can immediately undo the intercept from the stale pre-warp position.
pub fn desired_across_warp(
    source_region: Option<i32>,
    current_region: Option<i32>,
    warp_desired: bool,
    position_desired: Option<bool>,
) -> (Option<bool>, bool) {
    if source_region == current_region {
        (Some(warp_desired), true)
    } else {
        (position_desired, false)
    }
}

/// Emits a decline once per CHANGE rather than once per tick.
///
/// The latch runs every tick and the answer is `AlreadyCorrect` almost always; logging that at
/// 60 Hz would be the noise the whole codebase's one-shot announce bits exist to prevent. A
/// transition is the event worth a line -- and `ContradictedOn` appearing at all is the event this
/// module was written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeclineLatch {
    last: Option<Decline>,
}

impl DeclineLatch {
    pub const fn new() -> Self {
        Self { last: None }
    }

    /// `Some(decline)` when this one should be written to the log.
    pub fn admit(&mut self, decline: Decline) -> Option<Decline> {
        if self.last == Some(decline) {
            return None;
        }
        self.last = Some(decline);
        Some(decline)
    }

    /// A write clears the latch, so the next decline after one is always stated.
    pub fn on_write(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod replay {
    use super::*;

    /// ⭐ THE RED-FIRST ASSERTION, and it is bobler's 14:03:01 line exactly: armed, standing in an
    /// Ashen bucket (`desired = Some(true)`), flag currently OFF, and the world-burn corroborator
    /// saying the world is NOT burnt. Today that writes ON and the world is unrecoverable.
    #[test]
    fn walking_into_an_ashen_bucket_with_an_unburnt_world_does_not_write_on() {
        let d = decide_from_position(true, Some(true), false, Some(false));
        assert_eq!(
            d,
            Decision::Declined(Decline::ContradictedOn),
            "ON from position alone is what made bobler's world permanent"
        );
        assert_eq!(d.write(), None);
    }

    #[test]
    fn stale_source_position_cannot_undo_an_outbound_warp() {
        let (desired, keep_pending) =
            desired_across_warp(Some(1_105_011), Some(1_105_011), false, Some(true));
        assert_eq!(desired, Some(false));
        assert!(keep_pending);

        let (desired, keep_pending) =
            desired_across_warp(Some(1_105_011), Some(6_301_000), false, None);
        assert_eq!(desired, None);
        assert!(!keep_pending);
    }

    /// 🛑 THE FINALE MUST STILL WORK. The same observation through the WARP-TARGET seam is explicit
    /// intent and still writes ON -- `decide` has no corroborator and never refuses.
    #[test]
    fn a_warp_target_may_still_set_it_on() {
        assert_eq!(decide(true, Some(true), false), Decision::Write(true));
    }

    /// A genuinely burnt world writes ON from position, exactly as before.
    #[test]
    fn a_corroborated_world_still_writes_on() {
        assert_eq!(
            decide_from_position(true, Some(true), false, Some(true)),
            Decision::Write(true)
        );
    }

    /// 🛑 NO CORROBORATOR MEANS NO CHANGE. Seeds from an apworld that never emitted
    /// `capitalWorldBurnFlag` must behave exactly as they shipped; refusing on an absence would
    /// trade a rare trap for a common one.
    #[test]
    fn an_absent_corroborator_leaves_behaviour_untouched() {
        assert_eq!(
            decide_from_position(true, Some(true), false, None),
            Decision::Write(true)
        );
    }

    /// ⭐ OFF IS NEVER GUARDED, in any direction. It is self-correcting -- if it is wrong the next
    /// load says so -- and it is the direction that RECOVERS a stuck world. Guarding it would be
    /// the bug.
    #[test]
    fn off_is_never_refused() {
        for corroborator in [Some(true), Some(false), None] {
            assert_eq!(
                decide_from_position(true, Some(false), true, corroborator),
                Decision::Write(false),
                "OFF must never be blocked (corroborator {corroborator:?})"
            );
        }
    }

    /// The three silences that made 66 warps produce no evidence, now distinguishable.
    #[test]
    fn the_three_declines_are_told_apart() {
        assert_eq!(
            decide(false, Some(true), false),
            Decision::Declined(Decline::NotArmed)
        );
        assert_eq!(
            decide(true, None, false),
            Decision::Declined(Decline::Unresolvable)
        );
        assert_eq!(
            decide(true, Some(false), false),
            Decision::Declined(Decline::AlreadyCorrect(false))
        );
        // bobler's 66 warps were all this one.
        assert_eq!(
            decide_from_position(true, Some(false), false, Some(true)),
            Decision::Declined(Decline::AlreadyCorrect(false))
        );
    }

    /// 🛑 NOT ARMED BEATS EVERYTHING, including the guard. An inert reconciler must report inert
    /// rather than a refusal it never got far enough to make.
    #[test]
    fn not_armed_outranks_the_guard() {
        assert_eq!(
            decide_from_position(false, Some(true), false, Some(false)),
            Decision::Declined(Decline::NotArmed)
        );
    }

    /// Every reason is one ASCII clause and the refusal names its issue.
    #[test]
    fn the_reasons_are_ascii_and_the_refusal_is_findable() {
        for d in [
            Decline::NotArmed,
            Decline::Unresolvable,
            Decline::AlreadyCorrect(true),
            Decline::ContradictedOn,
        ] {
            assert!(d.reason().is_ascii(), "{}", d.reason());
            assert!(!d.reason().is_empty());
        }
        assert!(Decline::ContradictedOn.reason().contains("client#200"));
    }

    /// The latch states a transition and swallows the repeat -- the per-tick site would otherwise
    /// log `AlreadyCorrect` at 60 Hz forever.
    #[test]
    fn the_latch_speaks_on_change_only() {
        let mut l = DeclineLatch::new();
        assert_eq!(
            l.admit(Decline::AlreadyCorrect(false)),
            Some(Decline::AlreadyCorrect(false))
        );
        for _ in 0..1000 {
            assert_eq!(l.admit(Decline::AlreadyCorrect(false)), None);
        }
        // A different decline is a different event.
        assert_eq!(
            l.admit(Decline::ContradictedOn),
            Some(Decline::ContradictedOn)
        );
        // ...and the same one again is not.
        assert_eq!(l.admit(Decline::ContradictedOn), None);
    }

    /// 🛑 A WRITE RE-ARMS THE LATCH. Otherwise a fight between a write and a decline would state
    /// the decline once and then go quiet for the rest of the session.
    #[test]
    fn a_write_re_arms_the_latch() {
        let mut l = DeclineLatch::new();
        l.admit(Decline::AlreadyCorrect(true));
        assert_eq!(l.admit(Decline::AlreadyCorrect(true)), None);
        l.on_write();
        assert_eq!(
            l.admit(Decline::AlreadyCorrect(true)),
            Some(Decline::AlreadyCorrect(true)),
            "the first decline after a write is always stated"
        );
    }
}
