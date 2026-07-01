//! Pure region-lock decisions, extracted from `features.rs`.
//!
//! These are the decision halves only — the latch/flag side effects stay in the Windows code and
//! get covered later via the `GameHook` seam (PR-C). Here we lock the pure rules: when a region
//! counts as locked (→ kick), and when a natural-key clause set fires.

use std::collections::HashSet;

/// Decide whether the player should be KICKED this tick: the current region is in a locked range
/// AND the random-start guard allows it (non-random seed, or the random-start warp already done).
///
///  - `pr` — raw `play_region_id`. Overworld sub-areas report a 7-digit id (`subregion * 100`); the
///    major area reports the 5-digit subregion. We reduce a 7-digit id to its 5-digit subregion
///    (matches `features.rs`: `if pr >= 1_000_000 { pr / 100 }`).
///  - `area_lock_flags` — `[lo, hi, open_flag]` inclusive 5-digit subregion ranges; a range is
///    locked when its open flag is off.
///  - `random_start_done_flag` — `0` means non-random (no guard); else the kick waits until set.
pub fn kick_decision(
    pr: i32,
    area_lock_flags: &[[i32; 3]],
    random_start_done_flag: u32,
    get_flag: &dyn Fn(u32) -> bool,
) -> bool {
    let sub = if pr >= 1_000_000 { pr / 100 } else { pr };
    let locked = area_lock_flags
        .iter()
        .any(|e| sub >= e[0] && sub <= e[1] && !get_flag(e[2] as u32));
    if !locked {
        return false;
    }
    random_start_done_flag == 0 || get_flag(random_start_done_flag)
}

/// One natural-key clause: ALL items received AND ALL flags set => the clause is satisfied.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NkClause {
    pub items: Vec<String>,
    pub flags: Vec<u32>,
}

/// A region's natural-key trigger fires when ANY clause is satisfied (anyOf disjunction).
pub fn natural_key_fired(
    clauses: &[NkClause],
    received: &HashSet<String>,
    get_flag: &dyn Fn(u32) -> bool,
) -> bool {
    clauses.iter().any(|cl| {
        cl.items.iter().all(|n| received.contains(n)) && cl.flags.iter().all(|&f| get_flag(f))
    })
}

/// Rising-edge enforcement latch for region-lock (and reused for incoming DeathLink). `fire(active)`
/// returns true exactly once each time `active` rises false->true, and re-arms only when `active` goes
/// false. For region-lock, `active` = [`kick_decision`] (locked AND guard-open). This both throttles
/// to one action per lock-entry AND is the death-loop guard under KILL enforcement: after a kill the
/// player respawns; if they land STILL locked, `active` stays true so the latch won't re-fire — it
/// only re-arms once they leave the locked region. Pure + host-tested; the Windows code holds one of
/// these (per enforcement site) and calls `fire` each tick with the live decision.
#[derive(Debug, Default, Clone)]
pub struct EnforcementLatch {
    armed: bool,
}

impl EnforcementLatch {
    pub const fn new() -> Self {
        Self { armed: false }
    }

