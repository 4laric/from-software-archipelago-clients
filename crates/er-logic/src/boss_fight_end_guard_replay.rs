//! `boss_fight_end_guard_replay` — the END-site consistency guard for the boss-fight probe
//! (client#201).
//!
//! # The reading this exists to reject
//!
//! From bobler's `archipelago-2026-08-14.log`, client `0.4.2 (edaeb3b9be96)`, seed
//! `67703505917608922744` — two fights against `npc_param 35600972` (`c3560`, Metyr), both lost:
//!
//! ```text
//! boss-fight END: npc_param 35600972 outcome=PLAYER DOWN t=140.7s unseen=61.7s
//!   last boss 55821/56586 (98%) player 414/414 (100%)
//! boss-fight END: npc_param 35600972 outcome=PLAYER DOWN t=342.2s unseen=35.2s
//!   last boss 53340/56586 (94%) player 414/414 (100%)
//! ```
//!
//! **A player who is down is not at full HP.** The END line states both at once and nothing stops
//! it, because [`crate::boss_fight_sample::classify`] derives the outcome from the death-guard
//! latch while the HP pair is quoted from the last remembered reading — two independent signals
//! that were never checked against each other.
//!
//! # Why the pair is guaranteed, not unlucky
//!
//! 🛑 THIS IS STRUCTURAL, AND IT IS WHY THE CONTRADICTION IS IN *EVERY* LOST FIGHT RATHER THAN
//! SOME OF THEM. `eldenring_archipelago::boss_fight_probe::tick` reads the player, trips
//! [`crate::death_guard::lists_unsafe_to_touch`], latches the outcome, and **returns** —
//! discarding the one reading in the whole fight that could have contradicted `100%`. The
//! remembered pair keeps whatever the last pre-death sample held, which on a fight the player
//! never visibly took damage in is `414/414`. So the END line cannot disagree with itself no
//! matter what the game memory did.
//!
//! [`remember_on_death`] is that discarded reading's home: it replaces the player half of the
//! remembered pair while leaving the boss half alone, because the boss is usually already gone
//! from the live sets by the time the player dies and re-reading it is exactly what the probe's
//! `LAST_SEEN` exists to avoid.
//!
//! # ⚠️ What this does NOT fix
//!
//! The other half of #201 — `113 of 113` SAMPLE lines reading `player 414/414 (100%)` — is a
//! question about game memory that no host-side test can answer, and the source read
//! (`main_player.chr_ins.modules.data.{hp,max_hp}`) is textually identical to the boss read that
//! demonstrably works. Two hypotheses survive that evidence:
//!
//! 1. the channel is pinned (reads `max_hp` into both halves, or a field never refreshed after the
//!    chr loads — cf. #188); or
//! 2. the channel is live and bobler was **one-shot from full** both times, which at a 500 ms poll
//!    is indistinguishable from (1) in the trace as it is written today.
//!
//! ⭐ The guard plus [`remember_on_death`] is what tells those apart on the next playtest, and it
//! is why this lands before any change to the read itself: a death-tick reading of `0/414` proves
//! the channel is live and the deaths were one-shots, and a death-tick reading of `414/414` proves
//! the channel is pinned. Guessing at an offset first would have destroyed that discriminator.

use crate::boss_fight_sample::{Hp, Outcome};

/// The marker an impossible END pair is reported under.
///
/// Deliberately loud and deliberately greppable: the whole failure mode of #201 is a line that
/// reads like a measurement. This word is what stops a reader — or an aggregate — from spending it
/// as one.
pub const INSTRUMENT_FAULT: &str = "INSTRUMENT FAULT";

