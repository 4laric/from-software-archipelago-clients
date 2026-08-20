//! Open the goal region when every other goal item is held (world#768).
//!
//! # What changes
//!
//! The goal region's own Lock stops being an item in the pool. Nothing arrives, so the client has
//! to reach the same end state on its own: when the player holds every OTHER goal requirement, the
//! goal region's open flag and grace bundle are set — exactly the flags a Lock receipt produces
//! today (`ItemSemantics::RegionFlags([open] + lock_reveal_flags)`).
//!
//! # Why this is the enforcement world#694 could not find
//!
//! That issue looked for a way to gate the goal arena and ran out of mechanisms. `fogwall.rs` is
//! walk-through by design and says so ("This module never blocks or enforces anything"). Withholding
//! a VANILLA gate flag was datamined and killed: Enir-Ilim reads *possession* of Messmer's Kindling
//! (five `PlayerHasItem` sites, zero flag reads) and the Erdtree path has no key item at all. What
//! remained was the KICK — the repo's filed softlock precedent (world#589).
//!
//! None of that is needed, because there IS a key to withhold and it is OURS. The Ashen Capital is
//! reached by warping to its own graces (`SPEC-ashen-capital-lock.md:162`, *"never through the
//! capital's rune gate"*), so a player without the Lock cannot stand in the arena. No wall, no
//! eject, no cosmetic fog to explain a teleport.
//!
//! # 🛑 THE MODE QUESTION, AND WHY THERE IS NO `if natural_progression` HERE
//!
//! Alaric ruled (world#768) that under `natural_progression` the trigger is **the rune count
//! alone**. That does not need a branch, and the reason is worth stating so nobody adds one:
//!
//! NP mints **no region Lock items at all**, so the world's `goalRequiredItems` carries only the
//! Great Runes on such a seed. The single rule *"every goal item except the goal region's own lock
//! is held"* therefore evaluates to "the runes" under NP and to "locks + runes minus the goal lock"
//! normally. The mode-independence is a property of the WIRE, not of a conditional here.
//!
//! ⚠️ It also sidesteps the circularity that killed the first draft of this design. NP's world-side
//! completion condition is `can_reach(goal_region) && runes`, and keying the grant on reachability
//! would have been: open the region when the player can reach the region. This module never asks
//! about reachability — only about held items.
//!
//! `vanilla_placement` shares NP's branch world-side and has no lock items either, so it lands in
//! the same place for the same reason.
//!
//! # 🛑 FAIL OPEN, AND BE PRECISE ABOUT WHICH HALF
//!
//! Two different things are often both called "fail open" and only one of them is this module's:
//!
//! * **The CONDITION is evaluated honestly.** Not knowing whether an item is held is not a reason to
//!   open the goal.
//! * **An UNRESOLVABLE gate opens.** If the goal region's lock item cannot be identified at connect,
//!   the region can never open by any other route — the Lock is not in the pool — so withholding it
//!   is an unwinnable seed. An early-open goal region costs a player a spoiled ending; a
//!   never-open one costs the run, plus every other player's items placed inside it (world#589,
//!   forty-two of them). So [`Decision::OpenUnresolvable`] exists and is LOUD.
//!
//! The write side (re-apply until readback confirms) is the caller's, and is the discipline
//! world#200 was born from — the capital reconciler wrote once, trusted the latch, and left a
//! player in a burnt world.

use std::collections::{BTreeSet, HashMap};

/// What the caller should do this tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Hold the goal region shut: at least one goal item is still outstanding.
    Withhold {
        /// Goal items not yet held, sorted. For the log line and nothing else.
        outstanding: Vec<String>,
    },
    /// Every other goal item is held — set the open flag and the grace bundle.
    Open,
    /// The gate could not be resolved, so it opens rather than risking an unwinnable seed.
    /// The caller MUST log this at warn: it means the seed is playable but the arena is unguarded.
    OpenUnresolvable { why: &'static str },
}

impl Decision {
    /// Should the caller write the open flag + bundle this tick?
    pub fn opens(&self) -> bool {
        matches!(self, Decision::Open | Decision::OpenUnresolvable { .. })
    }
}

/// The gate's inputs, all taken from slot_data the world sent — never recomputed client-side.
///
/// 🛑 ONE LIST, NOT TWO. `core.py:783` (world) is explicit that the terminal conditions "are
/// supposed to read ONE list, and a lock missing from that list is exactly the drift the
/// 2026-07-30 alignment fixed". `item_goals` here is `goal.rs`'s parsed `goalRequiredItems` +
/// `great_rune_items`, i.e. the same list `is_met` waits on. Deriving a second copy from the item
/// map or the region table would reintroduce precisely that drift.
#[derive(Debug, Clone, Default)]
pub struct GoalGate {
    /// Every non-rune item the goal requires the player to HOLD.
    pub item_goals: Vec<String>,
    /// Full eligible Great Rune set and the number required from it.
    pub rune_goals: Vec<String>,
    pub runes_required: usize,
    /// The goal region's own lock item name, if it could be resolved. `None` => unresolvable.
    pub goal_lock_item: Option<String>,
}

