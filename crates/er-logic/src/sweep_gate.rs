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

#[cfg(test)]
mod tests {
    use super::gate_open;
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