/// Is this finished fight's own summary self-contradictory?
///
/// `Some(reason)` when the outcome says the player went down and the last player reading says they
/// were untouched. Everything else is `None`: a lost fight that ends on a real sub-100% reading is
/// a result, and `BOSS DOWN` / `unresolved` at full HP are both ordinary (you can win a fight
/// without being hit, and a bar can vanish for a dozen innocent reasons).
///
/// 🛑 `max <= 0` IS NOT A FAULT HERE. [`Hp::pct`] returns 0 for an unpopulated max, which is
/// already "not 100%", and a fight whose max never populated is a different defect with a
/// different owner. This guard answers one question and does not grow a second job — the same
/// argument [`crate::death_guard`]'s keeper test makes.
pub fn end_instrument_fault(outcome: Outcome, last: Option<(Hp, Hp)>) -> Option<&'static str> {
    let (_, player) = last?;
    if outcome == Outcome::PlayerDown && player.pct() >= 100 {
        return Some(
            "outcome=PLAYER DOWN with the last player sample at 100% is impossible; the player HP \
             channel did not measure this fight (client#201)",
        );
    }
    None
}

/// The pair to remember when the death guard trips.
///
/// The death tick already holds a player reading — [`crate::death_guard::lists_unsafe_to_touch`]
/// takes it as its own input — and it is the only reading in the fight taken *after* the damage
/// that ended it. Keeping it costs nothing and is the difference between an END line that
/// contradicts itself and one that says `player 0/414 (0%)`.
///
/// 🛑 THE BOSS HALF IS LEFT ALONE. Re-reading the boss here would mean walking the live character
/// sets on precisely the tick `lists_unsafe_to_touch` exists to forbid walking them on — the death
/// cam teardown that CTD'd this crate once already. `None` in means `None` out: a fight whose boss
/// was never once readable has nothing to pair the player reading with, and inventing a boss
/// reading to hang it off would be a fabricated observation.
pub fn remember_on_death(last: Option<(Hp, Hp)>, player: Hp) -> Option<(Hp, Hp)> {
    last.map(|(boss, _)| (boss, player))
}

#[cfg(test)]
mod replay {
    use super::*;
    use crate::boss_fight_sample::{classify, format_end};

    /// bobler's two Metyr fights, verbatim from the 2026-08-14 log: `(elapsed_ms, unseen_ms, last
    /// boss, last player)`. Both `outcome=PLAYER DOWN`.
    const METYR_LOST_FIGHTS: [(u64, u64, Hp, Hp); 2] = [
        (
            140_700,
            61_700,
            Hp {
                cur: 55821,
                max: 56586,
            },
            Hp { cur: 414, max: 414 },
        ),
        (
            342_200,
            35_200,
            Hp {
                cur: 53340,
                max: 56586,
            },
            Hp { cur: 414, max: 414 },
        ),
    ];

    /// ⭐ THE RED-FIRST ASSERTION. Written and run against the pre-fix build, where it fails: the
    /// END line for a `PLAYER DOWN` + `100%` pair is formatted as an ordinary result.
    ///
    /// It is stated through [`format_end`] rather than through [`end_instrument_fault`] alone on
    /// purpose — the issue asks for the fault to be *logged as a fault*, and a guard that computes
    /// a verdict nobody prints is the same silent instrument #201 is about.
    #[test]
    fn player_down_at_full_hp_is_logged_as_a_fault_not_a_result() {
        for (elapsed_ms, unseen_ms, boss, player) in METYR_LOST_FIGHTS {
            let line = format_end(
                35_600_972,
                Outcome::PlayerDown,
                elapsed_ms,
                unseen_ms,
                Some((boss, player)),
            );
            assert!(
                line.contains(INSTRUMENT_FAULT),
                "a PLAYER DOWN fight whose last player sample is {}% must be marked \
                 {INSTRUMENT_FAULT}, not reported as a measurement -- got: {line}",
                player.pct()
            );
        }
    }

    /// The guard fires on the synthetic pair the issue names, and the reason names the issue.
    #[test]
    fn guard_fires_on_the_synthetic_pair() {
        let fault = end_instrument_fault(
            Outcome::PlayerDown,
            Some((Hp::new(55821, 56586), Hp::new(414, 414))),
        );
        assert!(fault.is_some(), "PLAYER DOWN at 100% must fire the guard");
        assert!(
            fault.unwrap().contains("client#201"),
            "the fault line has to say where to read about itself"
        );
    }

