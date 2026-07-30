//! shop_value -- keep a rewritten shop row's price ABOVE the ware's own `sellValue`.
//!
//! # The rule this exists for
//!
//! **The Elden Ring purchase menu excludes any `ShopLineupParam` row whose `value` is below the
//! ware's own `sellValue`.** Not `value == 0`, not the write itself -- the RELATION. The row is not
//! greyed and not blank: it is never built into the list, and the empty cell is not selectable.
//!
//! Found 2026-07-30 after a day of wrong guesses, on 16/16 row-level predictions across two Corhyn
//! shelves with zero misses. Corroborating vanilla census: of 710 costType-0 non-material merchant
//! rows, **zero** ship `value < sellValue`. The only sub-`sellValue` rows in the game are the 65
//! walking-mausoleum duplication rows, which render through a different menu path -- which is why
//! the behaviour was never documented anywhere.
//!
//! # Why it is NOT a rune bug, which is the important half
//!
//! `Veteran's Helm` (protector 780000, `sellValue` 1000) written onto a 600-value slot is hidden,
//! while protector 201100 (`sellValue` 100) on a 1000-value slot renders two rows away on the same
//! shelf. So `shop_sell` alone can hide **any** check whose rewarded ware has a `sellValue` above
//! the slot's vanilla price -- a check-availability bug on arbitrary seeds, and progression placed
//! on such a row is uncollectable from the menu. It predates `rune_pricing` entirely.
//!
//! Runes were merely the 100% case: `rune_worth` is `GOODS_PRICE // 10`, which is exactly
//! `sellValue`, and `rune_pricing` rolls `randint(0, 1 * worth)` -- so every rolled price is
//! `<= sellValue` and the entire roll range sits inside the hidden region. That also retro-explains
//! the 2026-07-29 report "I have never seen a single rune priced below its value": the below-value
//! ones were there and invisible. The report was a direct observation of this rule.
//!
//! # The boundary is not settled
//!
//! Whether the menu tests `<` or `<=` is unknown -- a Cheat Engine poke was attempted and abandoned
//! (`ShopLineupParam` rows are not reachable by naive memory scanning). So the default clamp is
//! `sellValue + 1`, which is safe under either reading, and `ER_SHOP_VALUE_CLAMP=eq` switches to
//! `sellValue` so one build can settle it: if a clamped row still renders under `eq`, the test is
//! `<` and the `+1` can be relaxed; if it vanishes, the `+1` is load-bearing.

use eldenring::cs::{
    EquipParamAccessory, EquipParamGem, EquipParamGoods, EquipParamProtector, EquipParamWeapon,
    ShopLineupParam, SoloParamRepository,
};

/// `ShopLineupParam.equipType` -> the ware's `sellValue`, or `None` if the row does not resolve.
///
/// `None` is REFUSED, not defaulted: a made-up sell value would clamp a price to a number with no
/// basis, and the status quo (leave the price alone) is the honest failure. Counted by the caller.
pub fn ware_sell_value(repo: &SoloParamRepository, equip_type: u8, equip_id: i32) -> Option<i32> {
    if equip_id <= 0 {
        return None;
    }
    let id = equip_id as u32;
    match equip_type {
        0 => repo.get::<EquipParamWeapon>(id).map(|r| r.sell_value()),
        1 => repo.get::<EquipParamProtector>(id).map(|r| r.sell_value()),
        2 => repo.get::<EquipParamAccessory>(id).map(|r| r.sell_value()),
        3 => repo.get::<EquipParamGoods>(id).map(|r| r.sell_value()),
        4 => repo.get::<EquipParamGem>(id).map(|r| r.sell_value()),
        _ => None,
    }
}

/// `sellValue + 1` normally; `sellValue` under `ER_SHOP_VALUE_CLAMP=eq` (see module docs).
fn floor_for(sell_value: i32) -> i32 {
    let eq = std::env::var("ER_SHOP_VALUE_CLAMP")
        .map(|v| v.trim().eq_ignore_ascii_case("eq"))
        .unwrap_or(false);
    if eq {
        sell_value
    } else {
        sell_value.saturating_add(1)
    }
}

/// Raise `value` to the floor on every row in `rows`, and report what moved.
///
/// `rows` is `(row id, equipType, equipId)` -- the caller already knows what it wrote, so this does
/// not re-derive it from the row and cannot disagree with the write.
///
/// Only ever raises. A row already priced above its ware's `sellValue` is untouched, so this cannot
/// disturb the vanilla economy of any row we did not already rewrite.
pub fn clamp(repo: &mut SoloParamRepository, rows: &[(u32, u8, i32)], who: &str) {
    let mut planned: Vec<(u32, i32)> = Vec::new();
    let mut unresolved = 0u32;

    // Two passes: the sell-value lookups borrow the repo immutably, the writes need it mutably.
    for (id, etype, eid) in rows {
        match ware_sell_value(repo, *etype, *eid) {
            Some(sv) if sv > 0 => planned.push((*id, floor_for(sv))),
            Some(_) => {} // sellValue -1/0 = not sellable, no floor to clear
            None => unresolved += 1,
        }
    }

    let mut raised = 0u32;
    let mut first_note = String::new();
    for (id, floor) in &planned {
        if let Some(row) = repo.get_mut::<ShopLineupParam>(*id) {
            let before = row.value();
            if before < *floor {
                row.set_value(*floor);
                raised += 1;
                if raised == 1 {
                    first_note = format!(" (first: row {id} {before} -> {floor})");
                }
            }
        }
    }

    // Report even when nothing moved. "0 raised" on a seed with rune rows would mean the clamp is
    // not seeing the rows it is supposed to, and a silent no-op is exactly how a feature goes inert
    // without anyone noticing (CONTRIBUTING, *Runtime visibility*).
    log::info!(
        "shop-value-clamp[{who}]: {raised} of {} row(s) raised to clear their ware's sellValue{}; \
         {unresolved} row(s) had no resolvable ware",
        rows.len(),
        first_note
    );
}
