//! auto_upgrade + scadu side-effect logic, extracted from `upgrades.rs`. Game reads/writes go
//! through the `GameHook` seam; the id math is pure (decode is faithful to `upgrades.rs`:
//! REINFORCE_STEP = 100, base = row - row%100).

use std::collections::HashMap;

use crate::hook::GameHook;

/// ER id stride per smithing level (base = id - id%100).
const REINFORCE_STEP: i32 = 100;
/// Stored scadutree-blessing ceiling.
pub const SCADU_MAX_LEVEL: i32 = 20;
/// `EquipParamWeapon.materialSetId` of the somber-stone smithing track.
pub const SOMBER_MATERIAL_SET: i32 = 2200;
/// Somber track ceiling (its `EquipMtrlSetParam` chain only builds to +10).
pub const SOMBER_CAP: i32 = 10;

/// Classify a weapon's smithing TRACK + cap from its reinforce-run length and material set.
///
/// `run_cap` = the highest +N the weapon's `ReinforceParamWeapon` run supports (`<= 0` => not
/// upgradeable). `material_set_id` = `EquipParamWeapon.materialSetId` (`2200` = somber stones,
/// anything else = regular smithing). Returns `(cap, somber)`, or `None` when not upgradeable.
///
/// The track is decided by the MATERIAL the game charges, never the run length. The old
/// `somber = run_cap <= 10` heuristic disagreed with the game on the handful of vanilla rows whose
/// somber material rides a full-length (26-row) reinforce run -- notably the Occult Carian Knight's
/// Shield (`materialSetId 2200`, run 26). It was read as a +25 NORMAL weapon, so a +10 of it leaked
/// into the normal high-water mark and cross-upgraded received standard weapons to +10. The somber
/// cap is clamped to +10 so a mislengthed run can never push a somber weapon past its real ceiling.
pub fn classify_track(run_cap: i32, material_set_id: i32) -> Option<(i32, bool)> {
    if run_cap <= 0 {
        return None; // no reinforce rows -> not player-upgradeable
    }
    if material_set_id == SOMBER_MATERIAL_SET {
        Some((run_cap.min(SOMBER_CAP), true))
    } else {
        Some((run_cap, false)) // regular smithing (materialSetId 0), or clamp-safe default
    }
}

/// Decode a weapon FullID into `(base, reinforce_level)`. None for non-weapons or out-of-range rows.
pub fn decode_weapon_id(full_id: i32) -> Option<(i32, i32)> {
    if er_codec::item_category_of(full_id as u32) != er_codec::CATEGORY_WEAPON {
        return None;
    }
    let row = (full_id as u32 & er_codec::ROW_ID_MASK) as i32;
    if !(1_000_000..90_000_000).contains(&row) {
        return None;
    }
    let base = row - (row % REINFORCE_STEP);
    let level = row % REINFORCE_STEP;
    Some((base, level))
}

/// Bump a freshly granted weapon to the player's highest held reinforce level on its track
/// (raise-only, capped). Identity when off, off-world, non-weapon, unresolvable, or already
/// at/above target.
pub fn apply_auto_upgrade(hook: &dyn GameHook, on: bool, full_id: i32) -> i32 {
    if !on || !hook.in_world() {
        return full_id;
    }
    let Some((base, level)) = decode_weapon_id(full_id) else {
        return full_id;
    };
    let Some((cap, somber)) = hook.weapon_track_and_cap(base) else {
        return full_id;
    };
    let Some(target_raw) = hook.highest_held_level(somber) else {
        return full_id;
    };
    let target = target_raw.min(cap);
    if target <= level {
        return full_id; // already at/above target
    }
    let up = base + target;
    (full_id & !(er_codec::ROW_ID_MASK as i32)) | (up & er_codec::ROW_ID_MASK as i32)
}

/// Cumulative Scadutree Fragments required for each blessing level 0..=20 (vanilla curve).
pub const SCADU_CUM: [i32; 21] = [
    0, 1, 3, 5, 7, 9, 11, 13, 15, 17, 20, 23, 26, 29, 32, 35, 38, 41, 44, 47, 50,
];

