//! Trap effects -- the WORDS, the numbers and the arithmetic. No game access lives here.
//!
//! House split, the same one `region_lock` and `marker::refusal_toast` use: er-logic owns what a
//! trap IS and what the player is told, the client crate owns reaching into the game. Everything in
//! this file is host-testable, which matters more for traps than for most features: a trap is a
//! deliberate insult to the player and the difference between "annoying" and "save-ruining" is
//! arithmetic somebody has to be able to read.
//!
//! ## Scope of this module today
//!
//! Two traps, both of which need NO new reverse engineering (issue #114 tiers them):
//!
//! * **Rune Thief** -- halve the rune count. One typed call in the client, through `runes.rs` and
//!   its single-writer discipline.
//! * **No Flask** -- the flask heals NOTHING for a while. `changeHpEstusFlaskCorrectRate` and its
//!   MP twin are real `SpEffectParam` columns and vanilla row `12061` already sets both to 0 at
//!   `effectEndurance 5`, `spCategory 0` -- so this is one `apply_speffect` on a row we own, not
//!   the input-hook problem the design originally filed it as.
//!
//! 🛑 A trap's DURATION is a param field, not client bookkeeping: `effectEndurance` on the row we
//! apply. No timer, no tick loop, no state machine, and nothing to leak if the player quits mid-trap.
//! That is the finding the whole trap design rests on.

/// The traps this build can fire. `OptionSet` names will mirror these, so 🛑 a name added here later
/// is safe and a name REMOVED is a compat break -- never ship one you might withdraw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trap {
    RuneThief,
    NoFlask,
}

impl Trap {
    /// The yaml/option name. Stable identifier, lower_snake, never localised.
    pub fn key(self) -> &'static str {
        match self {
            Trap::RuneThief => "rune_thief",
            Trap::NoFlask => "no_flask",
        }
    }

    /// The line the player sees. ASCII only (`every_trap_line_is_ascii`) -- the in-game font draws
    /// `?` for anything else, and the v0.2.18 em-dash escape lived in a format string's constant
    /// part.
    ///
    /// Phrased as the EFFECT, not the receipt, exactly like the region-unlock line: "you received
    /// Rune Thief" is something the player has to translate; "half your runes are gone" is the
    /// thing that changed about their run.
    pub fn toast(self) -> &'static str {
        match self {
            Trap::RuneThief => "TRAP: Rune Thief -- half your runes are gone",
            // 🛑 Says HEALS NOTHING, not "cannot drink". The charge is still spent -- see
            // `NO_FLASK_SECONDS`. Promising a blocked animation would be a lie the player finds out
            // about at the worst possible moment.
            Trap::NoFlask => "TRAP: No Flask -- your flask heals nothing for 20s",
        }
    }
}

/// How long `NoFlask` lasts, in seconds, written to the row's `effectEndurance`.
///
/// 20 s is bobler's own ask. It is long enough to lose a fight and short enough that it cannot be
/// mistaken for a permanent break, which matters: the failure mode of getting this wrong is a
/// player who thinks their save is broken.
pub const NO_FLASK_SECONDS: f32 = 20.0;

// 🛑🛑 THE LINE BETWEEN A TRAP AND A SAVE-RUINING BUG, asserted at COMPILE TIME.
//
// `-1` means PERMANENT in this param, and every row in the down palette carries it. A trap that
// shipped a permanent duration would not inconvenience the player, it would end the character --
// so this must fail the BUILD, not a test run. (It began life as a `#[test]`; clippy correctly
// pointed out that an assertion over a `const` is constant, which is the argument for moving it
// here rather than for deleting it.)
const _: () = assert!(
    NO_FLASK_SECONDS > 0.0,
    "a trap with no duration never expires"
);
const _: () = assert!(
    NO_FLASK_SECONDS < 120.0,
    "longer than a boss fight is a broken save, not a trap"
);

/// The flask-healing multiplier `NoFlask` writes. 0.0 = the flask restores nothing.
///
/// Vanilla row `12061` sets exactly this pair, so the column is known-live rather than inferred
/// from its name -- which is the failure that broke enemy scaling once.
pub const NO_FLASK_CORRECT_RATE: f32 = 0.0;

/// Rune Thief's new total: half, rounded down.
///
/// Saturating by construction (`u32 / 2`), so there is no underflow branch to get wrong and a
/// player at 0 or 1 rune simply stays where they are. Split out from the client purely so the
/// arithmetic can be read and tested without a game.
pub fn rune_thief_target(current: u32) -> u32 {
    current / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rune_thief_halves_and_never_underflows() {
        assert_eq!(rune_thief_target(1_000), 500);
        assert_eq!(rune_thief_target(1), 0);
        assert_eq!(rune_thief_target(0), 0);
        assert_eq!(rune_thief_target(u32::MAX), u32::MAX / 2);
    }

    /// 🛑 A trap may impoverish the player; it may never ENRICH them. A sign error here is the one
    /// mistake in this file that would be reported as a cheat rather than as a bug.
    #[test]
    fn rune_thief_never_gives_runes() {
        for n in [0u32, 1, 2, 3, 7, 999, 1_000_000, u32::MAX] {
            assert!(rune_thief_target(n) <= n, "{n} -> {}", rune_thief_target(n));
        }
    }

    /// The duration property is asserted at COMPILE TIME beside the constant (see
    /// `NO_FLASK_SECONDS`), because a save-ruining value should fail the BUILD rather than a test
    /// somebody could skip. This case only pins that the constant is the one we documented.
    #[test]
    fn no_flask_duration_is_the_documented_twenty_seconds() {
        assert_eq!(NO_FLASK_SECONDS, 20.0);
    }

    #[test]
    fn no_flask_rate_heals_nothing() {
        assert_eq!(NO_FLASK_CORRECT_RATE, 0.0);
    }

    #[test]
    fn every_trap_line_is_ascii_and_names_itself() {
        let all = [Trap::RuneThief, Trap::NoFlask];
        // WITNESS: an empty list would make every assertion below vacuously true.
        assert_eq!(all.len(), 2);
        for t in all {
            assert!(t.toast().is_ascii(), "non-ASCII trap line: {}", t.toast());
            assert!(t.key().is_ascii());
            assert!(
                t.toast().starts_with("TRAP: "),
                "{} must announce itself",
                t.key()
            );
            assert!(
                t.key().chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{}",
                t.key()
            );
        }
    }

    /// Keys are the yaml surface; two traps sharing one is an option that cannot address them both.
    #[test]
    fn trap_keys_are_unique() {
        let keys: Vec<&str> = [Trap::RuneThief, Trap::NoFlask]
            .iter()
            .map(|t| t.key())
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "duplicate trap key in {keys:?}");
    }
}
