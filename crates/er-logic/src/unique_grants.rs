//! `unique_grants` — the flag-idempotent UNIQUE start-grant decision + its timeline replay.
//!
//! Sibling of [`crate::start_grant_replay`] / [`crate::torrent_start_replay`], for the next
//! start-grant class. The plain `startItems` drain is once-per-save by LEDGER (an index set /
//! persisted latch): correct for capacity grants (Torch, pot vessels) where a duplicate is
//! harmless, but wrong for UNIQUE key items (Spectral Steed Whistle, Spirit Calling Bell, Flask
//! of Wondrous Physick) — a session-state loss (reconnect, save-scum, wiped save file, seed
//! reset) replays the drain and hands the player a second bell; and if the item is ALSO a
//! randomized check, the pool copy plus the start grant double up too.
//!
//! The fix: these items already carry a vanilla OBTAINED-flag the game itself tracks possession
//! with (60100 whistle / 60110 bell / 60020 physick — the same flags the apworld's checks detect
//! and `keyitems.rs` sets on a pool receive). Make that flag the SINGLE SOURCE OF TRUTH:
//!
//!   * flag SET   -> the player has it (prior grant, pool pickup, vanilla hand-off, reload) -> SKIP
//!   * flag UNSET -> grant the goods AND set the flag with the grant
//!
//! Re-running the whole path is then a no-op by construction — no ledger, no persisted latch, no
//! session state to lose. slot_data wire: `uniqueStartGrants` = `[[fullId, obtainedFlag], ...]`
//! (see contract_gen.rs); the Windows `core.rs` unique-grant block must CALL
//! [`unique_grant_action`], not inline its own read (CONTRIBUTING: a green predicate with no
//! production caller is a spec, not a fix).
//!
//! ⚠ physick caveat: [`crate::flagpoll_baseline_replay`] pins 60020 (and 60000) as reading SET on
//! a FRESH save. If that holds, the physick unique grant SKIPS on a fresh save — inert, never
//! harmful. In-game confirm pending (Alaric); the whistle/bell flags are not on that fresh-save
//! default list.

/// Pure decision for one `[fullId, obtainedFlag]` pair: grant it? `flag_set` is the live read of
/// the pair's obtained-flag. `true` = grant the goods and set the flag; `false` = the player
/// already has it — skip, and never re-grant.
pub fn unique_grant_action(flag_set: bool) -> bool {
    !flag_set
}

#[cfg(test)]
mod replay {
    use super::*;

    /// Illustrative pair tokens (the live client reads the real ids from slot_data
    /// `uniqueStartGrants`; the harness needs stable values to track through the timeline).
    /// These mirror the real bell: goods 8158 / obtained-flag 60110.
    const BELL_FULL_ID: i32 = 0x4000_0000 | 8158;
    const BELL_FLAG: u32 = 60110;

    /// Grant policy under test: the OLD ledgered behaviour (session latch, lost on reload) vs the
    /// NEW flag-latched behaviour ([`unique_grant_action`]). The failing-without-the-fix /
    /// passing-with-it pair below toggles this.
    #[derive(Clone, Copy, PartialEq)]
    enum Policy {
        /// Pre-fix: grant unless THIS SESSION already granted (in-memory index ledger).
        SessionLedger,
        /// The fix: grant iff the obtained-flag is unset (flag = single source of truth).
        FlagLatch,
    }

    /// The frames that matter for this bug.
    enum Ev {
        /// A grant tick with a live world + settled inventory (the unique-grant block runs).
        Tick,
        /// Reconnect / client restart: SESSION state is lost; game flags + inventory persist.
        Reload,
        /// The player picks the bell up in the world as a randomized CHECK — the pool/receive
        /// path grants the goods and `keyitems.rs` sets its obtained-flag.
        PoolPickup,
        /// A genuinely fresh save: flags AND inventory reset (a new character must be granted).
        NewSave,
    }

    /// Game + client model for this timeline. Flags persist across `Reload` (save-backed);
    /// `session_granted` does not — that asymmetry IS the bug surface.
    struct UniqueGrantGame {
        flag_set: bool,
        /// Count of bell goods in the bag (the double-grant detector).
        bell_count: u32,
        /// The pre-fix in-memory ledger (lost on Reload).
        session_granted: bool,
    }

    impl UniqueGrantGame {
        fn new() -> Self {
            UniqueGrantGame {
                flag_set: false,
                bell_count: 0,
                session_granted: false,
            }
        }
    }

    /// Drive the timeline under `policy`. Returns the final game state.
    fn replay(events: &[Ev], policy: Policy) -> UniqueGrantGame {
        let mut g = UniqueGrantGame::new();
        for ev in events {
            match ev {
                Ev::Tick => {
                    let grant = match policy {
                        Policy::SessionLedger => !g.session_granted,
                        Policy::FlagLatch => unique_grant_action(g.flag_set),
                    };
                    if grant {
                        // grant_full_id(BELL_FULL_ID, 1) + try_set_event_flag(BELL_FLAG, true)
                        let _ = (BELL_FULL_ID, BELL_FLAG);
                        g.bell_count += 1;
                        g.flag_set = true;
                        g.session_granted = true;
                    }
                }
                Ev::Reload => g.session_granted = false,
                Ev::PoolPickup => {
                    g.bell_count += 1;
                    g.flag_set = true; // keyitems.rs COMPANION_ACQUIRE_FLAGS on receive
                }
                Ev::NewSave => {
                    g.flag_set = false;
                    g.bell_count = 0;
                    g.session_granted = false;
                }
            }
        }
        g
    }

    #[test]
    fn reload_regrants_under_the_session_ledger_but_not_the_flag_latch() {
        // THE BUG, expressed as a test: grant -> reconnect (session state lost) -> re-run.
        let timeline = [Ev::Tick, Ev::Reload, Ev::Tick, Ev::Reload, Ev::Tick];
        let old = replay(&timeline, Policy::SessionLedger);
        assert_eq!(
            old.bell_count, 3,
            "pre-fix ledger loses itself on every reload -> a bell per session (the double-grant)"
        );
        let new = replay(&timeline, Policy::FlagLatch);
        assert_eq!(
            new.bell_count, 1,
            "flag latch: the obtained-flag survives the reload, so re-runs SKIP"
        );
        assert!(new.flag_set, "the grant set the flag as part of granting");
    }

    #[test]
    fn pool_pickup_first_makes_the_start_grant_skip() {
        // The player found the bell as a CHECK before the start grant ran (late connect): the
        // receive path set 60110, so the unique start grant must not add a second bell.
        let g = replay(&[Ev::PoolPickup, Ev::Tick, Ev::Tick], Policy::FlagLatch);
        assert_eq!(
            g.bell_count, 1,
            "flag from the pool pickup makes the start grant a no-op"
        );
    }

    #[test]
    fn a_fresh_save_is_granted_again() {
        // The reset path: a NEW character has neither the flag nor the goods — the same seed's
        // start grant must fire again (the flag latch resets WITH the save, unlike a dll-side
        // persisted ledger, which would wrongly remember the old character).
        let g = replay(&[Ev::Tick, Ev::NewSave, Ev::Tick], Policy::FlagLatch);
        assert_eq!(
            g.bell_count, 1,
            "new save starts empty; exactly one fresh grant"
        );
        assert!(g.flag_set);
    }

    #[test]
    fn predicate_truth_table() {
        assert!(unique_grant_action(false), "flag unset -> grant");
        assert!(!unique_grant_action(true), "flag set -> skip");
    }
}
