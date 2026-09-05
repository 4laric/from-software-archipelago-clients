//! Read-only, copied-snapshot boundary for the source-built optional map engine.
//! No completion, visibility, or game-memory mutation is inferred from a hover.

pub const ABI_VERSION: u32 = 1;
pub const CAP_HOVER: u32 = 1;
pub const MAX_HOVER_AGE_MS: u32 = 300;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Info {
    pub abi_version: u32,
    pub struct_size: u32,
    pub capabilities: u32,
    pub hover_size: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Hover {
    pub struct_size: u32,
    pub status: u32,
    pub generation: u64,
    pub handle: u64,
    pub original_flag: u32,
    pub lot_table: u32,
    pub lot_row: u32,
    pub age_ms: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    UnsupportedAbi,
    UnsupportedLayout,
    MissingCapability,
    Disabled,
    MalformedSnapshot,
    StaleSnapshot,
}

pub fn negotiate(info: Info) -> Result<(), Refusal> {
    if info.abi_version != ABI_VERSION {
        return Err(Refusal::UnsupportedAbi);
    }
    if info.struct_size != size_of::<Info>() as u32 || info.hover_size != size_of::<Hover>() as u32
    {
        return Err(Refusal::UnsupportedLayout);
    }
    if info.capabilities & CAP_HOVER == 0 {
        return Err(Refusal::MissingCapability);
    }
    Ok(())
}

/// A handle only has meaning in this client session and engine generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Selection {
    session: u64,
    pub generation: u64,
    pub handle: u64,
    pub original_flag: u32,
    pub lot_table: u32,
    pub lot_row: u32,
}

#[derive(Default)]
pub struct Bridge {
    session: u64,
    enabled: bool,
    generation: Option<u64>,
    selected: Option<Selection>,
}

impl Bridge {
    /// Call on opt-in change, disconnect, seed change, or engine replacement.
    /// Existing selections become invalid even if the new engine reuses handles.
    pub fn reset(&mut self, enabled: bool) {
        self.session = self.session.wrapping_add(1);
        self.enabled = enabled;
        self.generation = None;
        self.selected = None;
    }

    pub fn accept(&mut self, hover: Hover) -> Result<Option<Selection>, Refusal> {
        // Errors and no-hover must clear the previous selection, never retain a ghost pin.
        self.selected = None;
        if !self.enabled {
            return Err(Refusal::Disabled);
        }
        if hover.struct_size != size_of::<Hover>() as u32 || hover.status > 1 {
            return Err(Refusal::MalformedSnapshot);
        }
        if hover.generation == 0 {
            return Err(Refusal::MalformedSnapshot);
        }
        if self.generation.is_some_and(|g| hover.generation < g) {
            return Err(Refusal::StaleSnapshot);
        }
        self.generation = Some(hover.generation);
        if hover.status == 0 {
            return Ok(None);
        }
        if hover.age_ms > MAX_HOVER_AGE_MS {
            return Err(Refusal::StaleSnapshot);
        }
        if hover.handle == 0 || hover.lot_table > 2 {
            return Err(Refusal::MalformedSnapshot);
        }
        let selected = Selection {
            session: self.session,
            generation: hover.generation,
            handle: hover.handle,
            original_flag: hover.original_flag,
            lot_table: hover.lot_table,
            lot_row: hover.lot_row,
        };
        self.selected = Some(selected);
        Ok(self.selected)
    }

    pub fn is_current(&self, selection: Selection) -> bool {
        self.enabled && self.selected == Some(selection)
    }
}

