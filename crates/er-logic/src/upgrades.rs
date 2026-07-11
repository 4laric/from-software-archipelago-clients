//! auto_upgrade + scadu side-effect logic, extracted from `upgrades.rs`. Game reads/writes go
//! through the `GameHook` seam; the id math is pure (decode is faithful to `upgrades.rs`:
//! REINFORCE_STEP = 100, base = row - row%100).

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
        assert_eq!(classify_track(25, SOMBER_MATERIAL_SET), Some((SOMBER_CAP, true)));
    }

    #[test]
    fn classify_regular_material_with_short_run_is_normal() {
        // Reverse mismatch rows: materialSetId 0 with an 11-row run -> NORMAL (cap = run), not somber.
        assert_eq!(classify_track(10, 0), Some((10, false)));
    }

    #[test]
    fn classify_standard_normal_and_somber() {
        assert_eq!(classify_track(25, 0), Some((25, false)));                    // vanilla normal weapon
        assert_eq!(classify_track(10, SOMBER_MATERIAL_SET), Some((10, true)));   // vanilla somber weapon
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
}
