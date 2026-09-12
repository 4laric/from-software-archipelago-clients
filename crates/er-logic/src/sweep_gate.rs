//! Boss-lock sweep gate (SPEC-boss-locks.md v0.1, BOSS_LOCKS_PATCH).
//!
//! Pure decision seam: a dungeon-sweep group whose trigger has a gate entry in slot_data
//! `sweepLockGates` only fires while the named boss-lock item is in the CUMULATIVE received
//! set. The caller (eldenring-archipelago core, section 5b flag-poll) re-evaluates every poll
//! tick, so a lock received AFTER the boss kill fires the held sweep retroactively on a later
//! tick -- "check for the sweep on boss-lock-obtain" falls out of polling; no staging needed.

/// `gate` = boss-lock item name for this trigger (`None` = ungated group: minidungeons,
/// chokepoint carves, and groups whose lock is not in this seed's pool).
/// `received` = membership test over ALL received item names (cumulative, reconnect-replayed).
pub fn gate_open<F: Fn(&str) -> bool>(gate: Option<&str>, received: F) -> bool {
    match gate {
        None => true,
        Some(name) => received(name),
    }
}

/// Translate legacy health-bar proxy keys only when reading a sweep's completion flag.
/// Keep seed group identities and lock gates intact. Scadutree Avatar's final defeat
/// event explicitly sets 2050480800; its three proxy entities do not reliably do so.
pub fn completion_flag(trigger: u32) -> u32 {
    match trigger {
        2050480810..=2050480812 => 2050480800,
        _ => trigger,
    }
}

#[cfg(test)]
mod tests {
    use super::{completion_flag, gate_open};
    use std::collections::HashSet;

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ungated_group_always_fires() {
        let r = set(&[]);
        assert!(gate_open(None, |n| r.contains(n)));
    }

    #[test]
    fn avatar_legacy_groups_wait_for_final_defeat_and_keep_their_gates() {
        let proxies = [2050480810, 2050480811, 2050480812];
        for trigger in proxies {
            // Neither a phase death nor manually setting the old proxy flag completes it.
            for set_flag in [2050480801, 2050480802, trigger] {
                assert_ne!(completion_flag(trigger), set_flag);
            }
            assert_eq!(completion_flag(trigger), 2050480800);
            assert!(!gate_open(Some("Avatar Lock"), |_| false));
            assert!(gate_open(Some("Avatar Lock"), |_| true));
        }
        // Current seeds and unrelated bosses retain their existing completion flags.
        for trigger in [2050480800, 21010800, 31220801, 31000800] {
            assert_eq!(completion_flag(trigger), trigger);
        }
    }

    #[test]
    fn gated_group_holds_until_lock_received() {
        // The REGION lock is not the BOSS lock -- holding Stormveil Lock alone must not sweep.
        let r = set(&["Stormveil Lock"]);
        assert!(!gate_open(Some("Godrick Lock"), |n| r.contains(n)));
    }

    #[test]
    fn lock_received_after_kill_fires_retroactively() {
        // Same call on a later poll tick: the received set now has the lock -> held sweep fires.
        let r = set(&["Stormveil Lock", "Godrick Lock"]);
        assert!(gate_open(Some("Godrick Lock"), |n| r.contains(n)));
    }
}
