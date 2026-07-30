//! shop_value -- make a rewritten shop row RENDER, without throwing away its price.
//!
//! # The rule this exists for
//!
//! **The Elden Ring purchase menu EXCLUDES any `ShopLineupParam` row whose `value` is below the
//! ware's own `sellValue`.** Not `value == 0`, not the write itself -- the RELATION. The row is not
//! greyed and not blank: it is never built into the list, and the empty cell is not selectable.
//!
//! Found 2026-07-30 on 16/16 row-level predictions across two Corhyn shelves, zero misses.
//! Vanilla corroboration: of 710 costType-0 non-material merchant rows, **zero** ship
//! `value < sellValue`. The only sub-`sellValue` rows in the game are the 65 walking-mausoleum
//! duplication rows, which render through a different menu path -- which is why the behaviour is
//! documented nowhere.
//!
//! # It is NOT a rune bug, which is the important half
//!
//! `Veteran's Helm` (protector 780000, `sellValue` 1000) written onto a 600-value slot is hidden,
//! while protector 201100 (`sellValue` 100) on a 1000-value slot renders two rows away on the same
//! shelf. `shop_sell` alone can therefore hide **any** check whose reward has a `sellValue` above
//! the slot's vanilla price, and progression on such a row is uncollectable. It predates
//! `rune_pricing`; runes were merely the 100% case, because `rune_worth` is `GOODS_PRICE // 10`
//! which is exactly `sellValue`, and the roll is `randint(0, 1 * worth)` -- the entire roll range
//! sits inside the hidden region.
//!
//! # Why we LOWER `sellValue` instead of RAISING `value`
//!
//! Raising the row's price to `sellValue + 1` makes it render and destroys the feature: a rune can
//! then never be a bargain, and break-even is the best the game allows. Alaric: another randomizer
//! manages under-price runes, so it is possible.
//!
//! The enabling datum (verified over all 35 rune goods -- 2900-2919, 2950-2964, 2990): **a rune's
//! payout is `SpEffectParam.soul` reached via `EquipParamGoods.refId_default`, and `soul` equals
//! `sellValue` EXACTLY.** So on a rune `sellValue` is redundant data that nothing downstream reads
//! for the payout. Lowering it below the rolled price clears the menu's exclusion and leaves the
//! bargain intact -- and it retires the `<` vs `<=` boundary question entirely, because `value - 1`
//! is strictly below the threshold under either reading.
//!
//! It also fixes the `Veteran's Helm` class the RIGHT way round: that slot stays at its 600-rune
//! price instead of inflating to 1001.
//!
//! # Blast radius
//!
//! - **Payout on use: zero.** The payout is `SpEffectParam.soul`; we never touch it.
//! - **Selling the ware back**: capped at `value - 1` for the session. The player paid `value` to
//!   get the copy, so there is no money pump -- only a slightly worse sell-back on a bargain.
//! - **Other vanilla rows selling the same ware**: their buy price is their own `value`, unchanged.
//!   Lowering `sellValue` can only make a row MORE visible, never less.
//! - 🛑 **Synthetic goods are REFUSED.** `er_codec` repurposes `EquipParamGoods.sellValue` as the
//!   local-quantity carrier on synthetic rows (`id > SYNTHETIC_GOODS_MIN_ID`), so writing it there
//!   would corrupt a grant quantity. Rune rows (2900-2964) are disjoint from that range and no
//!   caller should ever pass one, but the failure mode is a lost save, so the guard is explicit and
//!   unit-tested rather than left to the callers' good behaviour.
//!
//! # Fallback
//!
//! One assumption is load-bearing and only the game can settle it: that the menu reads `sellValue`
//! **live at list-build time** rather than from a boot-time cache. If it caches,
//! `ER_SHOP_VALUE_CLAMP=raise` restores the old raise-to-`sellValue + 1` behaviour (and `=eq` uses
//! `sellValue`) with no rebuild -- and rune rows reappearing under `raise` proves the guard ran and
//! the menu caches, which is a result, not a dead end.

use eldenring::cs::{
    EquipParamAccessory, EquipParamGem, EquipParamGoods, EquipParamProtector, EquipParamWeapon,
    ShopLineupParam, SoloParamRepository,
};

/// The check-lot placeholder good (`check_lots.rs`). Dressed at runtime with a borrowed icon and an
/// injected name; never a real ware, and not ours to reprice.
const PLACEHOLDER_GOODS: i32 = 8852;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Lower the ware's `sellValue` under the row's price. Keeps the bargain. Default.
    Lower,
    /// Raise the row's price to `sellValue + 1`. Loses the bargain; fallback if the menu caches.
    Raise,
    /// Raise the row's price to exactly `sellValue` -- only useful to probe the `<` vs `<=` edge.
    RaiseEq,
}

