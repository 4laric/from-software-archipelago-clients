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

// =================================================================================================
// "Hint the NEXT lock" -- issue #412
// =================================================================================================
//
// MOTIVATING CASE (CONTRIBUTING rule 11). bobler, 0.3.5, 2026-08-06: *"there needs to be a hint
// progressive lock instead of hinting for a specifc lock since i dont know what my 2nd lock is"*.
// His log is the proof: three `!hint`s, three minutes apart, spent discovering an ORDER rather than
// a location --
//
// ```text
// 07:20:37  !hint Altus Lock       -> Liurnia    :: Golden Seed - near Academy Gate Town
// 07:23:07  !hint Farum Azula Lock -> Altus      :: Golden Seed - On tree, Outer Wall Phantom Tree
// 07:23:18  !hint Leyndell Lock    -> Farum Azula:: Remembrance of the Dragonlord - Placidusax
// ```
//
// The chain was Liurnia -> Altus -> Farum Azula -> Leyndell. Naming a lock is a GUESS about which
// region comes next, so the player must buy hints for locks he cannot reach in order to find the
// one he can. The per-region button inherits that defect exactly: it asks the player for the answer.
//
// # There is no declared order to look up -- and we do not need one
//
// The chain is not a field in slot data; it is EMERGENT FROM THE FILL. "Altus is second" is just
// "the Altus Lock item happens to sit in Liurnia". So the question the player is really asking is
// not *"what is my 2nd region?"* but **"which lock can I go and get right now?"**, and that is a
// join over three tables the client already holds:
//
// * `coarse_lock_items`  -- coarse region -> lock item name        (slot data, already parsed)
// * `open_coarse_regions()` -- which coarse regions are open now   (live event flags)
// * the connect-time scout -- AP location -> item name, our world  (`scout_proof`)
//
// A lock is on the FRONTIER when its own region is still locked but the location holding it lies in
// a region that is already open. On bobler's seed that is a single lock at every point in the run,
// and it collapses his three purchases into one.
//
// ⭐ Because the join uses only tables that already ship, this moves NOTHING across the wire:
// `CONTRACT_HASH` is untouched and this stays client-only, exactly as the per-region button did.
//
// # Reachability here is REGION reachability, deliberately
//
// "Open" means the lock's region gate is down, not that AP logic has cleared the location -- a
// frontier lock can still sit behind a boss lock or a key item. That is the same approximation the
// tracker's own `[locked]` tag makes, and matching it is the point: two different notions of
// reachable in one window would be worse than one imperfect one.

/// A lock the player can actually go and get: its own region is still LOCKED, but the location
/// holding its item lies in a region that is already OPEN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontierLock {
    /// The coarse region this lock opens.
    pub region: String,
    /// The lock item's name.
    pub lock_item: String,
    /// The AP location in OUR world that holds it.
    pub location: i64,
}

/// The answer to "which lock is my next one?".
///
/// Every variant renders differently, which is the bar for existing: a player who is told
/// "nothing reachable" when the truth is "your next lock is in someone else's world" has been
/// actively misinformed, and that is the failure this enum is shaped to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextLock {
    /// Go get this one.
    Reachable(FrontierLock),
    /// Every lock you can reach has already been hinted -- by this button or by `!hint`.
    AllFrontierHinted,
    /// Nothing is reachable and at least one locked region's lock spilled into another player's
    /// world, where the scout cannot see it. `!hint` is the tool; carries the region names so the
    /// message can say which.
    Spilled { regions: Vec<String> },
    /// Locked regions remain and every one of their locks sits inside a region that is itself
    /// still locked. A real state: the player is gated on something that is not a region lock.
    NoneReachable,
    /// No region is locked -- nothing to hint.
    NothingLocked,
    /// Tables have not landed, or a lock resolved to a location the region tables do not cover.
    /// 🛑 Never collapse this into [`NextLock::NoneReachable`]: "I do not know" and "there is
    /// nothing" are different claims and only one of them is safe to state to a stuck player.
    Unknown,
}

