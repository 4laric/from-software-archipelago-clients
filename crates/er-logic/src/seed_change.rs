//! `seed_change` — pure decision for detecting a mid-session SEED CHANGE.
//!
//! Sibling of [`crate::flagpoll_baseline_replay`] / [`crate::region_lock_replay`], for the
//! reconnect-to-a-different-seed abort. The client parses slot_data behind a one-shot latch
//! (`slot_data_parsed`) and builds every per-seed table there — `valid_locations`, the flag-poll
//! map, region/goal/start configs, watermarks. If the player kills server A, starts server B on a
//! NEW seed, and reconnects WITHOUT reloading the ER save, that latch stays true: the client keeps
//! seed A's `valid_locations` while `archipelago_rs` rebuilds `local_locations_checked` for seed B.
//! A seed-A location id that is absent from seed B then slips past the stale `valid_locations`
//! guard into `is_local_location_checked`, which panics in a no-unwind FFI frame (abort). Even
//! without the panic, seed B's own new checks would never report (its tables were never built).
//!
//! The fix rebuilds the per-seed state whenever the room's seed_name changes. This module holds the
//! single pure decision so it is host-unit-tested: is the room seed a genuine CHANGE relative to
//! the seed the current slot_data was parsed for? The INITIAL parse (`parsed == None`) is NOT a
//! change — it is the first parse — and an empty room seed (RoomInfo not yet seen) is ignored so a
//! transient blank never triggers a spurious reset.

/// True iff `room` is a genuine seed CHANGE relative to the already-parsed seed:
/// the room seed is non-empty AND we have previously parsed a seed AND it differs.
///
/// - `(None, "A")` -> `false`  (initial parse — nothing parsed yet)
/// - `(Some("A"), "A")` -> `false`  (reconnect to the SAME seed — must not reset)
/// - `(Some("A"), "B")` -> `true`   (switched to a different seed)
/// - `(Some("A"), "")` -> `false`  (no RoomInfo seed yet — ignore, don't reset)
pub fn is_seed_change(parsed: Option<&str>, room: &str) -> bool {
    !room.is_empty() && parsed.is_some_and(|p| p != room)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_parse_is_not_a_change() {
        // parsed == None: the first slot_data parse, not a reconnect. Must not reset.
        assert!(!is_seed_change(None, "A"));
    }

    #[test]
    fn same_seed_reconnect_is_not_a_change() {
        // Reconnect to the SAME seed: resetting here would wipe the flag-poll baseline / save
        // persistence that reconnect-to-same-seed relies on. Must stay false.
        assert!(!is_seed_change(Some("A"), "A"));
    }

    #[test]
    fn different_seed_is_a_change() {
        // Server A killed, server B on a new seed, reconnect without an ER reload -> the bug.
        assert!(is_seed_change(Some("A"), "B"));
    }

    #[test]
    fn empty_room_seed_is_ignored() {
        // RoomInfo seed not yet populated: a transient blank must never trigger a reset.
        assert!(!is_seed_change(Some("A"), ""));
    }

    #[test]
    fn empty_room_seed_ignored_even_before_first_parse() {
        assert!(!is_seed_change(None, ""));
    }
}