pub fn mode() -> Mode {
    match std::env::var("ER_SHOP_VALUE_CLAMP") {
        Ok(v) if v.trim().eq_ignore_ascii_case("raise") => Mode::Raise,
        Ok(v) if v.trim().eq_ignore_ascii_case("eq") => Mode::RaiseEq,
        _ => Mode::Lower,
    }
}

/// What to do about one row. Pure, so the refusal below can be tested by a direct call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Already renders, or there is no threshold to clear. Leave it alone.
    Nothing,
    /// Write this `value` to the shop row.
    SetRowValue(i32),
    /// Write this `value` to the shop row AND this `sellValue` to the ware.
    LowerWare { row_value: i32, sell_value: i32 },
    /// The ware carries a repurposed `sellValue`; touching it would corrupt game state.
    RefuseSynthetic,
}

/// The whole decision, with no game access, so every arm is reachable from a unit test.
///
/// A guard the corpus never triggers is untested (CONTRIBUTING), and the synthetic-ware refusal is
/// exactly that kind of guard: no current caller can produce one, and if that ever changes the
/// failure is a corrupted grant quantity in a player's save. Hence: pure fn, direct test.
pub fn plan(value: i32, sell_value: i32, equip_type: u8, equip_id: i32, mode: Mode) -> Action {
    if equip_type == 3
        && (equip_id == PLACEHOLDER_GOODS
            || (equip_id > 0 && equip_id as u32 > er_codec::SYNTHETIC_GOODS_MIN_ID))
    {
        return Action::RefuseSynthetic;
    }
    if sell_value <= 0 {
        return Action::Nothing; // unsellable ware -- no threshold, cannot be excluded by this rule
    }
    match mode {
        Mode::Raise | Mode::RaiseEq => {
            let floor = if mode == Mode::Raise {
                sell_value.saturating_add(1)
            } else {
                sell_value
            };
            if value < floor {
                Action::SetRowValue(floor)
            } else {
                Action::Nothing
            }
        }
        Mode::Lower => {
            if value > sell_value {
                return Action::Nothing; // already renders under either boundary reading
            }
            // A free row would have to sit below sellValue 0, which is not expressible, so the
            // rolled-0 case costs one rune. Deliberate, and cheaper than the row not existing.
            let row_value = value.max(1);
            Action::LowerWare {
                row_value,
                sell_value: sell_value.min(row_value - 1),
            }
        }
    }
}

/// `ShopLineupParam.equipType` -> the ware's `sellValue`, or `None` if the row does not resolve.
///
/// `None` is REFUSED, not defaulted: an invented sell value would move a threshold on no basis.
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

fn set_ware_sell_value(
    repo: &mut SoloParamRepository,
    equip_type: u8,
    equip_id: i32,
    v: i32,
) -> bool {
    let id = equip_id as u32;
    match equip_type {
        0 => repo.get_mut::<EquipParamWeapon>(id).map(|r| r.set_sell_value(v)),
        1 => repo
            .get_mut::<EquipParamProtector>(id)
            .map(|r| r.set_sell_value(v)),
        2 => repo
            .get_mut::<EquipParamAccessory>(id)
            .map(|r| r.set_sell_value(v)),
        3 => repo.get_mut::<EquipParamGoods>(id).map(|r| r.set_sell_value(v)),
        4 => repo.get_mut::<EquipParamGem>(id).map(|r| r.set_sell_value(v)),
        _ => None,
    }
    .is_some()
}

