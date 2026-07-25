//! `shop_repoint` — should a shop CHECK row be repointed at the preview good the world chose for it?
//!
//! # The bug this exists to kill
//!
//! `shopPreviewGoods` tells the client "display this shop slot as goods row G". The client honours
//! that by rewriting G's **FMG name/info/caption** (`shop_preview`) and G's **iconId**
//! (`shop_icon`) — both keyed by the goods row id, both global to that row.
//!
//! That is only *visible* if the shop row actually SELLS G. The menu renders a slot from its
//! `ShopLineupParam.equipId`/`equipType`, not from slot_data.
//!
//! Originally it did: `shopPreviewGoods` was the slot's own vanilla ware, so renaming G renamed the
//! thing on the shelf. That is exactly why the 2026-07-12 "every Smithing Stone wears the telescope
//! icon" bug was *visible* — G was a real good the row sold, and the write was global.
//!
//! The world then fixed the destructiveness by REPOINTING the preview at a dedicated spare goods row
//! (`greenfield/spare_goods.tsv` — exists, no real name, referenced by no lot/shop/recipe): region
//! locks 2026-07-20, then every foreign slot 2026-07-22. The global write became safe — and
//! **inert**, because nothing ever wrote the spare onto the row. The shelf kept selling the vanilla
//! ware; the rename and the flower landed on a row no menu reads.
//!
//! Nothing errored. `shops.py` even logs, of an overflowing spare pool, that the slots "still flower,
//! just a shared name" — an invariant asserted in a comment with no test behind it, which is the
//! house failure mode (CONTRIBUTING, *"A comment that asserts a fact is a claim, and claims rot"*).
//! Confirmed by Alaric in-game 2026-07-25: foreign shop slots show the VANILLA ware.
//!
//! So the missing half is a production caller that writes the row. This module is its decision half:
//! pure, host-tested, no game and no I/O.
//!
//! # Why the "sold natively" arm comes first
//!
//! `shop_sell` rewrites OWN-WORLD sellable rewards so the slot natively sells the real item — correct
//! name, icon and lore, no FMG collision at all. For those rows the world deliberately leaves
//! `shopPreviewGoods` at the VANILLA ware, so a repoint decided on "preview != current ware" alone
//! would drag the row back off its reward and onto the vanilla good, undoing shop_sell and breaking
//! ECHO-DEDUP's param-revert guard (which re-reads the row to prove it still sells the reward).
//! `sold_natively` is therefore checked before anything else, and the caller must source it from
//! shop_sell's own record of the rows it wrote — never from a guess about the item's category.

/// `ShopLineupParam.equipType` selecting the GOODS param table (0 weapon, 1 protector, 2 accessory,
/// 3 goods, 4 gem, 5 custom weapon — confirmed against the vanilla ShopLineupParam dump; see
/// `shop_sell`'s field-encoding note).
pub const EQUIP_TYPE_GOODS: u8 = 3;

/// Why a row was left alone. Carried so the caller can log a TALLY rather than a bare count:
/// a repoint pass that writes 0 rows is indistinguishable from a broken one without it
/// (CONTRIBUTING, *"Log why things were skipped, not just that they were"*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `shop_sell` owns this row — it natively sells the real own-world reward. Never touch it.
    SoldNatively,
    /// No `shopPreviewGoods` entry for this location.
    NoPreview,
    /// The preview FullID is not a GOODS row. Every spare in the pool is a goods row, so this is
    /// either an un-repointed vanilla ware in another table or a malformed id — either way, writing
    /// it as `equipType 3` would point the slot at an unrelated goods row.
    NotGoods,
    /// The row already sells the preview good (idempotent re-run, the common case on tick).
    AlreadyPointed,
}

/// The decision for one shop check row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repoint {
    /// Write `(equipId, equipType)` onto the row.
    Write(i32, u8),
    Skip(SkipReason),
}

