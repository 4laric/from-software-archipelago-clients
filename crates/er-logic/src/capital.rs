//! Pure capital-version reconciler decisions — SPEC-capital-reconciler.md (apworld repo root).
//!
//! Leyndell ships as TWO mutually exclusive map versions selected by ONE save-persisted event
//! flag, 9116 (sole vanilla setter: Maliketh's death, m13_00_00_00.emevd:409):
//!
//!   * 9116 OFF -> Leyndell, Royal Capital (m11_00, play_region bucket 11000): Morgott + ~152
//!     checks.
//!   * 9116 ON  -> Leyndell, Ashen Capital (m11_05, bucket 11050) + the Elden Throne (m19_00,
//!     bucket 19000): the finale.
//!
//! Vanilla only ever SETS the flag, so the swap is one-way: in region-lock play the Farum Azula
//! Lock lets the player kill Maliketh before clearing Royal, and the burn then strands the Royal
//! checks permanently (a grace warp cannot reach m11_00 while 9116 is set). Pure-runtime means
//! 9116 is ours to write; the client keeps it matched to where the player actually is (per-tick
//! latch) or is warping to (warp-target intercept). Both decisions are HERE, pure and
//! host-tested by `capital_replay`; the game glue (`region.rs::tick_capital` /
//! `capital_warp_intercept`, `shop_flags.rs::run_capital_release`) only feeds observations in
//! and applies the returned write.
//!
//! THE RULES (approved design, two blast-radius refinements):
//!   * 9116 default OFF (Royal is the default capital); ON only in — or warping to — the Ashen
//!     Capital / Elden Throne.
//!   * Arming gate: INERT until the burn-done latch (flag 118, `common.emevd` $Event(900)'s
//!     final step, monotonic) reads set — the first burn is 100% the game's own sequence, and
//!     writing 9116 between Maliketh's death and 118 would fight the in-flight burn.
//!   * Once armed, every known position outside Ashen/Throne holds the reversible capital state
//!     OFF. The burn-done gate prevents this from fighting m13's setter during the burn.
//!   * Reconcile-don't-dispatch: write only on readback mismatch, re-apply per tick until it
//!     sticks, no cursor ever advances.

use serde_json::Value;

/// The slot_data-fed partition of Leyndell's MEASURED play_region buckets (KICK id space,
/// 5-digit; the apworld's `features/capital.py::capital_partition` hard-fails generation on an
/// unclaimed bucket, so a parsed config is total over the capital).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapitalSets {
    /// Buckets where 9116 must be held ON (11050 Ashen Capital, 19000 Elden Throne).
    pub ashen: Vec<i32>,
    /// Buckets where 9116 must be held OFF (11000 Royal Capital).
    pub royal: Vec<i32>,
}

/// Parsed capital-reconciler slot_data (the five `capital*` contract keys travel together;
/// absent keys are the off-wire — `parse` returns `None` and the client stays INERT).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapitalConfig {
    /// The Leyndell map-version selector (9116). `capitalBurnFlag`.
    pub burn_flag: u32,
    /// The burn-complete arming latch (118, monotonic). `capitalBurnDoneFlag`.
    pub burn_done_flag: u32,
    /// The Royal/Ashen bucket partition. `capitalAshenPlayRegions` / `capitalRoyalPlayRegions`.
    pub sets: CapitalSets,
    /// `[ShopLineupParam row, expected release flag, replacement]` re-keys (Enia's Maliketh
    /// armor rows release on 9116 itself; re-keyed to 118 so the OFF-default cannot de-stock
    /// them). `capitalReleaseRows`.
    pub release_rows: Vec<(u32, u32, u32)>,
    /// The reversible vanilla world-burn flag (300), held with the map selector so leaving Ashen
    /// restores the unburnt world and entering it retains the finale arena. `capitalWorldBurnFlag`.
    ///
    /// 🛑 OPTIONAL, AND ITS ABSENCE CHANGES NOTHING. It was emitted in slot_data long before
    /// anything read it; a seed from an apworld that omits it keeps exactly the behaviour it
    /// shipped with; only 9116 is then reconciled.
    pub world_burn_flag: Option<u32>,
    /// The vanilla pre-burn world-state flag (302). When entering Ashen it must be cleared;
    /// elsewhere it is deliberately left alone. `capitalPreBurnFlag`.
    pub pre_burn_flag: Option<u32>,
}