/// An explicitly armed, finite observation window. Historical results never become
/// a live selection, and accepting a sample still uses the 300 ms bridge guard.
#[derive(Default)]
pub struct Capture {
    bridge: Bridge,
    started_ms: Option<u64>,
    next_poll_ms: u64,
    pub recorded: Option<RecordedHover>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordedHover {
    pub selection: Selection,
    pub received_ms: u64,
    pub elapsed_ms: u64,
    pub source_age_ms: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureTick {
    Idle,
    Poll,
    TimedOut,
}

impl Capture {
    pub const WINDOW_MS: u64 = 30_000;
    pub const POLL_MS: u64 = 100;
    pub const SETTLE_MS: u64 = 3_000;

    pub fn arm(&mut self, now_ms: u64) {
        self.reset();
        self.bridge.reset(true);
        self.started_ms = Some(now_ms);
        self.next_poll_ms = now_ms.saturating_add(Self::SETTLE_MS);
    }

    pub fn reset(&mut self) {
        self.bridge.reset(false);
        self.started_ms = None;
        self.recorded = None;
    }

    pub fn active(&self) -> bool {
        self.started_ms.is_some()
    }

    /// A transport failure may mean the engine was replaced. Keep the finite
    /// recording window, but invalidate any prior engine generation.
    pub fn unavailable(&mut self) {
        self.bridge.reset(self.active());
    }

    pub fn tick(&mut self, now_ms: u64, input_released: bool) -> CaptureTick {
        let Some(started) = self.started_ms else {
            return CaptureTick::Idle;
        };
        if now_ms < started || now_ms - started >= Self::WINDOW_MS {
            self.started_ms = None;
            self.bridge.reset(false);
            return CaptureTick::TimedOut;
        }
        if !input_released {
            self.next_poll_ms = now_ms.saturating_add(Self::SETTLE_MS);
            return CaptureTick::Idle;
        }
        if now_ms < self.next_poll_ms {
            return CaptureTick::Idle;
        }
        // No catch-up burst after a slow frame.
        self.next_poll_ms = now_ms.saturating_add(Self::POLL_MS);
        CaptureTick::Poll
    }

    pub fn accept(&mut self, now_ms: u64, hover: Hover) -> Result<Option<RecordedHover>, Refusal> {
        let Some(started) = self.started_ms else {
            return Err(Refusal::Disabled);
        };
        if now_ms < started || now_ms - started >= Self::WINDOW_MS {
            self.started_ms = None;
            self.bridge.reset(false);
            return Err(Refusal::StaleSnapshot);
        }
        let Some(selection) = self.bridge.accept(hover)? else {
            return Ok(None);
        };
        let recorded = RecordedHover {
            selection,
            received_ms: now_ms,
            elapsed_ms: now_ms - started,
            source_age_ms: hover.age_ms,
        };
        self.recorded = Some(recorded);
        self.started_ms = None;
        self.bridge.reset(false);
        Ok(Some(recorded))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> Info {
        Info {
            abi_version: 1,
            struct_size: 16,
            capabilities: CAP_HOVER,
            hover_size: 40,
        }
    }

    fn hover(generation: u64) -> Hover {
        Hover {
            struct_size: 40,
            status: 1,
            generation,
            handle: 7,
            original_flag: 123,
            lot_table: 1,
            lot_row: 456,
            age_ms: 0,
        }
    }

    #[test]
    fn transport_failure_drops_generation_without_extending_window() {
        let mut capture = Capture::default();
        capture.arm(0);
        assert_eq!(
            capture.accept(
                3_000,
                Hover {
                    status: 0,
                    ..hover(8)
                }
            ),
            Ok(None)
        );
        capture.unavailable();
        assert!(capture.accept(3_100, hover(1)).unwrap().is_some());
        capture.arm(5_000);
        capture.unavailable();
        assert_eq!(capture.tick(35_000, true), CaptureTick::TimedOut);
    }

    #[test]
    fn capture_is_opt_in_bounded_and_never_catches_up() {
        let mut capture = Capture::default();
        assert_eq!(capture.tick(0, true), CaptureTick::Idle);
        capture.arm(10);
        assert_eq!(capture.tick(10, false), CaptureTick::Idle);
        assert_eq!(capture.tick(20, true), CaptureTick::Idle);
        assert_eq!(capture.tick(3_010, true), CaptureTick::Poll);
        assert_eq!(capture.tick(3_109, true), CaptureTick::Idle);
        assert_eq!(capture.tick(5_000, true), CaptureTick::Poll);
        assert_eq!(capture.tick(5_000, true), CaptureTick::Idle);
        assert_eq!(capture.tick(30_010, false), CaptureTick::TimedOut);
        assert_eq!(capture.tick(30_011, true), CaptureTick::Idle);
        assert_eq!(capture.accept(30_011, hover(1)), Err(Refusal::Disabled));
    }

    #[test]
    fn returning_to_client_restarts_grace_without_extending_deadline() {
        let mut capture = Capture::default();
        capture.arm(0);
        assert_eq!(capture.tick(10_000, false), CaptureTick::Idle);
        assert_eq!(capture.tick(12_999, true), CaptureTick::Idle);
        assert_eq!(capture.tick(13_000, true), CaptureTick::Poll);
        assert_eq!(capture.tick(29_000, false), CaptureTick::Idle);
        assert_eq!(capture.tick(30_000, true), CaptureTick::TimedOut);
    }

    #[test]
    fn capture_rejects_stale_then_records_once_and_rearm_clears_history() {
        let mut capture = Capture::default();
        capture.arm(100);
        assert_eq!(
            capture.accept(
                3_200,
                Hover {
                    age_ms: 301,
                    ..hover(1)
                }
            ),
            Err(Refusal::StaleSnapshot)
        );
        assert!(capture.active());
        assert_eq!(
            capture.accept(
                3_250,
                Hover {
                    status: 0,
                    ..hover(1)
                }
            ),
            Ok(None)
        );
        let recorded = capture
            .accept(
                3_400,
                Hover {
                    age_ms: 50,
                    ..hover(1)
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(recorded.received_ms, 3_400);
        assert_eq!(recorded.elapsed_ms, 3_300);
        assert_eq!(recorded.source_age_ms, 50);
        assert!(!capture.bridge.is_current(recorded.selection));
        assert!(!capture.active());
        assert_eq!(capture.accept(3_500, hover(2)), Err(Refusal::Disabled));
        assert_eq!(capture.recorded, Some(recorded));
        capture.arm(3_600);
        assert_eq!(capture.recorded, None);
        capture.reset();
        assert!(!capture.active());
        assert_eq!(capture.recorded, None);
    }

    #[test]
    fn capture_deadline_and_backwards_clock_reject_samples() {
        let mut capture = Capture::default();
        for now in [99, 30_100] {
            capture.arm(100);
            assert_eq!(capture.accept(now, hover(1)), Err(Refusal::StaleSnapshot));
            assert_eq!(capture.recorded, None);
            assert!(!capture.active());
        }
    }

    #[test]
    fn wire_layout_and_negotiation() {
        assert_eq!(size_of::<Info>(), 16);
        assert_eq!(size_of::<Hover>(), 40);
        assert_eq!(std::mem::offset_of!(Hover, generation), 8);
        assert_eq!(std::mem::offset_of!(Hover, original_flag), 24);
        assert_eq!(negotiate(info()), Ok(()));
        assert_eq!(
            negotiate(Info {
                abi_version: 2,
                ..info()
            }),
            Err(Refusal::UnsupportedAbi)
        );
        assert_eq!(
            negotiate(Info {
                hover_size: 48,
                ..info()
            }),
            Err(Refusal::UnsupportedLayout)
        );
        assert_eq!(
            negotiate(Info {
                capabilities: 0,
                ..info()
            }),
            Err(Refusal::MissingCapability)
        );
    }

    #[test]
    fn opt_in_and_session_replacement_invalidate_handles() {
        let mut bridge = Bridge::default();
        assert_eq!(bridge.accept(hover(1)), Err(Refusal::Disabled));
        bridge.reset(true);
        let old = bridge.accept(hover(1)).unwrap().unwrap();
        assert!(bridge.is_current(old));
        bridge.reset(true);
        let new = bridge.accept(hover(1)).unwrap().unwrap();
        assert!(!bridge.is_current(old));
        assert!(bridge.is_current(new));
        bridge.reset(false);
        assert!(!bridge.is_current(new));
    }

    #[test]
    fn partial_identity_is_preserved_without_claiming_binding() {
        let mut bridge = Bridge::default();
        bridge.reset(true);
        let selected = bridge
            .accept(Hover {
                original_flag: 0,
                ..hover(1)
            })
            .unwrap()
            .unwrap();
        assert_eq!(selected.original_flag, 0);
        assert_eq!(selected.lot_table, 1);
        assert_eq!(selected.lot_row, 456);
    }

    #[test]
    fn rebuild_and_delayed_snapshot_do_not_reselect_old_pin() {
        let mut bridge = Bridge::default();
        bridge.reset(true);
        let old = bridge.accept(hover(1)).unwrap().unwrap();
        bridge.accept(hover(2)).unwrap();
        assert!(!bridge.is_current(old));
        assert_eq!(bridge.accept(hover(1)), Err(Refusal::StaleSnapshot));
        assert!(!bridge.is_current(old));
    }

    #[test]
    fn closed_map_stale_or_malformed_hover_clear_selection() {
        let mut bridge = Bridge::default();
        bridge.reset(true);
        for invalid in [
            Hover {
                status: 0,
                ..hover(1)
            },
            Hover {
                age_ms: 301,
                ..hover(1)
            },
            Hover {
                handle: 0,
                ..hover(1)
            },
            Hover {
                status: 2,
                ..hover(1)
            },
            Hover {
                lot_table: 3,
                ..hover(1)
            },
        ] {
            let old = bridge.accept(hover(1)).unwrap().unwrap();
            let _ = bridge.accept(invalid);
            assert!(!bridge.is_current(old));
        }
    }
}