    /// 🛑 THE GUARD MUST NOT EAT REAL RESULTS. A lost fight that ends on a genuine sub-100%
    /// reading is exactly what the instrument is for, and a guard that flagged it would be worse
    /// than no guard: it would teach a reader to ignore the word.
    #[test]
    fn a_lost_fight_with_a_real_reading_is_not_a_fault() {
        for player in [Hp::new(0, 414), Hp::new(103, 414), Hp::new(413, 414)] {
            assert_eq!(
                end_instrument_fault(Outcome::PlayerDown, Some((Hp::new(55821, 56586), player))),
                None,
                "player {}/{} ({}%) is a measurement, not a fault",
                player.cur,
                player.max,
                player.pct()
            );
        }
    }

    /// Winning without being hit is ordinary, and so is an unresolved bar at full HP. Only
    /// `PLAYER DOWN` makes 100% impossible.
    #[test]
    fn only_player_down_makes_full_hp_impossible() {
        let untouched = Some((Hp::new(0, 56586), Hp::new(414, 414)));
        assert_eq!(end_instrument_fault(Outcome::BossDown, untouched), None);
        assert_eq!(end_instrument_fault(Outcome::Unresolved, untouched), None);
        assert!(end_instrument_fault(Outcome::PlayerDown, untouched).is_some());
    }

    /// A fight whose boss was never readable has no pair to check, and must not be reported as a
    /// fault — `format_end` already says "last unread" for it, which is the honest line.
    #[test]
    fn no_reading_is_not_a_fault() {
        assert_eq!(end_instrument_fault(Outcome::PlayerDown, None), None);
    }

    /// The death-tick reading replaces the player half and leaves the boss half untouched.
    #[test]
    fn remember_on_death_keeps_the_boss_and_replaces_the_player() {
        let boss = Hp::new(55821, 56586);
        let before = Some((boss, Hp::new(414, 414)));
        assert_eq!(
            remember_on_death(before, Hp::new(0, 414)),
            Some((boss, Hp::new(0, 414))),
            "the boss half must survive: the death tick is the one tick the live sets may not be \
             walked on"
        );
        assert_eq!(
            remember_on_death(None, Hp::new(0, 414)),
            None,
            "a fight whose boss was never read has nothing to pair the reading with"
        );
    }

    /// Overkill drives HP negative before teardown; the remembered reading carries it as-is and
    /// [`Hp::pct`] floors it at 0, so the END line reads `0%` rather than something negative.
    #[test]
    fn overkill_is_remembered_and_reads_as_zero_percent() {
        let remembered = remember_on_death(
            Some((Hp::new(53340, 56586), Hp::new(414, 414))),
            Hp::new(-77, 414),
        );
        let (_, player) = remembered.unwrap();
        assert_eq!(player.cur, -77, "the raw reading is not rounded on the way in");
        assert_eq!(player.pct(), 0);
        assert_eq!(
            end_instrument_fault(Outcome::PlayerDown, remembered),
            None,
            "an overkill death is a measurement"
        );
    }

    /// ⭐ END TO END, THE WAY THE PROBE RUNS IT: the death guard trips, the reading is remembered,
    /// the outcome is classified from the latch, and the line that comes out is a result rather
    /// than a fault. This is the shape bobler's log should have had.
    #[test]
    fn the_fixed_path_produces_a_result_and_the_broken_one_produces_a_fault() {
        let boss = Hp::new(55821, 56586);
        let pre_death = Some((boss, Hp::new(414, 414)));

        // What the probe does today: the death-tick reading is discarded.
        let discarded = format_end(
            35_600_972,
            classify(Some(boss), true),
            140_700,
            61_700,
            pre_death,
        );
        assert!(
            discarded.contains(INSTRUMENT_FAULT),
            "discarding the death reading has to be visible in the log: {discarded}"
        );

        // What it does once the death tick is remembered.
        let kept = format_end(
            35_600_972,
            classify(Some(boss), true),
            140_700,
            61_700,
            remember_on_death(pre_death, Hp::new(0, 414)),
        );
        assert!(
            !kept.contains(INSTRUMENT_FAULT),
            "a real death reading is a result: {kept}"
        );
        assert!(
            kept.contains("player 0/414 (0%)"),
            "the END line has to quote the reading that ended the fight: {kept}"
        );
        assert!(
            kept.contains("outcome=PLAYER DOWN"),
            "remembering the reading must not change the outcome: {kept}"
        );
    }
}
