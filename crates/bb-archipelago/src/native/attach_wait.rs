//! Bounded wait for a *fresh* shadPS4 eboot base (clients#418).
//!
//! The launcher spawns shadPS4 and the client as simultaneous siblings, and the
//! shad log is appended within a run. Reading it once at client startup
//! therefore yields the PREVIOUS run's `base_virtual_addr` -- a different,
//! unmapped base -- so `verify_base` reads an unmapped page and the client dies
//! with an error that blames the player's game build. The failure is guaranteed
//! by ordering, not by the build.
//!
//! The log is appended within a run but *truncated* at each launch (clients#440:
//! a playtester's file shrank 644KB to 577KB across a relaunch, and the eboot
//! base line sits near the top). So the freshness floor below is not monotonic:
//! when the client wins the race and records the previous run's large length,
//! shadPS4 then truncates and writes this run's base near offset 0, below the
//! floor. The wait therefore follows the file the way `tail -F` does -- a length
//! that has *shrunk* below the floor means rotation, and the floor resets to 0.
//! The truncated file's last base line is by definition this run's, so the same
//! poll's lookup against the reset floor is the fast-path check re-run.
//!
//! [`wait_for_verified_base`] fixes that ordering. It first tries the base the
//! log already carries -- when shadPS4 is up before the client that base is the
//! current one and live verification proves it -- and only when that base cannot
//! be confirmed does it wait: poll the log on an interval within a bounded
//! budget and accept a base only from a base line written *past* the log length
//! recorded at attach start. A stale line sits before that offset and can never
//! satisfy the wait.
//!
//! The three terminal outcomes are deliberately distinct, because they send the
//! player to three different places:
//!
//! * a verified, validated base -- attach proceeds;
//! * [`AttachWaitFailure::NoFreshBase`] -- no base line ever appeared past the
//!   freshness floor. Since a truncation resets that floor to 0, "the base is
//!   there but sits below the floor" is impossible by construction: this
//!   outcome now means nothing base-shaped was ever written to the file, so the
//!   configured `shad_log` is probably the wrong file. The
//!   message names the configured path and says what to compare it against, and
//!   deliberately says nothing about the Cheat Engine bridge: the build is not
//!   the suspect ([`AttachWaitFailure::is_stale_log_evidence`] is what lets
//!   `main.rs` withhold the build guidance);
//! * [`AttachWaitFailure::ImageRejected`] -- a base *was* confirmed and the
//!   image behind it did not match the contract. That is a genuinely
//!   unrecognised build, and `main.rs` appends the CE-bridge guidance to it.
//!
//! The loop is host-tested: time, sleeping and the log contents are all
//! injected, so a test appends a fresh base line "mid-wait" without a game, a
//! clock or a sleep.

use std::fmt;
use std::time::{Duration, Instant};

/// Poll interval and total budget for the fresh-base wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WaitPolicy {
    pub interval: Duration,
    pub budget: Duration,
}

impl Default for WaitPolicy {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            budget: Duration::from_secs(90),
        }
    }
}

/// Injected time: the wait never calls [`Instant::now`] or `std::thread::sleep`
/// directly, so tests drive the budget deterministically.
pub trait AttachClock {
    fn elapsed(&self) -> Duration;
    fn sleep(&mut self, duration: Duration);
}

/// The real clock: elapsed since construction, sleeping the calling thread.
#[derive(Debug)]
pub struct SystemAttachClock {
    start: Instant,
}

impl Default for SystemAttachClock {
    fn default() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl AttachClock for SystemAttachClock {
    fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// What a candidate base turned out to be when checked against live memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BaseCheck {
    /// Hook originals matched and the image validated: attach at this base.
    Attached,
    /// The base could not be confirmed (stale line, page not mapped yet, read
    /// error). Retryable -- keep waiting for a fresh base line.
    Unverified,
    /// The base *was* confirmed and the image behind it is not the contract's.
    /// Terminal: waiting longer cannot change a build.
    ImageRejected(String),
}

/// The two terminal failures of the wait, kept apart so the operator message can
/// be, too.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttachWaitFailure {
    /// The budget expired without any base line appearing past the freshness
    /// floor -- and since a truncation resets that floor to 0, without any base
    /// line appearing at all: the configured log is probably not the running
    /// shadPS4's log.
    NoFreshBase { shad_log: String, waited: Duration },
    /// A confirmed base whose image did not match the contract.
    ImageRejected { base: u64, detail: String },
}

