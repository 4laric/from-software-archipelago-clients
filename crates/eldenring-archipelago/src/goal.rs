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
    ///
    /// ALSO CARRIES `goalRequiredItems` (2026-07-30): this seed's kept Region Locks, minus the
    /// precollected start anchor. Same class of bug, one layer out. `core.set_rules` tells
    /// Archipelago the slot completes on `has_all(kept locks)` -- that is what fill balances around
    /// -- but `is_met` below checked the goal BOSS FLAGS ALONE, and OUR send is what ends the run.
    /// Region access is warp, so every kept region sits at sphere ~1 and fill may legitimately put
    /// the terminal region's Lock in sphere 0: measured over generated seeds, 25% of rolled draws
    /// made the goal region the SECOND region opened. The player killed the boss and the run ended
    /// while the world still claimed every lock was required. Two terminal conditions, one of them
    /// silently ignored. They are ONE list now.
    ///
    /// Absent on a foreign apworld, on natural_progression (which mints no Lock items at all), and
    /// on any pre-0.2.18 seed -> empty -> no added requirement, exactly as before.
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
    fn str_list(sd: &Value, key: &str) -> Vec<String> {
        sd.get(key)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
    let runes = str_list(sd, "great_rune_items");
    // `goalRequiredItems`: the kept Region Locks the world's own completion_condition requires.
    // Additive, never replacing -- a great_runes seed needs the runes AND the locks.
    let locks = str_list(sd, "goalRequiredItems");
    if !runes.is_empty() {
        log::info!(
            "goal: {} item(s) must be HELD, not merely their boss killed: {}",
            runes.len(),
            runes.join(", ")
        );
    }
    if !locks.is_empty() {
        log::info!(
            "goal: {} Region Lock(s) must also be HELD before Goal is sent (the world's own \
             completion condition; pre-0.2.18 seeds omit this key and behave as before)",
            locks.len()
        );
    }
    let mut item_goals: Vec<String> = runes;
    item_goals.extend(locks);
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

/// The goal as a line for the client log: WHICH locations end this run, and which region they
/// sit in. Pure so it can be tested; `log_goal` below emits it.
///
/// WHY THIS EXISTS (2026-08-09). `parse` above logs a COUNT -- "goal: 3 location(s) -- 3
/// flag-detected" -- and the generator's own log already names the answer ("goal = <Region>
/// (n location(s))"). But the artifact that actually reaches us when a player reports a bad
/// ending is the CLIENT log, not the generation log. On 2026-08-07 a `dlc_only` seed ended on
/// Romina in the Ancient Ruins of Rauh; the player read the early goal as a broken ending, and
/// nothing in his log could say which boss the goal even was, so triage began by asking him for
/// slot_data. The datapackage has carried the answer the whole time and we never printed it.
///
/// The region is DERIVED from the location name -- ours are "<Region> :: <what> [fNNNNNN]" --
/// so this needs no new slot_data key and no contract move. Returns None when there is nothing
/// to say; the empty-goal case is already a WARN in `parse` and must not be said twice.
pub fn describe_goal(sd: &Value, resolve: impl Fn(i64) -> Option<String>) -> Option<String> {
    let ids: Vec<i64> = sd
        .get("goalLocations")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return None;
    }
    // An id the datapackage cannot name is printed AS its id rather than dropped: a goal location
    // the datapackage does not know is itself the thing worth seeing in the log.
    let named: Vec<String> = ids
        .iter()
        .map(|id| resolve(*id).unwrap_or_else(|| format!("<unnamed id {id}>")))
        .collect();
    // Name the region ONLY when every name carries one and they all agree. A disagreeing or
    // unnameable set means either the resolution ladder did something we did not predict or we are
    // on a foreign datapackage -- and "goal: region Enir Ilim" over either of those is exactly the
    // confidently-wrong line this function exists to prevent. Say the locations, decline the region.
    let regions: Vec<Option<&str>> = named
        .iter()
        .map(|n| n.split_once(" :: ").map(|(r, _)| r))
        .collect();
    let agreed: Option<&str> = match regions.first().copied().flatten() {
        Some(r) if regions.iter().all(|x| *x == Some(r)) => Some(r),
        _ => None,
    };
    Some(match agreed {
        Some(r) => format!(
            "goal: region {r} -- {} location(s) end this run: {}",
            named.len(),
            named.join(", ")
        ),
        None => format!(
            "goal: {} location(s) end this run, not all in one named region: {}",
            named.len(),
            named.join(", ")
        ),
    })
}

