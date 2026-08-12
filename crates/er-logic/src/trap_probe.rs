//! Trap FEEL probe -- the three effects a playtester fires on demand, the words they read, and the
//! timing arithmetic. No game access lives here.
//!
//! ## Why this is separate from [`crate::traps`]
//!
//! `traps` is the AP ITEM surface: its names are a cross-repo string contract, its `OptionSet`
//! mirror is a compat surface, and 🛑 removing a name from it is a compat break. Nothing here is an
//! item. These are diagnostics fired from a function key to answer one question -- *does this feel
//! like a trap or like a bug?* -- and they must stay free to be renamed, retuned or deleted the
//! moment the answer is in. Folding them into `Trap` would quietly make three throwaway
//! diagnostics permanent.
//!
//! ## Why these three
//!
//! Each is a DIFFERENT KIND of insult, and the point is to find out which kind lands:
//!
//! * **Nightfall** -- environmental. Costs the player nothing directly; changes the world around
//!   them. `WorldAreaTime::request_time` is a typed one-call primitive, and the game crate carries
//!   `AiSightTimeOfDay`, so enemy sight really does change with it.
//! * **Stamina Halved** -- mechanical, and bobler's own ask. A ceiling held for 30s by
//!   [`Deadline`], not a param edit: `CSChrDataModule::stamina` is a plain `i32` that the game
//!   clamps and regenerates for us.
//! * **Blackout** -- sensory. The cheap stand-in for the blindness trap, without chasing the
//!   Curseblade speffect or writing an unknown gparam id.
//!
//! 🛑 EVERY VALUE THIS PROBE WRITES IS KNOWN-GOOD OR SELF-RESTORING. That is the property that made
//! it shippable to a playtest starting today rather than after a review round: nothing here edits a
//! param row, claims a speffect row, touches `special_effect`, or persists past the session. The
//! one thing it cannot promise is that the blackout is VISIBLE -- see [`BLACKOUT_FADE_SECONDS`].
//!
//! ## What it deliberately does NOT do
//!
//! No rune theft, no flask lockout, no weapon downgrade, no spawns. Those either already exist
//! behind the destructive `traps` probe or destroy progress, and this one is ON BY DEFAULT --
//! which is only defensible because every effect here wears off on its own.

/// A duration that has been started and will elapse once.
///
/// ⭐ THIS IS THE WHOLE STATE MACHINE, and it lives here rather than in the client because both of
/// its properties are ones a test can pin and a playtest cannot. [`Self::holds`] is what SUSTAINS
/// the stamina ceiling across ticks; [`Self::take_if_elapsed`] is what fires the blackout's restore
/// EXACTLY ONCE. Get the second one wrong in the obvious way -- "is it past the deadline?" without
/// disarming -- and the client re-issues a fade-in every frame forever, which reads as a stuck
/// screen rather than as a bug in a timer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Deadline(Option<u64>);

impl Deadline {
    /// Nothing pending.
    pub const fn new() -> Self {
        Self(None)
    }

    /// Start (or restart) the duration. Re-arming an armed deadline EXTENDS it, which is what a
    /// player mashing the key expects -- the alternative, ignoring the second press, reads as a
    /// dropped input.
    ///
    /// Saturating, because a deadline that wrapped would elapse instantly and silently.
    pub fn arm(&mut self, now_ms: u64, duration_ms: u64) {
        self.0 = Some(now_ms.saturating_add(duration_ms));
    }

    /// Is the effect still running? Pure and cheap enough for a per-tick call, so the client's
    /// sustain loop has nothing of its own to get wrong.
    pub fn holds(&self, now_ms: u64) -> bool {
        self.0.is_some_and(|end| now_ms < end)
    }

    /// Armed and not yet taken -- INCLUDING already past its end. Distinct from [`Self::holds`],
    /// which goes false the moment the deadline passes even though the restore is still OWED.
    ///
    /// 🛑 [`ProbeState::idle`] needs this one, not `holds`: a blackout whose deadline has passed
    /// but whose fade-in has not been issued is not idle, and treating it as idle is how the screen
    /// stays black.
    pub fn is_pending(&self) -> bool {
        self.0.is_some()
    }

