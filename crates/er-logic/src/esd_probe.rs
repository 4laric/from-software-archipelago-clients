//! `esd_probe` -- what a log-only ESD talk-event probe says, and how loud it is allowed to be.
//!
//! # Why this exists (er-archipelago#455, phase 1)
//!
//! Shop auto-hints want to fire when the player opens a merchant. The dispatch point is the game's
//! own ESD talk-event `invoke`, and `OPEN_REGULAR_SHOP = 22` is documented in the pinned crate's
//! `invoke-esd` example -- but "documented in an example" is not "observed in our build, at Kale,
//! with a usable row range". Phase 1 buys that observation and nothing else: install the detour,
//! log, do not hint.
//!
//! The falsifier is deliberately cheap. If no `id=22` with a sane ShopLineupParam range shows up
//! when Kale's shop opens, the whole feature dies for the cost of one build, before any hint logic
//! or any hot-path hint work exists to throw away.
//!
//! # The problem this module actually solves: VOLUME
//!
//! `invoke` is the single dispatch for EVERY ESD talk event -- every line of NPC dialogue, every
//! menu list rebuild, every idle chatter check, many times a second while a conversation is open.
//! Logging all of them produces a file nobody can grep and a probe that perturbs the thing it is
//! measuring. Logging a fixed sample instead would be worse: the sample could miss the one shop
//! open the whole exercise exists to witness.
//!
//! So the rule is asymmetric, and the asymmetry IS the design:
//!
//! * **Watched commands** ([`WATCHED`] -- the shop opens) log **every single time**, with args.
//!   They are rare, and they are the datum we came for. Nothing may suppress them.
//! * **Everything else** logs **once per `(talk_id, event_id)` pair**. That is exactly the
//!   enumeration the issue asks for -- which command ids exist, and which merchant's talkscript
//!   emits them -- at one line per distinct pair instead of one per dispatch.
//!
//! # Why the pair, not the event id alone
//!
//! The whitelist we are building is per-merchant: Twin Maidens, Enia, Dragon Communion and the
//! Ash-of-War vendor are all "a shop" and may not all open through command 22. Keying the ledger
//! on the event id alone would log the first merchant's `22` and then silently swallow every other
//! merchant's, which is the precise failure that would make us think the whitelist was complete
//! when it was not. `talk_id` is the discriminator, so it belongs in the key.
//!
//! # What this module does NOT do
//!
//! It does not decide anything about hints, and it does not interpret an argument. Phase 2 (the
//! `plan_shop_hints` pure function) is a separate change gated on what this probe reports. Baking
//! a guess about the row range in here would put an unverified premise in the very artifact that
//! exists to verify it.

use std::collections::HashSet;

/// `OPEN_REGULAR_SHOP` -- the buy menu. Named in the pinned crate's `examples/invoke-esd`; its
/// args are believed to be a ShopLineupParam row RANGE, which is what phase 1 is checking.
pub const OPEN_REGULAR_SHOP: i32 = 22;

/// `OPEN_SELL_SHOP` -- the sell menu. Logged alongside 22 because a merchant that opens both tells
/// us the pair fires together, and because `shop_sell` already has a stake in the sell side.
pub const OPEN_SELL_SHOP: i32 = 46;

/// Whether enough monotonic time has elapsed since the last ESD talk dispatch for inventory reads
/// and grants to be trusted. `last_activity_ms == 0` means the hook has not observed any talk yet.
/// Kept pure so the live hook's clock policy is host-tested rather than sleep-tested.
pub fn inventory_quiet(now_ms: u64, last_activity_ms: u64, quiet_ms: u64) -> bool {
    last_activity_ms == 0 || now_ms.saturating_sub(last_activity_ms) >= quiet_ms
}

/// Commands that log on EVERY dispatch rather than once. Rare by nature; never suppressed, not
/// even by [`DISTINCT_PAIR_CAP`].
pub const WATCHED: [i32; 2] = [OPEN_REGULAR_SHOP, OPEN_SELL_SHOP];

/// Ceiling on distinct `(talk_id, event_id)` pairs the ledger will remember and announce.
///
/// This bounds both the log and the memory a session can accrue on a hot path. It is a blast
/// radius limit, not a business rule -- a real session is expected to sit far below it, and
/// [`EsdProbeLedger::suppressed`] says so out loud if it does not.
pub const DISTINCT_PAIR_CAP: usize = 512;

/// What the caller should do with one observed dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeAction {
    /// A watched command. Log it with its arguments, every time, forever.
    LogWatched,
    /// First sighting of this `(talk_id, event_id)` pair. Log it once, with its arguments.
    LogFirstSighting,
    /// Seen before, or past the cap. Say nothing.
    Skip,
}

/// Remembers which `(talk_id, event_id)` pairs have already been announced.
#[derive(Debug, Default)]
pub struct EsdProbeLedger {
    seen: HashSet<(i32, i32)>,
    suppressed: u64,
}