/// `describe_goal`, emitted. Silent when there is nothing to say.
pub fn log_goal(sd: &Value, resolve: impl Fn(i64) -> Option<String>) {
    if let Some(line) = describe_goal(sd, resolve) {
        log::info!("{line}");
    }
}

/// The goal's ITEM requirement, as one line for the player-visible channel (world#656).
///
/// Thin wrapper: the sentence itself lives in [`er_logic::goal_text::required_items_line`] so it is
/// host-tested rather than gated behind a Windows-only build. `None` when the goal needs no items,
/// so a region_locks seed gains no banner line.
pub fn describe_required_items(cfg: &GoalConfig) -> Option<String> {
    er_logic::goal_text::required_items_line(&cfg.item_goals)
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

    // --- goalRequiredItems: the kept Region Locks (2026-07-30) -------------------------------------
    //
    // MOTIVATING CASE (CONTRIBUTING rule 11). Measured over generated seeds: on 25% of rolled
    // `num_regions` draws, fill placed the goal region's Lock in sphere 0 -- the goal region was the
    // SECOND region opened. The player killed the goal boss and `is_met` fired, while the world's own
    // `completion_condition` still required every kept lock. These tests are that seed, inverted.

    #[test]
    fn killing_the_goal_boss_on_region_two_is_not_finishing_the_seed() {
        // THE BUG. Every goal flag reads true (boss dead), every location checked -- but the player
        // holds none of the seed's other Region Locks. Goal must NOT fire.
        let cfg = parse(
            &json!({
                "goalLocations": [10],
                "goalRequiredItems": ["Caelid Lock", "Farum Azula Lock"]
            }),
            &lf(&[(10, 800)]),
        );
        assert_eq!(
            cfg.item_goals,
            vec!["Caelid Lock".to_string(), "Farum Azula Lock".to_string()]
        );
        assert!(
            !is_met(&cfg, |_| true, |_| true, |_| false),
            "goal boss dead on the second region, locks unheld -- the run is NOT over"
        );
        assert!(
            !is_met(&cfg, |_| true, |_| true, |n| n == "Caelid Lock"),
            "holding one of two locks is not done"
        );
        assert!(is_met(&cfg, |_| true, |_| true, |n| n.ends_with(" Lock")));
    }

    #[test]
    fn required_locks_compose_with_great_runes_rather_than_replacing_them() {
        // A great_runes seed needs the runes AND the locks. Neither key may shadow the other --
        // they arrive as two separate slot_data keys and must both land in item_goals.
        let cfg = parse(
            &json!({
                "goalLocations": [],
                "great_rune_items": ["Godrick's Great Rune"],
                "goalRequiredItems": ["Limgrave Lock"]
            }),
            &lf(&[]),
        );
        assert_eq!(cfg.item_goals.len(), 2, "one key overwrote the other");
        assert!(
            !is_met(&cfg, |_| true, |_| true, |n| n == "Limgrave Lock"),
            "locks held but the rune was never received"
        );
        assert!(
            !is_met(&cfg, |_| true, |_| true, |n| n == "Godrick's Great Rune"),
            "rune held but a kept lock is missing"
        );
        assert!(is_met(&cfg, |_| true, |_| true, |_| true));
    }

    #[test]
    fn a_seed_without_the_key_is_untouched() {
        // Pre-0.2.18 seeds, foreign apworlds, and natural_progression (which mints NO Lock items --
        // requiring them would deadlock the seed) all omit the key. Absent => no added requirement.
        let cfg = parse(&json!({"goalLocations": [10]}), &lf(&[(10, 800)]));
        assert!(cfg.item_goals.is_empty());
        assert!(is_met(&cfg, |_| true, |_| true, |_| false));
    }

    #[test]
    fn required_locks_alone_are_a_valid_goal() {
        // is_empty() already accounts for item_goals; pin it for THIS key too, or a lock-only config
        // would read as "can never be met" and fail closed on a legitimate seed.
        let cfg = parse(&json!({"goalRequiredItems": ["Limgrave Lock"]}), &lf(&[]));
        assert!(!cfg.is_empty());
        assert!(is_met(&cfg, |_| true, |_| true, |n| n == "Limgrave Lock"));
    }
}

