//! Pure tracker aggregation for the Option A tracker window (SPEC-item-tracker.md, Phase 1).
//!
//! Everything here is a read/aggregate step over state the client already holds each frame:
//! the AP checked/unchecked location sets, the cumulative received-item name set, and a
//! location->region lookup that is INJECTED by the caller (the static table lives with the caller,
//! not here — this module stays data-free and host-testable). The overlay builds one
//! [`TrackerModel`] snapshot per render and draws from it; nothing in this module touches the game.
//!
//! Two distinctions on top of the flat list (Alaric 2026-07-04):
//!  - IN-LOGIC: a location is reachable now iff its COARSE region's open flag is set. The client
//!    supplies the set of currently-open coarse regions (`open_coarse_regions`), derived from the
//!    same region-lock state that gates kicks; a location's coarse region comes from the injected
//!    `coarse_of` table (an empty string, or an unknown id, means "always accessible" — a safe
//!    under-enforcement, matching the apworld's "under-enforce beats wrong-kick" stance).
//!  - PROGRESSION SURFACE: the locations this world's own progression may occupy, fed in as a
//!    set of ids (`surface`). (Was called "big-ticket": a retired concept, see contract.has_class.)
//!
//! Hint bookkeeping ([`HintSet`]) is the "option (a)" standing set from the spec: the client feeds
//! each streamed `Print::Hint` in as a [`HintEntry`]; the set dedups by location id (the server
//! replays relevant hints on connect, so re-inserts are the norm) and the aggregation folds it into
//! the per-region rollups so hinted unchecked locations render marked.

use std::collections::{BTreeMap, HashMap, HashSet};

/// Region key used for grouping — the region's display name (matches the apworld region names the
/// injected location->region table is generated from).
pub type RegionId = String;

/// Bucket for locations the injected lookup doesn't know. Kept as a real rollup (not dropped) so
/// `done`/`total` stay honest even if the table lags behind the location list.
pub const UNKNOWN_REGION: &str = "(unknown region)";

/// True when `id`'s coarse region is currently accessible. `coarse_of` missing the id, or mapping
/// it to the empty string, means "no lock / always accessible" (under-enforce, never wrong-kick).
fn location_in_logic(
    id: u64,
    coarse_of: &HashMap<u64, RegionId>,
    open_coarse_regions: &HashSet<RegionId>,
) -> bool {
    match coarse_of.get(&id) {
        None => true,
        Some(coarse) if coarse.is_empty() => true,
        Some(coarse) => open_coarse_regions.contains(coarse),
    }
}

/// One standing hint, as parsed by the client from a streamed `Print::Hint` relevant to this slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintEntry {
    /// The hinted location (in OUR world — this is what gets marked in the checks tree).
    pub location_id: u64,
    /// The item sitting at that location.
    pub item_name: String,
    /// The other player the hint concerns (the receiver if they hinted our world, the finder if we
    /// hinted theirs).
    pub other_player: String,
    /// True when the hinted item is FOR us (someone else holds our check); false when the hint asks
    /// US to go get someone else's item.
    pub for_us: bool,
    /// The server's `found` flag: the hinted location has been collected (client#221).
    ///
    /// 🛑 PARSED FOR MONTHS AND THROWN AWAY. `archipelago-rs` reads it off the wire
    /// (`data/print.rs`, `Print::Hint { found, .. }`) and this struct had nowhere to put it, so a
    /// hint that had been found was indistinguishable from a live one -- and the panel header,
    /// which a player reads as "how much is still outstanding", counted both. bobler's screenshot
    /// said `Hints (9)` with collected locations in the list.
    ///
    /// ⭐ THE DEDUP ALREADY DOES THE HARD PART. `insert` overwrites on re-insert (latest wins), so
    /// a later `found: true` for the same location replaces the earlier `found: false` with no
    /// change to the set's semantics. The only reason that did not work was this missing field.
    pub found: bool,
}

/// Standing hint set, deduplicated by location id. Rebuilt each session from replay-on-connect.
#[derive(Debug, Default, Clone)]
pub struct HintSet {
    by_location: HashMap<u64, HintEntry>,
}

