//! `shop_hints` -- which of a merchant's slots an open shelf should announce to the multiworld.
//!
//! # The feature (er-archipelago#455, PHASE 2)
//!
//! Phase 1 ([`crate::esd_probe`]) bought one observation and nothing else: does the game's ESD talk
//! dispatch reach us, does `OPEN_REGULAR_SHOP` (command 22) fire at a real merchant, and are its two
//! arguments a usable `ShopLineupParam` row range. It does, it does, and they are -- boblerrr's
//! 2026-08-08 log has `cmd 22 args [101800, 101897]`, a 98-row span that appears verbatim as
//! `OpenRegularShop(101800, 101897)` in the decompiled talkscript. This module is the decision half
//! of what happens next: the player opens a shop, and every AP check on that shelf becomes a hint.
//!
//! It is pure. No game, no socket, no params -- the caller reads the live rows and the live event
//! flags and hands them in, which is what makes every rule below testable on any host.
//!
//! # Why the shelf is not the same thing as the range
//!
//! The range is what the talkscript ASKED for. What the player can actually see is narrower, in two
//! directions, and both of them are hints we must not send:
//!
//! * **Release-gated rows.** `eventFlag_forRelease` is the game's own "is this on the shelf yet"
//!   switch -- the Twin Maidens' bell-bearing tranches are the archetype, and it is why the naive
//!   read of that merchant is ~405 rows. A row whose release flag is CLEAR is not stocked, the
//!   player cannot see it, and hinting it leaks a reward from a shelf that does not exist yet. Each
//!   bell turn-in releases a tranche, the next open sees it, and the hint arrives then -- which is
//!   the correct time for it to arrive.
//! * **Purchased rows.** `eventFlag_forStock` SET means bought and out of stock (the same field the
//!   check poller watches, and the reason a sold-out slot renders as a blank cell). The location is
//!   already checked; a hint for it is noise about an item the player has.
//!
//! # 🛑 The range guard is not paranoia
//!
//! This runs inside the game's own dispatch frame. `normalize_range` refuses anything wider than
//! [`MAX_RANGE_ROWS`] because the alternative to refusing is a loop over an arbitrary integer pair
//! read out of a live event -- if an argument is ever something other than the row bound we believe
//! it is, the failure mode without the guard is a hang inside a frame, not a wrong hint. The widest
//! range in the whole decompiled talk corpus is 400 rows (`OpenTailoringShop(111000, 111399)`), so
//! the ceiling is an order of magnitude above anything real.
//!
//! # What it deliberately does NOT cover
//!
//! Only command 22, the regular buy menu. The corpus has four other shop opens --
//! `OpenAshOfWarShop`, `OpenEnhanceShop`, `OpenEquipmentChangeOfPurposeShop`, `OpenTailoringShop` --
//! and **their command ids are not known**: no `esd:` line in the 08-08 session carried one, and
//! deriving an id by matching arguments against literal call sites returns `EndMachine` for both 119
//! and 120, so it is demonstrably not sound on its own. Writing a guessed id in here would put an
//! unverified premise in the shipping path. Those merchants simply do not auto-hint yet, and the
//! caller says so rather than looking complete.

use std::collections::{HashMap, HashSet};

/// Widest `ShopLineupParam` row span this will walk for one shop open.
///
/// A blast-radius limit, not a business rule: the widest real range in the talk corpus is 400 rows.
pub const MAX_RANGE_ROWS: u32 = 4096;

/// One live `ShopLineupParam` row, as the caller read it off the param repository this frame.
///
/// Read fresh on every open, never cached: a map load streams the table back in, and both flags are
/// live game state that the last five minutes of play can have changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShopRow {
    /// `ShopLineupParam` row id.
    pub id: u32,
    /// `eventFlag_forStock` -- the purchase/tracking flag. `0` means the row is not a check.
    pub stock_flag: u32,
    /// `eventFlag_forRelease` -- the stocking gate. `0` means always stocked.
    pub release_flag: u32,
}

