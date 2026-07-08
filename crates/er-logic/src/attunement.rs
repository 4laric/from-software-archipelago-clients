//! `attunement` -- pure attunement counter for the region attunement-release gate
//! (SPEC-gf-boss-lock-tracker.md "Attunement-release design"). Same discipline as
//! [`crate::sweep_gate`] / [`crate::boss_felled`]: no I/O, no game deps, deterministic over args.
//!
//! A region "attunes" once the player has collected at least `threshold` of its freely-reachable
//! in-region checks (`member_ap_ids`). Attunement is counted from the SERVER checked-locations set
//! (authoritative -> survives save-load / reconnect / re-snapshot, the bug class the region-lock and
//! flag-poll baseline replays keep hitting), NOT from live event flags. The client feeds the member
//! id set + a `checked` closure over the server set; this module just counts and thresholds.
//!
//! Boss payout (the boss's own check + its dungeon-sweep members) is DEFERRED while the region is
//! un-attuned and burst-released the moment it attunes; [`newly_attuned`] is the once-only edge the
//! caller latches for the attunement bloom (grace reveal) and the release banner.

use std::collections::HashSet;

/// How many of `members` are checked in the server set.
pub fn attuned_count(members: &HashSet<i64>, checked: impl Fn(i64) -> bool) -> u32 {
    members.iter().filter(|&&m| checked(m)).count() as u32
}

/// Is the region attuned: at least `threshold` of its members collected. A `threshold` of 0 is
/// vacuously attuned (feature-off / no-gate region), matching the "absent => unchanged" contract.
pub fn attuned(members: &HashSet<i64>, threshold: u32, checked: impl Fn(i64) -> bool) -> bool {
    attuned_count(members, checked) >= threshold
}

/// Rising-edge detector for the once-only attunement bloom / release: `true` only on the
/// un-attuned -> attuned transition. Idempotent-safe for reconnect replay -- when `prev` is already
/// `true` (attuned in a prior session and re-derived from the replayed server set) this is `false`,
/// so the bloom + banner never re-fire. A `true -> false` drop (not expected from a monotonic
/// checked set) is also `false`.
pub fn newly_attuned(prev: bool, now: bool) -> bool {
    !prev && now
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members(ids: &[i64]) -> HashSet<i64> {
        ids.iter().copied().collect()
    }

    #[test]
    fn count_is_the_intersection_size() {
        let m = members(&[1, 2, 3, 4]);
        let checked = members(&[2, 4, 99]); // 99 is not a member
        assert_eq!(attuned_count(&m, |x| checked.contains(&x)), 2);
    }

    #[test]
    fn below_threshold_is_not_attuned() {
        let m = members(&[1, 2, 3, 4, 5]);
        let checked = members(&[1, 2]);
        assert!(!attuned(&m, 3, |x| checked.contains(&x)));
    }

    #[test]
    fn at_or_above_threshold_is_attuned() {
        let m = members(&[1, 2, 3, 4, 5]);
        let exactly = members(&[1, 2, 3]);
        assert!(attuned(&m, 3, |x| exactly.contains(&x))); // exactly at threshold
        let more = members(&[1, 2, 3, 4]);
        assert!(attuned(&m, 3, |x| more.contains(&x))); // above threshold
    }

    #[test]
    fn zero_threshold_is_vacuously_attuned() {
        let m = members(&[1, 2, 3]);
        let none: HashSet<i64> = HashSet::new();
        assert!(attuned(&m, 0, |x| none.contains(&x)));
        // even with no members at all
        let empty: HashSet<i64> = HashSet::new();
        assert!(attuned(&empty, 0, |_| false));
    }

    #[test]
    fn newly_attuned_fires_on_rising_edge_only() {
        assert!(newly_attuned(false, true)); // crossed this poll -> bloom fires once
        assert!(!newly_attuned(false, false)); // still un-attuned
        assert!(!newly_attuned(true, true)); // reconnect replay: already attuned -> stays false
        assert!(!newly_attuned(true, false)); // spurious drop -> no re-fire
    }

    #[test]
    fn counting_from_the_server_set_is_route_agnostic() {
        // Non-members in the checked set never inflate the count (side content from OTHER regions is
        // not counted here -- the caller feeds THIS region's member set only).
        let m = members(&[10, 11, 12]);
        let checked = members(&[10, 11, 12, 500, 501, 502]);
        assert_eq!(attuned_count(&m, |x| checked.contains(&x)), 3);
        assert!(attuned(&m, 3, |x| checked.contains(&x)));
    }
}