impl HintSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or refresh) a hint. Returns `true` only when the location was not already hinted;
    /// a re-insert for a known location updates the stored metadata (latest wins) and returns
    /// `false`, so connect-replay churn is a no-op for callers counting new hints.
    pub fn insert(&mut self, entry: HintEntry) -> bool {
        self.by_location.insert(entry.location_id, entry).is_none()
    }

    pub fn is_hinted(&self, location_id: u64) -> bool {
        self.by_location.contains_key(&location_id)
    }

    /// Iterate the standing hints (for the Hints panel). Unordered; callers sort for display.
    pub fn iter(&self) -> impl Iterator<Item = &HintEntry> {
        self.by_location.values()
    }

    pub fn len(&self) -> usize {
        self.by_location.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_location.is_empty()
    }

    /// Hints still worth acting on: the ones the server has NOT marked found (client#221).
    ///
    /// This is the number the Hints header shows. The header is what a player reads as "how much
    /// is still outstanding", so it has to mean that; [`Self::len`] stays the size of the record,
    /// which is what the list draws.
    ///
    /// 🛑 DELIBERATELY NOT A FILTER ON [`Self::iter`] OR [`Self::is_hinted`]. `is_hinted` feeds the
    /// checks tree's hinted marking, and a checked location's marking is a different question --
    /// changing both at once would make a regression in the tree impossible to attribute (the
    /// ruling on #221 says so explicitly).
    pub fn outstanding(&self) -> usize {
        self.by_location.values().filter(|h| !h.found).count()
    }
}

/// One unchecked location inside a region rollup, pre-folded with its render marks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncheckedLocation {
    pub location_id: u64,
    /// True when a standing hint names this location (render marked in the checks tree).
    pub hinted: bool,
    /// True for prominent locations (boss drops, progression, churches, maps, seedtrees).
    pub on_surface: bool,
    /// True when this location's coarse region is currently accessible.
    pub in_logic: bool,
}

/// Per-region progress: `done`/`total` for the header line, plus the unchecked locations to list
/// under the expanded node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionRollup {
    pub region: RegionId,
    pub done: usize,
    pub total: usize,
    /// True when this region's coarse region is currently accessible (all its locations share one
    /// coarse region, so this is region-wide). Regions with no lock read `true`.
    pub accessible: bool,
    /// Sorted by location id for a stable render order.
    pub unchecked: Vec<UncheckedLocation>,
}

impl RegionRollup {
    /// Every location in this region is checked — the overlay's hide-completed filter key.
    pub fn complete(&self) -> bool {
        self.done == self.total
    }
}

/// Snapshot the tracker window renders from, built once per frame inside the existing `Core`
/// borrow. Pure data — no locks, no game state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackerModel {
    /// Region rollups sorted by region name for a stable tree order.
    pub regions: Vec<RegionRollup>,
    /// Overall checked count (`done`) out of all known locations (`total`).
    pub done: usize,
    pub total: usize,
    /// Checks whose coarse region is accessible now: `done` (already checked) out of `total`
    /// in-logic. Remaining reachable checks = `in_logic_total - in_logic_done`.
    pub in_logic_done: usize,
    pub in_logic_total: usize,
    /// Progression-surface checks: `done` out of `total`.
    pub surface_done: usize,
    pub surface_total: usize,
    /// Cumulative received item names, sorted for a stable items-held panel. The progressive-tier
    /// reduction stays in [`crate::progressive`]; this is the raw feed for it.
    pub received_items: Vec<String>,
}

