//! Pure decision logic for repairing Radahn's post-kill festival state under an enemy randomizer.
//!
//! Vanilla event 1252382800 waits for the original Radahn character to die, then sets the arena
//! defeat flag, the global Radahn defeat flag, and the festival-afterglow flag in sequence. A host
//! enemy randomizer instead kills its replacement and supplies the arena defeat flag through its
//! compatibility path. Once that flag is set, vanilla's event exits at its first instruction and
//! can never backfill the two companion flags.
//!
//! This module deliberately does not decide or write the festival-over flag (9413). Vanilla common
//! event 3040 owns that transition because it also waits for Jerren's conversation latch and for
//! the player to leave the festival area.

pub const RADAHN_DEFEAT_FLAG: u32 = 1_252_380_800;
pub const RADAHN_GLOBAL_DEFEAT_FLAG: u32 = 9_130;
pub const RADAHN_AFTERGLOW_FLAG: u32 = 9_412;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FestivalState {
    pub defeated: bool,
    pub global_defeat: bool,
    pub afterglow: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompanionWrites {
    pub global_defeat: bool,
    pub afterglow: bool,
}

/// Return the missing companion flags implied by a locally set arena-defeat flag.
///
/// Set-only and idempotent: no defeat means no authority to change festival state, while a fully
/// converged vanilla kill produces no writes.
pub fn reconcile(state: FestivalState) -> CompanionWrites {
    if !state.defeated {
        return CompanionWrites::default();
    }
    CompanionWrites {
        global_defeat: !state.global_defeat,
        afterglow: !state.afterglow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_arena_defeat_never_authors_festival_progression() {
        for global_defeat in [false, true] {
            for afterglow in [false, true] {
                assert_eq!(
                    reconcile(FestivalState {
                        defeated: false,
                        global_defeat,
                        afterglow,
                    }),
                    CompanionWrites::default()
                );
            }
        }
    }

    #[test]
    fn randomizer_defeat_repairs_both_missing_companions() {
        assert_eq!(
            reconcile(FestivalState {
                defeated: true,
                global_defeat: false,
                afterglow: false,
            }),
            CompanionWrites {
                global_defeat: true,
                afterglow: true,
            }
        );
    }

    #[test]
    fn partial_and_converged_states_only_request_missing_writes() {
        assert_eq!(
            reconcile(FestivalState {
                defeated: true,
                global_defeat: true,
                afterglow: false,
            }),
            CompanionWrites {
                global_defeat: false,
                afterglow: true,
            }
        );
        assert_eq!(
            reconcile(FestivalState {
                defeated: true,
                global_defeat: false,
                afterglow: true,
            }),
            CompanionWrites {
                global_defeat: true,
                afterglow: false,
            }
        );
        assert_eq!(
            reconcile(FestivalState {
                defeated: true,
                global_defeat: true,
                afterglow: true,
            }),
            CompanionWrites::default()
        );
    }
}