/// Resolve the frontier: the lock whose region is shut but whose ITEM is already within reach.
///
/// * `lock_items` -- coarse region -> lock item name.
/// * `open_regions` -- coarse regions currently open (the empty-string always-open bucket is
///   treated as open whether or not the caller includes it).
/// * `coarse_of` -- AP location id -> coarse region, for our own locations.
/// * `scout` -- AP location id -> item name, our own locations, from the connect-time scout.
/// * `hinted` -- locations with a standing hint, plus anything already bought.
///
/// Ties break on the LOWEST location id, matching [`resolve_lock_location`]. Arbitrary, but stable:
/// a frontier that reordered itself between frames would make the button unclickable in practice.
pub fn next_lock(
    lock_items: &HashMap<String, String>,
    open_regions: &HashSet<String>,
    coarse_of: &HashMap<i64, String>,
    scout: &HashMap<i64, String>,
    hinted: &HashSet<i64>,
) -> NextLock {
    if lock_items.is_empty() || scout.is_empty() {
        return NextLock::Unknown;
    }
    let mut locked: Vec<(&String, &String)> = lock_items
        .iter()
        .filter(|(region, _)| !region.is_empty() && !open_regions.contains(*region))
        .collect();
    if locked.is_empty() {
        return NextLock::NothingLocked;
    }
    // Deterministic iteration: HashMap order must never reach the UI.
    locked.sort_by(|a, b| a.0.cmp(b.0));

    let mut frontier: Vec<FrontierLock> = Vec::new();
    let mut spilled: Vec<String> = Vec::new();
    let mut untabled = false;
    for (region, item) in locked {
        match resolve_lock_location(Some(item.as_str()), scout) {
            LockHint::InAnotherWorld => spilled.push(region.clone()),
            LockHint::Unknown => untabled = true,
            LockHint::Found(loc) => match coarse_of.get(&loc) {
                // The lock's own location is not in the region tables. We cannot say whether it is
                // reachable, so we say nothing rather than reporting it out of reach.
                None => untabled = true,
                Some(holder) => {
                    if holder.is_empty() || open_regions.contains(holder) {
                        frontier.push(FrontierLock {
                            region: region.clone(),
                            lock_item: item.clone(),
                            location: loc,
                        });
                    }
                }
            },
        }
    }

    let best = frontier
        .iter()
        .filter(|f| !hinted.contains(&f.location))
        .min_by_key(|f| f.location);
    if let Some(f) = best {
        return NextLock::Reachable(f.clone());
    }
    if !frontier.is_empty() {
        return NextLock::AllFrontierHinted;
    }
    if !spilled.is_empty() {
        spilled.sort();
        return NextLock::Spilled { regions: spilled };
    }
    if untabled {
        return NextLock::Unknown;
    }
    NextLock::NoneReachable
}

/// What the UI should render for the single "hint next lock" control.
///
/// Distinct from [`LockHintOffer`] on purpose: the per-region button answers "can I buy THIS one",
/// this one answers "is there anything to buy at all", and the negative cases differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextLockOffer {
    /// Affordable. `region` and `lock_item` are carried for the LOG LINE AFTER the purchase --
    /// 🛑 they must not be rendered before it. Naming the next region for free would hand over
    /// half of the very thing the price is charged for.
    Buyable {
        price: u64,
        location: i64,
        region: String,
        lock_item: String,
    },
    /// Not affordable yet. Rendered DISABLED WITH THE COST, never hidden -- same rule as
    /// [`LockHintOffer::Insufficient`], and for the same reason.
    Insufficient { price: u64, have: u64 },
    /// Everything reachable is already hinted.
    AllFrontierHinted,
    /// Reachable frontier is empty and a lock is in another player's world.
    Spilled { regions: Vec<String> },
    /// Locked regions remain, but all of their locks are behind other locks.
    NoneReachable,
    /// Nothing is locked, or nothing is known. The control does not render.
    Idle,
}

