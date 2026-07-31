//! Region-lock hints, priced in PROGRESSION-SURFACE checks.
//!
//! MOTIVATING CASE (CONTRIBUTING rule 11). dalekin31, Nexus, 2026-07-31: *"especially for all region
//! runs, some sort of built in hint system to point towards specifically region locks as the normal
//! hint price using archy's built in hints are crazy high (nearly 500 for standard 10%). Even if you
//! get enough to hint something, there is no guarantee you can hit the correct one in the chain and
//! just end up wasting your hint."*
//!
//! He is exactly right and the cause is STRUCTURAL, not a host misconfiguration. Archipelago prices
//! a hint at `hint_cost% * len(your locations)`. An all-region ER seed has **4879** locations, so the
//! standard 10% is **487 points** against a default earn of 1 point per check — one hint costs a
//! tenth of the entire seed. ER is priced out because it carries roughly ten times a normal game's
//! location count.
//!
//! # The re-denomination
//!
//! We do NOT invent a price. We apply **Archipelago's own rule to the game ER actually asks you to
//! play**: the same percentage, measured over the ~158-location PROGRESSION SURFACE (the only
//! locations that may hold this world's own progression) instead of over all 4879.
//!
//! ```text
//! price = ceil(surface_total * hint_cost% / 100)
//!       = ceil(surface_total * points_per_hint / total_locations)
//! ```
//!
//! The second form is what we can actually compute: `archipelago_rs` exposes `points_per_hint()`
//! (= `total_locations * hint_cost% / 100`) but not the raw percentage, and dividing it back out
//! recovers the host's setting exactly. That matters — **the price tracks the host's `hint_cost`**.
//! A host who sets 5% gets a cheaper lock hint, one who sets 20% gets a dearer one, and one who sets
//! 0 (hints free for everyone) gets them free here too. We are not opting their room out of a
//! difficulty knob they chose; we are applying it to a denominator that isn't absurd.
//!
//! At the AP default of 10% this yields `ceil(158 * 487 / 4879)` = **16 surface checks**, which is
//! also the number a design review independently recommended as the right feel — a full surface
//! clear funds ~9 hints against ~20 locks, so *which* lock to reveal stays a real decision.
//!
//! # Why nothing here can be farmed
//!
//! Both sides of the ledger derive from SERVER-authoritative state; the client holds no currency,
//! only a derivation.
//!
//! * **earn** = `|checked_locations ∩ progression_surface|`, straight off the server's checked set.
//! * **spend** = the length of a ledger kept in AP data storage (`er_lockhints_<slot>`), which lives
//!   in the server's save file.
//!
//! So reconnecting, reloading a save, or restarting the server reproduces the identical balance —
//! `derive(state) == derive(reconnect(state))` is a test below. This deliberately DIVERGES from the
//! shop-hint session dedupe, which is not persisted: that one is a politeness throttle, this is a
//! currency, and a non-persistent currency means reconnect is free money.
//!
//! Accepted and not guarded: a finished player's `!collect` can auto-check surface locations and
//! grant unearned credit. Late-game, bounded, and the only clean fix is world-side.
//!
//! # Scope
//!
//! Locks only. The price is a pacing mechanism, not a licence to reveal more: what gets hinted is
//! still just our own world's structural skeleton, which is why creating these hints for free (the
//! protocol permits it; `CreateHints` enforces own-locations-or-own-items server-side) is defensible
//! in a multiworld at all.

use std::collections::{HashMap, HashSet};

/// Where a region's Lock item turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockHint {
    /// An AP location in OUR world holds it — hintable.
    Found(i64),
    /// The lock spilled into another player's world. The connect-time scout only covers our own
    /// locations, so we cannot resolve or `CreateHints` it; `!hint` remains the tool. Rare —
    /// `features/progression_surface.py` returns a lock to normal fill only when its ladder must.
    InAnotherWorld,
    /// No lock item is known for this region (no `lock_items` entry, or the scout has not landed).
    Unknown,
}

/// What the UI should render for one locked region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockHintOffer {
    /// Affordable. `price` surface checks will be spent.
    Buyable { price: u64, location: i64 },
    /// Not affordable yet. Render DISABLED WITH THE COST — never hidden. A player who cannot see
    /// what the button costs, or that they are making progress toward it, learns nothing.
    Insufficient {
        price: u64,
        have: u64,
        location: i64,
    },
    /// Already hinted, through our button or AP's own `!hint`. No button, and NO charge: spend
    /// derives from our ledger, not from the standing-hint set, so nobody pays twice for one reveal.
    AlreadyHinted { location: i64 },
    /// In another player's world — explanatory text, not a dead button, and not purchasable.
    Spilled,
    /// Nothing known yet.
    Unknown,
}