    /// True on the rising edge of `active` (the one tick it goes false->true); re-arms when `active`
    /// is false. Idempotent while `active` stays true (returns false after the first).
    pub fn fire(&mut self, active: bool) -> bool {
        if active {
            !std::mem::replace(&mut self.armed, true)
        } else {
            self.armed = false;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // [lo, hi, open_flag] — a 5-digit subregion range gated on open flag 76980.
    const CAELID_LOCK: [i32; 3] = [60000, 60999, 76980];

    fn names(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn locked_region_with_open_flag_off_kicks() {
        // 5-digit subregion 60010, open flag off -> locked.
        assert!(kick_decision(60010, &[CAELID_LOCK], 0, &|_| false));
    }

    #[test]
    fn normalizes_7digit_overworld_id() {
        // Overworld reports subregion*100 = 60010 * 100 = 6_001_000 (>= 1_000_000);
        // /100 -> 60010, still inside [60000, 60999].
        assert!(kick_decision(6_001_000, &[CAELID_LOCK], 0, &|_| false));
    }

    #[test]
    fn open_flag_set_means_not_locked_no_kick() {
        assert!(!kick_decision(60010, &[CAELID_LOCK], 0, &|f| f == 76980));
    }

    #[test]
    fn region_outside_all_ranges_no_kick() {
        // 5-digit subregion 10000, not in [60000, 60999].
        assert!(!kick_decision(10000, &[CAELID_LOCK], 0, &|_| false));
    }

    #[test]
    fn random_start_guard_suppresses_kick_until_warp_done() {
        let done = 76950u32;
        // Locked region but the random-start warp hasn't fired -> guard suppresses the kick.
        assert!(!kick_decision(60010, &[CAELID_LOCK], done, &|_| false));
        // Once the done flag is set, the guard passes -> kick.
        assert!(kick_decision(60010, &[CAELID_LOCK], done, &|f| f == done));
    }

    #[test]
    fn nk_fully_satisfied_clause_fires() {
        let clauses = vec![NkClause {
            items: vec!["Rold Medallion".into()],
            flags: vec![11000800],
        }];
        let recv = names(&["Rold Medallion"]);
        assert!(natural_key_fired(&clauses, &recv, &|f| f == 11000800));
    }

    #[test]
    fn nk_item_present_but_flag_missing_does_not_fire() {
        let clauses = vec![NkClause {
            items: vec!["Rold Medallion".into()],
            flags: vec![11000800],
        }];
        let recv = names(&["Rold Medallion"]);
        assert!(!natural_key_fired(&clauses, &recv, &|_| false));
    }

    #[test]
    fn nk_flag_set_but_item_missing_does_not_fire() {
        let clauses = vec![NkClause {
            items: vec!["Rold Medallion".into()],
            flags: vec![11000800],
        }];
        assert!(!natural_key_fired(&clauses, &names(&[]), &|f| f == 11000800));
    }

    #[test]
    fn nk_second_clause_satisfied_fires_even_if_first_isnt() {
        let clauses = vec![
            NkClause {
                items: vec!["Missing".into()],
                flags: vec![],
            },
            NkClause {
                items: vec![],
                flags: vec![71000, 71001],
            },
        ];
        assert!(natural_key_fired(&clauses, &names(&[]), &|f| f == 71000 || f == 71001));
    }

    #[test]
    fn nk_empty_clause_is_vacuously_true() {
        let clauses = vec![NkClause::default()];
        assert!(natural_key_fired(&clauses, &names(&[]), &|_| false));
    }

    // --- EnforcementLatch (kick/kill rising-edge throttle + death-loop guard) ---

    #[test]
    fn latch_fires_once_on_entry() {
        let mut l = EnforcementLatch::new();
        assert!(l.fire(true)); // rising edge -> fire
        assert!(!l.fire(true)); // still locked -> no re-fire
        assert!(!l.fire(true));
    }

    #[test]
    fn latch_rearms_after_leaving_and_refires() {
        let mut l = EnforcementLatch::new();
        assert!(l.fire(true)); // enter locked -> fire
        assert!(!l.fire(false)); // leave (unlocked) -> re-arm, no fire
        assert!(l.fire(true)); // re-enter -> fire again
    }

    #[test]
    fn latch_death_loop_guard_no_refire_while_locked() {
        // Models a KILL whose respawn lands the player STILL in the locked region: `active` stays
        // true across the kill+respawn, so the latch must NOT re-fire (no death loop).
        let mut l = EnforcementLatch::new();
        assert!(l.fire(true)); // violation -> kill
        for _ in 0..100 {
            assert!(!l.fire(true)); // respawned still-locked -> never re-fires
        }
        assert!(!l.fire(false)); // finally leaves -> re-arm
        assert!(l.fire(true)); // a fresh violation later fires
    }

    #[test]
    fn latch_inactive_never_fires() {
        let mut l = EnforcementLatch::new();
        assert!(!l.fire(false));
        assert!(!l.fire(false));
    }
}