/// Held Scadutree Fragments -> blessing level (0..=20). Highest L with `frags >= SCADU_CUM[L]`.
/// Pure. Was a private copy in the client (`upgrades.rs::level_for_fragments`) with no test.
pub fn level_for_fragments(frag_qty: i32) -> i32 {
    let mut level = 0;
    for l in (0..=SCADU_MAX_LEVEL).rev() {
        if frag_qty >= SCADU_CUM[l as usize] {
            level = l;
            break;
        }
    }
    level
}

/// Scadutree Fragment goods row (no category nibble). A received AP item counts toward the blessing
/// iff `apIdsToItemIds` maps it to this row.
pub const SCADU_FRAGMENT_GOODS: i64 = 2_010_000;

/// Fragments delivered by the MULTIWORLD, from the received stream — the input to the blessing curve.
///
/// 🛑 WHY NOT THE BAG. This used to walk the player's inventory and sum held Scadutree Fragments,
/// carried over from the C++ client. That is wrong for a reason that only shows up in a DLC seed:
/// **revering at a grace CONSUMES the fragments.** Held count drops to 0, the derived level collapses
/// to 0, and the game-wide blessing silently switches off for a player who did nothing but play
/// normally. AP replays the whole received set on every connect, so a received count is stable across
/// reconnect and save-load, immune to consumption, needs no ledger, and costs no per-tick bag walk.
/// This is the same idiom `flask_reconcile::desired` already uses over "Progressive Flask Upgrade".
///
/// 🛑 MATCH ON THE ITEM ID, NOT THE NAME. `apIdsToItemIds` is the same map the grant path resolves
/// through, so this counts whatever the seed calls its fragments — including a foreign apworld
/// (Bedrock/fswap) whose item name differs from ours. A name match would fail silently on exactly
/// the seeds we cannot test.
///
/// Honours `itemCounts` (an AP item may grant a stack), defaulting to 1 — the same
/// `unwrap_or(1).max(1)` the grant path uses, so the blessing can never disagree with what was
/// actually handed over.
///
/// NOT COUNTED, and correctly so: fragments from `startItems` or a `!give` console grant never enter
/// the received stream. Neither is a normal way to obtain one, and counting out-of-band fragments
/// would be a route to smuggling blessing levels between saves.
pub fn fragments_from_received(
    received_ap_item_ids: impl IntoIterator<Item = i64>,
    item_map: &HashMap<i64, i64>,
    item_counts: &HashMap<i64, i64>,
) -> i32 {
    let mut total: i64 = 0;
    for ap_id in received_ap_item_ids {
        total += fragment_units_for(ap_id, item_map, item_counts) as i64;
    }
    total.clamp(0, i32::MAX as i64) as i32
}

/// How many Scadutree Fragments ONE received AP item is worth. 0 for everything that isn't one.
///
/// Split out from [`fragments_from_received`] so the client can accumulate inside the single pass it
/// already makes over the received stream (`core.rs`, the same loop that counts
/// "Progressive Flask Upgrade") instead of materialising a second list of every item id ever
/// received. The aggregate form is what the tests drive; this is what ships.
pub fn fragment_units_for(
    ap_item_id: i64,
    item_map: &HashMap<i64, i64>,
    item_counts: &HashMap<i64, i64>,
) -> i32 {
    let Some(&full_id) = item_map.get(&ap_item_id) else {
        return 0; // unmapped (region locks, boss keys) — never a fragment
    };
    // The category nibble lives in the high bits; the goods ROW is the low 28. This is the client's
    // own `param_id()` split (`cs/item_id.rs:56`, bits 27..0), restated rather than imported because
    // er-logic must stay free of the Windows-only crate.
    if (full_id & 0x0FFF_FFFF) != SCADU_FRAGMENT_GOODS {
        return 0;
    }
    // Same `unwrap_or(1).max(1)` the grant path uses, so the blessing can never disagree with the
    // quantity actually handed over.
    item_counts
        .get(&ap_item_id)
        .copied()
        .unwrap_or(1)
        .max(1)
        .clamp(0, i32::MAX as i64) as i32
}

