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

/// The item-name prefix every trap carries. The world mints synthetic items (`ITEMS` with no
/// `ITEM_GRANTS`) and the client recognises them HERE, by name, exactly as it recognises
/// `Boss Key: <Boss>`. That is what keeps traps off the contract entirely -- no slot_data key, no
/// `CONTRACT_HASH` move, no version lockstep.
pub const ITEM_PREFIX: &str = "Trap: ";

impl Trap {
    /// The item name the world mints for this trap.
    ///
    /// 🛑 CROSS-REPO STRING CONTRACT WITH NO GATE BEHIND IT. `greenfield/eldenring/features/traps.py`
    /// carries the same two strings, and its `test_gf_traps` pins them literally. Change one side
    /// and NOTHING breaks: the item still arrives, is still filler, and silently never fires.
    pub fn item_name(self) -> &'static str {
        match self {
            Trap::RuneThief => "Trap: Rune Thief",
            Trap::NoFlask => "Trap: No Flask",
        }
    }

    /// The trap a received item name denotes, or `None` for anything else.
    ///
    /// Exact match, not "starts with the prefix": an unknown `Trap: ...` name is a world newer than
    /// this client, and firing the wrong effect would be worse than firing none. The caller logs it.
    pub fn from_item_name(name: &str) -> Option<Self> {
        ALL.iter().copied().find(|t| t.item_name() == name)
    }
}

/// Every trap this build can fire. One place, so a new variant cannot be half-added.
pub const ALL: [Trap; 2] = [Trap::RuneThief, Trap::NoFlask];

/// Rune Thief's new total: half, rounded down.
///
/// Saturating by construction (`u32 / 2`), so there is no underflow branch to get wrong and a
/// player at 0 or 1 rune simply stays where they are. Split out from the client purely so the
/// arithmetic can be read and tested without a game.
pub fn rune_thief_target(current: u32) -> u32 {
    current / 2
}

/// A trap that arrived while the player could not receive it.
///
/// 🛑 WHY A QUEUE AND NOT A RETURN VALUE. Fired from a HOTKEY, "cannot act right now" is fine: the
/// player presses the key again. Fired from an ITEM, it is a LOSS -- the item is already marked
/// received, the server will never resend it, and a trap that quietly evaporated is indistinguishable
/// from a trap that was never in the pool. Issue #114 rule 2: never fire while the player is not in
/// control, DEFER with a starvation cap, and never cancel.
///
/// Deliberately NOT a timer. It holds names and one clock reading; the caller polls it on the tick
/// it already runs, which is the same shape `attunement_replay` uses for the deferred boss payout.
#[derive(Debug, Default)]
pub struct TrapQueue {
    pending: Vec<Trap>,
    /// When the head of the queue started waiting, for [`Self::overdue`]. `None` when empty.
    waiting_since_ms: Option<u64>,
}

/// How long a trap may sit undeliverable before the client says so out loud, in ms.
///
/// Mirrors the boss-defer cap. It is a REPORTING threshold, not a deadline: nothing is dropped when
/// it passes, because the alternative to holding is losing the item outright.
pub const DEFER_WARN_MS: u64 = 30_000;

