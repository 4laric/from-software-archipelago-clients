//! `sweep_implied` -- fire a boss sweep on a LATER boss's flag when its own never lands.
//!
//! MOTIVATING CASE, 2026-09-13. colombius (apworld/client 0.5.7) killed Needle Knight Leda in
//! Enir-Ilim and her 48-member sweep group [trigger flag 20010850] never paid out. His client log
//! (`archipelago-2026-09-13 (1).log`) shows the sweep-watch census reporting `20010850(48)` NOT set
//! for the entire session, while `20010800` -- Promised Consort Radahn's defeat flag, same map
//! m20_01 -- was ALREADY `=SET` at session start.
//!
//! Why the group's own flag can never land: the game's own event
//! `elden_ring_artifacts/event/m20_01_00_00.emevd.dcx.js` lines 909-929 (event 20012850) sets
//! 20010850 only after ALL FIVE gank participants 20010850..20010854 are dead. In some NPC
//! quest-state rosters fewer than five spawn, so the terminal AND never completes even though the
//! player won the fight and walked on.
//!
//! Why Radahn implies it: the Leda gank is fought on the stairs up to Divine Gate Front Cross,
//! physically BEFORE Radahn's arena. A player who has Radahn's defeat flag has necessarily put the
//! Leda encounter behind them, so 20010800 is a sound witness for 20010850. The implication is
//! one-directional and must stay that way -- Leda dead says nothing about Radahn.
//!
//! This is deliberately a tiny static table, not a general inference engine. Each entry costs a
//! documented geographic argument; anything less certain does not belong here.

/// `(sweep trigger flag, flags whose being set implies the trigger's encounter is behind us)`.
///
/// Sorted by trigger. Kept short on purpose -- see the module note.
pub const SWEEP_IMPLIED_BY: &[(u32, &[u32])] = &[
    // Needle Knight Leda's Enir-Ilim gank <- Promised Consort Radahn (2026-09-13, colombius).
    (20010850, &[20010800]),
];

/// Has `flag`'s sweep trigger effectively fired?
///
/// Returns the WITNESS flag that fired it: `flag` itself when its own flag is set (the honest
/// answer always wins), otherwise the first implier in `SWEEP_IMPLIED_BY` that reads set. `None`
/// when neither -- including for any flag with no entry in the table, which is almost all of them.
///
/// `get` is the caller's flag reader; it is called at most once per candidate and never for a
/// trigger that already reads set.
pub fn sweep_trigger_fired(flag: u32, get: impl Fn(u32) -> bool) -> Option<u32> {
    if get(flag) {
        return Some(flag);
    }
    SWEEP_IMPLIED_BY
        .iter()
        .find(|&&(trigger, _)| trigger == flag)
        .and_then(|&(_, impliers)| impliers.iter().copied().find(|&i| get(i)))
}

#[cfg(test)]
mod tests {
    use super::{sweep_trigger_fired, SWEEP_IMPLIED_BY};

    #[test]
    fn the_groups_own_flag_wins_as_witness() {
        // Both set: the honest witness is the group's own flag, not the implier.
        assert_eq!(sweep_trigger_fired(20010850, |_| true), Some(20010850));
    }

    #[test]
    fn radahn_fires_ledas_group_when_her_own_flag_never_landed() {
        assert_eq!(
            sweep_trigger_fired(20010850, |f| f == 20010800),
            Some(20010800)
        );
    }

    #[test]
    fn neither_the_trigger_nor_an_implier_is_no_fire() {
        assert_eq!(sweep_trigger_fired(20010850, |_| false), None);
    }

    #[test]
    fn an_unlisted_trigger_has_no_impliers_and_never_fires_early() {
        for flag in [20010800, 21010800, 31220801, 2050480800] {
            assert_eq!(sweep_trigger_fired(flag, |_| false), None);
            assert_eq!(sweep_trigger_fired(flag, |f| f == flag), Some(flag));
        }
    }

    #[test]
    fn the_implication_is_one_directional() {
        // Leda dead says nothing about Radahn's group.
        assert_eq!(sweep_trigger_fired(20010800, |f| f == 20010850), None);
    }

    #[test]
    fn no_trigger_implies_itself_and_the_table_has_no_duplicate_triggers() {
        for (i, &(trigger, impliers)) in SWEEP_IMPLIED_BY.iter().enumerate() {
            assert!(!impliers.contains(&trigger), "{trigger} implies itself");
            assert!(!impliers.is_empty(), "{trigger} has an empty implier list");
            assert!(
                !SWEEP_IMPLIED_BY[..i].iter().any(|&(t, _)| t == trigger),
                "{trigger} listed twice"
            );
        }
    }
}
