//! `merchant_bells` -- talk to a merchant, and their Bell Bearing is already at the Twin Maidens
//! (er-archipelago#325).
//!
//! # The request, and what it actually means
//!
//! boblerrr, Nexus, 2026-08-03: *"Add an option to receive bell bearings directly from merchants on
//! first interaction -- Matt's Randomizer has a similar setting for this."* Taken literally that
//! would put a vanilla Bell Bearing in the bag -- but every one of those bells is ALSO an
//! Archipelago item in this seed's pool (`Nomadic Merchant's Bell Bearing [1]` is ap id 7001065),
//! so granting one would hand the player a second copy of an item the multiworld is tracking.
//!
//! What the player wants from that item is the SHOP, so this option delivers the shop and nothing
//! else: opening a merchant's buy menu sets the flag the Twin Maiden Husks would have set had you
//! handed them that merchant's bell. The pool is untouched, and the bell itself remains a real AP
//! item worth finding -- it just arrives already spent.
//!
//! # 🛑 Why one flag write is the whole mechanism
//!
//! `features/shops.py` recorded for months that the bell -> shop join "is NOT derivable matt-free
//! from disk", because the bell flags appear nowhere in `ShopLineupParam`. That is true and it is
//! not the join. `tools/datamine_bell_handins.py` found it in the Twin Maidens' own talk ESD:
//!
//! * handing a bell over runs `SetEventFlag(11109710 + n)`;
//! * that flag makes a menu ENTRY appear;
//! * the entry calls `OpenRegularShop(begin, end)` on **the merchant's own block**, not a copy.
//!
//! Kale's talk opens `100500..100524`; the Maidens' "Kale's Bell Bearing" entry opens
//! `100500..100524`. So nothing is released, nothing is duplicated, and the AP check on a row fires
//! identically whichever counter you bought it at -- it is the same row with the same
//! `eventFlag_forStock`. Writing the flag is therefore not a simulation of the hand-in; it IS the
//! hand-in, minus the item.
//!
//! # It cannot double-fire, and that is structural
//!
//! The decision reads the live flag. Set already => `AlreadyHandedIn`, no write, no toast. So a
//! player who found the bell the honest way, handed it in, and later walks past the merchant sees
//! nothing, and a merchant whose menu is opened forty times is written once.
//!
//! ⚠️ ONE CONSEQUENCE, and it is worth saying out loud rather than discovering in a playtest: once
//! the flag is set, the Maidens' "Offer a bell bearing" list will not show that bell again (its
//! condition is *have the item AND flag clear*). A copy of the bell that arrives from the
//! multiworld afterwards is therefore inert -- correctly, since its effect is already yours, but it
//! does sit in the bag looking unused.
//!
//! # What it does not cover
//!
//! Twelve bells work the other way round -- they release rows inside the Maidens' own block via
//! `eventFlag_forRelease` and have no menu entry and no shop range (the four peddlers and most of
//! the DLC sellers). They are absent from the table on purpose; see
//! `tools/datamine_bell_handins.py` for why inferring their merchant is worse than omitting them.
//! And the trigger is the regular buy menu only, so a vendor reached through Ash of War, tailoring,
//! upgrading or change-of-purpose does not fire -- those commands' ids are still unobserved.

use crate::merchant_bell_table::bell_for_range;

/// What a shop open means for this feature. Every arm is logged by the caller: a feature that can
/// decline silently is indistinguishable from one that is broken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The seed did not turn the option on.
    Disabled,
    /// This range belongs to no bell-bearing merchant (the Twin Maidens' own shelf, a DLC peddler,
    /// or a range the table has never seen).
    NoBell,
    /// The flag is already set -- handed in for real, or by an earlier open this session.
    AlreadyHandedIn { flag: u32, name: &'static str },
    /// Write `flag` and tell the player.
    HandIn { flag: u32, name: &'static str },
}

/// Decide what a buy-menu open over `ShopLineupParam` rows `[begin, end]` should do.
///
/// Pure: `is_set` is the caller's live event-flag read, which is what keeps every rule here
/// testable on any host. `enabled` is the seed's `options.merchant_bells_on_talk`.
pub fn plan_hand_in(begin: i32, end: i32, enabled: bool, is_set: impl Fn(u32) -> bool) -> Outcome {
    if !enabled {
        return Outcome::Disabled;
    }
    let Some((flag, name)) = bell_for_range(begin, end) else {
        return Outcome::NoBell;
    };
    if is_set(flag) {
        Outcome::AlreadyHandedIn { flag, name }
    } else {
        Outcome::HandIn { flag, name }
    }
}

/// The player-facing notice. ASCII only -- it is drawn by the game's own font, which has no glyph
/// for anything else (`merchant_bell_table` refuses a non-ASCII name at generation time).
pub fn toast_text(name: &str) -> String {
    format!("{name} delivered to the Twin Maiden Husks")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merchant_bell_table::MERCHANT_BELLS;

    const KALE: (i32, i32, u32) = (100_500, 100_524, 11_109_720);

    #[test]
    fn an_off_seed_never_looks_anything_up() {
        assert_eq!(
            plan_hand_in(KALE.0, KALE.1, false, |_| panic!("must not read a flag")),
            Outcome::Disabled
        );
    }

    #[test]
    fn opening_a_bell_merchant_hands_the_bell_in() {
        assert_eq!(
            plan_hand_in(KALE.0, KALE.1, true, |_| false),
            Outcome::HandIn {
                flag: KALE.2,
                name: "Kale's Bell Bearing"
            }
        );
    }

    /// The motivating case for the idempotence rule: a player who handed the bell in for real
    /// walks back past the merchant. Nothing is written and nothing is announced.
    #[test]
    fn a_bell_already_handed_in_is_not_re_announced() {
        assert_eq!(
            plan_hand_in(KALE.0, KALE.1, true, |f| f == KALE.2),
            Outcome::AlreadyHandedIn {
                flag: KALE.2,
                name: "Kale's Bell Bearing"
            }
        );
    }

    /// 🛑 The one range the 2026-08-08 probe actually observed is the Twin Maidens' own buy menu.
    /// It must resolve to nothing, or standing at the hub would hand in whatever it collided with.
    #[test]
    fn the_twin_maidens_own_shelf_hands_nothing_in() {
        assert_eq!(
            plan_hand_in(101_800, 101_897, true, |_| false),
            Outcome::NoBell
        );
    }

    #[test]
    fn every_merchant_in_the_table_plans_its_own_bell() {
        for (lo, hi, flag, name) in MERCHANT_BELLS {
            assert_eq!(
                plan_hand_in(lo, hi, true, |_| false),
                Outcome::HandIn { flag, name },
                "range {lo}..{hi}"
            );
        }
    }

    #[test]
    fn the_notice_is_ascii_and_names_the_bell() {
        let t = toast_text("Kale's Bell Bearing");
        assert!(t.is_ascii());
        assert!(t.contains("Kale's Bell Bearing"));
    }
}