/// Build the per-frame tracker snapshot.
///
///  - `checked_locations` / `unchecked_locations` — the AP client's two location sets. Assumed
///    disjoint (the server guarantees it); each id counts once toward its region's total.
///  - `received_item_names` — the cumulative `received_all` name set kept by `core.rs`.
///  - `region_of` — injected location->fine-region lookup (grouping). Unknown ids group under
///    [`UNKNOWN_REGION`] rather than being dropped.
///  - `coarse_of` — injected location->coarse-region lookup (in-logic key). Empty/absent = always
///    accessible.
///  - `surface` — the progression-surface location ids (starred in the tracker).
///  - `open_coarse_regions` — coarse regions the client currently has open (from region-lock state).
///  - `hints` — the standing [`HintSet`]; unchecked locations it names come back `hinted: true`.
#[allow(clippy::too_many_arguments)]
pub fn build_tracker_model(
    checked_locations: &[u64],
    unchecked_locations: &[u64],
    received_item_names: &HashSet<String>,
    region_of: &HashMap<u64, RegionId>,
    coarse_of: &HashMap<u64, RegionId>,
    surface: &HashSet<u64>,
    open_coarse_regions: &HashSet<RegionId>,
    hints: &HintSet,
) -> TrackerModel {
    // BTreeMap keyed by region name => rollups come out name-sorted for free.
    fn rollup<'a>(
        per_region: &'a mut BTreeMap<RegionId, RegionRollup>,
        region_of: &HashMap<u64, RegionId>,
        id: u64,
    ) -> &'a mut RegionRollup {
        let region = region_of
            .get(&id)
            .cloned()
            .unwrap_or_else(|| UNKNOWN_REGION.to_string());
        per_region
            .entry(region.clone())
            .or_insert_with(|| RegionRollup {
                region,
                done: 0,
                total: 0,
                accessible: true,
                unchecked: Vec::new(),
            })
    }

    let mut per_region: BTreeMap<RegionId, RegionRollup> = BTreeMap::new();
    let (mut in_logic_done, mut in_logic_total) = (0usize, 0usize);
    let (mut surface_done, mut surface_total) = (0usize, 0usize);

    for &id in checked_locations {
        let reachable = location_in_logic(id, coarse_of, open_coarse_regions);
        let prominent = surface.contains(&id);
        if reachable {
            in_logic_done += 1;
            in_logic_total += 1;
        }
        if prominent {
            surface_done += 1;
            surface_total += 1;
        }
        let r = rollup(&mut per_region, region_of, id);
        r.done += 1;
        r.total += 1;
        r.accessible = reachable;
    }
    for &id in unchecked_locations {
        let reachable = location_in_logic(id, coarse_of, open_coarse_regions);
        let prominent = surface.contains(&id);
        if reachable {
            in_logic_total += 1;
        }
        if prominent {
            surface_total += 1;
        }
        let r = rollup(&mut per_region, region_of, id);
        r.total += 1;
        r.accessible = reachable;
        r.unchecked.push(UncheckedLocation {
            location_id: id,
            hinted: hints.is_hinted(id),
            on_surface: prominent,
            in_logic: reachable,
        });
    }

    let mut regions: Vec<RegionRollup> = per_region.into_values().collect();
    for r in &mut regions {
        r.unchecked.sort_by_key(|u| u.location_id);
    }

    let mut received_items: Vec<String> = received_item_names.iter().cloned().collect();
    received_items.sort();

    TrackerModel {
        done: checked_locations.len(),
        total: checked_locations.len() + unchecked_locations.len(),
        in_logic_done,
        in_logic_total,
        surface_done,
        surface_total,
        regions,
        received_items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region_table(entries: &[(u64, &str)]) -> HashMap<u64, RegionId> {
        entries.iter().map(|&(id, r)| (id, r.to_string())).collect()
    }

    fn open_set(names: &[&str]) -> HashSet<RegionId> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn hint(location_id: u64, item: &str, player: &str, for_us: bool) -> HintEntry {
        hint_found(location_id, item, player, for_us, false)
    }

    /// The same, with the server's `found` flag (client#221).
    fn hint_found(
        location_id: u64,
        item: &str,
        player: &str,
        for_us: bool,
        found: bool,
    ) -> HintEntry {
        HintEntry {
            found,
            location_id,
            item_name: item.to_string(),
            other_player: player.to_string(),
            for_us,
        }
    }

    /// ⭐ THE RED-FIRST ASSERTION (client#221). bobler's panel read `Hints (9)` with collected
    /// locations still counted. The header must count only what is still outstanding.
    #[test]
    fn a_found_hint_stops_counting_but_stays_in_the_record() {
        let mut hints = HintSet::new();
        hints.insert(hint(10, "Godrick's Great Rune", "bobler", true));
        hints.insert(hint(11, "Bolt of Gransax", "boblerflips", false));
        assert_eq!(hints.outstanding(), 2, "both live to start with");

        // The server streams the same location again with `found: true` once it is collected.
        hints.insert(hint_found(
            11,
            "Bolt of Gransax",
            "boblerflips",
            false,
            true,
        ));

        assert_eq!(hints.outstanding(), 1, "the found one stops counting");
        assert_eq!(hints.len(), 2, "...and stays in the record, per the ruling");
    }

    /// 🛑 THE ALL-FOUND SET IS NOT AN EMPTY SET. The panel prints "  none yet" on `is_empty()`, and
    /// a list of nine dimmed found hints reading "none yet" is exactly the kind of thing that ships
    /// looking fine because nobody had nine found hints while testing.
    #[test]
    fn an_all_found_set_reads_zero_but_is_not_empty() {
        let mut hints = HintSet::new();
        hints.insert(hint_found(10, "A", "p", true, true));
        hints.insert(hint_found(11, "B", "p", false, true));
        assert_eq!(hints.outstanding(), 0, "the header reads Hints (0)");
        assert!(
            !hints.is_empty(),
            "but the list is NOT empty -- no 'none yet'"
        );
        assert_eq!(hints.len(), 2);
    }

    /// 🛑 `is_hinted` IS DELIBERATELY UNCHANGED. It feeds the checks tree's hinted marking, and a
    /// checked location's marking is a different question; the ruling says not to move both at
    /// once, so that a tree regression stays attributable.
    #[test]
    fn found_does_not_change_is_hinted() {
        let mut hints = HintSet::new();
        hints.insert(hint_found(10, "A", "p", true, true));
        assert!(
            hints.is_hinted(10),
            "a found hint is still a hint as far as the checks tree is concerned"
        );
    }

    /// Reconnect: the server replays relevant hints INCLUDING found ones, so a restart must bring
    /// it back dimmed and uncounted rather than as a fresh live hint. The dedup already does this;
    /// this pins that the `found` flag survives the replay path.
    #[test]
    fn connect_replay_reinserts_a_found_hint_as_found() {
        let mut hints = HintSet::new();
        hints.insert(hint_found(10, "A", "p", true, true));
        // Replay: same location, same metadata, arriving again.
        assert!(
            !hints.insert(hint_found(10, "A", "p", true, true)),
            "a re-insert is not a new hint"
        );
        assert_eq!(hints.outstanding(), 0, "still uncounted after replay");
        assert_eq!(hints.len(), 1, "and not duplicated");
    }

    /// ⚠️ AND THE FLAG CAN GO BACK. Nothing in the protocol promises `found` is monotonic, and the
    /// set is latest-wins by construction -- so a later `found: false` must restore the count
    /// rather than being quietly ignored.
    #[test]
    fn latest_wins_in_both_directions() {
        let mut hints = HintSet::new();
        hints.insert(hint_found(10, "A", "p", true, true));
        assert_eq!(hints.outstanding(), 0);
        hints.insert(hint_found(10, "A", "p", true, false));
        assert_eq!(hints.outstanding(), 1, "latest wins, both ways");
    }

    /// An empty set is still empty, and still reads zero -- the ordinary case must not regress.
    #[test]
    fn an_empty_set_is_still_empty() {
        let hints = HintSet::new();
        assert_eq!(hints.outstanding(), 0);
        assert!(hints.is_empty());
    }

    #[test]
    fn done_total_math_per_region_and_overall() {
        // Limgrave: 2 of 3 done. Caelid: 1 of 2 done. Overall: 3 of 5.
        let table = region_table(&[
            (1, "Limgrave"),
            (2, "Limgrave"),
            (3, "Limgrave"),
            (10, "Caelid"),
            (11, "Caelid"),
        ]);
        let m = build_tracker_model(
            &[1, 2, 10],
            &[3, 11],
            &HashSet::new(),
            &table,
            &HashMap::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HintSet::new(),
        );

        assert_eq!((m.done, m.total), (3, 5));
        assert_eq!(m.regions.len(), 2);
        // BTreeMap ordering: Caelid before Limgrave.
        let caelid = &m.regions[0];
        assert_eq!(
            (caelid.region.as_str(), caelid.done, caelid.total),
            ("Caelid", 1, 2)
        );
        assert_eq!(caelid.unchecked.len(), 1);
        assert_eq!(caelid.unchecked[0].location_id, 11);
        let limgrave = &m.regions[1];
        assert_eq!(
            (limgrave.region.as_str(), limgrave.done, limgrave.total),
            ("Limgrave", 2, 3)
        );
        assert!(!limgrave.complete());
        // Empty coarse table => everything always-accessible.
        assert_eq!((m.in_logic_done, m.in_logic_total), (3, 5));
        assert!(limgrave.accessible);
    }

    #[test]
    fn locked_region_all_unchecked_and_unknown_bucket() {
        // A locked region the player hasn't touched: total counted, done 0, everything listed.
        let table = region_table(&[(20, "Haligtree"), (21, "Haligtree")]);
        // Location 99 is missing from the table -> lands in the UNKNOWN_REGION bucket, not dropped.
        let m = build_tracker_model(
            &[],
            &[21, 20, 99],
            &HashSet::new(),
            &table,
            &HashMap::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HintSet::new(),
        );

        assert_eq!((m.done, m.total), (0, 3));
        let unknown = m
            .regions
            .iter()
            .find(|r| r.region == UNKNOWN_REGION)
            .unwrap();
        assert_eq!((unknown.done, unknown.total), (0, 1));
        let hali = m.regions.iter().find(|r| r.region == "Haligtree").unwrap();
        assert_eq!((hali.done, hali.total), (0, 2));
        assert!(!hali.complete());
        // Unchecked list is id-sorted for a stable render regardless of input order.
        let ids: Vec<u64> = hali.unchecked.iter().map(|u| u.location_id).collect();
        assert_eq!(ids, vec![20, 21]);
    }

    #[test]
    fn in_logic_gates_on_open_coarse_region() {
        // Two fine regions under two coarse regions; only Limgrave (coarse) is open.
        let region = region_table(&[(1, "Coastal Cave"), (2, "Coastal Cave"), (3, "Castle Sol")]);
        let coarse = region_table(&[
            (1, "Limgrave"),
            (2, "Limgrave"),
            (3, "Mountaintops of the Giants"),
        ]);
        let open = open_set(&["Limgrave"]);
        let m = build_tracker_model(
            &[1],
            &[2, 3],
            &HashSet::new(),
            &region,
            &coarse,
            &HashSet::new(),
            &open,
            &HintSet::new(),
        );

        // 1 (checked, Limgrave-open) + 2 (unchecked, Limgrave-open) are in-logic; 3 (Mountaintops
        // closed) is not. in_logic_done = 1 (the checked one), in_logic_total = 2.
        assert_eq!((m.in_logic_done, m.in_logic_total), (1, 2));
        let coastal = m
            .regions
            .iter()
            .find(|r| r.region == "Coastal Cave")
            .unwrap();
        assert!(coastal.accessible);
        assert!(coastal.unchecked.iter().all(|u| u.in_logic));
        let sol = m.regions.iter().find(|r| r.region == "Castle Sol").unwrap();
        assert!(!sol.accessible);
        assert!(sol.unchecked.iter().all(|u| !u.in_logic));
    }

    #[test]
    fn empty_coarse_string_is_always_in_logic() {
        // Roundtable-style always-open region: coarse = "" reads accessible even with nothing open.
        let region = region_table(&[(5, "Roundtable Hold")]);
        let coarse = region_table(&[(5, "")]);
        let m = build_tracker_model(
            &[],
            &[5],
            &HashSet::new(),
            &region,
            &coarse,
            &HashSet::new(),
            &HashSet::new(),
            &HintSet::new(),
        );
        assert_eq!((m.in_logic_done, m.in_logic_total), (0, 1));
        assert!(m.regions[0].accessible);
        assert!(m.regions[0].unchecked[0].in_logic);
    }

    #[test]
    fn surface_counts_and_marks() {
        let region = region_table(&[(1, "Limgrave"), (2, "Limgrave"), (3, "Limgrave")]);
        let big: HashSet<u64> = [1u64, 3u64].iter().copied().collect();
        let m = build_tracker_model(
            &[1],
            &[2, 3],
            &HashSet::new(),
            &region,
            &HashMap::new(),
            &big,
            &HashSet::new(),
            &HintSet::new(),
        );
        // id 1 (checked) + id 3 (unchecked) are on the surface => total 2, done 1.
        assert_eq!((m.surface_done, m.surface_total), (1, 2));
        let lim = &m.regions[0];
        let u3 = lim.unchecked.iter().find(|u| u.location_id == 3).unwrap();
        assert!(u3.on_surface);
        let u2 = lim.unchecked.iter().find(|u| u.location_id == 2).unwrap();
        assert!(!u2.on_surface);
    }

    #[test]
    fn hinted_unchecked_locations_are_marked() {
        let table = region_table(&[(1, "Limgrave"), (2, "Limgrave"), (3, "Limgrave")]);
        let mut hints = HintSet::new();
        // Hint on an unchecked location -> marked. Hint on a CHECKED location -> queryable in the
        // set but never surfaces in the unchecked list (already done).
        assert!(hints.insert(hint(2, "Rold Medallion", "OtherGuy", true)));
        assert!(hints.insert(hint(1, "Progressive Sword", "OtherGuy", false)));

        let m = build_tracker_model(
            &[1],
            &[2, 3],
            &HashSet::new(),
            &table,
            &HashMap::new(),
            &HashSet::new(),
            &HashSet::new(),
            &hints,
        );
        let limgrave = &m.regions[0];
        assert_eq!(
            limgrave.unchecked,
            vec![
                UncheckedLocation {
                    location_id: 2,
                    hinted: true,
                    on_surface: false,
                    in_logic: true
                },
                UncheckedLocation {
                    location_id: 3,
                    hinted: false,
                    on_surface: false,
                    in_logic: true
                },
            ]
        );
        assert!(
            hints.is_hinted(1),
            "checked-location hint stays queryable for the Hints panel"
        );
    }

    #[test]
    fn hint_dedup_by_location_latest_metadata_wins() {
        let mut hints = HintSet::new();
        assert!(hints.insert(hint(7, "Dectus Medallion (Left)", "Alice", true)));
        // Connect-replay re-delivers the same hint: not new, but metadata refreshes.
        assert!(!hints.insert(hint(7, "Dectus Medallion (Left)", "Alice", true)));
        assert_eq!(hints.len(), 1);

        assert!(!hints.insert(hint(7, "Dectus Medallion (Right)", "Bob", false)));
        assert_eq!(hints.len(), 1);
        let stored = hints.iter().next().unwrap();
        assert_eq!(stored.item_name, "Dectus Medallion (Right)");
        assert_eq!(stored.other_player, "Bob");
        assert!(!stored.for_us);

        assert!(hints.is_hinted(7));
        assert!(!hints.is_empty());
        assert!(!hints.is_hinted(8));
    }

    #[test]
    fn received_items_sorted_and_empty_inputs_ok() {
        let received: HashSet<String> = [
            "Scadutree Fragment",
            "Academy Glintstone Key",
            "Rold Medallion",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let m = build_tracker_model(
            &[],
            &[],
            &received,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HintSet::new(),
        );
        assert_eq!((m.done, m.total), (0, 0));
        assert!(m.regions.is_empty());
        assert_eq!(
            m.received_items,
            vec![
                "Academy Glintstone Key",
                "Rold Medallion",
                "Scadutree Fragment"
            ]
        );
    }
}