/// Why rows were passed over. Carried so the caller can log a TALLY rather than a bare count: a
/// shop open that hints nothing is otherwise indistinguishable from a broken one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SkipTally {
    /// Row id outside the opened range -- belongs to a different shelf.
    pub out_of_range: u32,
    /// `stock_flag == 0`: vanilla ware, not an Archipelago check.
    pub not_a_check: u32,
    /// Release-gated and the gate is clear: not on the shelf yet.
    pub not_released: u32,
    /// Stock flag already set: bought, out of stock, location already checked.
    pub purchased: u32,
    /// Stock flag is not one of this seed's check flags.
    pub unknown_flag: u32,
    /// Location already hinted -- earlier this session, or by another row in this same range.
    pub already_hinted: u32,
}

/// What one shop open should send.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShopHintPlan {
    /// AP location ids to hint, deduplicated and ascending. ONE `create_hints` call for the whole
    /// vector: the server does a full `ctx.save()` per hint-creating packet, so a per-row packet
    /// would turn a 12-hint shelf into 12 saves.
    pub locations: Vec<i64>,
    pub tally: SkipTally,
}

impl ShopHintPlan {
    /// Nothing to send. Not the same as "nothing happened" -- read [`ShopHintPlan::tally`] for why.
    pub fn is_empty(&self) -> bool {
        self.locations.is_empty()
    }
}

/// Turn the two raw `cmd 22` arguments into an inclusive row range, or refuse.
///
/// `None` means the pair is not a range we are willing to walk: negative (row ids are unsigned),
/// inverted, or wider than [`MAX_RANGE_ROWS`]. Refusing is deliberate -- see the module header.
pub fn normalize_range(begin: i32, end: i32) -> Option<(u32, u32)> {
    if begin < 0 || end < 0 || end < begin {
        return None;
    }
    let (lo, hi) = (begin as u32, end as u32);
    // `>=` because the range is inclusive at both ends: `hi - lo` is one less than the number
    // of rows walked, so this admits exactly MAX_RANGE_ROWS of them.
    if hi - lo >= MAX_RANGE_ROWS {
        return None;
    }
    Some((lo, hi))
}