/// THE blessing decision, as one pure function.
///
/// `mode`: 0 = off (never write), 1 = player_only (level from held fragments), 2 = scaled (ALSO floor
/// to the DLC area's expected blessing, so a DLC region you unlock with no fragments still meets its
/// enemies' assumption). `floor` is the per-region floor for the player's CURRENT play_region -- 0
/// outside a DLC bucket, so mode 2 is naturally inert in the base game.
///
/// Fragments and floor compose as MAX: the floor lifts you to the area's expectation, and collected
/// fragments still count above it. The caller applies the result raise-only (`raise_stored_blessing`),
/// so a real, higher DLC blessing is never stomped.
///
/// `None` = do not write at all (mode off). This is the decision that shipped ON for every DLC seed on
/// 2026-07-11 (the option had been frozen OFF, which meant the floor wire was never even emitted and
/// the client's floor path was dead code) -- it had no test at all until this one.
pub fn blessing_target(mode: i32, frag_qty: i32, floor: i32) -> Option<i32> {
    if mode != 1 && mode != 2 {
        return None;
    }
    let from_frags = level_for_fragments(frag_qty);
    let target = if mode == 2 {
        from_frags.max(floor)
    } else {
        from_frags
    };
    Some(target.clamp(0, SCADU_MAX_LEVEL))
}

/// Must the blessing applier skip this tick because the player is dead or dying?
///
/// WHY IT IS A FUNCTION. `scadu_blessing::drive` walks `chr_ins.special_effect` to find the vanilla
/// rung the engine has live. That list TEARS DOWN at the death-cam transition and iterating it
/// there CTDs -- `no_equip_load.rs:78-83` is the shipped instance. The guard existed as a bare
/// inline `if` with no test, which is precisely the case SPEC §7 called out ("the death guard ...
/// must be called directly with synthetic input, not assumed covered") and which shipped uncovered
/// anyway.
///
/// 🛑 ONE RULE, ONE IMPLEMENTATION. This delegates to
/// [`crate::death_guard::lists_unsafe_to_touch`]; it is a NAME at the blessing's call site, not a
/// second copy. I first wrote this as its own `hp <= 0` and documented "four sites" -- the real
/// count was FIVE (`no_equip_load`, `no_fall_damage`, `scaling`, this, plus DeathLink's two
/// unrelated uses), which is how a miscount in a comment becomes folklore.
///
/// DeathLink's `hp <= 0` tests are deliberately NOT unified -- see `death_guard`'s module docs.
pub fn blessing_blocked_by_death(player_hp: i32) -> bool {
    crate::death_guard::lists_unsafe_to_touch(player_hp)
}

/// The rates to write this tick, or `None` if the applier must not write at all.
///
/// Folds the two refusals that were unreachable from any seed corpus into one decision with a
/// production caller: the clone row missing from `SpEffectParam` (`row_present == false`), and
/// [`clone_rates`] returning its no-op pair. A `(1.0, 1.0)` write is not harmless -- it is a
/// pointless param mutation every tick and it makes "we wrote nothing" indistinguishable from "we
/// wrote a neutral value" in any later readback.
pub fn blessing_write_or_skip(
    a_target: f32,
    a_active: f32,
    row_present: bool,
) -> Option<(f32, f32)> {
    if !row_present {
        return None;
    }
    let (attack, cut) = clone_rates(a_target, a_active);
    if attack == 1.0 && cut == 1.0 {
        return None;
    }
    Some((attack, cut))
}

