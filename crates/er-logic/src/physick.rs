//! physick -- what counts as a Flask of Wondrous Physick TEAR, decided from the game's own fields.
//!
//! Pure. The caller supplies the two `EQUIP_PARAM_GOODS_ST` fields we cannot read without the game
//! (`goodsType`, `sortId`), exactly the way [`crate::auto_equip`] takes `wep_type` /
//! `protectorCategory`. Nothing here knows about memory layout -- see `physick_probe` in the client
//! for the live half (#334 phase 2).
//!
//! ## The instrument: `goodsType == 10`
//!
//! Datamined off the committed `gen_inputs` bundle (`EquipParamGoods.csv`, 2326 rows):
//!
//! ```text
//! goodsType 10 : 43 rows  ->  40 real tears  ->  37 of those in the AP item pool
//! ```
//!
//! ## NEVER match on the NAME
//!
//! It is wrong in both directions. Sixteen goods are named "...Tear" and are NOT tears (Larval Tear,
//! Sacred Tear, Silver Tear Husk, Mimic Tear Ashes, "Asimi, Silver Tear"...), and three real tears
//! do not carry the word as a token: Speckled Hard*tear*, Crimson Bubble*tear*, Opaline Bubble*tear*.
//!
//! ## Rows 42 / 43 / 44 are dummies, and "require a name" does NOT exclude them
//!
//! The phase-1 handoff said these three "have no `GoodsName`". **They do.** All three resolve in
//! `GoodsName.fmg.xml`, to the literal string `[ERROR]` -- one of 119 such entries in the file. A
//! classifier that keys on *"the FMG has an entry for this row"* therefore lets all three straight
//! through and will happily try to mix a nameless dummy.
//!
//! The correct instrument is the one the repo already uses for FromSoft's unused rows
//! (`tools/find_spare_goods.py`: *"empty Name, `sortId 999999` + `sortGroupId 255`"*):
//! **`sortId == 999999`**. Verified to separate the three dummies from the forty real tears
//! perfectly, and it is a general marker -- 171 of the 2326 goods rows carry it.
//!
//! (`iconId == 0`, `maxNum == 99` and `rarity == 0` separate them just as cleanly; `sortId` is
//! preferred only because the repo already treats it as the unused-row marker, so one convention
//! covers both call sites.)
//!
//! ## Near-duplicate rows: nothing may assume one row per tear
//!
//! Three pairs share a name and differ only in `disableParam_NT` / `sortId` / `iconId`. Only the
//! lower row of each pair is in the AP catalog (the catalog is keyed by NAME), which is exactly why
//! 40 real tears yield 37 pool items. A live read that returns `11003` will NOT resolve to an AP
//! item unless it is folded to `11002` first -- see [`canonical_row`].
//!
//! ## Tears are KEY items
//!
//! `InventoryItemsData.multiplay_key_items` is documented in the pinned crate as holding the
//! `REGENERATIVE_MATERIAL` and `WONDROUS_PHYSICK_TEAR` copies of the **key items** list. Any client
//! code that resolves a received tear to a handle by walking `normal_entries()` alone -- which is
//! what `auto_equip::tick()` does -- will miss every one of them and retry forever. That is the
//! #296 failure shape and it is indistinguishable from it in a log.

/// `EquipParamGoods.goodsType` for a Wondrous Physick tear.
pub const GOODS_TYPE_PHYSICK_TEAR: u8 = 10;

/// FromSoft's "this row is unused" marker in `sortId`. Shared with `tools/find_spare_goods.py`.
pub const UNUSED_SORT_ID: i32 = 999_999;

/// Category nibble of a GOODS FullID. FullID = `(category << 28) | param_row`; goods = 4.
pub const CATEGORY_GOODS: u32 = 0x4000_0000;

/// Mask for the category nibble of a FullID.
pub const CATEGORY_MASK: u32 = 0xF000_0000;

/// The near-duplicate rows, `(alias, canonical)`. Datamined: each pair shares a `GoodsName` and
/// differs only in `disableParam_NT` / `sortId` / `iconId`. The canonical member is the one the AP
/// catalog resolves, so a live read must be folded through [`canonical_row`] before any lookup.
pub const DUPLICATE_ROWS: &[(u32, u32)] = &[
    (11003, 11002), // Crimson Crystal Tear
    (11005, 11004), // Cerulean Crystal Tear
    (11017, 11016), // Ruptured Crystal Tear
];