    /// True on the FIRST poll at or after the deadline, and never again until re-armed.
    ///
    /// 🛑 Disarms itself. That is the point of the name and the reason it returns `bool` rather
    /// than exposing the deadline: a caller that has to remember to disarm is a caller that will
    /// forget.
    pub fn take_if_elapsed(&mut self, now_ms: u64) -> bool {
        match self.0 {
            Some(end) if now_ms >= end => {
                self.0 = None;
                true
            }
            _ => false,
        }
    }

    /// Cancel without firing, for a teardown that must not leave an effect owing a restore.
    pub fn disarm(&mut self) {
        self.0 = None;
    }
}

/// The effects this probe can fire. Diagnostics, not items -- see the module note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeelEffect {
    Nightfall,
    StaminaHalved,
    Blackout,
}

/// Every effect, one place, so a new one cannot be half-added.
pub const ALL: [FeelEffect; 3] = [
    FeelEffect::Nightfall,
    FeelEffect::StaminaHalved,
    FeelEffect::Blackout,
];

impl FeelEffect {
    /// Stable lower_snake identifier. Log-facing, never localised.
    pub fn key(self) -> &'static str {
        match self {
            FeelEffect::Nightfall => "nightfall",
            FeelEffect::StaminaHalved => "stamina_halved",
            FeelEffect::Blackout => "blackout",
        }
    }

    /// The line the player sees. ASCII only -- the in-game font draws `?` for anything else, and
    /// the v0.2.18 em-dash escape lived in a format string's constant part.
    ///
    /// Phrased as the EFFECT and its DURATION, like `traps::toast`. A trap toast that does not say
    /// how long it lasts produces the one report we cannot use: "it broke my stamina", from a
    /// player who quit before it wore off.
    pub fn toast(self) -> &'static str {
        match self {
            FeelEffect::Nightfall => "PROBE: Nightfall -- it is suddenly midnight",
            FeelEffect::StaminaHalved => "PROBE: Stamina Halved -- half stamina for 30s",
            FeelEffect::Blackout => "PROBE: Blackout -- the lights go out for 2s",
        }
    }

    /// How long the client must keep doing something about it. `0` = fire and forget.
    pub fn duration_ms(self) -> u64 {
        match self {
            // Time keeps flowing on its own and a grace rest resets it, so there is nothing to
            // restore and nothing to sustain. Genuinely instantaneous, not "not implemented yet".
            FeelEffect::Nightfall => 0,
            FeelEffect::StaminaHalved => STAMINA_HALVED_MS,
            FeelEffect::Blackout => BLACKOUT_MS,
        }
    }
}

/// Midnight, as `(hour, minute, second)` for `WorldAreaTime::request_time`.
///
/// `AiSightTimeOfDay::Midnight` is a real variant in the game crate, so this is the hour the engine
/// itself treats as darkest rather than a number that merely looks late.
pub const NIGHTFALL_TIME: (u32, u32, u32) = (0, 0, 0);

/// How long the stamina ceiling is held. 30s was bobler's own ask.
pub const STAMINA_HALVED_MS: u64 = 30_000;

/// How long the screen stays dark.
///
/// 🛑 SHORT ON PURPOSE. A blackout is the one effect here that can be mistaken for a crash, and a
/// player who alt-tabs to check is a player whose session we interrupted to ask a question about
/// vibes.
pub const BLACKOUT_MS: u64 = 2_500;

/// Seconds the fade itself takes, each way.
///
/// 🛑 THE ONE UNVERIFIED THING IN THIS MODULE. `CSFade` carries NINE fade plates and nothing on
/// record says which one composites over normal play, so the client fades all of them and restores
/// all of them. If the reading comes back "the screen never went dark", that is a PLATE question,
/// not a timer question -- which is why the log line asks for that case by name.
pub const BLACKOUT_FADE_SECONDS: f32 = 0.4;

// 🛑 THE LINE BETWEEN A PROBE AND A RUINED SESSION, asserted at COMPILE TIME rather than in a test,
// because the failure mode is a playtester who cannot see. Same argument as
// `traps::NO_FLASK_SECONDS`, same shape.
const _: () = assert!(
    BLACKOUT_MS > 0,
    "a blackout with no duration never lifts -- the restore is scheduled off this"
);
const _: () = assert!(
    BLACKOUT_MS < 10_000,
    "longer than a few seconds reads as a crash, not as a trap"
);
const _: () = assert!(
    STAMINA_HALVED_MS < 120_000,
    "longer than a boss fight is a broken save, not a trap"
);

