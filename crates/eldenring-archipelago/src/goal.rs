//! Goal-send (SPEC-goal-send-20260701.md): detect seed completion and report it.
//!
//! The apworld ships `goalLocations` — the exact AP location-id set its Victory rule uses
//! (single boss drop for final_boss/elden_beast/capital/messmer/godrick; the Remembrance /
//! Boss Reward group for all_remembrances/all_bosses). Detection is hybrid, LOCAL-FIRST:
//!
//! - **flag goals** — ids with a `locationFlags` entry complete only when their guarding
//!   vanilla event flag (boss DefeatFlag) reads true in-game. Immune to another slot's
//!   `!collect` marking our locations checked, and reload-safe (flags persist in the save).
//! - **checked goals** — ids missing from the detection table fall back to the server-truth
//!   checked set (also satisfies dungeon-sweep-completed members, whose own flag never fired).
//!
//! An EMPTY goal set is never met (ending_condition 0/1 seeds emit empty `goalLocations`
//! until `patch_apworld_goal_locations_all_endings.py` lands; also the safe posture for
//! malformed slot_data). The caller latches `sent_goal` per session — a re-send on reconnect
//! is idempotent server-side (ds3/sdt precedent). Deliberately NOT persisted in SaveState:
//! goal-send is report-side, like `mark_checked`.

use std::collections::HashMap;

use serde_json::Value;

pub struct GoalConfig {
    /// Guarding vanilla event flags (from `locationFlags`) for flag-detectable goal locations.
    pub flag_goals: Vec<u32>,
    /// Goal location ids with no detection-flag entry: done when the checked set has them.
    pub checked_goals: Vec<i64>,
}

impl GoalConfig {
    pub fn is_empty(&self) -> bool {
        self.flag_goals.is_empty() && self.checked_goals.is_empty()
    }
}

/// Split `goalLocations` into flag-detected vs checked-fallback buckets against the
/// already-parsed `locationFlags` map. Tolerant: missing/malformed key -> empty config.
pub fn parse(sd: &Value, loc_flags: &HashMap<i64, u32>) -> GoalConfig {
    let mut flag_goals = Vec::new();
    let mut checked_goals = Vec::new();
    let ids: Vec<i64> = sd
        .get("goalLocations")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();
    for id in ids {
        match loc_flags.get(&id) {
            Some(&f) => flag_goals.push(f),
            None => checked_goals.push(id),
        }
    }
    if flag_goals.is_empty() && checked_goals.is_empty() {
        log::warn!(
            "goal: goalLocations empty -- this slot can NEVER send Goal \
             (ending_condition 0/1 pre-patch, or contract drift)"
        );
    } else {
        log::info!(
            "goal: {} location(s) -- {} flag-detected, {} checked-fallback",
            flag_goals.len() + checked_goals.len(),
            flag_goals.len(),
            checked_goals.len()
        );
    }
    GoalConfig {
        flag_goals,
        checked_goals,
    }
}

/// True when EVERY goal location is done: flag goals via `flag_read` (vanilla event flags),
/// checked goals via `is_checked` (server-truth checked set; caller pre-filters against
/// `valid_locations` -- `is_local_location_checked` panics on datapackage-unknown ids).
/// An empty config is never met.
pub fn is_met(
    cfg: &GoalConfig,
    flag_read: impl Fn(u32) -> bool,
    is_checked: impl Fn(i64) -> bool,
) -> bool {
    if cfg.is_empty() {
        return false;
    }
    cfg.flag_goals.iter().all(|&f| flag_read(f)) && cfg.checked_goals.iter().all(|&l| is_checked(l))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lf(pairs: &[(i64, u32)]) -> HashMap<i64, u32> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn empty_goal_set_is_never_met() {
        let cfg = parse(&json!({}), &lf(&[]));
        assert!(cfg.is_empty());
        assert!(!is_met(&cfg, |_| true, |_| true));
    }

    #[test]
    fn flag_goals_require_all_flags() {
        let cfg = parse(
            &json!({"goalLocations": [10, 20]}),
            &lf(&[(10, 800), (20, 850)]),
        );
        assert_eq!(cfg.flag_goals, vec![800, 850]);
        assert!(cfg.checked_goals.is_empty());
        assert!(!is_met(&cfg, |f| f == 800, |_| false)); // one boss down, one to go
        assert!(is_met(&cfg, |_| true, |_| false)); // checked set never consulted
    }

    #[test]
    fn table_missing_ids_use_checked_fallback() {
        let cfg = parse(&json!({"goalLocations": [10, 99]}), &lf(&[(10, 800)]));
        assert_eq!(cfg.flag_goals, vec![800]);
        assert_eq!(cfg.checked_goals, vec![99]);
        assert!(!is_met(&cfg, |_| true, |_| false)); // flag done, fallback not checked
        assert!(is_met(&cfg, |_| true, |l| l == 99));
    }

    #[test]
    fn malformed_slot_data_is_tolerated() {
        let cfg = parse(&json!({"goalLocations": "oops"}), &lf(&[(10, 800)]));
        assert!(cfg.is_empty());
        let cfg = parse(
            &json!({"goalLocations": [10, "bad", null]}),
            &lf(&[(10, 800)]),
        );
        assert_eq!(cfg.flag_goals, vec![800]); // non-int members skipped, not fatal
    }
}