/// Decide which locations one shop open announces.
///
/// * `range` -- the inclusive row span from [`normalize_range`].
/// * `rows` -- live `ShopLineupParam` rows. Rows outside `range` are counted and ignored, so the
///   caller may hand in a wider read without the result changing.
/// * `flag_to_loc` -- `eventFlag_forStock -> AP location id`, inverted from the seed's check table.
///   The same table `shop_sell` and `shop_repoint` invert; nothing new travels in slot data for
///   this feature, which is why it moves no contract hash.
/// * `flag_set` -- is this event flag set RIGHT NOW.
/// * `hinted` -- locations already announced this session. Session-scoped on purpose (DS3
///   precedent): if something goes wrong the player can quit out and every hint is re-sent.
///
/// Own and foreign items alike are hinted. The trigger is "the player is looking at this shelf",
/// and what the player is looking at does not depend on who the reward belongs to.
pub fn plan_shop_hints(
    range: (u32, u32),
    rows: &[ShopRow],
    flag_to_loc: &HashMap<u32, i64>,
    flag_set: &dyn Fn(u32) -> bool,
    hinted: &HashSet<i64>,
) -> ShopHintPlan {
    let (lo, hi) = range;
    let mut tally = SkipTally::default();
    // Locations claimed by THIS batch, so two rows sharing one stock flag cannot ask twice.
    let mut batch: HashSet<i64> = HashSet::new();
    let mut locations: Vec<i64> = Vec::new();

    for row in rows {
        if !(lo..=hi).contains(&row.id) {
            tally.out_of_range += 1;
            continue;
        }
        if row.stock_flag == 0 {
            tally.not_a_check += 1;
            continue;
        }
        // Release BEFORE purchase: an unreleased row is not on the shelf at all, and its stock flag
        // is a fact about a slot the player cannot see. Reporting it as "purchased" would be a
        // wrong reason in the tally, and the tally is the only thing that explains a quiet open.
        if row.release_flag != 0 && !flag_set(row.release_flag) {
            tally.not_released += 1;
            continue;
        }
        if flag_set(row.stock_flag) {
            tally.purchased += 1;
            continue;
        }
        let loc = match flag_to_loc.get(&row.stock_flag) {
            Some(&l) => l,
            None => {
                tally.unknown_flag += 1;
                continue;
            }
        };
        if hinted.contains(&loc) || !batch.insert(loc) {
            tally.already_hinted += 1;
            continue;
        }
        locations.push(loc);
    }

    // Ascending so the log line, the packet and the tests all read the same order whatever order
    // the caller walked the param table in.
    locations.sort_unstable();
    ShopHintPlan { locations, tally }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u32, stock: u32, release: u32) -> ShopRow {
        ShopRow {
            id,
            stock_flag: stock,
            release_flag: release,
        }
    }

    fn map(pairs: &[(u32, i64)]) -> HashMap<u32, i64> {
        pairs.iter().copied().collect()
    }

    fn nothing_set(_: u32) -> bool {
        false
    }

    /// THE MOTIVATING CASE (er-archipelago#455, and boblerrr's ask in the shape he asked for it):
    /// the player opens Kale, whose real observed range is `101800..=101897`, and the two AP checks
    /// on that shelf are announced to the multiworld.
    #[test]
    fn opening_kales_shelf_hints_its_checks() {
        let rows = [
            row(101_800, 280_060, 0),
            row(101_801, 280_590, 0),
            row(101_805, 0, 0), // vanilla ware
        ];
        let plan = plan_shop_hints(
            normalize_range(101_800, 101_897).unwrap(),
            &rows,
            &map(&[(280_060, 7_774_858), (280_590, 7_774_859)]),
            &nothing_set,
            &HashSet::new(),
        );
        assert_eq!(plan.locations, vec![7_774_858, 7_774_859]);
        assert_eq!(plan.tally.not_a_check, 1);
    }

    /// A bought slot is a blank cell on the shelf and a location the server already has. Hinting it
    /// tells the multiworld about an item the player is holding.
    #[test]
    fn a_purchased_row_is_not_hinted() {
        let rows = [row(101_800, 280_060, 0)];
        let plan = plan_shop_hints(
            (101_800, 101_897),
            &rows,
            &map(&[(280_060, 7_774_858)]),
            &|f| f == 280_060,
            &HashSet::new(),
        );
        assert!(plan.is_empty());
        assert_eq!(plan.tally.purchased, 1);
    }

    /// THE TWIN MAIDENS CASE. A bell-bearing tranche is in the range long before it is on the
    /// shelf; the release flag is the game's own answer to "can the player see this", and the hint
    /// must wait for it. Same row, gate set, hints -- so the tranche announces itself on the first
    /// open after the turn-in.
    #[test]
    fn a_release_gated_row_waits_for_its_bell() {
        let rows = [row(101_516, 250_160, 9_116)];
        let flags = map(&[(250_160, 7_770_500)]);
        let before = plan_shop_hints(
            (101_500, 101_599),
            &rows,
            &flags,
            &nothing_set,
            &HashSet::new(),
        );
        assert!(before.is_empty());
        assert_eq!(before.tally.not_released, 1);

        let after = plan_shop_hints(
            (101_500, 101_599),
            &rows,
            &flags,
            &|f| f == 9_116,
            &HashSet::new(),
        );
        assert_eq!(after.locations, vec![7_770_500]);
    }

    /// THE SHARED-FLAG LOSSY CASE. `flag -> location` is many-to-one: several `ShopLineupParam`
    /// rows can carry the same `eventFlag_forStock` (a set sold in pieces, a merchant duplicated
    /// across dialogue branches). Both rows resolve to ONE location, and the packet must carry it
    /// once -- a duplicate in a single `create_hints` is a wasted round trip on a packet the server
    /// full-saves for.
    #[test]
    fn two_rows_sharing_a_stock_flag_hint_one_location() {
        let rows = [row(101_800, 280_060, 0), row(101_850, 280_060, 0)];
        let plan = plan_shop_hints(
            (101_800, 101_897),
            &rows,
            &map(&[(280_060, 7_774_858)]),
            &nothing_set,
            &HashSet::new(),
        );
        assert_eq!(plan.locations, vec![7_774_858]);
        assert_eq!(plan.tally.already_hinted, 1);
    }

    /// Re-opening the same shelf is the common case (buy one thing, reopen, buy another). The
    /// session ledger keeps that from re-announcing the whole shop every time.
    #[test]
    fn a_location_hinted_earlier_this_session_is_quiet() {
        let rows = [row(101_800, 280_060, 0), row(101_801, 280_590, 0)];
        let hinted: HashSet<i64> = [7_774_858].into_iter().collect();
        let plan = plan_shop_hints(
            (101_800, 101_897),
            &rows,
            &map(&[(280_060, 7_774_858), (280_590, 7_774_859)]),
            &nothing_set,
            &hinted,
        );
        assert_eq!(plan.locations, vec![7_774_859]);
        assert_eq!(plan.tally.already_hinted, 1);
    }

    /// The caller may hand in a wider read of the param table than the shelf it opened. Only the
    /// opened range is the shelf -- the adjacent merchant's rows are not on it.
    #[test]
    fn rows_outside_the_opened_range_belong_to_another_shelf() {
        let rows = [row(101_799, 280_000, 0), row(101_800, 280_060, 0)];
        let plan = plan_shop_hints(
            (101_800, 101_897),
            &rows,
            &map(&[(280_000, 111), (280_060, 7_774_858)]),
            &nothing_set,
            &HashSet::new(),
        );
        assert_eq!(plan.locations, vec![7_774_858]);
        assert_eq!(plan.tally.out_of_range, 1);
    }

    /// A stock flag that is not one of the seed's check flags is a vanilla shop row that happens to
    /// have a tracking flag. Counted separately from `not_a_check`, because "this shelf has rows we
    /// could not resolve" and "this shelf is all vanilla" are different findings.
    #[test]
    fn a_stock_flag_outside_the_seed_is_counted_not_hinted() {
        let rows = [row(101_800, 999_999, 0)];
        let plan = plan_shop_hints(
            (101_800, 101_897),
            &rows,
            &map(&[(280_060, 7_774_858)]),
            &nothing_set,
            &HashSet::new(),
        );
        assert!(plan.is_empty());
        assert_eq!(plan.tally.unknown_flag, 1);
    }

    #[test]
    fn a_real_observed_range_is_accepted_at_its_real_width() {
        // boblerrr 2026-08-08, verbatim: `cmd 22 args [Int32(101800), Int32(101897)]`.
        assert_eq!(normalize_range(101_800, 101_897), Some((101_800, 101_897)));
        // The widest open in the whole talk corpus: OpenTailoringShop(111000, 111399).
        assert_eq!(normalize_range(111_000, 111_399), Some((111_000, 111_399)));
        // A single-row shelf is a range too.
        assert_eq!(normalize_range(101_800, 101_800), Some((101_800, 101_800)));
    }

    /// The guard exists because this walks inside the game's dispatch frame: an argument pair that
    /// is not the row bounds we believe it is must cost one refusal line, never a loop over two
    /// billion ids.
    #[test]
    fn an_unwalkable_argument_pair_is_refused() {
        assert_eq!(normalize_range(101_897, 101_800), None); // inverted
        assert_eq!(normalize_range(-1, 500), None); // negative
        assert_eq!(normalize_range(0, i32::MAX), None); // absurd width
        assert_eq!(normalize_range(0, MAX_RANGE_ROWS as i32), None); // exactly at the ceiling
    }

    /// Order is fixed here, not left to however the caller walked the param table, so the log line
    /// and the packet are reproducible between two opens of the same shelf.
    #[test]
    fn the_plan_is_ordered_independently_of_the_row_walk() {
        let rows = [row(101_802, 280_590, 0), row(101_800, 280_060, 0)];
        let plan = plan_shop_hints(
            (101_800, 101_897),
            &rows,
            &map(&[(280_060, 7_774_858), (280_590, 7_774_859)]),
            &nothing_set,
            &HashSet::new(),
        );
        assert_eq!(plan.locations, vec![7_774_858, 7_774_859]);
    }

    /// A shelf with nothing to say still has to say WHY, or a working feature and a broken one look
    /// identical in the log.
    #[test]
    fn a_silent_shelf_still_reports_its_reasons() {
        let rows = [
            row(101_800, 0, 0),
            row(101_801, 280_060, 0),
            row(101_802, 280_590, 4_242),
        ];
        let plan = plan_shop_hints(
            (101_800, 101_897),
            &rows,
            &map(&[(280_060, 7_774_858), (280_590, 7_774_859)]),
            &|f| f == 280_060,
            &HashSet::new(),
        );
        assert!(plan.is_empty());
        assert_eq!(plan.tally.not_a_check, 1);
        assert_eq!(plan.tally.purchased, 1);
        assert_eq!(plan.tally.not_released, 1);
    }
}