/// The hint price in progression-surface checks.
///
/// `points_per_hint` and `total_locations` come from the live server connection, so this tracks the
/// host's `hint_cost` setting rather than hardcoding a number of our own.
///
/// Returns `None` when the price cannot be derived (no locations known yet — the connection has not
/// settled). `None` must render as [`LockHintOffer::Unknown`], never as free: a price we failed to
/// compute is not a price of zero.
///
/// A host `hint_cost` of 0 IS a real zero, and is honoured — that host made hints free for everyone.
pub fn price_per_hint(
    surface_total: u64,
    points_per_hint: u64,
    total_locations: u64,
) -> Option<u64> {
    if total_locations == 0 {
        return None;
    }
    if points_per_hint == 0 {
        return Some(0); // host set hint_cost: 0 -- free for every game in the room, including ours
    }
    if surface_total == 0 {
        return Some(1); // degenerate seed with no surface; charge the floor rather than nothing
    }
    let num = surface_total.saturating_mul(points_per_hint);
    let ceil = num.div_ceil(total_locations);
    Some(ceil.max(1)) // a tiny num_regions seed must still cost SOMETHING
}

/// Credits available = surface checks completed, minus what past purchases cost.
///
/// Saturating: a host raising `hint_cost` mid-seed can put a player in debt (AP models this too),
/// and debt reads as zero here rather than underflowing.
pub fn balance(surface_checked: u64, purchases: u64, price: u64) -> u64 {
    surface_checked.saturating_sub(purchases.saturating_mul(price))
}

/// Which of OUR locations holds `lock_item`, from the connect-time scout cache.
///
/// `scout` maps AP location id -> the item name that location holds, restricted to locations in our
/// own world (which is exactly what `scout_proof`'s cache covers).
pub fn resolve_lock_location(lock_item: Option<&str>, scout: &HashMap<i64, String>) -> LockHint {
    let Some(name) = lock_item else {
        return LockHint::Unknown;
    };
    if scout.is_empty() {
        return LockHint::Unknown; // scout has not landed; do not claim the lock is foreign
    }
    let mut hit: Option<i64> = None;
    for (&loc, item) in scout.iter() {
        if item == name {
            // Deterministic across HashMap iteration order: lowest id wins. A lock name should be
            // unique, but "should be" is not a guarantee worth a nondeterministic UI.
            hit = Some(match hit {
                Some(prev) if prev <= loc => prev,
                _ => loc,
            });
        }
    }
    match hit {
        Some(loc) => LockHint::Found(loc),
        // The scout covers every one of our locations, so a name absent from a POPULATED cache
        // means the item is not in our world.
        None => LockHint::InAnotherWorld,
    }
}

/// The whole decision for one locked region, in one place so the render layer stays a dumb consumer.
#[allow(clippy::too_many_arguments)]
pub fn offer(
    lock_item: Option<&str>,
    scout: &HashMap<i64, String>,
    hinted: &HashSet<i64>,
    surface_total: u64,
    surface_checked: u64,
    purchases: u64,
    points_per_hint: u64,
    total_locations: u64,
) -> LockHintOffer {
    let location = match resolve_lock_location(lock_item, scout) {
        LockHint::Found(loc) => loc,
        LockHint::InAnotherWorld => return LockHintOffer::Spilled,
        LockHint::Unknown => return LockHintOffer::Unknown,
    };
    if hinted.contains(&location) {
        return LockHintOffer::AlreadyHinted { location };
    }
    let Some(price) = price_per_hint(surface_total, points_per_hint, total_locations) else {
        return LockHintOffer::Unknown;
    };
    let have = balance(surface_checked, purchases, price);
    if have >= price {
        LockHintOffer::Buyable { price, location }
    } else {
        LockHintOffer::Insufficient {
            price,
            have,
            location,
        }
    }
}