/// Blessing-clone rate pair: `(attack, cut)` to write into our repurposed `SpEffectParam` row.
///
/// WHY A RATIO AND NOT `A(t)` DIRECTLY. The vanilla Scadutree ladder is ONE scalar: every level row
/// `20000100 + n` sets all five `atk*DmgCorrectRate` channels to `A(n)` and all eight
/// `*DamageCutRate` channels to `1/A(n)`, exactly (RECON-scadutree-blessing-speffect-20260729 §2).
/// Inside the Land of Shadow the engine is refreshing a REAL rung `k` every tick, so if our clone
/// also carried the full `A(t)` the two would multiply and the player would double-dip. Carrying
/// `A(t)/A(k)` makes the product exactly `A(t)` wherever the player stands, and `k = 0` (no vanilla
/// rung active -- i.e. the whole base game) gives `A(t)/1.0 = A(t)`. So ONE formula covers base game
/// and DLC and the spec's double-dip rule (§3.4) reduces to this function.
///
/// Both inputs are READ FROM THE PARAM ROWS at runtime (`base + t`, `base + k`), never from a table
/// we carry: the curve then self-updates across game patches and we never ship FromSoft's numbers
/// (`er-foreign-list-provenance-rule`).
///
/// Returns `(1.0, 1.0)` -- a true no-op -- for every input that isn't a sane raise:
///
/// * `a_active <= 0.0` or non-finite: the caller's read of the active rung failed or the row was
///   garbage. A ratio against 0 is `inf`; refusing is the only safe answer.
/// * `a_target` non-finite, or `<= 0.0`.
/// * `a_active >= a_target`: the player's own blessing already meets or beats our target. The clone
///   must NEVER carry a value below 1.0 -- that would be a DEBUFF, silently taking away a blessing
///   the player earned. Compose as `max`, never as a sum and never as a subtraction.
///
/// 🛑 These branches are unreachable from any seed corpus, so they are called directly in the tests
/// below rather than assumed covered (`guard-absent-from-corpus-needs-a-direct-call`).
pub fn clone_rates(a_target: f32, a_active: f32) -> (f32, f32) {
    const NOOP: (f32, f32) = (1.0, 1.0);
    if !a_target.is_finite() || !a_active.is_finite() {
        return NOOP;
    }
    if a_target <= 0.0 || a_active <= 0.0 {
        return NOOP;
    }
    if a_active >= a_target {
        // Player's real blessing already >= our target. Raise-only: never write a cut > 1 / attack < 1.
        return NOOP;
    }
    let attack = a_target / a_active;
    if !attack.is_finite() || attack < 1.0 {
        return NOOP;
    }
    (attack, 1.0 / attack)
}

/// Clamp a blessing target to the seed's cap (`scaduBlessingCap` from slot_data).
///
/// Separate from [`blessing_target`] on purpose: the cap is a SEED fact that arrives over the wire,
/// while `blessing_target` is the per-tick decision. A cap <= 0 or absent means "no extra cap" and
/// falls back to [`SCADU_MAX_LEVEL`] -- an absent key must never silently pin the blessing to 0
/// (`er-unfreezing-an-option-needs-the-class-default`: an unset value that reads as the floor is how
/// a feature ships inert).
pub fn apply_blessing_cap(target: i32, cap: i32) -> i32 {
    let cap = if cap <= 0 { SCADU_MAX_LEVEL } else { cap };
    target.clamp(0, cap.min(SCADU_MAX_LEVEL))
}

