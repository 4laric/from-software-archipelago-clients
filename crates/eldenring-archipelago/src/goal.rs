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
    /// `great_rune_items` -- item NAMES the player must HOLD (have RECEIVED) before Goal can fire.
    ///
    /// THE BUG THIS FIXES. The `great_runes` ending's own docstring promises "ALSO **collect** Great
    /// Runes", and AP's victory rule is exactly that: `state.has(rune)`. But the client's goal was
    /// LOCATION-based, and the apworld expressed "collect Godrick's Great Rune" as "check the location
    /// Godrick's boss drop" -- i.e. KILL GODRICK. With item shuffle on (frozen ON), Godrick's Great
    /// Rune is NOT at Godrick; it is anywhere in the multiworld. So you could send Goal having killed
    /// every rune boss and never held a single Great Rune, and the run would end.
    ///
    /// A kill is not a collection. Goal now requires the ITEM.
    pub item_goals: Vec<String>,
}

impl GoalConfig {
    pub fn is_empty(&self) -> bool {
        self.flag_goals.is_empty() && self.checked_goals.is_empty() && self.item_goals.is_empty()
    }
}

/// Split `goalLocations` into flag-detected vs checked-fallback buckets against the
/// already-parsed `locationFlags` map. Tolerant: missing/malformed key -> empty config.
pub fn parse(sd: &Value, loc_flags: &HashMap<i64, u32>) -> GoalConfig {
    let mut flag_goals = Vec::new();
    let mut checked_goals = Vec::new();
    // `great_rune_items`: item NAMES that must have been RECEIVED. It shipped for months as a
    // NO-READ DIAGNOSTIC -- the apworld sent the answer and the client never looked, which is exactly
    // how the bug survived. Absent on a foreign apworld and on any ending needing no items -> empty,
    // which adds no requirement.
    let item_goals: Vec<String> = sd
        .get("great_rune_items")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    if !item_goals.is_empty() {
        log::info!(
            "goal: {} item(s) must be HELD, not merely their boss killed: {}",
            item_goals.len(),
            item_goals.join(", ")
        );
    }
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

    // FOREIGN-APWORLD GOAL (`goal`). Bedrock's apworld emits no `goalLocations` at all -- it emits
    //
    //     "goal": [boss.flag for boss in self.goal_bosses]
    //
    // i.e. the boss DEFEAT FLAGS directly, not AP location ids. Same intent, one step further along:
    // we would have mapped ids -> flags via loc_flags anyway, and he hands us the flags. So take them
    // as flag goals as-is.
    //
    // Without this a Bedrock seed can NEVER be completed -- the goal set parses empty, `is_empty()` is
    // true forever, and the client never sends Goal. The slot is unwinnable, silently. This is the
    // single thing standing between our client and a playable foreign seed, and it is ten lines.
    //
    // `goalLocations` still WINS when present: our own seeds are untouched. Only consulted as a
    // fallback, so a world that emits both is unaffected.
    if flag_goals.is_empty() && checked_goals.is_empty() {
        let foreign: Vec<u32> = sd
            .get("goal")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_u64())
                    .filter(|&f| f != 0)
                    .map(|f| f as u32)
                    .collect()
            })
            .unwrap_or_default();
        if !foreign.is_empty() {
            log::info!(
                "goal: no `goalLocations` -- using the foreign `goal` key ({} boss defeat flag(s)). \
                 This is the Bedrock-apworld shape: flags, not location ids.",
                foreign.len()
            );
            flag_goals = foreign;
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
        item_goals,
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
    has_item: impl Fn(&str) -> bool,
) -> bool {
    if cfg.is_empty() {
        return false;
    }
    cfg.flag_goals.iter().all(|&f| flag_read(f))
        && cfg.checked_goals.iter().all(|&l| is_checked(l))
        // HELD, not killed. See GoalConfig::item_goals.
        && cfg.item_goals.iter().all(|n| has_item(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(super) fn lf(pairs: &[(i64, u32)]) -> HashMap<i64, u32> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn empty_goal_set_is_never_met() {
        let cfg = parse(&json!({}), &lf(&[]));
        assert!(cfg.is_empty());
        assert!(!is_met(&cfg, |_| true, |_| true, |_| false));
    }

    #[test]
    fn flag_goals_require_all_flags() {
        let cfg = parse(
            &json!({"goalLocations": [10, 20]}),
            &lf(&[(10, 800), (20, 850)]),
        );
        assert_eq!(cfg.flag_goals, vec![800, 850]);
        assert!(cfg.checked_goals.is_empty());
        assert!(!is_met(&cfg, |f| f == 800, |_| false, |_| false)); // one boss down, one to go
        assert!(is_met(&cfg, |_| true, |_| false, |_| false)); // checked set never consulted
    }

    #[test]
    fn table_missing_ids_use_checked_fallback() {
        let cfg = parse(&json!({"goalLocations": [10, 99]}), &lf(&[(10, 800)]));
        assert_eq!(cfg.flag_goals, vec![800]);
        assert_eq!(cfg.checked_goals, vec![99]);
        assert!(!is_met(&cfg, |_| true, |_| false, |_| false)); // flag done, fallback not checked
        assert!(is_met(&cfg, |_| true, |l| l == 99, |_| false));
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

#[cfg(test)]
mod foreign_goal {
    //! A FOREIGN SEED MUST BE WINNABLE.
    //!
    //! Bedrock's apworld emits `goal` (boss defeat FLAGS), never `goalLocations` (AP location ids).
    //! Without the fallback below his seed parses an empty goal set, `is_empty()` is true forever,
    //! and the client never sends Goal -- the slot is unwinnable and says nothing about it.
    use super::tests::lf; // the great_rune_items cases below share the sibling module's flag-map helper
    use super::*;
    use serde_json::json;

    #[test]
    fn bedrock_goal_flags_are_taken_as_flag_goals() {
        // His shape, hand-written from his fill_slot_data -- not copied from his data.
        let sd = json!({ "goal": [9101u64, 9118u64], "apIdsToItemIds": {} });
        let cfg = parse(&sd, &HashMap::new());
        assert_eq!(cfg.flag_goals, vec![9101u32, 9118u32]);
        assert!(cfg.checked_goals.is_empty());
        assert!(!cfg.is_empty(), "a Bedrock seed must be COMPLETABLE");
    }

    #[test]
    fn goal_locations_still_wins_when_present() {
        // Our own seeds must be byte-for-byte unaffected: `goal` is a FALLBACK, never an override.
        let mut lf = HashMap::new();
        lf.insert(7770001i64, 60510u32);
        let sd = json!({ "goalLocations": [7770001i64], "goal": [9999u64] });
        let cfg = parse(&sd, &lf);
        assert_eq!(
            cfg.flag_goals,
            vec![60510u32],
            "goalLocations must win; `goal` is fallback only"
        );
        assert!(!cfg.flag_goals.contains(&9999));
    }

    #[test]
    fn neither_key_is_still_never_met() {
        // The safe posture: an empty goal set is NEVER satisfied. Do not regress that into
        // "no goal == instant victory".
        let cfg = parse(&json!({}), &HashMap::new());
        assert!(cfg.is_empty());
    }

    #[test]
    fn zero_and_malformed_goal_entries_are_dropped_not_trusted() {
        let sd = json!({ "goal": [0u64, 9101u64, "nonsense"] });
        let cfg = parse(&sd, &HashMap::new());
        assert_eq!(
            cfg.flag_goals,
            vec![9101u32],
            "flag 0 is not a flag; a string is not a flag"
        );
    }

    // --- great_rune_items: HELD, not killed (2026-07-14) ---------------------------------------------

    #[test]
    fn empty_item_goals_add_no_requirement() {
        // Every existing seed: no `great_rune_items` key -> the item predicate is never consulted, so a
        // location-only goal behaves exactly as before. (has_item returns false throughout above.)
        let cfg = parse(&json!({"goalLocations": [10]}), &lf(&[(10, 800)]));
        assert!(cfg.item_goals.is_empty());
        assert!(is_met(&cfg, |_| true, |_| true, |_| false));
    }

    #[test]
    fn killing_the_boss_is_not_holding_the_rune() {
        // THE BUG. The great_runes ending promises "collect Great Runes" and AP enforces state.has().
        // The client used to fire Goal on the boss LOCATION being checked -- but with item shuffle on,
        // Godrick's Great Rune is not at Godrick. Every location done, rune never received => NOT met.
        let cfg = parse(
            &json!({"goalLocations": [10], "great_rune_items": ["Godrick's Great Rune"]}),
            &lf(&[(10, 800)]),
        );
        assert_eq!(cfg.item_goals, vec!["Godrick's Great Rune".to_string()]);
        assert!(
            !is_met(&cfg, |_| true, |_| true, |_| false),
            "boss dead and every location checked, but the rune was never RECEIVED -- Goal must not fire"
        );
        assert!(is_met(
            &cfg,
            |_| true,
            |_| true,
            |n| n == "Godrick's Great Rune"
        ));
    }

    #[test]
    fn every_item_goal_is_required() {
        let cfg = parse(
            &json!({"goalLocations": [], "great_rune_items": ["A", "B"]}),
            &lf(&[]),
        );
        assert!(
            !is_met(&cfg, |_| true, |_| true, |n| n == "A"),
            "holding one of two is not done"
        );
        assert!(is_met(&cfg, |_| true, |_| true, |n| n == "A" || n == "B"));
    }

    #[test]
    fn item_goals_alone_are_a_valid_goal() {
        // is_empty() must account for item_goals, or a goal made only of items would read as EMPTY
        // and "can never be met" -- the exact fail-closed branch that would silently brick the ending.
        let cfg = parse(&json!({"great_rune_items": ["A"]}), &lf(&[]));
        assert!(!cfg.is_empty());
        assert!(is_met(&cfg, |_| true, |_| true, |n| n == "A"));
    }
}