impl EsdProbeLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Classify one dispatch, recording it as seen.
    ///
    /// A watched command short-circuits BEFORE the cap check, so the cap can never eat the datum
    /// the probe exists to capture. It is still recorded, so the pair count stays honest.
    pub fn observe(&mut self, talk_id: i32, event_id: i32) -> ProbeAction {
        let key = (talk_id, event_id);
        if WATCHED.contains(&event_id) {
            // Insert for the census, but the return value does not depend on it: a second shop
            // open is exactly as interesting as the first.
            if self.seen.len() < DISTINCT_PAIR_CAP {
                self.seen.insert(key);
            }
            return ProbeAction::LogWatched;
        }
        if self.seen.contains(&key) {
            return ProbeAction::Skip;
        }
        if self.seen.len() >= DISTINCT_PAIR_CAP {
            self.suppressed = self.suppressed.saturating_add(1);
            return ProbeAction::Skip;
        }
        self.seen.insert(key);
        ProbeAction::LogFirstSighting
    }

    /// Distinct pairs recorded.
    pub fn distinct(&self) -> usize {
        self.seen.len()
    }

    /// Dispatches dropped because the ledger was full.
    ///
    /// Non-zero means the enumeration in the log is INCOMPLETE. A reader who does not know that
    /// would conclude the command-id inventory is finished when it is truncated, so whoever
    /// renders this must render it.
    pub fn suppressed(&self) -> u64 {
        self.suppressed
    }

    /// Whether the ledger is at its ceiling.
    pub fn is_full(&self) -> bool {
        self.seen.len() >= DISTINCT_PAIR_CAP
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE MOTIVATING CASE (er-archipelago#455): Alaric opens Kale, and command 22 reaches the log.
    #[test]
    fn opening_kale_reports_the_shop_command() {
        let mut ledger = EsdProbeLedger::new();
        assert_eq!(
            ledger.observe(1_000, OPEN_REGULAR_SHOP),
            ProbeAction::LogWatched
        );
    }

    /// The acceptance test for the asymmetry: a SECOND shop open is not a duplicate to be
    /// swallowed. If the whole point is reading the row range off a real run, then re-opening the
    /// shop after a bell turn-in must produce a second line -- that is how the tranche behaviour
    /// gets observed at all.
    #[test]
    fn a_watched_command_logs_every_time_not_once() {
        let mut ledger = EsdProbeLedger::new();
        for _ in 0..5 {
            assert_eq!(
                ledger.observe(1_000, OPEN_REGULAR_SHOP),
                ProbeAction::LogWatched
            );
        }
        assert_eq!(ledger.distinct(), 1);
    }

    #[test]
    fn an_ordinary_command_logs_once_then_goes_quiet() {
        let mut ledger = EsdProbeLedger::new();
        assert_eq!(ledger.observe(1_000, 19), ProbeAction::LogFirstSighting);
        assert_eq!(ledger.observe(1_000, 19), ProbeAction::Skip);
        assert_eq!(ledger.observe(1_000, 19), ProbeAction::Skip);
    }

    /// The whitelist is per-merchant, so the same command id under a different talkscript is a NEW
    /// sighting. Keying on the event id alone would hide every merchant after the first.
    #[test]
    fn the_same_command_under_another_talkscript_is_a_new_sighting() {
        let mut ledger = EsdProbeLedger::new();
        assert_eq!(ledger.observe(1_000, 19), ProbeAction::LogFirstSighting);
        assert_eq!(ledger.observe(2_000, 19), ProbeAction::LogFirstSighting);
        assert_eq!(ledger.distinct(), 2);
    }

    #[test]
    fn past_the_cap_new_pairs_are_dropped_and_counted() {
        let mut ledger = EsdProbeLedger::new();
        for i in 0..DISTINCT_PAIR_CAP as i32 {
            assert_eq!(ledger.observe(i, 19), ProbeAction::LogFirstSighting);
        }
        assert!(ledger.is_full());
        assert_eq!(ledger.observe(999_999, 19), ProbeAction::Skip);
        assert_eq!(ledger.suppressed(), 1);
        // A pair recorded BEFORE the cap still reads as seen, not as suppressed.
        assert_eq!(ledger.observe(0, 19), ProbeAction::Skip);
        assert_eq!(ledger.suppressed(), 1);
    }

    /// The cap is a blast-radius limit on CHATTER. It must never silence the falsifier -- a probe
    /// that fills up on dialogue and then swallows the shop open would report nothing and look
    /// like a refutation.
    #[test]
    fn the_cap_never_silences_a_watched_command() {
        let mut ledger = EsdProbeLedger::new();
        for i in 0..DISTINCT_PAIR_CAP as i32 {
            ledger.observe(i, 19);
        }
        assert!(ledger.is_full());
        assert_eq!(
            ledger.observe(999_999, OPEN_REGULAR_SHOP),
            ProbeAction::LogWatched
        );
        assert_eq!(
            ledger.observe(999_998, OPEN_SELL_SHOP),
            ProbeAction::LogWatched
        );
        assert_eq!(ledger.suppressed(), 0);
    }

    #[test]
    fn a_fresh_ledger_has_nothing_to_report() {
        let ledger = EsdProbeLedger::new();
        assert_eq!(ledger.distinct(), 0);
        assert_eq!(ledger.suppressed(), 0);
        assert!(!ledger.is_full());
    }

    #[test]
    fn inventory_is_safe_before_any_talk_activity() {
        assert!(inventory_quiet(50_000, 0, 2_000));
    }

    #[test]
    fn talk_activity_holds_inventory_until_the_quiet_window_expires() {
        assert!(!inventory_quiet(10_000, 9_999, 2_000));
        assert!(!inventory_quiet(11_998, 9_999, 2_000));
        assert!(inventory_quiet(11_999, 9_999, 2_000));
        // A defensive saturating subtraction keeps a clock anomaly fail-closed.
        assert!(!inventory_quiet(9_998, 9_999, 2_000));
    }
}