/// The stamina ceiling to hold, given the player's max.
///
/// Integer halving, floored at 0: `max_stamina` comes off a live module, and a negative or absurd
/// read must not become a worse write. A ceiling of 0 is survivable (no sprint, no roll, full
/// regen the moment it lifts); a NEGATIVE one written into `stamina` would not be.
pub fn stamina_ceiling(max_stamina: i32) -> i32 {
    (max_stamina / 2).max(0)
}

/// What to write into `stamina`, or `None` when it is already at or under the ceiling.
///
/// ⭐ RETURNS `None` FOR THE COMMON CASE ON PURPOSE. The clamp runs every tick for 30 seconds, and
/// a player who is already exhausted must not have their stamina RE-WRITTEN each frame -- that
/// stamps on the engine's own regeneration mid-update, and it is the difference between "half
/// stamina" and "stamina pinned to exactly half, visibly never regenerating".
pub fn stamina_clamp(current: i32, max_stamina: i32) -> Option<i32> {
    let ceiling = stamina_ceiling(max_stamina);
    (current > ceiling).then_some(ceiling)
}

/// Everything the client must remember between ticks. One struct, so the tick has one thing to
/// hold and the tests have one thing to drive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProbeState {
    /// While this holds, the stamina ceiling is re-applied every tick; when it elapses the client
    /// takes it once, to say so in the log.
    pub stamina: Deadline,
    /// When this elapses the fade-in is issued -- once.
    pub blackout: Deadline,
}

impl ProbeState {
    pub const fn new() -> Self {
        Self {
            stamina: Deadline::new(),
            blackout: Deadline::new(),
        }
    }

    /// Record that `effect` just fired. Returns whether the client has follow-up work, which is how
    /// the caller knows an instantaneous effect needs no tick at all.
    pub fn arm(&mut self, effect: FeelEffect, now_ms: u64) -> bool {
        match effect {
            FeelEffect::Nightfall => false,
            FeelEffect::StaminaHalved => {
                self.stamina.arm(now_ms, STAMINA_HALVED_MS);
                true
            }
            FeelEffect::Blackout => {
                self.blackout.arm(now_ms, BLACKOUT_MS);
                true
            }
        }
    }