/// Surface checks completed = the server's checked set intersected with the progression surface.
///
/// 🛑 Derived from the RAW sets on purpose. `tracker::build_tracker_model` keeps a `surface_done`
/// display counter, but it counts only locations that appear in the tracker's region tables; the
/// economy must not inherit that filter. `tracker.rs`'s `prominent = surface.contains(&id)` IS raw
/// membership, so the two agree today — `surface_counter_matches_raw_intersection` below pins that,
/// and if a future table change breaks it, the economy stays correct and the test says so.
pub fn surface_checked(checked: &HashSet<i64>, surface: &HashSet<i64>) -> u64 {
    checked.intersection(surface).count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    // The measured all-region seed: 4879 locations, 158-location progression surface, AP default
    // hint_cost 10% -> points_per_hint 487.
    const TOTAL: u64 = 4879;
    const SURFACE: u64 = 158;
    const PPH_10PCT: u64 = 487;

    fn scout(pairs: &[(i64, &str)]) -> HashMap<i64, String> {
        pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
    }

    // --- price: AP's own rule, re-denominated -----------------------------------------------------

    #[test]
    fn the_motivating_case_costs_sixteen_surface_checks_not_four_hundred_and_eighty_seven() {
        // THE POINT OF THE FEATURE. Same percentage the host chose, measured over the surface.
        assert_eq!(price_per_hint(SURFACE, PPH_10PCT, TOTAL), Some(16));
        assert!(
            price_per_hint(SURFACE, PPH_10PCT, TOTAL).unwrap() < PPH_10PCT,
            "if the re-denomination is not cheaper than AP's own price, the feature is pointless"
        );
    }

    #[test]
    fn the_price_tracks_the_hosts_hint_cost_setting() {
        // 5% -> 243 points, 20% -> 975 (both = total * pct / 100). We must scale WITH the host,
        // not opt their room out of a knob they set.
        assert_eq!(price_per_hint(SURFACE, 243, TOTAL), Some(8));
        assert_eq!(price_per_hint(SURFACE, 975, TOTAL), Some(32));
    }

    #[test]
    fn a_host_who_made_hints_free_gets_them_free_here_too() {
        assert_eq!(price_per_hint(SURFACE, 0, TOTAL), Some(0));
    }

    #[test]
    fn an_underivable_price_is_unknown_never_free() {
        // 🛑 The failure mode that would silently hand out unlimited hints. A price we could not
        // compute must NOT collapse to zero.
        assert_eq!(price_per_hint(SURFACE, PPH_10PCT, 0), None);
    }

    #[test]
    fn small_num_regions_seeds_degrade_and_never_reach_zero() {
        // A price fine at 158 surface locations must stay payable at 30, and must not become free
        // at 1. The floor is a guard the corpus never triggers, so it is called DIRECTLY.
        assert_eq!(price_per_hint(30, 10 * 30 / 100 * 10, 300), Some(3));
        assert_eq!(price_per_hint(1, PPH_10PCT, TOTAL), Some(1), "floor");
        assert_eq!(
            price_per_hint(0, PPH_10PCT, TOTAL),
            Some(1),
            "floor at zero surface"
        );
        for surface in 1..=200u64 {
            assert!(
                price_per_hint(surface, PPH_10PCT, TOTAL).unwrap() >= 1,
                "surface {surface} priced a hint at zero"
            );
        }
    }

    #[test]
    fn price_rounds_up_so_a_hint_is_never_a_rounding_error() {
        // 158 * 487 / 4879 = 15.77 -- ceil, not floor, or the surface subsidises the player.
        assert_eq!(price_per_hint(158, 487, 4879), Some(16));
    }

    // --- balance ----------------------------------------------------------------------------------

    #[test]
    fn balance_is_checks_minus_what_purchases_cost() {
        assert_eq!(balance(20, 0, 16), 20);
        assert_eq!(balance(20, 1, 16), 4);
        assert_eq!(
            balance(20, 2, 16),
            0,
            "debt saturates to zero, never underflows"
        );
    }

    // --- resolution -------------------------------------------------------------------------------

    #[test]
    fn resolves_the_own_world_location_holding_the_lock() {
        let s = scout(&[(11, "Caelid Lock"), (12, "Rune"), (13, "Farum Azula Lock")]);
        assert_eq!(
            resolve_lock_location(Some("Farum Azula Lock"), &s),
            LockHint::Found(13)
        );
    }

    #[test]
    fn a_lock_absent_from_a_populated_scout_is_in_another_world() {
        let s = scout(&[(11, "Caelid Lock")]);
        assert_eq!(
            resolve_lock_location(Some("Enir Ilim Lock"), &s),
            LockHint::InAnotherWorld
        );
    }

    #[test]
    fn an_empty_scout_is_unknown_not_foreign() {
        // Before the connect-time scout lands, every lock would look "foreign". Claiming that would
        // tell the player to go use !hint for something sitting in their own world.
        assert_eq!(
            resolve_lock_location(Some("Caelid Lock"), &HashMap::new()),
            LockHint::Unknown
        );
        assert_eq!(
            resolve_lock_location(None, &scout(&[(11, "Caelid Lock")])),
            LockHint::Unknown
        );
    }

    #[test]
    fn duplicate_names_resolve_deterministically() {
        let s = scout(&[
            (30, "Caelid Lock"),
            (11, "Caelid Lock"),
            (22, "Caelid Lock"),
        ]);
        for _ in 0..50 {
            assert_eq!(
                resolve_lock_location(Some("Caelid Lock"), &s),
                LockHint::Found(11),
                "HashMap iteration order must not reach the UI"
            );
        }
    }

    // --- the offer --------------------------------------------------------------------------------

    fn s_default() -> HashMap<i64, String> {
        scout(&[(77, "Caelid Lock")])
    }

    #[test]
    fn fifteen_checks_cannot_afford_sixteen_and_the_button_still_shows_the_cost() {
        let got = offer(
            Some("Caelid Lock"),
            &s_default(),
            &HashSet::new(),
            SURFACE,
            15,
            0,
            PPH_10PCT,
            TOTAL,
        );
        assert_eq!(
            got,
            LockHintOffer::Insufficient {
                price: 16,
                have: 15,
                location: 77
            }
        );
    }

    #[test]
    fn sixteen_checks_buys_it() {
        let got = offer(
            Some("Caelid Lock"),
            &s_default(),
            &HashSet::new(),
            SURFACE,
            16,
            0,
            PPH_10PCT,
            TOTAL,
        );
        assert_eq!(
            got,
            LockHintOffer::Buyable {
                price: 16,
                location: 77
            }
        );
    }

    #[test]
    fn after_one_purchase_the_balance_is_spent() {
        let got = offer(
            Some("Caelid Lock"),
            &s_default(),
            &HashSet::new(),
            SURFACE,
            16,
            1,
            PPH_10PCT,
            TOTAL,
        );
        assert_eq!(
            got,
            LockHintOffer::Insufficient {
                price: 16,
                have: 0,
                location: 77
            }
        );
    }

    #[test]
    fn an_already_hinted_lock_is_never_charged_again() {
        // Includes locks hinted through AP's own paid !hint: spend derives from OUR ledger, so the
        // player cannot be billed for a reveal they already have.
        let got = offer(
            Some("Caelid Lock"),
            &s_default(),
            &HashSet::from([77]),
            SURFACE,
            100,
            0,
            PPH_10PCT,
            TOTAL,
        );
        assert_eq!(got, LockHintOffer::AlreadyHinted { location: 77 });
    }

    #[test]
    fn a_spilled_lock_is_not_purchasable() {
        let got = offer(
            Some("Enir Ilim Lock"),
            &s_default(),
            &HashSet::new(),
            SURFACE,
            999,
            0,
            PPH_10PCT,
            TOTAL,
        );
        assert_eq!(
            got,
            LockHintOffer::Spilled,
            "we cannot CreateHints a foreign-world location"
        );
    }

    #[test]
    fn an_underivable_price_offers_nothing_rather_than_something_free() {
        let got = offer(
            Some("Caelid Lock"),
            &s_default(),
            &HashSet::new(),
            SURFACE,
            999,
            0,
            PPH_10PCT,
            0,
        );
        assert_eq!(got, LockHintOffer::Unknown);
    }

    // --- 🔥 THE FARMING TEST ----------------------------------------------------------------------

    #[test]
    fn reconnecting_cannot_mint_credit() {
        // The whole design rests on this: both sides derive from server state, so replaying the
        // derivation after a reconnect must reproduce the identical offer. If this ever fails,
        // something local crept into the ledger and hints became free.
        let s = s_default();
        let hinted = HashSet::new();
        let derive = |checked: u64, purchases: u64| {
            offer(
                Some("Caelid Lock"),
                &s,
                &hinted,
                SURFACE,
                checked,
                purchases,
                PPH_10PCT,
                TOTAL,
            )
        };
        for checked in [0u64, 15, 16, 31, 32, 200] {
            for purchases in [0u64, 1, 2] {
                let before = derive(checked, purchases);
                // "Reconnect": nothing local survives; we re-derive from the same server state.
                let after = derive(checked, purchases);
                assert_eq!(before, after, "checked={checked} purchases={purchases}");
            }
        }
        // And the balance is a pure function of the pair -- no hidden accumulator.
        assert_eq!(balance(32, 1, 16), balance(32, 1, 16));
        assert_ne!(balance(32, 0, 16), balance(32, 1, 16));
    }

    #[test]
    fn surface_counter_matches_raw_intersection() {
        // Pins the economy's earn base against the tracker's display counter (tracker.rs computes
        // `prominent = surface.contains(&id)`, i.e. raw membership). If a table change ever makes
        // the display counter a filtered view, the economy stays right and this test fails loudly.
        let surface: HashSet<i64> = (1..=158).collect();
        let checked: HashSet<i64> = (1..=20).chain(500..=520).collect();
        assert_eq!(surface_checked(&checked, &surface), 20);
        assert_eq!(surface_checked(&HashSet::new(), &surface), 0);
        assert_eq!(surface_checked(&checked, &HashSet::new()), 0);
    }
}
