//! shop_repaint.rs — the pure half of per-shop-open FMG repainting (world #937 / clients#231).
//!
//! ## Why a repaint exists
//!
//! An FMG name belongs to a goods ROW, and the spare-row pool (~79) is smaller than a seed's shop
//! checks (up to ~500). The world now COLORS the pool (er-archipelago `shop_coloring.py`): slots
//! visible in the SAME menu never share a row, but slots in different regular-shop menus reuse rows
//! freely — because the client can rewrite a row's name to the OPEN menu's claimant at shop open.
//! One menu renders at a time, and the hover probe measured the buy menu re-looking names up every
//! frame, so a rewrite at the open edge is what the player sees. This module decides WHAT to write;
//! `fmg_inject::rewrite_in_place` is the arm that writes it.
//!
//! ## Why padded, in-place writes
//!
//! `extend_swap_overrides` REBUILDS a whole category block per call and leaks the old one by design
//! — GoodsCaption blocks are megabytes, so a rebuild per merchant greet is a leak the module header
//! explicitly forbids. Instead the BASELINE pass (shop_preview's once-per-arm extend-swap) pads
//! every override string with trailing NULs to a fixed capacity; a repaint then rewrites the text
//! inside its own slot, zero allocation, zero leak. Override strings each get a PRIVATE slot in the
//! rebuilt block (only vanilla strings are deduped — fmg_inject's build layout), so a padded slot
//! is exclusively ours to rewrite.
//!
//! 🛑 ASCII ONLY in anything that renders in game (er-toast-strings-are-ascii-only). Capacities are
//! UTF-16 code units; `pad_units` truncates on a code-unit boundary and drops a trailing lone high
//! surrogate rather than emit an unpaired one.

use crate::name_override::{shop_shared_label, ShopLabel};

/// UTF-16 code units reserved per GoodsName entry (terminator excluded). The longest shipped shop
/// name is an AP item name; 96 covers every name observed in scouts to date with headroom.
pub const PAD_NAME: usize = 96;
/// Units per GoodsInfo/GoodsCaption entry. The largest caption we emit is the shared-row
/// explainer (~350 units); 384 holds it with headroom without bloating the block (79 rows * 768 B).
pub const PAD_CAPTION: usize = 384;

/// Encode to UTF-16 and zero-pad to EXACTLY `cap` units. Longer input is truncated at `cap`; if the
/// cut would strand a high surrogate at the end, it is dropped too. The fixed length is the point:
/// every padded slot has `cap + 1` units of storage (fmg_inject writes the vec plus one terminator),
/// so a later in-place rewrite of at most `cap` units can never leave its slot.
pub fn pad_units(s: &str, cap: usize) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().take(cap).collect();
    if v.last()
        .is_some_and(|&last| (0xD800..0xDC00).contains(&last))
    {
        v.pop(); // an unpaired high surrogate renders as garbage; a clean cut does not
    }
    v.resize(cap, 0);
    v
}

/// Fold one OPEN MENU's claims — `(goods_row, label)` per AP slot on the visible shelf — into the
/// per-row overrides to write. The world's coloring makes a within-menu row collision impossible for
/// seeds generated after #937, but this client also connects to OLDER seeds (and to overflow rows,
/// where sharing is deliberate), so the fold keeps the honest degradation: one row claimed under two
/// DIFFERENT names gets the shared label, never a coin-flip between real names. Same-name claims are
/// not a collision (the funnyfail rule: labels are compared, not counted).
///
/// Order-stable: rows come out in first-claim order, so repeated repaints of one shelf write
/// byte-identical sequences.
pub fn per_shop_overrides(claims: &[(u32, ShopLabel)]) -> Vec<(u32, ShopLabel)> {
    let mut order: Vec<u32> = Vec::new();
    let mut folded: Vec<(u32, Vec<ShopLabel>)> = Vec::new();
    for (row, label) in claims {
        match folded.iter_mut().find(|(r, _)| r == row) {
            Some((_, ls)) => ls.push(label.clone()),
            None => {
                order.push(*row);
                folded.push((*row, vec![label.clone()]));
            }
        }
    }
    order
        .into_iter()
        .map(|row| {
            let ls = &folded
                .iter()
                .find(|(r, _)| *r == row)
                .expect("pushed above")
                .1;
            let mut names: Vec<&str> = ls.iter().map(|l| l.name.as_str()).collect();
            names.sort_unstable();
            names.dedup();
            let label = if names.len() > 1 {
                shop_shared_label(names.len())
            } else {
                ls[0].clone()
            };
            (row, label)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name_override::{shop_label, ItemKind};

    fn lbl(name: &str) -> ShopLabel {
        shop_label(
            name,
            "bob",
            "Hollow Knight",
            ItemKind::from_flags(true, false, false),
        )
    }

    #[test]
    fn pad_is_exactly_cap_and_zero_filled() {
        let v = pad_units("Grimmchild", 16);
        assert_eq!(v.len(), 16);
        assert_eq!(String::from_utf16_lossy(&v[..10]), "Grimmchild");
        assert!(v[10..].iter().all(|&u| u == 0));
    }

    #[test]
    fn pad_truncates_and_never_strands_a_high_surrogate() {
        let v = pad_units("abcdef", 3);
        assert_eq!(v, vec![b'a' as u16, b'b' as u16, b'c' as u16]);
        // "a" + U+1F600 (surrogate pair): cap 2 would cut between the pair — the high half must go.
        let v = pad_units("a\u{1F600}", 2);
        assert_eq!(v[0], b'a' as u16);
        assert_eq!(v[1], 0, "lone high surrogate dropped, slot zero-padded");
    }

    #[test]
    fn shared_caption_fits_the_caption_pad() {
        // THE SIZING FACT the constant encodes: the largest label we ever write must fit its pad,
        // or the in-place rewrite silently truncates the honest explainer.
        let shared = shop_shared_label(12);
        assert!(shared.caption.encode_utf16().count() <= PAD_CAPTION);
        assert!(shared.name.encode_utf16().count() <= PAD_NAME);
    }

    #[test]
    fn distinct_rows_pass_through_with_their_own_labels() {
        let out = per_shop_overrides(&[(9401, lbl("Grimmchild")), (9402, lbl("Mothwing Cloak"))]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, 9401);
        assert_eq!(out[0].1.name, "Grimmchild");
        assert_eq!(out[1].1.name, "Mothwing Cloak");
    }

    #[test]
    fn a_row_claimed_under_two_names_gets_the_shared_label_not_a_coinflip() {
        let out = per_shop_overrides(&[(9401, lbl("Grimmchild")), (9401, lbl("Mothwing Cloak"))]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1.name, "Archipelago Items");
        assert!(out[0].1.caption.contains("2 DIFFERENT items"));
    }

    #[test]
    fn same_name_twice_on_one_row_keeps_the_specific_name() {
        // labels are compared, not counted (funnyfail rule)
        let out = per_shop_overrides(&[(9401, lbl("Grimmchild")), (9401, lbl("Grimmchild"))]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1.name, "Grimmchild");
    }

    #[test]
    fn output_order_is_first_claim_order() {
        let out = per_shop_overrides(&[
            (9500, lbl("C")),
            (9400, lbl("A")),
            (9500, lbl("C")),
            (9450, lbl("B")),
        ]);
        let rows: Vec<u32> = out.iter().map(|(r, _)| *r).collect();
        assert_eq!(rows, vec![9500, 9400, 9450]);
    }
}