    /// Is there anything at all for the tick to do? Lets the per-frame hook return before it
    /// reaches for a single singleton in the overwhelmingly common case -- this probe is ON BY
    /// DEFAULT, so its idle cost is paid by every player on the build.
    ///
    /// 🛑 `is_pending`, NOT `holds`: an elapsed-but-untaken blackout still owes a fade-in.
    pub fn idle(&self) -> bool {
        !self.stamina.is_pending() && !self.blackout.is_pending()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE MOTIVATING CASE (CONTRIBUTING rule 11): bobler presses the blackout key, the screen goes
    /// dark, and it comes back on its own without him touching anything.
    #[test]
    fn a_blackout_restores_itself_exactly_once() {
        let mut state = ProbeState::new();
        assert!(state.arm(FeelEffect::Blackout, 1_000));
        assert!(state.blackout.holds(1_500), "still dark mid-effect");
        assert!(
            !state.blackout.take_if_elapsed(1_500),
            "must not restore early"
        );

        assert!(
            state.blackout.take_if_elapsed(1_000 + BLACKOUT_MS),
            "the restore fires on the first tick at or after the deadline"
        );
        // The regression this guards: a fade-in re-issued every frame forever reads as a stuck
        // screen, not as a bug in a timer.
        assert!(!state.blackout.take_if_elapsed(1_000 + BLACKOUT_MS + 1));
        assert!(!state.blackout.take_if_elapsed(9_999_999));
        assert!(state.idle(), "nothing owed once the restore has fired");
    }

    #[test]
    fn a_pending_restore_is_not_idle_even_after_its_deadline() {
        // The distinction `holds` alone cannot express, and the whole reason `is_pending` exists.
        let mut state = ProbeState::new();
        state.arm(FeelEffect::Blackout, 0);
        let after = BLACKOUT_MS + 5_000;
        assert!(!state.blackout.holds(after));
        assert!(
            !state.idle(),
            "the fade-in is still owed -- an idle tick here leaves the screen black"
        );
        assert!(state.blackout.take_if_elapsed(after));
        assert!(state.idle());
    }

    #[test]
    fn the_stamina_ceiling_holds_for_its_whole_duration_and_then_lifts() {
        let mut state = ProbeState::new();
        state.arm(FeelEffect::StaminaHalved, 100);
        assert!(state.stamina.holds(100));
        assert!(state.stamina.holds(100 + STAMINA_HALVED_MS - 1));
        assert!(
            !state.stamina.holds(100 + STAMINA_HALVED_MS),
            "the ceiling lifts on its own -- nothing restores stamina by hand"
        );
        assert!(
            state.stamina.take_if_elapsed(100 + STAMINA_HALVED_MS),
            "and the client takes it once, so the log can say it wore off"
        );
        assert!(state.idle());
    }

    /// Re-arming extends rather than being ignored: a second keypress is an input, not a mistake.
    #[test]
    fn re_arming_extends() {
        let mut d = Deadline::new();
        d.arm(0, 1_000);
        d.arm(500, 1_000);
        assert!(d.holds(1_400), "the later press moved the end out to 1500");
    }

    #[test]
    fn arming_never_wraps() {
        let mut d = Deadline::new();
        d.arm(u64::MAX, 30_000);
        // Without the saturation, `u64::MAX + 30_000` wraps to 29_999 -- a deadline already in the
        // past for any ordinary clock reading, so the effect would restore itself before it was
        // ever visible. Saturating leaves it pinned at the end of time instead, which is the
        // failure we can live with.
        assert!(
            !d.take_if_elapsed(30_000),
            "a wrapped deadline would elapse instantly and silently"
        );
        assert!(d.holds(u64::MAX - 1));
    }

    /// ⭐ The clamp declines to write when there is nothing to do. Rewriting an already-low value
    /// every frame is what turns "half stamina" into "stamina that visibly never regenerates".
    #[test]
    fn the_clamp_only_writes_when_it_must() {
        assert_eq!(stamina_clamp(160, 160), Some(80));
        assert_eq!(stamina_clamp(81, 160), Some(80));
        assert_eq!(stamina_clamp(80, 160), None, "already at the ceiling");
        assert_eq!(stamina_clamp(12, 160), None, "already exhausted");
    }

    #[test]
    fn the_ceiling_is_never_negative() {
        // `max_stamina` comes off a live module; a garbage read must not become a garbage write.
        assert_eq!(stamina_ceiling(-40), 0);
        assert_eq!(stamina_ceiling(0), 0);
        assert_eq!(stamina_ceiling(1), 0);
    }

    #[test]
    fn an_instantaneous_effect_asks_the_tick_for_nothing() {
        let mut state = ProbeState::new();
        assert!(
            !state.arm(FeelEffect::Nightfall, 0),
            "nightfall has nothing to restore -- time flows on its own"
        );
        assert!(state.idle());
    }

    /// Every line reaches the in-game font, which draws `?` for anything it cannot render.
    #[test]
    fn every_line_is_ascii_and_names_itself() {
        for effect in ALL {
            assert!(
                effect.toast().is_ascii(),
                "{}: non-ASCII reaches the game font as `?`",
                effect.key()
            );
            assert!(effect.key().is_ascii());
            assert!(
                effect
                    .key()
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_'),
                "{}: keys are lower_snake",
                effect.key()
            );
            assert!(
                effect.toast().starts_with("PROBE: "),
                "{}: the player must be able to tell a diagnostic from a real trap",
                effect.key()
            );
        }
    }

    /// A duration of 0 means "nothing to do later" and must never be armed as a real deadline.
    #[test]
    fn duration_and_arming_agree() {
        let mut state = ProbeState::new();
        for effect in ALL {
            assert_eq!(
                state.arm(effect, 0),
                effect.duration_ms() > 0,
                "{}: duration_ms and arm disagree about whether the tick has work",
                effect.key()
            );
        }
    }
}
