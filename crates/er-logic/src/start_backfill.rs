//! Inventory-verified start-item backfill (pure).
//!
//! `grants::drain_start_items` grants the start items once, gated by a persisted BOOLEAN
//! (`start_items_granted`). That boolean is set the first time a character connects, so a character
//! first played BEFORE an item was added to `startItems` never receives the new item -- the flag
//! says "already granted" even though the item was never in the bag. (Live 2026-07-18: a
//! Roundtable-hub character with the Flask of Crimson Tears in `startItems` but no healing flask;
//! the getItemFlag `60000` "obtained" was also set on the fresh save, so nothing noticed.)
//!
//! This is the backstop: given the held inventory, compute which `startItems` are NOT actually in
//! it -- verifying against the bag, not a boolean. Repetition in `startItems` encodes quantity (13
//! copies == grant 13x), and is preserved: an item present even once satisfies ALL its copies (we
//! don't top up a partly-used stack), an entirely-absent item yields all its copies back.
//!
//! Flask nuance: the Flask of Crimson Tears / Cerulean Tears each span a CONTIGUOUS goods-id range
//! covering every empty/charged pair for upgrade levels +0..+12 (Crimson 1000..=1025, Cerulean
//! 1050..=1075 -- verified against EquipParamGoods + GoodsName). ANY member (empty, charged, OR any
//! +N) counts as "have a flask", so the whole family is satisfied: never re-grant the base +0 flask
//! to a player who already holds an upgraded one (live 2026-07-20: a base Flask of Crimson Tears was
//! granted to a character already holding Flask of Crimson Tears +4, ids 1008/1009).

use std::collections::HashSet;

const CATEGORY_GOODS: u32 = 0x4000_0000;
const CATEGORY_MASK: u32 = 0xF000_0000;
const ROW_MASK: u32 = 0x0FFF_FFFF;

/// Goods-row ranges interchangeable for "do you have this flask": every empty/charged pair across
/// upgrade levels +0..+12. Crimson (HP) 1000..=1025, Cerulean (FP) 1050..=1075.
const FLASK_RANGES: &[std::ops::RangeInclusive<u32>] = &[1000..=1025, 1050..=1075];

fn flask_range(row: u32) -> Option<&'static std::ops::RangeInclusive<u32>> {
    FLASK_RANGES.iter().find(|fam| fam.contains(&row))
}

/// The `start_items` FullIDs NOT present in `present` (the set of held inventory item ids, encoded
/// identically to FullIDs: `(category<<28) | row`). Flask families are satisfied by ANY member.
/// Order- and repetition-preserving.
pub fn missing_start_items(present: &HashSet<u32>, start_items: &[i32]) -> Vec<i32> {
    start_items
        .iter()
        .copied()
        .filter(|&fid| {
            let id = fid as u32;
            if present.contains(&id) {
                return false;
            }
            if id & CATEGORY_MASK == CATEGORY_GOODS {
                if let Some(fam) = flask_range(id & ROW_MASK) {
                    if fam.clone().any(|r| present.contains(&(CATEGORY_GOODS | r))) {
                        return false;
                    }
                }
            }
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(ids: &[u32]) -> HashSet<u32> {
        ids.iter().copied().collect()
    }
    const FLASK_HP: i32 = (CATEGORY_GOODS | 1001) as i32; // charged crimson
    const FLASK_HP_EMPTY: u32 = CATEGORY_GOODS | 1000; // empty crimson
    const WEAPON_X: i32 = 0x0000_2710; // weapon row 10000

    #[test]
    fn absent_items_are_returned_present_ones_dropped() {
        let present = set(&[WEAPON_X as u32]);
        let start = [WEAPON_X, FLASK_HP];
        assert_eq!(missing_start_items(&present, &start), vec![FLASK_HP]);
    }

    #[test]
    fn empty_flask_satisfies_the_family() {
        // Player holds only the EMPTY crimson flask; the charged-id start item must NOT re-grant.
        let present = set(&[FLASK_HP_EMPTY]);
        assert!(missing_start_items(&present, &[FLASK_HP]).is_empty());
    }

    #[test]
    fn upgraded_flask_satisfies_the_family() {
        // Live 2026-07-20: holding Flask of Crimson Tears +4 (charged id 1009) must NOT re-grant the
        // base +0 (id 1001). Any level in the family range satisfies it.
        let present = set(&[CATEGORY_GOODS | 1009]);
        assert!(missing_start_items(&present, &[FLASK_HP]).is_empty());
        // ... and an upgraded Cerulean (id 1059 = +4 charged) satisfies the FP start item.
        let fp = (CATEGORY_GOODS | 1051) as i32;
        assert!(missing_start_items(&set(&[CATEGORY_GOODS | 1059]), &[fp]).is_empty());
    }

    #[test]
    fn no_flask_at_all_backfills_it() {
        let present = set(&[WEAPON_X as u32]);
        assert_eq!(missing_start_items(&present, &[FLASK_HP]), vec![FLASK_HP]);
    }

    #[test]
    fn repetition_is_preserved_for_absent_quantity() {
        let present: HashSet<u32> = HashSet::new();
        let start = [WEAPON_X, WEAPON_X, WEAPON_X];
        assert_eq!(missing_start_items(&present, &start), vec![WEAPON_X; 3]);
    }

    #[test]
    fn present_stack_is_not_topped_up() {
        // One copy held -> all copies considered satisfied (no over-grant of a partly-used stack).
        let present = set(&[WEAPON_X as u32]);
        assert!(missing_start_items(&present, &[WEAPON_X, WEAPON_X]).is_empty());
    }

    #[test]
    fn empty_start_list_is_empty() {
        assert!(missing_start_items(&HashSet::new(), &[]).is_empty());
    }
}
