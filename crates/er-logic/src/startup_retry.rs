//! Retry policy for the game-singleton lookups the client performs at startup.
//!
//! # Motivating case
//!
//! Players see a fatal `Could not translate RVA to VA` modal over an otherwise healthy game --
//! reported on Windows in `4laric/er-archipelago#475`, and hit intermittently by three separate
//! players: one who had launched fine "2-3 times" before it appeared, one whose install worked on
//! a laptop but not on a newer, faster PC, and one running a second overlay mod alongside ours.
//!
//! The message is misleading. It is `SystemInitError::InvalidRva`, and the path that reaches it is
//! not an address translation at all:
//!
//! * `from-singleton`'s `map()` documents itself as "may not contain all singletons if it is
//!   called before Dantelion2 reflection is initialized by the process", and its `all_null()`
//!   decides that by reading the *live* pointers. Called early it hands back a partial map with no
//!   `CSTask` in it, so the lookup returns `InstanceError::NotFound`.
//! * `CSTaskImp::wait_for_instance` retries `InstanceError::Null` in a loop but *returns* on
//!   `NotFound` -- so the branch the dependency documents as transient is the one treated as
//!   permanent, while the branch meaning "found it, it just is not built yet" gets the patience.
//! * `wait_for_system_init` only waits on the CSWindow `hInstance`, which is populated earlier than
//!   Dantelion2 reflection, so returning from it does not mean the map is ready.
//!
//! The window is small, which is why it is intermittent, and anything that shifts startup timing
//! (disk speed, machine load, another native loading alongside ours) can land inside it.
//!
//! # What this module is
//!
//! The policy half -- pure, host-tested, no game. The Windows-side caller
//! (`eldenring-archipelago`'s `game::run_recurring_task`) owns the attempt and the sleeping; this
//! decides whether there is any point trying again, and words the give-up message when there is not.

use std::time::Duration;

/// How long to keep re-asking the game for a singleton before treating the failure as real.
///
/// Generous on purpose: the cost of waiting too long is a late error modal, and the cost of giving
/// up too early is the bug this module exists to fix.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(30);

/// How long to wait between attempts. Short enough that a player never notices the recovery.
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(50);

/// How long a single attempt may block before we count it as failed and consult the policy.
///
/// This exists because the caller must NOT pass `Duration::MAX`: with an infinite per-attempt
/// timeout `SystemInitError::Timeout` can never be constructed, so every failure in the path --
/// including a genuine "the game never came up" -- arrives wearing the `InvalidRva` text.
pub const DEFAULT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);

/// Whether a failed singleton lookup is worth repeating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total time from the first attempt after which we stop.
    pub deadline: Duration,
    /// Gap between attempts.
    pub interval: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            deadline: DEFAULT_DEADLINE,
            interval: DEFAULT_INTERVAL,
        }
    }
}

/// What the caller should do after an attempt failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Sleep for this long, then attempt again.
    RetryAfter(Duration),
    /// The deadline has passed. Report the failure to the player.
    GiveUp,
}

impl RetryPolicy {
    /// Decides what to do after a failed attempt, given how long we have been trying.
    ///
    /// The returned delay is clamped so sleeping it can never carry us past the deadline --
    /// otherwise a long interval would let the real wait overshoot by most of an interval.
    pub fn after_failure(&self, elapsed: Duration) -> Next {
        match self.deadline.checked_sub(elapsed) {
            None | Some(Duration::ZERO) => Next::GiveUp,
            Some(remaining) => Next::RetryAfter(self.interval.min(remaining)),
        }
    }

    /// The player-facing text for a failure that outlived the deadline.
    ///
    /// Deliberately says what to try, because "quit and launch again" genuinely clears this and
    /// nothing in the client's own state has to change for it to work. ASCII only: this renders in
    /// the in-game overlay.
    pub fn give_up_message(&self, attempts: u32, elapsed: Duration, cause: &str) -> String {
        format!(
            "The game did not finish registering its internal objects, so the Archipelago client could not start. Tried {attempts} times over {:.1}s. This is a startup timing problem rather than a broken install -- quitting and launching the game again usually clears it. If it happens on every launch, please report it with your archipelago-<date>.log and the list of other DLL mods you load. Underlying error: {cause}",
            elapsed.as_secs_f32(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_while_inside_the_deadline() {
        let policy = RetryPolicy::default();

        assert_eq!(
            policy.after_failure(Duration::ZERO),
            Next::RetryAfter(DEFAULT_INTERVAL),
        );
        assert_eq!(
            policy.after_failure(Duration::from_secs(29)),
            Next::RetryAfter(DEFAULT_INTERVAL),
        );
    }

    /// The motivating case: the first attempt fails because the map is still empty, and the policy
    /// has to say "try again" rather than surfacing that as fatal.
    #[test]
    fn a_failure_on_the_first_attempt_is_not_fatal() {
        assert!(matches!(
            RetryPolicy::default().after_failure(Duration::ZERO),
            Next::RetryAfter(_),
        ));
    }

    #[test]
    fn gives_up_once_the_deadline_is_reached() {
        let policy = RetryPolicy::default();

        assert_eq!(policy.after_failure(DEFAULT_DEADLINE), Next::GiveUp);
        assert_eq!(policy.after_failure(Duration::from_secs(31)), Next::GiveUp);
    }

    /// A long interval must not let the wait run past the deadline.
    #[test]
    fn the_last_delay_is_clamped_to_what_is_left() {
        let policy = RetryPolicy {
            deadline: Duration::from_secs(10),
            interval: Duration::from_secs(4),
        };

        assert_eq!(
            policy.after_failure(Duration::from_secs(9)),
            Next::RetryAfter(Duration::from_secs(1)),
        );
    }

    /// A zero deadline still lets the caller make its one attempt -- it just never repeats it.
    #[test]
    fn a_zero_deadline_gives_up_immediately() {
        let policy = RetryPolicy {
            deadline: Duration::ZERO,
            interval: DEFAULT_INTERVAL,
        };

        assert_eq!(policy.after_failure(Duration::ZERO), Next::GiveUp);
    }

    #[test]
    fn the_give_up_message_tells_the_player_what_to_do() {
        let message = RetryPolicy::default().give_up_message(
            7,
            Duration::from_millis(2500),
            "Could not translate RVA to VA",
        );

        assert!(message.contains("7 times"), "{message}");
        assert!(message.contains("2.5s"), "{message}");
        assert!(message.contains("launching the game again"), "{message}");
        assert!(
            message.contains("Could not translate RVA to VA"),
            "{message}"
        );
    }

    /// The overlay renders this, and the overlay is ASCII-only.
    #[test]
    fn the_give_up_message_is_ascii() {
        let message =
            RetryPolicy::default().give_up_message(1, Duration::from_secs(30), "some cause");

        assert!(message.is_ascii(), "{message}");
    }
}