/// Decide whether the goal region should be opened.
///
/// `held` answers "is this item in the player's possession", and is the client's ownership seam --
/// the same question `goal.rs::is_met` asks for `item_goals`.
pub fn decide(gate: &GoalGate, held: &dyn Fn(&str) -> bool) -> Decision {
    let Some(goal_lock) = gate.goal_lock_item.as_deref() else {
        return Decision::OpenUnresolvable {
            why: "the goal region's lock item did not resolve, and it is not in the pool -- \
                  withholding it would make the seed unwinnable",
        };
    };

    // The goal region's own lock is excluded: it is the thing being granted, so requiring it would
    // be requiring the output as an input.
    let mut outstanding: BTreeSet<String> = gate
        .item_goals
        .iter()
        .map(String::as_str)
        .filter(|name| *name != goal_lock)
        .filter(|name| !held(name))
        .map(str::to_owned)
        .collect();
    let held_runes = gate.rune_goals.iter().filter(|name| held(name)).count();
    if held_runes < gate.runes_required {
        outstanding.insert(format!(
            "Great Runes ({held_runes}/{})",
            gate.runes_required
        ));
    }

    if outstanding.is_empty() {
        Decision::Open
    } else {
        Decision::Withhold {
            outstanding: outstanding.into_iter().collect(),
        }
    }
}

/// The exact event-flag convergence target for a goal-region unlock.
///
/// A resolved gate touches only its own open flag and bundle. An unresolved gate cannot identify
/// which withheld Lock the world removed from the pool, so failing open means opening every region
/// apparatus the seed advertised. That is deliberately broad: contract drift may spoil traversal,
/// but it cannot strand the run or foreign players' items behind an impossible Lock.
pub fn flags_to_open(
    open_flag: Option<u32>,
    lock_item: Option<&str>,
    region_open_flags: &HashMap<String, u32>,
    lock_reveal_flags: &HashMap<String, Vec<u32>>,
    region_graces: &HashMap<String, Vec<u32>>,
) -> Vec<u32> {
    let mut out = Vec::new();
    if let Some(flag) = open_flag.filter(|flag| *flag != 0) {
        out.push(flag);
        if let Some(lock) = lock_item {
            if let Some(flags) = lock_reveal_flags.get(lock) {
                out.extend(flags.iter().copied());
            }
            if let Some(flags) = region_graces.get(lock) {
                out.extend(flags.iter().copied());
            }
        }
    } else {
        out.extend(region_open_flags.values().copied());
        out.extend(lock_reveal_flags.values().flatten().copied());
        out.extend(region_graces.values().flatten().copied());
    }
    out.retain(|flag| *flag != 0);
    out.sort_unstable();
    out.dedup();
    out
}