#[cfg(test)]
mod goal_echo {
    //! THE CLIENT LOG MUST NAME THE ENDING.
    //!
    //! Motivating case, 2026-08-07: a `dlc_only` seed goaled on Romina and the player reported a
    //! broken ending. The ladder was right -- Romina carries a Remembrance and the draw never kept
    //! Enir Ilim -- but proving that took his slot_data, because his client log said only
    //! "goal: 1 location(s) -- 1 flag-detected". These assert the log can answer it alone.
    use super::*;
    use serde_json::json;

    /// Our real location-name shape: "<Region> :: <what> [fNNNNNN]".
    fn er_names(id: i64) -> Option<String> {
        Some(
            match id {
                7770775 => "Ancient Ruins :: Remembrance of the Saint of the Bud - Romina [f510600]",
                7770770 => "Enir Ilim :: Remembrance of a God and a Lord - Promised Consort Radahn [f510430]",
                7770300 => "Ashen Capital :: Remembrance of Hoarah Loux - Godfrey [f510070]",
                7770301 => "Ashen Capital :: Elden Remembrance - Elden Beast [f510230]",
                _ => return None,
            }
            .to_string(),
        )
    }

    #[test]
    fn the_romina_seed_names_its_region_and_its_boss() {
        let line = describe_goal(&json!({"goalLocations": [7770775]}), er_names).unwrap();
        assert!(line.contains("region Ancient Ruins"), "{line}");
        assert!(line.contains("Romina"), "{line}");
        // The whole point: this line alone answers "which boss ends my run".
        assert!(line.contains("1 location(s)"), "{line}");
    }

    #[test]
    fn a_two_boss_finale_still_names_one_region() {
        let line = describe_goal(&json!({"goalLocations": [7770300, 7770301]}), er_names).unwrap();
        assert!(line.contains("region Ashen Capital"), "{line}");
        assert!(
            line.contains("Godfrey") && line.contains("Elden Beast"),
            "{line}"
        );
    }

    #[test]
    fn regions_that_disagree_are_not_collapsed_into_one_claim() {
        // Not a shape the ladder produces today -- which is the reason to assert it. If it ever
        // does, the log must say so rather than confidently name the first region it saw.
        let line = describe_goal(&json!({"goalLocations": [7770775, 7770770]}), er_names).unwrap();
        assert!(!line.contains("region Ancient Ruins"), "{line}");
        assert!(line.contains("not all in one named region"), "{line}");
        assert!(line.contains("Romina") && line.contains("Radahn"), "{line}");
    }

    #[test]
    fn an_id_the_datapackage_cannot_name_is_printed_not_dropped() {
        // Foreign datapackage / contract drift: losing the id silently is how a goal set stops
        // being auditable at all.
        let line = describe_goal(&json!({"goalLocations": [7770775, 999]}), er_names).unwrap();
        assert!(line.contains("<unnamed id 999>"), "{line}");
        assert!(line.contains("2 location(s)"), "{line}");
    }

    #[test]
    fn nothing_to_say_says_nothing() {
        // `parse` already WARNs on the empty set; a second line about it would be noise, and the
        // Bedrock `goal`-key seeds legitimately have no `goalLocations` at all.
        assert!(describe_goal(&json!({}), er_names).is_none());
        assert!(describe_goal(&json!({"goalLocations": []}), er_names).is_none());
        assert!(describe_goal(&json!({"goal": [9101]}), er_names).is_none());
    }
}