/// Make every row in `rows` renderable. `rows` is `(row id, equipType, equipId)` -- what the caller
/// wrote, not a re-read, so this cannot disagree with the write it is guarding.
pub fn render_guard(repo: &mut SoloParamRepository, rows: &[(u32, u8, i32)], who: &str) {
    let m = mode();

    // Decide first (immutable reads), then write. Two passes keeps the borrows apart, and makes the
    // decision inspectable as data rather than as a side effect buried in a loop.
    let mut plans: Vec<(u32, u8, i32, Action)> = Vec::new();
    let (mut unresolved, mut refused) = (0u32, 0u32);
    for (id, etype, eid) in rows {
        match ware_sell_value(repo, *etype, *eid) {
            Some(sv) => {
                let a = plan(
                    repo.get::<ShopLineupParam>(*id)
                        .map(|r| r.value())
                        .unwrap_or(i32::MAX),
                    sv,
                    *etype,
                    *eid,
                    m,
                );
                if a == Action::RefuseSynthetic {
                    refused += 1;
                } else if a != Action::Nothing {
                    plans.push((*id, *etype, *eid, a));
                }
            }
            None => unresolved += 1,
        }
    }

    let (mut rows_moved, mut wares_moved) = (0u32, 0u32);
    let mut first = String::new();
    for (id, etype, eid, action) in &plans {
        match action {
            Action::SetRowValue(v) => {
                if let Some(row) = repo.get_mut::<ShopLineupParam>(*id) {
                    if first.is_empty() {
                        first = format!(" (first: row {id} value {} -> {v})", row.value());
                    }
                    row.set_value(*v);
                    rows_moved += 1;
                }
            }
            Action::LowerWare {
                row_value,
                sell_value,
            } => {
                if let Some(row) = repo.get_mut::<ShopLineupParam>(*id) {
                    if row.value() != *row_value {
                        row.set_value(*row_value);
                        rows_moved += 1;
                    }
                }
                if set_ware_sell_value(repo, *etype, *eid, *sell_value) {
                    wares_moved += 1;
                    if first.is_empty() {
                        first = format!(
                            " (first: row {id} @{row_value}, ware type {etype} id {eid} sellValue \
                             -> {sell_value})"
                        );
                    }
                }
            }
            _ => {}
        }
    }

    // Report every pass, including the all-zero one: "0 moved" on a seed that has rune rows means
    // this guard is not seeing the rows it is meant to, and a silent no-op is exactly how a feature
    // goes inert without anyone noticing (CONTRIBUTING, *Runtime visibility*).
    log::info!(
        "shop-value[{m:?}]({who}): {} row(s) considered, {rows_moved} row value(s) set, \
         {wares_moved} ware sellValue(s) lowered{first}; {unresolved} unresolved ware(s), \
         {refused} synthetic ware(s) REFUSED",
        rows.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // The motivating case: a rune priced at a bargain must render WITHOUT losing the bargain.
    #[test]
    fn a_rune_priced_below_its_worth_lowers_the_ware_not_the_bargain() {
        // Golden Rune [10]: sellValue 5000 (== its payout), rolled to 137.
        let a = plan(137, 5000, 3, 2909, Mode::Lower);
        assert_eq!(
            a,
            Action::LowerWare {
                row_value: 137,
                sell_value: 136
            },
            "the rolled price must survive; only the ware's sellValue moves"
        );
    }

    #[test]
    fn a_row_already_above_its_wares_sell_value_is_untouched() {
        assert_eq!(plan(1500, 100, 1, 201100, Mode::Lower), Action::Nothing);
    }

    // Veteran's Helm: sellValue 1000 on a 600-rune slot. Hidden in game; must render at 600, NOT be
    // inflated to 1001 -- the price the player sees is the slot's, which is the point.
    #[test]
    fn the_veterans_helm_class_keeps_its_slot_price() {
        assert_eq!(
            plan(600, 1000, 1, 780000, Mode::Lower),
            Action::LowerWare {
                row_value: 600,
                sell_value: 599
            }
        );
    }

    #[test]
    fn a_free_row_costs_one_rune_because_sell_value_cannot_go_below_zero() {
        assert_eq!(
            plan(0, 200, 3, 2900, Mode::Lower),
            Action::LowerWare {
                row_value: 1,
                sell_value: 0
            }
        );
    }

    #[test]
    fn an_unsellable_ware_has_no_threshold_to_clear() {
        assert_eq!(plan(600, -1, 3, 8000, Mode::Lower), Action::Nothing);
    }

    // 🛑 The guard no caller can currently trigger, hence the direct call. `er_codec` repurposes
    // EquipParamGoods.sellValue as the local-quantity carrier above SYNTHETIC_GOODS_MIN_ID, so a
    // write there corrupts a grant quantity in a live save.
    #[test]
    fn a_synthetic_goods_ware_is_refused_not_repriced() {
        let synthetic = (er_codec::SYNTHETIC_GOODS_MIN_ID + 1) as i32;
        assert_eq!(
            plan(10, 5000, 3, synthetic, Mode::Lower),
            Action::RefuseSynthetic
        );
        assert_eq!(
            plan(10, 5000, 3, PLACEHOLDER_GOODS, Mode::Lower),
            Action::RefuseSynthetic
        );
        // ...and the refusal is about GOODS specifically: the same id as a weapon is a real ware.
        assert_ne!(plan(10, 5000, 0, synthetic, Mode::Lower), Action::RefuseSynthetic);
    }

    #[test]
    fn raise_mode_is_the_old_behaviour_and_still_available_as_a_fallback() {
        assert_eq!(plan(137, 5000, 3, 2909, Mode::Raise), Action::SetRowValue(5001));
        assert_eq!(plan(137, 5000, 3, 2909, Mode::RaiseEq), Action::SetRowValue(5000));
    }
}