/// Decide whether this shop check row must be repointed at its preview good.
///
/// * `preview_full_id` — the `shopPreviewGoods` value for this row's AP location (an ER **FullID**,
///   category nibble included), or `None` if the location has no entry.
/// * `cur_equip_id` / `cur_equip_type` — what the LIVE row sells right now. Read fresh every pass:
///   a map load streams `ShopLineupParam` back in and reverts runtime writes, so "already pointed"
///   is a fact about this tick, never a latch.
/// * `sold_natively` — `shop_sell` rewrote this row to sell the real own-world reward.
///
/// Idempotent by construction: re-running against a row this returned `Write` for yields
/// `Skip(AlreadyPointed)`.
pub fn decide(
    preview_full_id: Option<i64>,
    cur_equip_id: i32,
    cur_equip_type: u8,
    sold_natively: bool,
) -> Repoint {
    // FIRST, and not negotiable — see the module header.
    if sold_natively {
        return Repoint::Skip(SkipReason::SoldNatively);
    }
    let Some(fid) = preview_full_id else {
        return Repoint::Skip(SkipReason::NoPreview);
    };
    let q = fid as u32;
    if er_codec::item_category_of(q) != er_codec::CATEGORY_GOODS {
        return Repoint::Skip(SkipReason::NotGoods);
    }
    let want = er_codec::row_id_of(q) as i32;
    if cur_equip_id == want && cur_equip_type == EQUIP_TYPE_GOODS {
        return Repoint::Skip(SkipReason::AlreadyPointed);
    }
    Repoint::Write(want, EQUIP_TYPE_GOODS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOODS: i64 = 0x4000_0000;
    const WEAPON_NIBBLE: i64 = 0x0000_0000;
    const SPARE: i64 = 9314; // first foreign-slot spare above the 8852 floor (greenfield/spare_goods.tsv)

    #[test]
    fn repoints_a_vanilla_goods_row_at_its_spare() {
        // Vanilla ware: goods row 9510 (the "9 Leyndell Locks" perfume bottle). Preview: spare 9314.
        assert_eq!(
            decide(Some(GOODS | SPARE), 9510, 3, false),
            Repoint::Write(9314, 3)
        );
    }

    #[test]
    fn repoints_a_weapon_slot_cross_type() {
        // A WEAPON row (equipType 0) whose reward is foreign: the spare is a goods row, so the write
        // is cross-category. SHOP_CTD_GUARD was removed 2026-07-11 and shop_stock already does this
        // on 455 infinite rows; the decision does not special-case it, but the test pins that.
        assert_eq!(
            decide(Some(GOODS | SPARE), 1030000, 0, false),
            Repoint::Write(9314, 3)
        );
    }

    #[test]
    fn never_touches_a_row_shop_sell_owns() {
        // The dangerous one: shop_sell rewrote this row to sell the real own-world reward, and the
        // world left the preview at the VANILLA ware. Deciding on "preview != current" alone would
        // write the vanilla good back over the reward.
        assert_eq!(
            decide(Some(GOODS | 9510), 1030000, 0, true),
            Repoint::Skip(SkipReason::SoldNatively)
        );
    }

    #[test]
    fn idempotent_once_pointed() {
        assert_eq!(
            decide(Some(GOODS | SPARE), 9314, 3, false),
            Repoint::Skip(SkipReason::AlreadyPointed)
        );
    }

    #[test]
    fn same_row_id_in_another_table_is_not_already_pointed() {
        // equipId 9314 with equipType 0 is a WEAPON row that merely shares the number. Comparing the
        // id without the type would call this settled and leave a weapon on the shelf.
        assert_eq!(
            decide(Some(GOODS | SPARE), 9314, 0, false),
            Repoint::Write(9314, 3)
        );
    }

    #[test]
    fn a_non_goods_preview_is_refused_not_coerced() {
        // A weapon-category preview FullID. Writing its row id as equipType 3 would point the slot at
        // whatever goods row happens to share the number. Refuse instead (CONTRIBUTING rule 1).
        assert_eq!(
            decide(Some(WEAPON_NIBBLE | 1030000), 9510, 3, false),
            Repoint::Skip(SkipReason::NotGoods)
        );
    }

    #[test]
    fn no_preview_entry_is_a_skip_not_a_zero_write() {
        assert_eq!(
            decide(None, 9510, 3, false),
            Repoint::Skip(SkipReason::NoPreview)
        );
    }
}