/// Raise the stored scadutree blessing to `level` (clamped to `[0, SCADU_MAX_LEVEL]`); never lowers.
///   None => PlayerGameData unreachable; Some(None) => already >= target; Some(Some((was, now))) => raised.
pub fn raise_stored_blessing(hook: &mut dyn GameHook, level: i32) -> Option<Option<(i32, i32)>> {
    let target = level.clamp(0, SCADU_MAX_LEVEL);
    let cur = hook.scadutree_blessing()?;
    if cur >= target {
        return Some(None);
    }
    hook.set_scadutree_blessing(target);
    Some(Some((cur, target)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::fake::FakeGame;

    fn weapon_hook(somber: bool, held: i32, cap: i32) -> FakeGame {
        let mut g = FakeGame::new();
        g.set_in_world(true);
        g.set_track_cap(1_000_000, Some((cap, somber)));
        g.set_held_level(somber, Some(held));
        g
    }

    #[test]
    fn target_is_highest_owned_level_on_track() {
        let g = weapon_hook(false, 12, 25);
        assert_eq!(apply_auto_upgrade(&g, true, 1_000_000), 1_000_012);
    }

    #[test]
    fn raise_only_never_lowers() {
        let g = weapon_hook(false, 12, 25);
        assert_eq!(apply_auto_upgrade(&g, true, 1_000_015), 1_000_015);
    }

    #[test]
    fn target_clamped_to_weapon_cap() {
        let g = weapon_hook(true, 20, 10); // somber cap 10, held +20
        assert_eq!(apply_auto_upgrade(&g, true, 1_000_000), 1_000_010);
    }

    #[test]
    fn off_is_identity() {
        let g = weapon_hook(false, 12, 25);
        assert_eq!(apply_auto_upgrade(&g, false, 1_000_000), 1_000_000);
    }

    #[test]
    fn off_world_is_identity() {
        let mut g = weapon_hook(false, 12, 25);
        g.set_in_world(false);
        assert_eq!(apply_auto_upgrade(&g, true, 1_000_000), 1_000_000);
    }

    #[test]
    fn non_weapon_passes_through() {
        let g = weapon_hook(false, 12, 25);
        let goods = (er_codec::CATEGORY_GOODS | 2_010_000) as i32;
        assert_eq!(apply_auto_upgrade(&g, true, goods), goods);
    }

    #[test]
    fn unresolvable_track_or_bag_is_identity() {
        let mut g = FakeGame::new();
        g.set_in_world(true);
        g.set_track_cap(1_000_000, None);
        assert_eq!(apply_auto_upgrade(&g, true, 1_000_000), 1_000_000);

        let mut g2 = FakeGame::new();
        g2.set_in_world(true);
        g2.set_track_cap(1_000_000, Some((25, false)));
        g2.set_held_level(false, None);
        assert_eq!(apply_auto_upgrade(&g2, true, 1_000_000), 1_000_000);
    }

    #[test]
    fn scadu_raises_when_target_higher() {
        let mut g = FakeGame::new();
        g.set_stored_blessing(Some(3));
        assert_eq!(raise_stored_blessing(&mut g, 7), Some(Some((3, 7))));
        assert_eq!(g.last_scadu_write(), Some(7));
    }

    #[test]
    fn scadu_raise_only_leaves_higher_untouched() {
        let mut g = FakeGame::new();
        g.set_stored_blessing(Some(15));
        assert_eq!(raise_stored_blessing(&mut g, 10), Some(None));
        assert_eq!(g.last_scadu_write(), None);
    }

    #[test]
    fn scadu_clamps_to_max() {
        let mut g = FakeGame::new();
        g.set_stored_blessing(Some(0));
        assert_eq!(raise_stored_blessing(&mut g, 99), Some(Some((0, 20))));
    }

    #[test]
    fn scadu_clamps_negative_to_zero_no_write() {
        let mut g = FakeGame::new();
        g.set_stored_blessing(Some(0));
        assert_eq!(raise_stored_blessing(&mut g, -5), Some(None));
    }

    #[test]
    fn scadu_unreachable_returns_none() {
        let mut g = FakeGame::new();
        g.set_stored_blessing(None);
        assert_eq!(raise_stored_blessing(&mut g, 10), None);
    }

    // ---- classify_track: TRACK from materialSetId, cap from the run (the cross-track bug fix) -----
    #[test]
    fn classify_somber_material_with_full_run_is_somber_capped() {
        // The bug row: Occult Carian Knight's Shield -- somber material (2200) but a 26-row reinforce
        // run. Must be SOMBER and clamped to +10, NOT a +25 normal weapon (that leak cross-upgraded
        // received standard weapons to +10).
        assert_eq!(
            classify_track(25, SOMBER_MATERIAL_SET),
            Some((SOMBER_CAP, true))
        );
    }

    #[test]
    fn classify_regular_material_with_short_run_is_normal() {
        // Reverse mismatch rows: materialSetId 0 with an 11-row run -> NORMAL (cap = run), not somber.
        assert_eq!(classify_track(10, 0), Some((10, false)));
    }

    #[test]
    fn classify_standard_normal_and_somber() {
        assert_eq!(classify_track(25, 0), Some((25, false))); // vanilla normal weapon
        assert_eq!(classify_track(10, SOMBER_MATERIAL_SET), Some((10, true))); // vanilla somber weapon
    }

    #[test]
    fn classify_not_upgradeable_is_none() {
        assert_eq!(classify_track(0, 0), None);
        assert_eq!(classify_track(-1, SOMBER_MATERIAL_SET), None);
    }

    #[test]
    fn classify_unknown_material_defaults_normal_never_panics() {
        // materialSetId -1/other with a real run -> treat as normal (never somber-leak, never crash).
        assert_eq!(classify_track(25, -1), Some((25, false)));
    }

    // ============================================================================================
    // fragments_from_received -- the blessing's INPUT. Held-count was the original bug.
    // ============================================================================================

    fn maps(pairs: &[(i64, i64)], counts: &[(i64, i64)]) -> (HashMap<i64, i64>, HashMap<i64, i64>) {
        (
            pairs.iter().copied().collect(),
            counts.iter().copied().collect(),
        )
    }

    /// Goods FullID = category nibble in the high bits | row. 0x40000000 is the goods category the
    /// seed's apIdsToItemIds actually carries, so the match must survive it.
    const GOODS: i64 = 0x4000_0000;

    #[test]
    fn counts_only_items_that_map_to_the_fragment_row() {
        let (m, c) = maps(
            &[
                (100, GOODS | SCADU_FRAGMENT_GOODS),
                (101, GOODS | SCADU_FRAGMENT_GOODS),
                (102, GOODS | 2_010_001), // the NEXT goods row — must not count
            ],
            &[],
        );
        assert_eq!(fragments_from_received([100, 101, 102], &m, &c), 2);
    }

    #[test]
    fn an_unmapped_item_is_not_a_fragment() {
        // Region locks and boss keys are deliberately absent from apIdsToItemIds. A lookup miss must
        // be 0, never a panic and never a default-counted item.
        let (m, c) = maps(&[(100, GOODS | SCADU_FRAGMENT_GOODS)], &[]);
        assert_eq!(fragments_from_received([999, 100, 999], &m, &c), 1);
        assert_eq!(fragments_from_received([], &m, &c), 0);
    }

    #[test]
    fn item_counts_are_honoured_so_a_stack_is_not_one_fragment() {
        // 🛑 The scale trap. If a seed ever stacks fragments, counting ITEMS instead of UNITS would
        // undercount and silently cap the curve below what the player was given.
        let (m, c) = maps(&[(100, GOODS | SCADU_FRAGMENT_GOODS)], &[(100, 5)]);
        assert_eq!(fragments_from_received([100, 100], &m, &c), 10);
    }

    #[test]
    fn a_zero_or_negative_item_count_still_grants_one() {
        // Mirrors the grant path's `unwrap_or(1).max(1)`: a malformed itemCounts entry must not make
        // a delivered fragment worth nothing. Called directly — no seed produces this.
        let (m, c) = maps(&[(100, GOODS | SCADU_FRAGMENT_GOODS)], &[(100, 0)]);
        assert_eq!(fragments_from_received([100], &m, &c), 1);
        let (m, c) = maps(&[(100, GOODS | SCADU_FRAGMENT_GOODS)], &[(100, -7)]);
        assert_eq!(fragments_from_received([100], &m, &c), 1);
    }

    #[test]
    fn the_received_count_never_falls_when_the_game_consumes_fragments() {
        // THE MOTIVATING CASE (rule 11). Revering at a DLC grace CONSUMES fragments, which is what
        // made the old held-count reading collapse to level 0 mid-run. The received stream is a
        // ledger of what was DELIVERED, so the same input replays identically afterwards.
        let (m, c) = maps(&[(100, GOODS | SCADU_FRAGMENT_GOODS)], &[]);
        let delivered: Vec<i64> = std::iter::repeat_n(100, 26).collect();
        let before = fragments_from_received(delivered.clone(), &m, &c);
        // ... player reveres; the bag is now empty. AP replays the identical stream on reconnect.
        let after = fragments_from_received(delivered, &m, &c);
        assert_eq!(before, 26);
        assert_eq!(
            after, before,
            "a consumed fragment must not lower the blessing"
        );
        assert_eq!(
            level_for_fragments(after),
            12,
            "26 fragments is exactly the cap-12 rung"
        );
    }

    #[test]
    fn the_shipped_pool_can_actually_reach_the_cap() {
        // 🛑 REACHABILITY, pinned. data.py carries 46 `Scadutree Fragment` locations, so at most 46
        // items can ever be received. Under the vanilla curve that is level 18 -- so a cap of 20
        // would be UNREACHABLE and the option would promise a rung no seed can deliver. Cap 12 needs
        // 26 and clears it with room. If someone raises SCADU_BLESSING_CAP, this is the check that
        // should stop them at 19.
        const SHIPPED_FRAGMENT_LOCATIONS: i32 = 46;
        assert_eq!(level_for_fragments(SHIPPED_FRAGMENT_LOCATIONS), 18);
        assert!(
            SCADU_CUM[12] <= SHIPPED_FRAGMENT_LOCATIONS,
            "cap 12 must be reachable from the shipped pool"
        );
        assert!(
            SCADU_CUM[20] > SHIPPED_FRAGMENT_LOCATIONS,
            "if this ever passes, the pool grew and cap 20 became reachable — revisit the cap"
        );
    }

    // ============================================================================================
    // clone_rates -- Lever D's arithmetic. Every branch here is called DIRECTLY: none of them is
    // reachable from a seed corpus, and a guard the corpus never triggers is untested.
    // ============================================================================================

    /// The base game: nothing active, so the clone carries the full curve.
    #[test]
    fn clone_rates_no_active_rung_is_the_full_curve() {
        // A(0) = 1.0 (the identity row). A(20) = 2.05.
        let (atk, cut) = clone_rates(2.05, 1.0);
        assert!((atk - 2.05).abs() < 1e-6, "attack {atk}");
        // The ladder's own identity: cut == 1/attack, exactly.
        assert!((cut - 1.0 / 2.05).abs() < 1e-6, "cut {cut}");
    }

    /// In the DLC with a real rung live: the clone supplies only the DIFFERENCE, so the product the
    /// player actually experiences is A(target) and not A(target)*A(active).
    #[test]
    fn clone_rates_composes_to_exactly_the_target() {
        let a_target = 2.05; // level 20
        let a_active = 1.425; // level ~5, a real revere
        let (atk, cut) = clone_rates(a_target, a_active);
        // What the damage pipeline sees = vanilla rung * our clone.
        assert!(
            (a_active * atk - a_target).abs() < 1e-5,
            "product {}",
            a_active * atk
        );
        // And the defensive half composes the same way, in reverse.
        assert!(((1.0 / a_active) * cut - 1.0 / a_target).abs() < 1e-5);
    }

    /// 🛑 The one that would silently hurt the player: their real blessing already beats our target.
    /// A naive ratio gives attack < 1 = a DEBUFF. Must be a no-op instead.
    #[test]
    fn clone_rates_never_debuffs_when_active_exceeds_target() {
        assert_eq!(clone_rates(1.425, 2.05), (1.0, 1.0));
        // Exactly equal is also a no-op, not a 1.0 computed through a division.
        assert_eq!(clone_rates(2.05, 2.05), (1.0, 1.0));
    }

    /// A failed read of the active rung yields 0.0; dividing by it is `inf`.
    #[test]
    fn clone_rates_zero_active_is_a_noop_not_infinity() {
        assert_eq!(clone_rates(2.05, 0.0), (1.0, 1.0));
        assert_eq!(clone_rates(2.05, -1.0), (1.0, 1.0));
        assert_eq!(clone_rates(0.0, 1.0), (1.0, 1.0));
    }

    #[test]
    fn clone_rates_non_finite_is_a_noop() {
        assert_eq!(clone_rates(f32::NAN, 1.0), (1.0, 1.0));
        assert_eq!(clone_rates(2.05, f32::NAN), (1.0, 1.0));
        assert_eq!(clone_rates(f32::INFINITY, 1.0), (1.0, 1.0));
        assert_eq!(clone_rates(2.05, f32::INFINITY), (1.0, 1.0));
    }

    /// The pair is always reciprocal -- that property is what makes the composition exact, so it is
    /// asserted across the whole ladder rather than at one point.
    #[test]
    fn clone_rates_pair_is_always_reciprocal() {
        for step in 1..=40 {
            let a_target = 1.0 + step as f32 * 0.05;
            for astep in 0..=step {
                let a_active = 1.0 + astep as f32 * 0.05;
                let (atk, cut) = clone_rates(a_target, a_active);
                assert!(atk >= 1.0, "clone must never debuff: {atk}");
                assert!(
                    (atk * cut - 1.0).abs() < 1e-5,
                    "not reciprocal: {atk} {cut}"
                );
            }
        }
    }

    // ============================================================================================

    // ---- SPEC §7 bullet 5: guards no corpus reaches, called DIRECTLY -------------------------

    #[test]
    fn the_death_guard_blocks_exactly_at_and_below_zero_hp() {
        assert!(
            blessing_blocked_by_death(0),
            "hp 0 is the death-cam edge: the list is tearing down"
        );
        assert!(
            blessing_blocked_by_death(-1),
            "negative hp is observed before the respawn edge"
        );
        assert!(!blessing_blocked_by_death(1));
        assert!(!blessing_blocked_by_death(i32::MAX));
    }

    #[test]
    fn a_missing_clone_row_refuses_to_write() {
        // The row is absent from SpEffectParam (a modded regulation, a future patch renumbering).
        assert_eq!(blessing_write_or_skip(2.0, 1.0, false), None);
    }

    #[test]
    fn a_noop_rate_pair_is_not_written() {
        // a_active >= a_target -> clone_rates returns (1.0, 1.0). Writing that every tick is a
        // pointless mutation AND erases the difference between "skipped" and "wrote neutral".
        assert_eq!(blessing_write_or_skip(1.5, 1.5, true), None);
        assert_eq!(blessing_write_or_skip(1.0, 2.0, true), None);
    }

    #[test]
    fn a_real_raise_is_written_through() {
        let got = blessing_write_or_skip(2.0, 1.0, true).expect("a genuine raise must write");
        assert!(
            (got.0 - 2.0).abs() < 1e-6,
            "attack {} should be A(t)/A(k) = 2.0",
            got.0
        );
        assert!(
            (got.1 - 0.5).abs() < 1e-6,
            "cut {} should be its reciprocal",
            got.1
        );
    }

    // apply_blessing_cap
    // ============================================================================================

    #[test]
    fn cap_absent_means_the_ladder_ceiling_not_zero() {
        // 🛑 The failure this pins: an unset/absent scaduBlessingCap reading as 0 would clamp every
        // blessing to 0 and ship the feature inert. Absent => SCADU_MAX_LEVEL.
        assert_eq!(apply_blessing_cap(20, 0), 20);
        assert_eq!(apply_blessing_cap(20, -1), 20);
    }

    #[test]
    fn cap_clamps_and_never_exceeds_the_ladder() {
        assert_eq!(apply_blessing_cap(20, 12), 12);
        assert_eq!(apply_blessing_cap(5, 12), 5);
        assert_eq!(apply_blessing_cap(99, 99), 20); // cap can't lift past the real ceiling
        assert_eq!(apply_blessing_cap(-3, 12), 0);
    }
}
