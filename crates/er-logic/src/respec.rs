//! Lifecycle of a native rebirth menu. A request is one shot; it must never
//! reopen later because the player happened to finish loading or leave combat.

pub const OPEN_TIMEOUT_MS: u64 = 5_000;
pub const SETTLE_MS: u64 = 2_000;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum State {
    #[default]
    Idle,
    Opening(u64),
    Open,
    Closing(u64),
    Settling(u64),
    /// Keep the callback owner alive, but never invoke it again this process.
    Faulted {
        hold_effects: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    Open,
    /// Both the menu and its finalize job have retired.
    Closed,
    Pending,
    OwnerLost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    Opened,
    Closed,
    FailedToOpen,
    FailedToClose,
    Interrupted,
}

impl State {
    pub fn request(&mut self, now: u64) -> bool {
        if *self != Self::Idle {
            return false;
        }
        *self = Self::Opening(now);
        true
    }

    pub fn holds_effects(self) -> bool {
        !matches!(
            self,
            Self::Idle
                | Self::Faulted {
                    hold_effects: false
                }
        )
    }

    pub fn owns_input(self) -> bool {
        matches!(self, Self::Opening(_) | Self::Open | Self::Closing(_))
    }

    pub fn observe(&mut self, now: u64, observation: Observation) -> Option<Notice> {
        if *self == Self::Idle {
            return None;
        }
        if observation == Observation::OwnerLost {
            let first = !matches!(self, Self::Faulted { .. });
            *self = Self::Faulted {
                hold_effects: false,
            };
            return first.then_some(Notice::Interrupted);
        }
        match (*self, observation) {
            (Self::Opening(_), Observation::Open) => {
                *self = Self::Open;
                Some(Notice::Opened)
            }
            (Self::Opening(start), _) if now.saturating_sub(start) >= OPEN_TIMEOUT_MS => {
                *self = Self::Faulted {
                    hold_effects: observation != Observation::Closed,
                };
                Some(Notice::FailedToOpen)
            }
            (Self::Open | Self::Closing(_), Observation::Closed) => {
                *self = Self::Settling(now);
                Some(Notice::Closed)
            }
            (Self::Open, Observation::Pending) => {
                *self = Self::Closing(now);
                None
            }
            (Self::Closing(_), Observation::Open) => {
                *self = Self::Open;
                None
            }
            (Self::Closing(start), Observation::Pending)
                if now.saturating_sub(start) >= OPEN_TIMEOUT_MS =>
            {
                *self = Self::Faulted { hold_effects: true };
                Some(Notice::FailedToClose)
            }
            (Self::Settling(start), Observation::Closed)
                if now.saturating_sub(start) >= SETTLE_MS =>
            {
                *self = Self::Idle;
                None
            }
            (Self::Settling(_), Observation::Open) => {
                *self = Self::Open;
                None
            }
            (Self::Settling(_), Observation::Pending) => {
                *self = Self::Settling(now);
                None
            }
            (Self::Faulted { .. }, Observation::Closed) => {
                *self = Self::Faulted {
                    hold_effects: false,
                };
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delayed_open_is_not_mistaken_for_cancel() {
        let mut state = State::Idle;
        assert!(state.request(0));
        assert!(!state.request(1));
        assert_eq!(state.observe(100, Observation::Closed), None);
        assert!(state.holds_effects());
        assert_eq!(state.observe(200, Observation::Open), Some(Notice::Opened));
        assert_eq!(state.observe(300, Observation::Pending), None);
        assert_eq!(state, State::Closing(300));
        assert_eq!(
            state.observe(400, Observation::Closed),
            Some(Notice::Closed)
        );
        assert!(!state.owns_input());
        assert!(state.holds_effects());
        state.observe(400 + SETTLE_MS, Observation::Closed);
        assert_eq!(state, State::Idle);
        assert!(state.request(3_000));
    }

    #[test]
    fn callback_still_pending_at_timeout_is_never_freed_or_reused() {
        let mut state = State::Opening(0);
        assert_eq!(
            state.observe(OPEN_TIMEOUT_MS, Observation::Pending),
            Some(Notice::FailedToOpen)
        );
        assert!(state.holds_effects());
        assert!(!state.owns_input()); // let the user see the failure / quit
        assert!(!state.request(6_000));
        state.observe(7_000, Observation::Open);
        assert!(state.holds_effects());
        state.observe(8_000, Observation::Closed);
        assert!(!state.holds_effects());
        assert!(!state.request(9_000));
    }

    #[test]
    fn unload_releases_ap_but_never_retargets_old_callbacks() {
        for initial in [State::Opening(0), State::Open, State::Settling(0)] {
            let mut state = initial;
            assert_eq!(
                state.observe(100, Observation::OwnerLost),
                Some(Notice::Interrupted)
            );
            assert!(!state.holds_effects());
            assert!(!state.request(200));
            assert_eq!(state.observe(300, Observation::OwnerLost), None);
        }
    }

    #[test]
    fn reappearing_menu_restarts_settle_window() {
        let mut state = State::Settling(0);
        state.observe(1_000, Observation::Open);
        assert_eq!(state, State::Open);
        state.observe(2_000, Observation::Closed);
        state.observe(3_000, Observation::Pending);
        state.observe(4_000, Observation::Closed);
        assert!(state.holds_effects());
        state.observe(5_000, Observation::Closed);
        assert_eq!(state, State::Idle);
    }
}