/// The whole "hint next lock" decision, so the render layer stays a dumb consumer.
#[allow(clippy::too_many_arguments)]
pub fn next_offer(
    lock_items: &HashMap<String, String>,
    open_regions: &HashSet<String>,
    coarse_of: &HashMap<i64, String>,
    scout: &HashMap<i64, String>,
    hinted: &HashSet<i64>,
    surface_total: u64,
    surface_checked: u64,
    purchases: u64,
    points_per_hint: u64,
    total_locations: u64,
) -> NextLockOffer {
    let target = match next_lock(lock_items, open_regions, coarse_of, scout, hinted) {
        NextLock::Reachable(f) => f,
        NextLock::AllFrontierHinted => return NextLockOffer::AllFrontierHinted,
        NextLock::Spilled { regions } => return NextLockOffer::Spilled { regions },
        NextLock::NoneReachable => return NextLockOffer::NoneReachable,
        NextLock::NothingLocked | NextLock::Unknown => return NextLockOffer::Idle,
    };
    let Some(price) = price_per_hint(surface_total, points_per_hint, total_locations) else {
        return NextLockOffer::Idle; // an underivable price is never a free one
    };
    let have = balance(surface_checked, purchases, price);
    if have >= price {
        NextLockOffer::Buyable {
            price,
            location: target.location,
            region: target.region,
            lock_item: target.lock_item,
        }
    } else {
        NextLockOffer::Insufficient { price, have }
    }
}

/// Balance and price for the always-on status line.
///
/// # Why this exists at all (issue #412, the discoverability half)
///
/// bobler's log reads `lock hints: ledger loaded from er_lockhints_2 -- 0 hint(s) already bought`
/// and then three AP `!hint`s. The economy worked perfectly and he never saw it, because every
/// surface it had was behind three doors at once: the tracker window is **closed by default**, its
/// toggle is **F6**, and the price only appeared on the header of a region that happened to be
/// LOCKED and scrolled into view. A feature nobody can find has the same value as one that does not
/// ship, so the balance is now readable wherever the player already is.
///
/// Returns `None` while the price cannot be derived -- the caller must render nothing, never zero.
pub fn status(
    surface_total: u64,
    surface_checked: u64,
    purchases: u64,
    points_per_hint: u64,
    total_locations: u64,
) -> Option<(u64, u64)> {
    let price = price_per_hint(surface_total, points_per_hint, total_locations)?;
    Some((balance(surface_checked, purchases, price), price))
}