impl AttachWaitFailure {
    /// True when the evidence points at the configured log path rather than at
    /// the game build. `main.rs` uses this to withhold the "load the Cheat
    /// Engine table" guidance, which would be wrong advice here.
    pub fn is_stale_log_evidence(&self) -> bool {
        matches!(self, Self::NoFreshBase { .. })
    }
}

impl fmt::Display for AttachWaitFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoFreshBase { shad_log, waited } => write!(
                formatter,
                "waited {} seconds but shadPS4 never logged a new eboot base after the client started, \
                 so no base in the log can be trusted to belong to this run. \
                 The configured shad_log is {shad_log} -- check that it is the log of the shadPS4 you actually launched: \
                 a portable install writes user\\log\\shad_log.txt beside shadPS4.exe, \
                 while an installed one writes %APPDATA%\\shadPS4\\user\\log\\shad_log.txt.",
                waited.as_secs()
            ),
            Self::ImageRejected { base, detail } => write!(
                formatter,
                "the eboot image at the confirmed base 0x{base:X} did not validate against the contract: {detail}"
            ),
        }
    }
}

impl std::error::Error for AttachWaitFailure {}

/// The single line printed when the wait actually begins -- once, not per poll.
pub const WAITING_NOTICE: &str = "Waiting for shadPS4 to load the game...";

/// Parse the last logged eboot `base_virtual_addr` whose line begins at or after
/// `min_offset` bytes into the log.
///
/// This is [`super::mem::logged_eboot_base`] with a freshness floor: the offset
/// is what makes a previous run's line unable to satisfy the wait, since it was
/// already in the file when attach recorded the length.
pub fn logged_eboot_base_after(log_text: &str, min_offset: usize) -> Option<u64> {
    let mut offset = 0usize;
    let mut pending = false;
    let mut found = None;
    for raw in log_text.split_inclusive('\n') {
        let line_offset = offset;
        offset += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        if line.contains("Loading module eboot.bin") {
            pending = true;
            continue;
        }
        if pending && let Some(idx) = line.find("base_virtual_addr") {
            let rest = &line[idx..];
            if let Some(hex_start) = rest.find("0x") {
                let hex: String = rest[hex_start + 2..]
                    .chars()
                    .take_while(|c| c.is_ascii_hexdigit())
                    .collect();
                if let Ok(value) = u64::from_str_radix(&hex, 16) {
                    pending = false;
                    if line_offset >= min_offset {
                        found = Some(value);
                    }
                }
            }
        }
    }
    found
}