/// Is this goods row a physick tear? Both fields come straight off `EQUIP_PARAM_GOODS_ST`.
///
/// `sort_id` is required, not optional: `goods_type` alone admits the three `[ERROR]` dummy rows.
pub fn is_tear(goods_type: u8, sort_id: i32) -> bool {
    goods_type == GOODS_TYPE_PHYSICK_TEAR && sort_id != UNUSED_SORT_ID
}

/// The GOODS param row inside a FullID, or `None` if the FullID is not goods.
///
/// Kept deliberately narrow: a bare row and a FullID are different values (`11002` vs
/// `0x40002AFA`), and conflating them is how a memory scan reports the wrong representation.
pub fn goods_row(full_id: i32) -> Option<u32> {
    let v = full_id as u32;
    if v & CATEGORY_MASK == CATEGORY_GOODS {
        Some(v & !CATEGORY_MASK)
    } else {
        None
    }
}

/// The GOODS FullID for a param row.
pub fn goods_full_id(row: u32) -> i32 {
    (CATEGORY_GOODS | (row & !CATEGORY_MASK)) as i32
}

/// Fold a near-duplicate row onto the row the AP catalog knows. Identity for every other row, and
/// idempotent, so it is safe to apply more than once.
pub fn canonical_row(row: u32) -> u32 {
    for &(alias, canonical) in DUPLICATE_ROWS {
        if row == alias {
            return canonical;
        }
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goods_type_ten_is_a_tear() {
        // 11002 Crimson Crystal Tear, sortId 201000.
        assert!(is_tear(10, 201_000));
        // 2011070 Deflecting Hardtear, sortId 201141 (DLC).
        assert!(is_tear(10, 201_141));
    }

    #[test]
    fn other_goods_types_are_not_tears() {
        for gt in [0u8, 1, 2, 3, 9, 11, 14] {
            assert!(
                !is_tear(gt, 201_000),
                "goodsType {} classified as a tear",
                gt
            );
        }
    }

    /// THE MOTIVATING CASE (CONTRIBUTING rule 11). Rows 42/43/44 carry `goodsType 10` AND have a
    /// `GoodsName` entry -- its text is the literal `[ERROR]`. Only `sortId` rejects them, so this
    /// test fails the moment the predicate is weakened back to `goods_type == 10`.
    #[test]
    fn error_named_dummy_rows_are_rejected() {
        assert!(
            !is_tear(GOODS_TYPE_PHYSICK_TEAR, UNUSED_SORT_ID),
            "a sortId-999999 dummy classified as a tear"
        );
        // The dummies differ from the real tears ONLY in the unused-row marker: same goodsType.
        assert!(is_tear(GOODS_TYPE_PHYSICK_TEAR, 201_000));
    }

    #[test]
    fn full_id_round_trips() {
        assert_eq!(goods_row(0x4000_2AFAu32 as i32), Some(11002));
        assert_eq!(goods_full_id(11002), 0x4000_2AFAu32 as i32);
        assert_eq!(goods_full_id(2_011_070), 0x401E_AFBEu32 as i32);
        // Not goods: a weapon (category 0) and a protector (category 1).
        assert_eq!(goods_row(0x0000_2710), None);
        assert_eq!(goods_row(0x1000_2710), None);
    }

    #[test]
    fn duplicates_fold_onto_the_catalog_row() {
        assert_eq!(canonical_row(11003), 11002);
        assert_eq!(canonical_row(11005), 11004);
        assert_eq!(canonical_row(11017), 11016);
        // Idempotent, and identity elsewhere.
        assert_eq!(canonical_row(canonical_row(11003)), 11002);
        assert_eq!(canonical_row(11011), 11011);
        assert_eq!(canonical_row(2_011_070), 2_011_070);
    }

    /// The aliases must never also appear as canonical targets, or folding would not terminate.
    #[test]
    fn duplicate_table_is_acyclic() {
        for &(alias, canonical) in DUPLICATE_ROWS {
            assert_ne!(alias, canonical);
            assert!(
                !DUPLICATE_ROWS.iter().any(|&(a, _)| a == canonical),
                "row {} is both a canonical target and an alias",
                canonical
            );
        }
    }
}
