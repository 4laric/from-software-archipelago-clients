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

/// The value an UNOCCUPIED mixture slot holds: `OptionalItemId::NONE`, i.e. `u32::MAX`.
///
/// MEASURED, not assumed (probe v2, 2026-08-03 22:33): both slots read `0xFFFFFFFF` on a fresh
/// load, and slot A returned to `0xFFFFFFFF` on an unmix. It also happens to be exactly the pinned
/// crate's `OptionalItemId::NONE`, which is the structural confirmation that the field is
/// `[OptionalItemId; 2]` and not two loose dwords that happen to hold ids.
pub const EMPTY_SLOT: u32 = u32::MAX;

/// The Flask of Wondrous Physick holds exactly two tears.
pub const MIXTURE_SLOTS: usize = 2;

/// Which mixture slot a received tear should occupy, given the two slots as read from the game and
/// the tear's position in the AP RECEIVED STREAM.
///
/// `mixture` is raw: [`EMPTY_SLOT`] for an unoccupied slot, otherwise a GOODS FullID.
///
/// Returns `None` when the tear is ALREADY mixed. That is not a courtesy -- re-writing a slot with
/// the value it already holds would churn the menu for no behaviour change.
///
/// ## The policy
///
/// `auto_equip` follows the community **French Challenge** ruleset (Alaric's ruling, 2026-08-03),
/// which names flask contents among the things that auto-equip and must STAY equipped. So the fix
/// for an unwanted mixture is **WHERE the tear lands, never WHETHER it is mixed**.
///
/// 1. **Already mixed -> `None`.**
/// 2. **First empty slot.**
/// 3. **Both full -> alternate**, `ordinal % 2`.
///
/// Rule 3 was `slot 0, always` when this shipped, mirroring [`crate::auto_equip::slot_for_accessory`].
/// Alaric played it and reported the tell: the fourth tear clobbered slot 0 like the third, so
/// **slot 1 froze permanently** on whatever arrived second and only one of the two ever changed
/// again. That is the opposite of what the talisman policy's own rationale asks for ("ends up with
/// the four most recent talismans rather than one").
///
/// ## 🛑 Why the ordinal, and not a flip-flop counter
///
/// The reconciler replays the WHOLE received set on every reconnect, so the policy has to be a pure
/// function of things that replay identically. A local "which slot did I clobber last" flag is not:
/// it resets at connect and desyncs from its live phase. With three tears A, B, C:
///
/// ```text
/// live    A -> slot 0 (empty)   B -> slot 1 (empty)   C -> clobber(flag=0) = slot 0   => {C, B}
/// replay  A -> clobber(flag=0) = slot 0 => {A, B}
///         B -> already mixed, no-op
///         C -> clobber(flag=1) = slot 1 => {A, C}     <-- the flask silently rearranged
/// ```
///
/// The AP received stream, by contrast, is replayed in the same order every time, so a tear's
/// ORDINAL in it is stable -- the same property `flask_reconcile` already leans on to run with no
/// ledger at all. Keying rule 3 off the ordinal alternates AND converges:
///
/// ```text
/// live    A(0) -> 0   B(1) -> 1   C(2) -> 2%2 = 0     => {C, B}
/// replay  A(0) -> 0%2 = 0 => {A, B}   B(1) -> no-op   C(2) -> 0 => {C, B}   <-- same
/// ```
///
/// `replaying_the_received_set_converges` is the acceptance test, and it FAILS for a flip-flop.
///
/// Comparison is by [`canonical_row`], so a near-duplicate row cannot slip past the already-mixed
/// check and mix the same tear twice under two ids.
pub fn slot_for_tear(
    mixture: [u32; MIXTURE_SLOTS],
    incoming_full_id: i32,
    ordinal: u64,
) -> Option<usize> {
    let incoming = canonical_row(goods_row(incoming_full_id)?);

    let occupant = |slot: u32| -> Option<u32> {
        if slot == EMPTY_SLOT {
            None
        } else {
            goods_row(slot as i32).map(canonical_row)
        }
    };

    // Already mixed -> nothing to do.
    if mixture.iter().any(|&s| occupant(s) == Some(incoming)) {
        return None;
    }
    // First empty slot.
    if let Some(i) = mixture.iter().position(|&s| occupant(s).is_none()) {
        return Some(i);
    }
    // Both occupied -> alternate by received-stream position, so neither slot freezes.
    Some((ordinal % MIXTURE_SLOTS as u64) as usize)
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

    /// Apply a whole received stream to a starting mixture, the way `auto_equip::tick` does:
    /// ordinal `i` for the `i`th tear in the stream.
    fn run(start: [u32; MIXTURE_SLOTS], stream: &[u32]) -> [u32; MIXTURE_SLOTS] {
        let mut m = start;
        for (i, &row) in stream.iter().enumerate() {
            let full = goods_full_id(row);
            if let Some(slot) = slot_for_tear(m, full, i as u64) {
                m[slot] = full as u32;
            }
        }
        m
    }

    const EMPTY: [u32; MIXTURE_SLOTS] = [EMPTY_SLOT, EMPTY_SLOT];

    #[test]
    fn empty_flask_fills_the_first_slot() {
        assert_eq!(slot_for_tear(EMPTY, goods_full_id(11002), 0), Some(0));
    }

    #[test]
    fn one_occupied_slot_fills_the_other() {
        let m = [goods_full_id(11002) as u32, EMPTY_SLOT];
        assert_eq!(slot_for_tear(m, goods_full_id(11004), 1), Some(1));
        // ...and the reverse, in case the game ever fills B first.
        let m = [EMPTY_SLOT, goods_full_id(11004) as u32];
        assert_eq!(slot_for_tear(m, goods_full_id(11002), 1), Some(0));
    }

    /// 🛑 THE MOTIVATING CASE (rule 11). Alaric played the always-clobber-slot-0 build and reported
    /// it: the third tear took slot 0, and so did the FOURTH, so slot 1 froze forever on whatever
    /// arrived second. Both slots must churn.
    #[test]
    fn a_full_flask_alternates_instead_of_freezing_slot_one() {
        // Four tears into an empty flask: 0 and 1 fill, then 2 and 3 clobber ALTERNATELY.
        let m = run(EMPTY, &[11002, 11004, 11001, 11012]);
        assert_eq!(
            m,
            [goods_full_id(11001) as u32, goods_full_id(11012) as u32]
        );
        // Neither of the first two survives -- under the old policy 11004 would still be in slot 1.
        assert_ne!(m[1], goods_full_id(11004) as u32);
    }

    /// 🛑 THE ACCEPTANCE TEST for keying rule 3 off the received-stream ordinal instead of a local
    /// flip-flop. The reconciler replays the WHOLE received set on every reconnect, so replaying it
    /// from the state it produced must be a fixed point. A flip-flop counter FAILS this at n = 3.
    #[test]
    fn replaying_the_received_set_converges() {
        let streams: [&[u32]; 6] = [
            &[11002],
            &[11002, 11004],
            &[11002, 11004, 11001],
            &[11002, 11004, 11001, 11012],
            &[11002, 11004, 11001, 11012, 11011],
            &[11002, 11004, 11001, 11012, 11011, 11025],
        ];
        for stream in streams {
            let live = run(EMPTY, stream);
            let replayed = run(live, stream);
            assert_eq!(
                live,
                replayed,
                "reconnect rearranged the flask for a {}-tear stream",
                stream.len()
            );
            // And replaying twice more must not drift either.
            assert_eq!(live, run(run(live, stream), stream));
        }
    }

    /// Idempotence is load-bearing: the reconciler replays the whole received set on every
    /// reconnect, so a tear that is already mixed must be a no-op and not a slot rotation.
    #[test]
    fn an_already_mixed_tear_is_a_no_op() {
        let m = [goods_full_id(11002) as u32, goods_full_id(11004) as u32];
        assert_eq!(slot_for_tear(m, goods_full_id(11002), 7), None);
        assert_eq!(slot_for_tear(m, goods_full_id(11004), 8), None);
        // Also when the other slot is free -- "first empty" must not win over "already mixed".
        let m = [goods_full_id(11002) as u32, EMPTY_SLOT];
        assert_eq!(slot_for_tear(m, goods_full_id(11002), 3), None);
    }

    /// A near-duplicate row must not mix the same tear twice under two ids.
    #[test]
    fn near_duplicates_count_as_already_mixed() {
        let m = [goods_full_id(11003) as u32, EMPTY_SLOT];
        assert_eq!(slot_for_tear(m, goods_full_id(11002), 0), None);
        let m = [goods_full_id(11002) as u32, EMPTY_SLOT];
        assert_eq!(slot_for_tear(m, goods_full_id(11003), 0), None);
    }

    #[test]
    fn a_non_goods_id_is_never_mixed() {
        assert_eq!(slot_for_tear(EMPTY, 0x0000_2710, 0), None); // weapon
        assert_eq!(slot_for_tear(EMPTY, 0x1000_2710, 1), None); // protector
    }

    /// The empty sentinel is a MEASURED value; pin it so a future edit cannot quietly swap it for
    /// a plausible-looking 0 or -1-as-i32.
    #[test]
    fn empty_slot_is_optional_item_id_none() {
        assert_eq!(EMPTY_SLOT, 0xFFFF_FFFF);
        assert_eq!(EMPTY_SLOT, u32::MAX);
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
