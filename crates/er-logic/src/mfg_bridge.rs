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
