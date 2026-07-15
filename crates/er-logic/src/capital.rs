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
//!   * The latch is SCOPED to the capital buckets: elsewhere -> `None` (leave the flag alone).
//!     Holding OFF globally would fight m13's setter during the burn; outside the capitals the
//!     next warp's intercept restores the Royal default instead.
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
    Some(CapitalConfig {
        burn_flag,
        burn_done_flag,
        sets: CapitalSets { ashen, royal },
        release_rows,
    })
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

/// Per-tick latch: what 9116 must be while STANDING at `play_region`.
/// `Some(true)` = hold ON (Ashen/Throne bucket), `Some(false)` = hold OFF (Royal bucket),
/// `None` = leave the flag alone (everywhere else — the scoped-latch refinement; the next
/// warp's intercept restores the Royal default instead).
pub fn capital_flag_state(sets: &CapitalSets, play_region: i32) -> Option<bool> {
    let b = bucket_of_play_region(play_region);
    if sets.ashen.contains(&b) {
        Some(true)
    } else if sets.royal.contains(&b) {
        Some(false)
    } else {
        None
    }
}

/// The capital bucket a warp target encodes, or `None` when the target is not an 8-digit
/// dungeon-grace entity id. Warp targets are BONFIRE ENTITY ids (`BonfireWarpParam
/// .bonfireEntityId`, the space `warp_to_grace` already speaks). Bucket rule
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
/// Ashen/Throne target -> `Some(true)`; ANY other resolvable target (including Royal m11_00,
/// Roundtable, every overworld grace) -> `Some(false)` — every warp anywhere except the 7
/// Ashen/Throne graces restores the Royal default. `None` only for an unresolvable target
/// (0 / not a grace entity id): leave the flag alone rather than guess.
pub fn capital_flag_state_for_warp_target(sets: &CapitalSets, target: u32) -> Option<bool> {
    if target == 0 {
        return None;
    }
    match warp_target_bucket(target) {
        Some(b) if sets.ashen.contains(&b) => Some(true),
        // Royal target, non-capital dungeon grace, or a 10-digit overworld tile id: all
        // resolvable, none Ashen -> OFF.
        _ => Some(false),
    }
}

/// Reconcile-don't-dispatch: the ONE flag write (if any) this observation demands.
/// `None` = leave the flag alone. The arming gate keeps the first burn 100% vanilla; a desired
/// state equal to the current readback needs no write (write on readback mismatch ONLY — the
/// reconciler never toggles gratuitously). The caller re-applies every tick until the readback
/// matches; no latch, no cursor.
pub fn reconcile_write(burn_done: bool, desired: Option<bool>, current: bool) -> Option<bool> {
    if !burn_done {
        return None; // pre-burn / mid-burn: INERT by design (the arming gate)
    }
    match desired {
        Some(want) if want != current => Some(want),
        _ => None,
    }
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
    fn elsewhere_the_latch_leaves_the_flag_alone() {
        let s = sets();
        assert_eq!(
            capital_flag_state(&s, 11_100),
            None,
            "Roundtable is never a capital"
        );
        assert_eq!(capital_flag_state(&s, 60_000), None, "Limgrave");
        assert_eq!(
            capital_flag_state(&s, 6_100_000),
            None,
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
    fn warping_to_ashen_or_throne_writes_on() {
        let s = sets();
        // All 6 Ashen m11_05 graces (rows 110500-110505) + the Elden Throne grace.
        for g in 11_051_950..=11_051_955u32 {
            assert_eq!(
                capital_flag_state_for_warp_target(&s, g),
                Some(true),
                "grace {g}"
            );
        }
        assert_eq!(
            capital_flag_state_for_warp_target(&s, 19_001_950),
            Some(true)
        );
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
    fn parse_requires_all_keys_together_and_nonempty_sides() {
        let full = json!({
            "capitalBurnFlag": 9116,
            "capitalBurnDoneFlag": 118,
            "capitalAshenPlayRegions": [11050, 19000],
            "capitalRoyalPlayRegions": [11000],
            "capitalReleaseRows": [[101516, 9116, 118], [101517, 9116, 118],
                                    [101518, 9116, 118], [101519, 9116, 118]],
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

        // Absent keys are the off-wire (option off / old apworld): INERT, not an error.
        assert_eq!(parse(&json!({})), None);
        assert_eq!(parse(&json!({ "capitalBurnFlag": 9116 })), None);
        // An empty bucket side would make the latch permissive exactly there: treat as absent.
        let mut empty_side = full.clone();
        empty_side["capitalRoyalPlayRegions"] = json!([]);
        assert_eq!(parse(&empty_side), None);
    }
}