/// The one-line status for the log. Callers latch on the transition, not on this string.
pub fn status_line(d: &Decision, goal_region: &str) -> String {
    match d {
        Decision::Open => format!("goal-gate: every goal item is held -- opening {goal_region}"),
        Decision::OpenUnresolvable { why } => {
            format!("goal-gate: OPENING {goal_region} WITHOUT A GATE -- {why}")
        }
        Decision::Withhold { outstanding } => format!(
            "goal-gate: {} goal item(s) outstanding, {goal_region} stays shut -- {}",
            outstanding.len(),
            outstanding.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(items: &[&str], lock: Option<&str>) -> GoalGate {
        GoalGate {
            item_goals: items.iter().map(|s| s.to_string()).collect(),
            rune_goals: Vec::new(),
            runes_required: 0,
            goal_lock_item: lock.map(str::to_owned),
        }
    }

    fn holding<'a>(names: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
        move |n: &str| names.contains(&n)
    }

    fn flag_maps() -> (
        HashMap<String, u32>,
        HashMap<String, Vec<u32>>,
        HashMap<String, Vec<u32>>,
    ) {
        (
            HashMap::from([
                ("Ashen Capital Lock".to_string(), 70),
                ("Enir Ilim Lock".to_string(), 80),
            ]),
            HashMap::from([
                ("Ashen Capital Lock".to_string(), vec![71]),
                ("Enir Ilim Lock".to_string(), vec![81]),
            ]),
            HashMap::from([
                ("Ashen Capital Lock".to_string(), vec![72, 73]),
                ("Enir Ilim Lock".to_string(), vec![82]),
            ]),
        )
    }

    #[test]
    fn a_resolved_gate_writes_only_its_own_apparatus() {
        let (open, reveal, graces) = flag_maps();
        assert_eq!(
            flags_to_open(
                Some(70),
                Some("Ashen Capital Lock"),
                &open,
                &reveal,
                &graces,
            ),
            vec![70, 71, 72, 73]
        );
    }

    #[test]
    fn an_unresolved_withheld_gate_fails_open_across_the_advertised_seed() {
        let (open, reveal, graces) = flag_maps();
        assert_eq!(
            flags_to_open(None, None, &open, &reveal, &graces),
            vec![70, 71, 72, 73, 80, 81, 82]
        );
    }

    #[test]
    fn withholds_while_any_goal_item_is_outstanding() {
        let g = gate(
            &["Limgrave Lock", "Caelid Lock", "Ashen Capital Lock"],
            Some("Ashen Capital Lock"),
        );
        let d = decide(&g, &holding(&["Limgrave Lock"]));
        assert_eq!(
            d,
            Decision::Withhold {
                outstanding: vec!["Caelid Lock".into()]
            }
        );
        assert!(!d.opens());
    }

    #[test]
    fn opens_when_every_other_goal_item_is_held() {
        let g = gate(
            &["Limgrave Lock", "Caelid Lock", "Ashen Capital Lock"],
            Some("Ashen Capital Lock"),
        );
        assert_eq!(
            decide(&g, &holding(&["Limgrave Lock", "Caelid Lock"])),
            Decision::Open
        );
    }

    #[test]
    fn the_goal_regions_own_lock_is_never_required_of_itself() {
        // The whole point: it is not in the pool, so requiring it would never be satisfiable.
        let g = gate(&["Ashen Capital Lock"], Some("Ashen Capital Lock"));
        assert_eq!(decide(&g, &holding(&[])), Decision::Open);
    }

    #[test]
    fn natural_progression_is_the_rune_count_alone_with_no_branch() {
        // NP mints no lock items, so the wire carries runes only -- and the single rule reduces to
        // "hold the runes" without this module knowing the mode exists (world#768, Alaric).
        let mut g = gate(&[], Some("Ashen Capital Lock"));
        g.rune_goals = [
            "Godrick's Great Rune",
            "Rykard's Great Rune",
            "Radahn's Great Rune",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        g.runes_required = 2;
        assert!(!decide(&g, &holding(&["Godrick's Great Rune"])).opens());
        assert_eq!(
            decide(
                &g,
                &holding(&["Godrick's Great Rune", "Rykard's Great Rune"])
            ),
            Decision::Open
        );
    }

    #[test]
    fn an_unresolvable_gate_opens_rather_than_stranding_the_run() {
        // 🛑 The Lock is not in the pool. A gate that cannot resolve and refuses to open is an
        // unwinnable seed plus every foreign item placed inside the region (world#589).
        let g = gate(&["Limgrave Lock"], None);
        let d = decide(&g, &holding(&[]));
        assert!(matches!(d, Decision::OpenUnresolvable { .. }));
        assert!(d.opens(), "an unresolvable gate must OPEN, never strand");
    }

    #[test]
    fn an_empty_goal_opens() {
        // No requirements at all (region_locks goal on a seed with nothing kept, or a foreign
        // apworld's shape). Nothing to wait for.
        assert_eq!(
            decide(&gate(&[], Some("Ashen Capital Lock")), &holding(&[])),
            Decision::Open
        );
    }

    #[test]
    fn outstanding_is_sorted_and_deduped_for_a_stable_log_line() {
        let g = gate(
            &[
                "Caelid Lock",
                "Altus Lock",
                "Caelid Lock",
                "Ashen Capital Lock",
            ],
            Some("Ashen Capital Lock"),
        );
        match decide(&g, &holding(&[])) {
            Decision::Withhold { outstanding } => {
                assert_eq!(
                    outstanding,
                    vec!["Altus Lock".to_string(), "Caelid Lock".to_string()]
                )
            }
            other => panic!("expected Withhold, got {other:?}"),
        }
    }

    #[test]
    fn status_line_says_which_items_are_outstanding() {
        let g = gate(
            &["Caelid Lock", "Ashen Capital Lock"],
            Some("Ashen Capital Lock"),
        );
        let s = status_line(&decide(&g, &holding(&[])), "Ashen Capital");
        assert!(s.contains("Caelid Lock"), "{s}");
        assert!(s.contains("Ashen Capital"), "{s}");
        let open = status_line(&decide(&g, &holding(&["Caelid Lock"])), "Ashen Capital");
        assert!(open.contains("opening"), "{open}");
    }
}