/// Parse the capital keys out of slot_data. `None` = INERT (option off / old apworld / a
/// malformed emission — an empty bucket side would make the latch permissive exactly there, so
/// it is treated as absent, never guessed around).
pub fn parse(sd: &Value) -> Option<CapitalConfig> {
    let burn_flag = sd.get("capitalBurnFlag")?.as_u64()? as u32;
    let burn_done_flag = sd.get("capitalBurnDoneFlag")?.as_u64()? as u32;
    let ashen = int_list(sd.get("capitalAshenPlayRegions")?)?;
    let royal = int_list(sd.get("capitalRoyalPlayRegions")?)?;
    if burn_flag == 0 || burn_done_flag == 0 || ashen.is_empty() || royal.is_empty() {
        return None;
    }
    let release_rows = sd
        .get("capitalReleaseRows")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let r = r.as_array()?;
                    if r.len() < 3 {
                        return None;
                    }
                    Some((
                        r[0].as_u64()? as u32,
                        r[1].as_u64()? as u32,
                        r[2].as_u64()? as u32,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    // Optional, and NOT part of the `return None` validation above: a missing corroborator must
    // leave the reconciler configured and working, not switch it off.
    let world_burn_flag = sd
        .get("capitalWorldBurnFlag")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .filter(|&f| f != 0);
    let pre_burn_flag = sd
        .get("capitalPreBurnFlag")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .filter(|&f| f != 0);
    Some(CapitalConfig {
        burn_flag,
        burn_done_flag,
        sets: CapitalSets { ashen, royal },
        release_rows,
        world_burn_flag,
        pre_burn_flag,
    })
}

/// Current values of the three reversible capital-state flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CapitalState {
    pub burn: bool,
    pub world_burn: Option<bool>,
    pub pre_burn: Option<bool>,
}

/// Mismatched flag writes needed to reach one capital version.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CapitalWrites {
    pub burn: Option<bool>,
    pub world_burn: Option<bool>,
    pub pre_burn: Option<bool>,
}

/// Reconcile the complete capital state, not only map selector 9116.
///
/// World-burn follows the same desired state as 9116. Pre-burn is asymmetric: Ashen requires it
/// OFF, while Royal/outside leaves it untouched, matching the apworld contract.
pub fn reconcile_state(
    burn_done: bool,
    desired: Option<bool>,
    current: CapitalState,
) -> CapitalWrites {
    let Some(want) = desired.filter(|_| burn_done) else {
        return CapitalWrites::default();
    };
    CapitalWrites {
        burn: (current.burn != want).then_some(want),
        world_burn: current
            .world_burn
            .filter(|&value| value != want)
            .map(|_| want),
        pre_burn: (want && current.pre_burn == Some(true)).then_some(false),
    }
}

fn int_list(v: &Value) -> Option<Vec<i32>> {
    let a = v.as_array()?;
    a.iter().map(|x| x.as_i64().map(|n| n as i32)).collect()
}

/// 7-digit interior play_region ids (`bucket * 100 + sub`) reduce to their 5-digit bucket —
/// the SAME rule `region_lock::kick_decision` applies.
fn bucket_of_play_region(pr: i32) -> i32 {
    if pr >= 1_000_000 {
        pr / 100
    } else {
        pr
    }
}

/// Per-tick latch: what the reversible capital state must be while STANDING at `play_region`.
/// `Some(true)` = hold ON (Ashen/Throne bucket); every other known region holds OFF. An unreadable
/// position is represented by the caller not invoking this function, rather than a guessed id.
pub fn capital_flag_state(sets: &CapitalSets, play_region: i32) -> Option<bool> {
    let b = bucket_of_play_region(play_region);
    if sets.ashen.contains(&b) {
        Some(true)
    } else {
        Some(false)
    }
}

/// The capital bucket a warp target encodes, or `None` when the target is not an 8-digit
/// dungeon-grace-shaped id. Menu warps expose BONFIRE ENTITY ids (`BonfireWarpParam
/// .bonfireEntityId`); client warps call LuaWarp with that value minus 1000. Bucket rule
/// `id / 10_000 * 10`, verified against EVERY capital BonfireWarpParam row (2026-07-14):
/// Royal 11001950-11001959 -> 11000; Ashen 11051950-11051955 -> 11050; Throne 19001950 ->
/// 19000; Roundtable 11102950 -> 11100 (never a capital). 10-digit overworld tile ids are
/// never a capital.
pub fn warp_target_bucket(target: u32) -> Option<i32> {
    if !(10_000_000..100_000_000).contains(&target) {
        return None; // not an 8-digit dungeon grace (overworld tile / malformed)
    }
    Some((target / 10_000 * 10) as i32)
}

