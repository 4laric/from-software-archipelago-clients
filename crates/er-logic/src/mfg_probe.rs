//! Pure capability classification for the optional MapForGoblins integration seam.

/// The only safe conclusions the client can draw from the loaded module's public exports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Capability {
    NotLoaded,
    ObserveOnly,
    IncompleteApi,
    ControllableV1,
}

pub fn classify(
    loaded: bool,
    has_version: bool,
    has_set_state_v1: bool,
    has_clear_v1: bool,
) -> Capability {
    if !loaded {
        Capability::NotLoaded
    } else if !has_version && !has_set_state_v1 && !has_clear_v1 {
        Capability::ObserveOnly
    } else if has_version && has_set_state_v1 && has_clear_v1 {
        Capability::ControllableV1
    } else {
        Capability::IncompleteApi
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_module_is_not_loaded() {
        assert_eq!(classify(false, false, false, false), Capability::NotLoaded);
    }

    #[test]
    fn current_module_without_exports_is_observe_only() {
        assert_eq!(classify(true, false, false, false), Capability::ObserveOnly);
    }

    #[test]
    fn partial_contract_is_never_treated_as_safe() {
        assert_eq!(classify(true, true, true, false), Capability::IncompleteApi);
    }

    #[test]
    fn complete_v1_surface_is_controllable() {
        assert_eq!(classify(true, true, true, true), Capability::ControllableV1);
    }
}