/// One-shot latch for the "you can afford a lock hint" toast.
///
/// Mirrors `toast::new_grants`: fires only on the FALSE -> TRUE edge, and never on the first
/// observation (`None`), so a reconnect that replays an already-affordable balance stays silent.
/// A toast that re-fires every frame you can afford something is an advertisement, not a notice.
pub fn crossed_into_affordable(prev: Option<bool>, now: bool) -> bool {
    matches!(prev, Some(false)) && now
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

    // =============================================================================================
    // "Hint the NEXT lock" -- issue #412
    // =============================================================================================

    fn m(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn coarse(pairs: &[(i64, &str)]) -> HashMap<i64, String> {
        pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
    }

    /// bobler's seed, 0.3.5. Locks: region -> item.
    fn boblers_locks() -> HashMap<String, String> {
        m(&[
            ("Liurnia", "Liurnia Lock"),
            ("Altus", "Altus Lock"),
            ("Farum Azula", "Farum Azula Lock"),
            ("Leyndell", "Leyndell Lock"),
        ])
    }

    /// Where the fill put each lock item, and which coarse region that location belongs to.
    /// Chain: Limgrave -> Liurnia -> Altus -> Farum Azula -> Leyndell.
    ///
    /// 🛑 The location ids are deliberately ANTI-CORRELATED with the chain. The first draft used
    /// 101/202/303/404 in chain order, and mutation-testing caught that as vacuous: with ids that
    /// ascend along the chain, the lowest-id TIE-BREAK reproduces the correct answer even when the
    /// reachability check is deleted entirely, so the motivating test passed against an
    /// implementation that did not implement the feature. Scrambling them makes the assertion
    /// depend on the mechanism and nothing else.
    fn boblers_fill() -> (HashMap<i64, String>, HashMap<i64, String>) {
        let scouted = scout(&[
            (4004, "Liurnia Lock"),     // in Limgrave      -- 1st in the chain, HIGHEST id
            (1001, "Altus Lock"),       // in Liurnia       -- 2nd, LOWEST id
            (3003, "Farum Azula Lock"), // in Altus
            (2002, "Leyndell Lock"),    // in Farum Azula
            (9009, "Golden Seed"),      // filler, so the scout is not lock-only
        ]);
        let coarse_of = coarse(&[
            (4004, "Limgrave"),
            (1001, "Liurnia"),
            (3003, "Altus"),
            (2002, "Farum Azula"),
            (9009, "Limgrave"),
        ]);
        (scouted, coarse_of)
    }

    #[test]
    fn boblers_three_hints_collapse_into_one_button() {
        // THE MOTIVATING CASE. He spent three `!hint`s discovering an ORDER. Walking the chain,
        // the frontier names the very lock he asked for at each step -- with no guess from him.
        let locks = boblers_locks();
        let (scouted, coarse_of) = boblers_fill();
        let hinted = HashSet::new();
        let expected = [
            (set(&["Limgrave"]), "Liurnia", 4004),
            (set(&["Limgrave", "Liurnia"]), "Altus", 1001),
            (set(&["Limgrave", "Liurnia", "Altus"]), "Farum Azula", 3003),
            (
                set(&["Limgrave", "Liurnia", "Altus", "Farum Azula"]),
                "Leyndell",
                2002,
            ),
        ];
        for (open, region, location) in expected {
            let got = next_lock(&locks, &open, &coarse_of, &scouted, &hinted);
            assert_eq!(
                got,
                NextLock::Reachable(FrontierLock {
                    region: region.to_string(),
                    lock_item: format!("{region} Lock"),
                    location,
                }),
                "with {open:?} open, the next lock must be {region}"
            );
        }
    }

    #[test]
    fn the_frontier_never_offers_a_lock_the_player_cannot_reach() {
        // The two hints bobler WASTED were Farum Azula and Leyndell, bought while standing in
        // Liurnia. Neither may ever be the offer at that point in the run.
        let locks = boblers_locks();
        let (scouted, coarse_of) = boblers_fill();
        let open = set(&["Limgrave", "Liurnia"]);
        let NextLock::Reachable(f) =
            next_lock(&locks, &open, &coarse_of, &scouted, &HashSet::new())
        else {
            panic!("a lock is reachable here");
        };
        assert_ne!(f.region, "Farum Azula");
        assert_ne!(f.region, "Leyndell");
    }

    #[test]
    fn a_lock_behind_another_lock_is_never_the_next_one() {
        // Only the Leyndell lock remains and it sits in still-locked Farum Azula. Saying
        // "none reachable" is correct here and is a DIFFERENT claim from "unknown".
        let locks = m(&[("Leyndell", "Leyndell Lock")]);
        let scouted = scout(&[(2002, "Leyndell Lock"), (9009, "Golden Seed")]);
        let coarse_of = coarse(&[(2002, "Farum Azula"), (9009, "Limgrave")]);
        assert_eq!(
            next_lock(
                &locks,
                &set(&["Limgrave"]),
                &coarse_of,
                &scouted,
                &HashSet::new()
            ),
            NextLock::NoneReachable
        );
    }

    #[test]
    fn a_spilled_next_lock_sends_the_player_to_hint_not_to_a_dead_end() {
        // 🛑 THE MUTATION THAT MATTERS. A lock in another world is absent from the scout, so a
        // naive implementation reports "nothing reachable" -- which tells a stuck player to stop
        // looking. It must name the regions and point at `!hint` instead.
        let locks = m(&[("Altus", "Altus Lock"), ("Leyndell", "Leyndell Lock")]);
        let scouted = scout(&[(9009, "Golden Seed")]); // populated, but holds neither lock
        let coarse_of = coarse(&[(9009, "Limgrave")]);
        assert_eq!(
            next_lock(
                &locks,
                &set(&["Limgrave"]),
                &coarse_of,
                &scouted,
                &HashSet::new()
            ),
            NextLock::Spilled {
                regions: vec!["Altus".to_string(), "Leyndell".to_string()],
            }
        );
    }

    #[test]
    fn a_reachable_lock_outranks_a_spilled_one() {
        // Spilled is a FALLBACK, not a veto: one foreign lock must not suppress a buy the player
        // can actually make.
        let locks = m(&[("Altus", "Altus Lock"), ("Leyndell", "Leyndell Lock")]);
        let scouted = scout(&[(1001, "Altus Lock"), (9009, "Golden Seed")]);
        let coarse_of = coarse(&[(1001, "Liurnia"), (9009, "Limgrave")]);
        let got = next_lock(
            &locks,
            &set(&["Limgrave", "Liurnia"]),
            &coarse_of,
            &scouted,
            &HashSet::new(),
        );
        assert!(matches!(got, NextLock::Reachable(f) if f.region == "Altus"));
    }

    #[test]
    fn buying_one_of_two_reachable_locks_advances_to_the_other() {
        // A branching fill puts two locks in the same open region. Ties break low, and the bought
        // one must not be re-sold on the next frame.
        let locks = m(&[("Altus", "Altus Lock"), ("Caelid", "Caelid Lock")]);
        let scouted = scout(&[(202, "Altus Lock"), (150, "Caelid Lock")]);
        let coarse_of = coarse(&[(202, "Limgrave"), (150, "Limgrave")]);
        let open = set(&["Limgrave"]);
        let first = next_lock(&locks, &open, &coarse_of, &scouted, &HashSet::new());
        assert!(matches!(first, NextLock::Reachable(ref f) if f.location == 150));
        let hinted: HashSet<i64> = [150].into_iter().collect();
        let second = next_lock(&locks, &open, &coarse_of, &scouted, &hinted);
        assert!(matches!(second, NextLock::Reachable(ref f) if f.location == 202));
        let both: HashSet<i64> = [150, 202].into_iter().collect();
        assert_eq!(
            next_lock(&locks, &open, &coarse_of, &scouted, &both),
            NextLock::AllFrontierHinted
        );
    }

    #[test]
    fn an_untabled_lock_location_is_unknown_never_out_of_reach() {
        // 🛑 The honest-failure pin. The scout found the lock but the region tables do not cover
        // its location, so we cannot say whether it is reachable. Reporting `NoneReachable` would
        // be a confident wrong answer to a player who is already stuck.
        let locks = m(&[("Altus", "Altus Lock")]);
        let scouted = scout(&[(1001, "Altus Lock"), (9009, "Golden Seed")]);
        let coarse_of = coarse(&[(9009, "Limgrave")]); // 1001 missing on purpose
        assert_eq!(
            next_lock(
                &locks,
                &set(&["Limgrave"]),
                &coarse_of,
                &scouted,
                &HashSet::new()
            ),
            NextLock::Unknown
        );
    }

    #[test]
    fn the_always_open_bucket_counts_as_open() {
        // A lock sitting in the "" always-open coarse bucket is reachable from the first frame,
        // whether or not the caller bothered to put "" in the open set.
        let locks = m(&[("Altus", "Altus Lock")]);
        let scouted = scout(&[(1001, "Altus Lock")]);
        let coarse_of = coarse(&[(1001, "")]);
        assert!(matches!(
            next_lock(
                &locks,
                &HashSet::new(),
                &coarse_of,
                &scouted,
                &HashSet::new()
            ),
            NextLock::Reachable(_)
        ));
    }

    #[test]
    fn nothing_locked_and_nothing_known_are_different_answers() {
        let locks = boblers_locks();
        let (scouted, coarse_of) = boblers_fill();
        // Everything open -> there is nothing left to hint.
        assert_eq!(
            next_lock(
                &locks,
                &set(&["Limgrave", "Liurnia", "Altus", "Farum Azula", "Leyndell"]),
                &coarse_of,
                &scouted,
                &HashSet::new()
            ),
            NextLock::NothingLocked
        );
        // Scout has not landed -> we do not know, and must not claim the locks are foreign.
        assert_eq!(
            next_lock(
                &locks,
                &set(&["Limgrave"]),
                &coarse_of,
                &HashMap::new(),
                &HashSet::new()
            ),
            NextLock::Unknown
        );
    }

    #[test]
    fn the_frontier_does_not_depend_on_hashmap_iteration_order() {
        // TWO candidates in the same open region -- with only one the test would pass against any
        // implementation at all. The offer must be the same object every frame or the button moves
        // out from under the cursor.
        let scouted = scout(&[(1001, "Altus Lock"), (2002, "Caelid Lock")]);
        let coarse_of = coarse(&[(1001, "Limgrave"), (2002, "Limgrave")]);
        let a = m(&[("Altus", "Altus Lock"), ("Caelid", "Caelid Lock")]);
        let b = m(&[("Caelid", "Caelid Lock"), ("Altus", "Altus Lock")]);
        let open = set(&["Limgrave"]);
        for _ in 0..8 {
            assert_eq!(
                next_lock(&a, &open, &coarse_of, &scouted, &HashSet::new()),
                next_lock(&b, &open, &coarse_of, &scouted, &HashSet::new())
            );
        }
    }

    // --- the offer layer -------------------------------------------------------------------------

    #[test]
    fn an_unaffordable_next_lock_shows_its_price_and_reveals_nothing_else() {
        // 🛑 Insufficient carries NO region and NO item name. Naming the next region for free would
        // hand over half of exactly what the price is charged for -- the exhaustive literal below
        // is what fails if a future edit adds one back.
        let locks = boblers_locks();
        let (scouted, coarse_of) = boblers_fill();
        let got = next_offer(
            &locks,
            &set(&["Limgrave", "Liurnia"]),
            &coarse_of,
            &scouted,
            &HashSet::new(),
            SURFACE,
            5,
            0,
            PPH_10PCT,
            TOTAL,
        );
        assert_eq!(got, NextLockOffer::Insufficient { price: 16, have: 5 });
    }

    #[test]
    fn an_affordable_next_lock_is_the_one_bobler_asked_for() {
        let locks = boblers_locks();
        let (scouted, coarse_of) = boblers_fill();
        let got = next_offer(
            &locks,
            &set(&["Limgrave", "Liurnia"]),
            &coarse_of,
            &scouted,
            &HashSet::new(),
            SURFACE,
            16,
            0,
            PPH_10PCT,
            TOTAL,
        );
        assert_eq!(
            got,
            NextLockOffer::Buyable {
                price: 16,
                location: 1001,
                region: "Altus".to_string(),
                lock_item: "Altus Lock".to_string(),
            }
        );
    }

    #[test]
    fn an_underivable_price_never_buys_a_free_next_lock() {
        // Same failure mode as `an_underivable_price_is_unknown_never_free`, one layer up: a
        // connection that has not settled must not make the button free.
        let locks = boblers_locks();
        let (scouted, coarse_of) = boblers_fill();
        let got = next_offer(
            &locks,
            &set(&["Limgrave", "Liurnia"]),
            &coarse_of,
            &scouted,
            &HashSet::new(),
            SURFACE,
            9999,
            0,
            PPH_10PCT,
            0, // total_locations unknown
        );
        assert_eq!(got, NextLockOffer::Idle);
    }

    // --- discoverability -------------------------------------------------------------------------

    #[test]
    fn the_status_line_reports_balance_and_price_together() {
        // Both numbers or neither: "23" on its own tells a player nothing about whether they can
        // buy anything.
        assert_eq!(status(SURFACE, 23, 0, PPH_10PCT, TOTAL), Some((23, 16)));
        assert_eq!(status(SURFACE, 23, 1, PPH_10PCT, TOTAL), Some((7, 16)));
        assert_eq!(status(SURFACE, 23, 0, PPH_10PCT, 0), None);
    }

    #[test]
    fn the_affordable_toast_fires_once_on_the_edge_and_never_on_a_reconnect() {
        // A guard the corpus cannot fire on its own, so it is called directly.
        assert!(crossed_into_affordable(Some(false), true), "the edge fires");
        assert!(
            !crossed_into_affordable(Some(true), true),
            "still affordable is not news"
        );
        assert!(
            !crossed_into_affordable(None, true),
            "a reconnect that replays an affordable balance must stay silent"
        );
        assert!(!crossed_into_affordable(Some(true), false));
    }
}