/// Wait, bounded, for a base this run can be trusted to own.
///
/// `start_log` is the log text read at attach start; its length is the initial
/// freshness floor. `read_log` re-reads the file each poll (a read error is
/// retryable -- the log can be rotated or momentarily locked, so it yields
/// `None`). A poll whose text is *shorter* than the floor means shadPS4
/// truncated the log for this run, so the floor drops to 0 and the same poll
/// reconsiders the whole file (clients#440). `check`
/// runs the live verification (`verify_base` + `require_validated_image`) for a
/// candidate. `notice` receives [`WAITING_NOTICE`] exactly once, when waiting
/// begins.
pub fn wait_for_verified_base<C, L, V, N>(
    shad_log_display: &str,
    start_log: &str,
    mut read_log: L,
    mut check: V,
    mut notice: N,
    clock: &mut C,
    policy: WaitPolicy,
) -> Result<u64, AttachWaitFailure>
where
    C: AttachClock,
    L: FnMut() -> Option<String>,
    V: FnMut(u64) -> BaseCheck,
    N: FnMut(&str),
{
    // The base already in the log: current when shadPS4 started first, stale
    // when it did not. Live verification -- not the file -- decides which.
    if let Some(base) = super::mem::logged_eboot_base(start_log) {
        match check(base) {
            BaseCheck::Attached => return Ok(base),
            BaseCheck::ImageRejected(detail) => {
                return Err(AttachWaitFailure::ImageRejected { base, detail });
            }
            BaseCheck::Unverified => {}
        }
    }

    let mut floor = start_log.len();
    let mut announced = false;
    loop {
        if !announced {
            notice(WAITING_NOTICE);
            announced = true;
        }
        clock.sleep(policy.interval);
        if let Some(text) = read_log() {
            // Rotation, `tail -F` style: a log shorter than the floor was
            // truncated by this run's shadPS4, so every byte in it -- including
            // a base line near offset 0 -- belongs to this run.
            if text.len() < floor {
                floor = 0;
            }
            if let Some(base) = logged_eboot_base_after(&text, floor) {
                match check(base) {
                    BaseCheck::Attached => return Ok(base),
                    BaseCheck::ImageRejected(detail) => {
                        return Err(AttachWaitFailure::ImageRejected { base, detail });
                    }
                    BaseCheck::Unverified => {}
                }
            }
        }
        if clock.elapsed() >= policy.budget {
            return Err(AttachWaitFailure::NoFreshBase {
                shad_log: shad_log_display.to_owned(),
                waited: clock.elapsed(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    const STALE: &str = "Loading module eboot.bin\n  base_virtual_addr ..: 0x5570000\n";
    const FRESH_LINE: &str = "Loading module eboot.bin\n  base_virtual_addr ..: 0x8000000\n";

    /// A clock that only advances when the wait sleeps.
    #[derive(Default)]
    struct FakeClock {
        elapsed: Duration,
        sleeps: u32,
    }

    impl AttachClock for FakeClock {
        fn elapsed(&self) -> Duration {
            self.elapsed
        }

        fn sleep(&mut self, duration: Duration) {
            self.elapsed += duration;
            self.sleeps += 1;
        }
    }

    fn policy() -> WaitPolicy {
        WaitPolicy {
            interval: Duration::from_secs(1),
            budget: Duration::from_secs(10),
        }
    }

    #[test]
    fn a_base_appended_mid_wait_is_accepted_while_the_stale_one_is_not() {
        let log = Rc::new(RefCell::new(String::from(STALE)));
        let appender = Rc::clone(&log);
        let mut polls = 0u32;
        let mut clock = FakeClock::default();
        let mut notices = Vec::new();

        let base = wait_for_verified_base(
            "C:\\shad\\shad_log.txt",
            STALE,
            || {
                polls += 1;
                if polls == 3 {
                    appender.borrow_mut().push_str(FRESH_LINE);
                }
                Some(appender.borrow().clone())
            },
            |candidate| {
                // The stale base never verifies; the fresh one does.
                if candidate == 0x8000000 {
                    BaseCheck::Attached
                } else {
                    BaseCheck::Unverified
                }
            },
            |line| notices.push(line.to_owned()),
            &mut clock,
            policy(),
        )
        .expect("the appended base is accepted");

        assert_eq!(base, 0x8000000);
        assert_eq!(clock.sleeps, 3, "waited, rather than failing immediately");
        assert_eq!(notices, vec![WAITING_NOTICE.to_owned()]);
    }

    #[test]
    fn the_waiting_notice_is_printed_once_not_per_poll() {
        let mut clock = FakeClock::default();
        let mut notices = Vec::new();
        let failure = wait_for_verified_base(
            "C:\\shad\\shad_log.txt",
            STALE,
            || Some(String::from(STALE)),
            |_| BaseCheck::Unverified,
            |line| notices.push(line.to_owned()),
            &mut clock,
            policy(),
        )
        .expect_err("no fresh line ever appears");

        assert!(failure.is_stale_log_evidence());
        assert!(clock.sleeps >= 10, "polled repeatedly: {}", clock.sleeps);
        assert_eq!(
            notices,
            vec![WAITING_NOTICE.to_owned()],
            "one notice for the whole wait"
        );
    }

    #[test]
    fn budget_expiry_without_a_fresh_line_names_the_configured_log_path() {
        let mut clock = FakeClock::default();
        let failure = wait_for_verified_base(
            "D:\\Games\\shadPS4\\user\\log\\shad_log.txt",
            STALE,
            || Some(String::from(STALE)),
            |_| BaseCheck::Unverified,
            |_| {},
            &mut clock,
            policy(),
        )
        .expect_err("the budget expires");

        let message = failure.to_string();
        assert!(
            message.contains("D:\\Games\\shadPS4\\user\\log\\shad_log.txt"),
            "{message}"
        );
        assert!(
            message.contains("user\\log\\shad_log.txt beside shadPS4.exe"),
            "{message}"
        );
        assert!(message.contains("%APPDATA%\\shadPS4"), "{message}");
        assert!(
            !message.to_ascii_lowercase().contains("cheat engine"),
            "the log path is the suspect here, not the build: {message}"
        );
        assert!(failure.is_stale_log_evidence());
        assert!(message.is_ascii(), "in-client text is ASCII only");
    }

    #[test]
    fn a_fresh_base_with_a_rejected_image_is_a_build_failure_not_a_log_failure() {
        let log = Rc::new(RefCell::new(String::from(STALE)));
        let appender = Rc::clone(&log);
        let mut clock = FakeClock::default();

        let failure = wait_for_verified_base(
            "C:\\shad\\shad_log.txt",
            STALE,
            || {
                appender.borrow_mut().push_str(FRESH_LINE);
                Some(appender.borrow().clone())
            },
            |candidate| {
                if candidate == 0x8000000 {
                    BaseCheck::ImageRejected(String::from("assert consume_hook mismatched"))
                } else {
                    BaseCheck::Unverified
                }
            },
            |_| {},
            &mut clock,
            policy(),
        )
        .expect_err("a confirmed base with a wrong image is terminal");

        assert_eq!(
            failure,
            AttachWaitFailure::ImageRejected {
                base: 0x8000000,
                detail: String::from("assert consume_hook mismatched"),
            }
        );
        assert!(
            !failure.is_stale_log_evidence(),
            "this one must keep the CE-bridge guidance"
        );
        assert!(failure.to_string().contains("did not validate"));
    }

    #[test]
    fn an_already_running_game_attaches_without_waiting_at_all() {
        let mut clock = FakeClock::default();
        let mut notices = Vec::new();
        let base = wait_for_verified_base(
            "C:\\shad\\shad_log.txt",
            STALE,
            || panic!("must not poll when the logged base already verifies"),
            |_| BaseCheck::Attached,
            |line| notices.push(line.to_owned()),
            &mut clock,
            policy(),
        )
        .expect("the base already in the log verifies");

        assert_eq!(base, 0x5570000);
        assert_eq!(clock.sleeps, 0);
        assert!(notices.is_empty(), "no waiting notice when nothing waited");
    }

    #[test]
    fn an_unrecognised_image_at_the_pre_existing_base_fails_immediately() {
        let mut clock = FakeClock::default();
        let failure = wait_for_verified_base(
            "C:\\shad\\shad_log.txt",
            STALE,
            || panic!("must not wait out the budget for a wrong build"),
            |_| BaseCheck::ImageRejected(String::from("serial CUSA00900")),
            |_| {},
            &mut clock,
            policy(),
        )
        .expect_err("a wrong build is terminal at once");

        assert!(!failure.is_stale_log_evidence());
        assert_eq!(clock.sleeps, 0);
    }

    #[test]
    fn logged_eboot_base_after_ignores_lines_before_the_freshness_floor() {
        let combined = format!("{STALE}{FRESH_LINE}");
        assert_eq!(logged_eboot_base_after(&combined, 0), Some(0x8000000));
        assert_eq!(
            logged_eboot_base_after(&combined, STALE.len()),
            Some(0x8000000)
        );
        assert_eq!(logged_eboot_base_after(STALE, STALE.len()), None);
        // The floor is compared against the *base line* offset, not the marker.
        assert_eq!(logged_eboot_base_after(STALE, 0), Some(0x5570000));
    }

    /// clients#440: shadPS4 truncates the log at launch, so the run's own base
    /// line lands near offset 0 -- below a floor taken from the previous run.
    #[test]
    fn a_truncated_log_resets_the_floor_and_its_base_is_accepted() {
        // A big previous-run log: the client won the race and recorded this
        // length before shadPS4 truncated.
        let old_run = format!("{}{}", "noise line\n".repeat(4000), STALE);
        let fresh_run = format!("{FRESH_LINE}{}", "noise line\n".repeat(3));
        assert!(
            fresh_run.len() < old_run.len(),
            "the truncated run must be the shorter file"
        );

        let mut polls = 0u32;
        let mut clock = FakeClock::default();
        let base = wait_for_verified_base(
            "C:\\shad\\shad_log.txt",
            &old_run,
            || {
                polls += 1;
                if polls < 3 {
                    Some(old_run.clone())
                } else {
                    Some(fresh_run.clone())
                }
            },
            |candidate| {
                if candidate == 0x8000000 {
                    BaseCheck::Attached
                } else {
                    BaseCheck::Unverified
                }
            },
            |_| {},
            &mut clock,
            policy(),
        )
        .expect("the truncated log's base belongs to this run");

        assert_eq!(base, 0x8000000);
        assert_eq!(
            clock.sleeps, 3,
            "accepted on the very poll that saw the shrink, not a later one"
        );
    }

    /// The clients#419 guarantee, unchanged: without a shrink, a line that was
    /// already in the file when the wait started is still not this run's.
    #[test]
    fn a_stale_line_in_a_log_that_never_shrinks_is_still_rejected() {
        let old_run = format!("{}{}", "noise line\n".repeat(4000), STALE);
        let mut clock = FakeClock::default();
        let failure = wait_for_verified_base(
            "C:\\shad\\shad_log.txt",
            &old_run,
            // Grows, never shrinks: the only base line stays below the floor.
            || Some(format!("{old_run}more noise\n")),
            |_| BaseCheck::Unverified,
            |_| {},
            &mut clock,
            policy(),
        )
        .expect_err("a stale line below an intact floor can never satisfy the wait");

        assert!(failure.is_stale_log_evidence());
    }

    #[test]
    fn a_base_written_after_the_truncation_is_accepted_once_it_appears() {
        let old_run = format!("{}{}", "noise line\n".repeat(4000), STALE);
        let truncated = String::from("startup line\n");
        let with_base = format!("{truncated}{FRESH_LINE}");

        let mut polls = 0u32;
        let mut clock = FakeClock::default();
        let base = wait_for_verified_base(
            "C:\\shad\\shad_log.txt",
            &old_run,
            || {
                polls += 1;
                match polls {
                    1 => Some(old_run.clone()),
                    // Truncated, but shadPS4 has not logged the base yet.
                    2 | 3 => Some(truncated.clone()),
                    _ => Some(with_base.clone()),
                }
            },
            |candidate| {
                if candidate == 0x8000000 {
                    BaseCheck::Attached
                } else {
                    BaseCheck::Unverified
                }
            },
            |_| {},
            &mut clock,
            policy(),
        )
        .expect("a base written after the shrink is this run's");

        assert_eq!(base, 0x8000000);
        assert_eq!(clock.sleeps, 4);
    }

    #[test]
    fn a_truncated_log_that_never_carries_a_base_still_expires_the_budget() {
        let old_run = format!("{}{}", "noise line\n".repeat(4000), STALE);
        let mut clock = FakeClock::default();
        let failure = wait_for_verified_base(
            "C:\\shad\\shad_log.txt",
            &old_run,
            || Some(String::from("startup line\n")),
            |_| BaseCheck::Unverified,
            |_| {},
            &mut clock,
            policy(),
        )
        .expect_err("nothing base-shaped ever appears");

        assert!(
            failure.is_stale_log_evidence(),
            "the wrong-path guidance is exactly right when the file has no base line"
        );
        assert!(clock.sleeps >= 10);
    }

    #[test]
    fn a_log_that_cannot_be_re_read_is_retried_not_fatal() {
        let mut clock = FakeClock::default();
        let mut polls = 0u32;
        let failure = wait_for_verified_base(
            "C:\\shad\\shad_log.txt",
            STALE,
            || {
                polls += 1;
                None
            },
            |_| BaseCheck::Unverified,
            |_| {},
            &mut clock,
            policy(),
        )
        .expect_err("the budget still bounds it");

        assert!(polls >= 10, "kept retrying the unreadable log: {polls}");
        assert!(failure.is_stale_log_evidence());
    }
}