impl TrapQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept a trap for delivery. FIFO -- two traps in one batch fire in the order they arrived.
    pub fn push(&mut self, trap: Trap, now_ms: u64) {
        if self.pending.is_empty() {
            self.waiting_since_ms = Some(now_ms);
        }
        self.pending.push(trap);
    }

    /// The next trap to fire, or `None` while the player cannot receive one.
    ///
    /// `can_fire` is the CALLER's judgement (in world, alive, settled) -- er-logic does not reach
    /// into the game to form it. One per poll, so a batch of five does not land as one event the
    /// player cannot parse.
    pub fn poll(&mut self, now_ms: u64, can_fire: bool) -> Option<Trap> {
        if !can_fire || self.pending.is_empty() {
            return None;
        }
        let trap = self.pending.remove(0);
        self.waiting_since_ms = (!self.pending.is_empty()).then_some(now_ms);
        Some(trap)
    }

    /// Has the head waited longer than [`DEFER_WARN_MS`]? For ONE log line, not for dropping.
    pub fn overdue(&self, now_ms: u64) -> bool {
        match self.waiting_since_ms {
            Some(t) => now_ms.saturating_sub(t) >= DEFER_WARN_MS,
            None => false,
        }
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
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
    // ---- the queue -----------------------------------------------------------------------------

    #[test]
    fn a_trap_that_cannot_fire_is_held_not_dropped() {
        let mut q = TrapQueue::new();
        q.push(Trap::RuneThief, 0);
        // Not in world for a full minute: polled repeatedly, never delivered, never lost.
        for t in (0..60_000).step_by(1_000) {
            assert_eq!(q.poll(t, false), None);
        }
        assert_eq!(
            q.len(),
            1,
            "the trap was dropped -- the server will never resend it"
        );
        assert_eq!(q.poll(60_000, true), Some(Trap::RuneThief));
        assert!(q.is_empty());
    }

    #[test]
    fn traps_fire_in_arrival_order_one_per_poll() {
        let mut q = TrapQueue::new();
        q.push(Trap::RuneThief, 0);
        q.push(Trap::NoFlask, 0);
        // One per poll: a batch must not land as a single unreadable event.
        assert_eq!(q.poll(10, true), Some(Trap::RuneThief));
        assert_eq!(q.poll(10, true), Some(Trap::NoFlask));
        assert_eq!(q.poll(10, true), None);
    }

    #[test]
    fn overdue_reports_and_does_not_drop() {
        let mut q = TrapQueue::new();
        assert!(!q.overdue(1_000_000), "an empty queue is never overdue");
        q.push(Trap::NoFlask, 1_000);
        assert!(!q.overdue(1_000 + DEFER_WARN_MS - 1));
        assert!(q.overdue(1_000 + DEFER_WARN_MS));
        // 🛑 The cap is a REPORTING threshold. Passing it must not lose the trap.
        assert_eq!(q.len(), 1);
        assert_eq!(q.poll(9_999_999, true), Some(Trap::NoFlask));
    }

    #[test]
    fn the_wait_clock_restarts_for_the_next_trap_in_line() {
        let mut q = TrapQueue::new();
        q.push(Trap::RuneThief, 0);
        q.push(Trap::NoFlask, 0);
        assert_eq!(q.poll(DEFER_WARN_MS, true), Some(Trap::RuneThief));
        // The survivor has only just reached the head; it is not instantly overdue on the old clock.
        assert!(
            !q.overdue(DEFER_WARN_MS),
            "the second trap inherited the first one's wait"
        );
    }

    // ---- the cross-repo name contract ----------------------------------------------------------

    /// 🛑 THE STRINGS `greenfield/eldenring/features/traps.py` MINTS. Nothing enforces this across
    /// the repo boundary -- change one side and the item silently never fires -- so both sides pin
    /// the literals and `test_gf_traps` is the other half of this test.
    #[test]
    fn item_names_are_the_ones_the_world_mints() {
        assert_eq!(Trap::RuneThief.item_name(), "Trap: Rune Thief");
        assert_eq!(Trap::NoFlask.item_name(), "Trap: No Flask");
    }

    #[test]
    fn every_trap_round_trips_through_its_item_name() {
        assert_eq!(ALL.len(), 2, "WITNESS: nothing was swept");
        for t in ALL {
            assert_eq!(Trap::from_item_name(t.item_name()), Some(t));
            assert!(t.item_name().starts_with(ITEM_PREFIX), "{}", t.item_name());
            assert!(t.item_name().is_ascii());
        }
    }

    /// An unknown `Trap: ...` is a WORLD NEWER THAN THIS CLIENT. Refusing is right: firing the
    /// wrong effect would be worse than firing none, and the caller logs the name.
    #[test]
    fn an_unknown_trap_name_is_refused_not_guessed() {
        assert_eq!(Trap::from_item_name("Trap: Reversed Controls"), None);
        assert_eq!(Trap::from_item_name("Boss Key: Godrick"), None);
        assert_eq!(Trap::from_item_name("Smithing Stone [1]"), None);
        assert_eq!(Trap::from_item_name(""), None);
    }
}