/// Warp-target intercept: what 9116 must be for the load that `target` is about to resolve.
/// The target's MAP VERSION is authoritative: any m11_05/m19 target selects Ashen, including an
/// Ashen duplicate whose displayed name is shared with a Royal grace. A shared name is not shared
/// state -- forcing Ashen Queen's Bedchamber to Royal makes the Godfrey approach replay the burn
/// transition and return to Ashen instead of entering the fight.
///
/// LuaWarp exposes two target-shaped id spaces in practice: menu travel passes the bonfire entity
/// id, while the client calls it with `entity - 1000`. The bucket rule deliberately accepts both;
/// subtracting 1000 does not cross any capital map's 10,000 boundary. Every non-Ashen resolvable
/// target restores the Royal default. `None` is reserved for zero, where there is no target to
/// classify.
pub fn capital_flag_state_for_warp_target(sets: &CapitalSets, target: u32) -> Option<bool> {
    if target == 0 {
        return None;
    }
    Some(warp_target_bucket(target).is_some_and(|bucket| sets.ashen.contains(&bucket)))
}

/// Reconcile-don't-dispatch: the ONE flag write (if any) this observation demands.
/// `None` = leave the flag alone. The arming gate keeps the first burn 100% vanilla; a desired
/// state equal to the current readback needs no write (write on readback mismatch ONLY — the
/// reconciler never toggles gratuitously). The caller re-applies every tick until the readback
/// matches; no latch, no cursor.
pub fn reconcile_write(burn_done: bool, desired: Option<bool>, current: bool) -> Option<bool> {
    // 🛑 ONE DECISION, NOT A DRIFTING TWIN (client#200). This is now a projection of
    // `capital_guard::decide`, which returns the same answer plus the REASON when it declines.
    // Three different declines -- not armed, unresolvable, already correct -- used to collapse into
    // one `None` here, and both call sites only log inside `if let Some(w)`; that is why 66 warps
    // in bobler's 2026-08-15 log produced no evidence at all. Callers that only need the value keep
    // this signature byte for byte.
    crate::capital_guard::decide(burn_done, desired, current).write()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sets() -> CapitalSets {
        CapitalSets {
            ashen: vec![11_050, 19_000],
            royal: vec![11_000],
        }
    }

    #[test]
    fn royal_buckets_hold_off() {
        let s = sets();
        assert_eq!(capital_flag_state(&s, 11_000), Some(false));
        // 7-digit interior play regions normalize by /100, the kick_decision rule.
        assert_eq!(capital_flag_state(&s, 1_100_010), Some(false));
    }

    #[test]
    fn ashen_and_throne_buckets_hold_on() {
        let s = sets();
        assert_eq!(capital_flag_state(&s, 11_050), Some(true));
        assert_eq!(capital_flag_state(&s, 19_000), Some(true));
        assert_eq!(capital_flag_state(&s, 1_105_001), Some(true));
        assert_eq!(capital_flag_state(&s, 1_900_002), Some(true));
    }

    #[test]
    fn anywhere_outside_ashen_holds_the_royal_state() {
        let s = sets();
        assert_eq!(
            capital_flag_state(&s, 11_100),
            Some(false),
            "Roundtable holds the Royal state"
        );
        assert_eq!(capital_flag_state(&s, 60_000), Some(false), "Limgrave");
        assert_eq!(
            capital_flag_state(&s, 6_100_000),
            Some(false),
            "7-digit non-capital"
        );
    }

    #[test]
    fn warping_to_a_royal_grace_writes_off() {
        let s = sets();
        // All 9 Royal m11_00 graces (rows 110000-110009).
        for g in 11_001_950..=11_001_959u32 {
            assert_eq!(
                capital_flag_state_for_warp_target(&s, g),
                Some(false),
                "grace {g}"
            );
        }
    }

    #[test]
    fn every_ashen_grace_selects_the_ashen_version() {
        let s = sets();
        for g in [
            11_051_950, 11_051_951, 11_051_952, 11_051_953, 11_051_954, 11_051_955, 19_001_950,
        ] {
            assert_eq!(
                capital_flag_state_for_warp_target(&s, g),
                Some(true),
                "grace {g}"
            );
        }
    }

    #[test]
    fn lua_warp_argument_and_entity_spaces_choose_the_same_version() {
        let s = sets();
        // Bobler's menu warp was logged as arg 11051954; the old hook added 1000 and handed the
        // classifier 11052954. The client-initiated form is 11050954. All three are m11_05.
        for g in [11_050_954, 11_051_954, 11_052_954] {
            assert_eq!(
                capital_flag_state_for_warp_target(&s, g),
                Some(true),
                "Ashen Queen's Bedchamber target {g} must remain Ashen"
            );
        }
        for g in [11_000_954, 11_001_954, 11_002_954] {
            assert_eq!(
                capital_flag_state_for_warp_target(&s, g),
                Some(false),
                "Royal Queen's Bedchamber target {g} must remain Royal"
            );
        }
    }

    #[test]
    fn any_other_warp_restores_the_royal_default() {
        let s = sets();
        assert_eq!(
            capital_flag_state_for_warp_target(&s, 11_102_950),
            Some(false),
            "Roundtable warp writes OFF -- every warp home restores Royal"
        );
        assert_eq!(
            capital_flag_state_for_warp_target(&s, 1_046_360_950),
            Some(false),
            "10-digit overworld tile grace: never a capital -> OFF"
        );
        assert_eq!(
            capital_flag_state_for_warp_target(&s, 0),
            None,
            "unresolvable target: leave the flag alone, never guess"
        );
    }

    #[test]
    fn reconcile_write_is_gated_and_mismatch_only() {
        // Pre-burn: never a write, whatever is desired.
        assert_eq!(reconcile_write(false, Some(true), false), None);
        assert_eq!(reconcile_write(false, Some(false), true), None);
        // Armed: write on mismatch only.
        assert_eq!(reconcile_write(true, Some(true), false), Some(true));
        assert_eq!(reconcile_write(true, Some(false), true), Some(false));
        assert_eq!(
            reconcile_write(true, Some(true), true),
            None,
            "readback match: no write"
        );
        assert_eq!(reconcile_write(true, Some(false), false), None);
        // No opinion (outside the capitals / unresolvable target): no write.
        assert_eq!(reconcile_write(true, None, true), None);
        assert_eq!(reconcile_write(true, None, false), None);
    }

    #[test]
    fn complete_state_follows_ashen_and_royal() {
        let burnt = CapitalState {
            burn: true,
            world_burn: Some(true),
            pre_burn: Some(false),
        };
        assert_eq!(
            reconcile_state(true, Some(false), burnt),
            CapitalWrites {
                burn: Some(false),
                world_burn: Some(false),
                pre_burn: None,
            },
            "leaving Ashen must clear both selectors; 9116 alone leaves the world burnt"
        );

        let royal = CapitalState {
            burn: false,
            world_burn: Some(false),
            pre_burn: Some(true),
        };
        assert_eq!(
            reconcile_state(true, Some(true), royal),
            CapitalWrites {
                burn: Some(true),
                world_burn: Some(true),
                pre_burn: Some(false),
            }
        );
    }

    #[test]
    fn goal_gate_world_burn_is_cleared_even_when_9116_is_already_off() {
        let after_goal_gate = CapitalState {
            burn: false,
            world_burn: Some(true),
            pre_burn: Some(false),
        };
        assert_eq!(
            reconcile_state(true, Some(false), after_goal_gate),
            CapitalWrites {
                burn: None,
                world_burn: Some(false),
                pre_burn: None,
            }
        );
    }

    #[test]
    fn pre_burn_is_untouched_outside_ashen_and_everything_is_armed() {
        let current = CapitalState {
            burn: false,
            world_burn: Some(false),
            pre_burn: Some(true),
        };
        assert_eq!(
            reconcile_state(true, Some(false), current),
            CapitalWrites::default()
        );
        assert_eq!(
            reconcile_state(false, Some(true), current),
            CapitalWrites::default()
        );
    }

    #[test]
    fn parse_requires_all_keys_together_and_nonempty_sides() {
        let full = json!({
            "capitalBurnFlag": 9116,
            "capitalBurnDoneFlag": 118,
            "capitalAshenPlayRegions": [11050, 19000],
            "capitalRoyalPlayRegions": [11000],
            "capitalReleaseRows": [[101516, 9116, 118], [101517, 9116, 118],
                                    [101518, 9116, 118], [101519, 9116, 118]],
            "capitalWorldBurnFlag": 300,
            "capitalPreBurnFlag": 302,
        });
        let c = parse(&full).expect("full emission parses");
        assert_eq!(c.burn_flag, 9116);
        assert_eq!(c.burn_done_flag, 118);
        assert_eq!(
            c.sets,
            super::CapitalSets {
                ashen: vec![11050, 19000],
                royal: vec![11000]
            }
        );
        assert_eq!(c.release_rows.len(), 4);
        assert_eq!(c.release_rows[0], (101516, 9116, 118));
        assert_eq!(c.world_burn_flag, Some(300));
        assert_eq!(c.pre_burn_flag, Some(302));

        // Absent keys are the off-wire (option off / old apworld): INERT, not an error.
        assert_eq!(parse(&json!({})), None);
        assert_eq!(parse(&json!({ "capitalBurnFlag": 9116 })), None);
        // An empty bucket side would make the latch permissive exactly there: treat as absent.
        let mut empty_side = full.clone();
        empty_side["capitalRoyalPlayRegions"] = json!([]);
        assert_eq!(parse(&empty_side), None);
    }
}
