//! The event-flag arming gate (clients#420).
//!
//! The Bloodborne event-flag manager is a guest global that is null until the
//! game is well into boot -- plausibly only once a character save is loaded.
//! The launcher starts shadPS4 and the client together, so native attach
//! routinely reaches [`crate::event_flags::LiveEventFlags::attach_at_base`]
//! before that global exists. Treating it as terminal exits the client during a
//! normal startup, and (worse) inherited the unrecognised-build guidance, which
//! points at a Cheat Engine lane that does not provide flag reads either.
//!
//! So the flag half is armed *lazily*. Attach succeeds with the gate
//! [`FlagGate::pending`], native item delivery is armed immediately, and the
//! client loop -- which already polls every tick -- retries the flag attach on
//! its own cadence. While pending, `read_event_flag` answers `None` ("the live
//! accessor is not available", never "the flag is false") and
//! `location_context` reports `gameplay_ready: false`, which is the *existing*
//! send-gate shape: `require_runtime_context` returns `Ok(None)` and
//! `poll_locations` reports nothing. No parallel gate is invented, and checks
//! cannot be missed by waiting: they cannot fire before gameplay anyway.
//!
//! Exactly one line is emitted when the wait begins and exactly one when the
//! gate arms; a pending gate is otherwise silent no matter how long it waits.

use anyhow::Result;

use crate::event_flags::is_manager_not_initialized;

/// Printed once when the gate is created pending.
pub const WAITING_NOTICE: &str = "Waiting for the game to finish loading (event flags are not ready yet; load your character). Item delivery is armed already; location checks arm automatically once the game is ready.";

/// Printed once when the gate arms.
pub const ARMED_NOTICE: &str = "Bloodborne event flags are ready; location checks are now armed.";

/// A live flag accessor that is either armed or still waiting for the guest
/// event-flag manager to exist.
#[derive(Debug)]
pub struct FlagGate<F> {
    accessor: Option<F>,
    announced_armed: bool,
}

impl<F> FlagGate<F> {
    /// An already-armed gate: the manager existed at attach time.
    pub fn armed(accessor: F) -> Self {
        Self {
            accessor: Some(accessor),
            announced_armed: true,
        }
    }

    /// A pending gate. Emits [`WAITING_NOTICE`] exactly once, here.
    pub fn pending(notice: &mut dyn FnMut(&str)) -> Self {
        notice(WAITING_NOTICE);
        Self {
            accessor: None,
            announced_armed: false,
        }
    }

    pub fn is_armed(&self) -> bool {
        self.accessor.is_some()
    }

    /// The armed accessor, or `None` while pending.
    pub fn armed_mut(&mut self) -> Option<&mut F> {
        self.accessor.as_mut()
    }

    /// Try to arm, if not armed already.
    ///
    /// * armed already -- `attach` is not called, nothing is printed;
    /// * `attach` succeeds -- the gate arms and [`ARMED_NOTICE`] is printed
    ///   once, ever;
    /// * `attach` reports the clients#420 not-initialized state -- the gate
    ///   stays pending and returns `Ok(())`: this is a normal wait, never an
    ///   error, however long it lasts;
    /// * any other failure (signature mismatch, process gone) propagates, so a
    ///   genuine refusal is still reported rather than waited on forever.
    pub fn poll(
        &mut self,
        attach: impl FnOnce() -> Result<F>,
        notice: &mut dyn FnMut(&str),
    ) -> Result<()> {
        if self.accessor.is_some() {
            return Ok(());
        }
        match attach() {
            Ok(accessor) => {
                self.accessor = Some(accessor);
                if !self.announced_armed {
                    self.announced_armed = true;
                    notice(ARMED_NOTICE);
                }
                Ok(())
            }
            Err(error) if is_manager_not_initialized(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_flags::EventFlagManagerNotInitialized;

    fn not_initialized() -> anyhow::Error {
        anyhow::Error::new(EventFlagManagerNotInitialized)
            .context("attaching live Bloodborne event flags")
    }

    #[test]
    fn a_manager_that_appears_later_arms_the_gate_and_announces_once() {
        let mut lines = Vec::new();
        let mut gate = {
            let mut sink = |line: &str| lines.push(line.to_string());
            FlagGate::<u32>::pending(&mut sink)
        };
        assert!(!gate.is_armed());

        // Two ticks with the manager still null: silent, no error.
        for _ in 0..2 {
            let mut sink = |line: &str| lines.push(line.to_string());
            gate.poll(|| Err(not_initialized()), &mut sink).unwrap();
            assert!(!gate.is_armed());
        }

        // The character finishes loading.
        let mut sink = |line: &str| lines.push(line.to_string());
        gate.poll(|| Ok(7u32), &mut sink).unwrap();
        assert!(gate.is_armed());
        assert_eq!(gate.armed_mut().copied(), Some(7));

        // Further polls neither re-attach nor re-announce.
        for _ in 0..3 {
            let mut sink = |line: &str| lines.push(line.to_string());
            gate.poll(|| panic!("must not re-attach once armed"), &mut sink)
                .unwrap();
        }
        assert_eq!(
            lines,
            vec![WAITING_NOTICE.to_string(), ARMED_NOTICE.to_string()]
        );
    }

    #[test]
    fn a_gate_that_stays_pending_never_errors_and_never_repeats_its_notice() {
        let mut lines = Vec::new();
        let mut gate = {
            let mut sink = |line: &str| lines.push(line.to_string());
            FlagGate::<u32>::pending(&mut sink)
        };
        for _ in 0..500 {
            let mut sink = |line: &str| lines.push(line.to_string());
            gate.poll(|| Err(not_initialized()), &mut sink).unwrap();
        }
        assert!(!gate.is_armed());
        assert!(gate.armed_mut().is_none());
        assert_eq!(lines, vec![WAITING_NOTICE.to_string()]);
    }

    #[test]
    fn a_real_refusal_still_propagates_instead_of_waiting_forever() {
        let mut lines = Vec::new();
        let mut gate = {
            let mut sink = |line: &str| lines.push(line.to_string());
            FlagGate::<u32>::pending(&mut sink)
        };
        let mut sink = |line: &str| lines.push(line.to_string());
        let error = gate
            .poll(
                || {
                    Err(anyhow::anyhow!(
                        "event-flag signature mismatch at eboot+0x17D6EFA"
                    ))
                },
                &mut sink,
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("signature mismatch"));
        assert!(!gate.is_armed());
    }

    #[test]
    fn an_armed_gate_announces_nothing() {
        let mut lines: Vec<String> = Vec::new();
        let mut gate = FlagGate::armed(3u32);
        let mut sink = |line: &str| lines.push(line.to_string());
        gate.poll(|| panic!("must not re-attach"), &mut sink)
            .unwrap();
        assert!(lines.is_empty());
        assert!(gate.is_armed());
    }

    #[test]
    fn the_waiting_notices_are_ascii_single_line_and_never_mention_the_bridge() {
        for notice in [WAITING_NOTICE, ARMED_NOTICE] {
            assert!(notice.is_ascii(), "notice must be ascii: {notice}");
            assert!(!notice.contains('\n'));
            assert!(!notice.contains("Cheat Engine"));
            assert!(!notice.contains("ce-bridge"));
        }
    }
}
