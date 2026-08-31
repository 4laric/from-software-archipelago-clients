//! Pure decision for repairing Morgott's post-boss Erdtree progression.

pub const MORGOTT_DEFEAT_FLAG: u32 = 11_000_800;
pub const ERDTREE_APPROACHED_FLAG: u32 = 11_000_500;
pub const MORGOTT_PROGRESSION_COMPLETE_FLAG: u32 = 11_000_501;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MorgottProgressionState {
    pub defeated: bool,
    pub erdtree_approached: bool,
    pub progression_complete: bool,
}

/// Whether AP should synthesize the Erdtree interaction that the randomized arena can strand.
/// Vanilla event 11002501 remains responsible for applying SpEffects 4281/4283 and setting the
/// final completion flag; this only supplies its missing interaction prerequisite.
pub fn should_mark_erdtree_approached(state: MorgottProgressionState) -> bool {
    state.defeated && !state.erdtree_approached && !state.progression_complete
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn randomized_morgott_defeat_repairs_missing_interaction() {
        assert!(should_mark_erdtree_approached(MorgottProgressionState {
            defeated: true,
            erdtree_approached: false,
            progression_complete: false,
        }));
    }

    #[test]
    fn never_advances_before_defeat_or_rewrites_completed_state() {
        for state in [
            MorgottProgressionState {
                defeated: false,
                erdtree_approached: false,
                progression_complete: false,
            },
            MorgottProgressionState {
                defeated: true,
                erdtree_approached: true,
                progression_complete: false,
            },
            MorgottProgressionState {
                defeated: true,
                erdtree_approached: false,
                progression_complete: true,
            },
        ] {
            assert!(!should_mark_erdtree_approached(state));
        }
    }
}
