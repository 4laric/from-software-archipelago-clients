//! The tracker's region model, built from slot_data instead of a baked table.
//!
//! # Why this exists
//!
//! `tracker_regions.rs` used to carry this: a generated 279 KB table of
//! `(ap_id, fine_region, coarse_region, on_surface, missable)` for all 4879 locations, produced by
//! the apworld's `tools/gen_location_regions.py` and committed to THIS repo.
//!
//! It was wrong in a way that a bigger table could not fix. The apworld builds `locationFlags` and
//! `regionOpenFlags` from `[HUB] + kept`, and under `num_regions` the kept set is a per-seed
//! SUBSET. A table generated once described the DEFAULT seed and quietly mis-described every
//! reduced one: locations in regions the seed does not contain, grouped and marked in-logic as
//! though they were there. That is a CORPUS fact standing in for a SEED fact -- the same shape as
//! the `num_regions` bug that voided "the item exists" claims.
//!
//! Sending it makes it right for every seed. It also retires a whole class of commit to this repo:
//! an apworld region or tag change used to require a regenerated `.rs` pushed here, with the world
//! CI's cross-repo diff gate as the thing that noticed when it had not been.
//!
//! # What is NOT here, deliberately
//!
//! * **`missable`.** The baked table carried it and `missable_set()` had **no production caller** --
//!   an emitted-but-unconsumed half-feature. It is not replaced by a slot_data key; when a consumer
//!   exists, add the key then. Shipping the wire for a feature that does not exist is how the
//!   contract ledger fills up with things nobody can delete.
//! * **`on_surface`.** Already sent as `progressionSurfaceLocations`, and the client already
//!   prefers it; the baked copy was only a constructor default.
//! * **The lock ITEM name.** Derived as `"<coarse> Lock"`, which is the key format `regionOpenFlags`
//!   is contractually documented to use. One convention, stated in the contract, applied here.
//!
//! # Absence is reported, never papered over
//!
//! There is no fallback to a stale baked table -- that is the point. If `locationRegions` is
//! missing (an older apworld), the tables are EMPTY and [`TablesStatus`] says why, so the caller
//! logs "inert because X" rather than showing a tracker that silently groups nothing. This mirrors
//! how `progressionSurfaceLocations` is already handled, and CONTRIBUTING's rule that a tolerant
//! path must announce its status.

use std::collections::HashMap;

use serde_json::Value;

use crate::tracker::RegionId;

/// Location -> region maps for the tracker. Empty is a legitimate state; read [`TablesStatus`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TrackerTables {
    /// AP location id -> FINE region name (the tracker's visual grouping).
    pub region: HashMap<u64, RegionId>,
    /// AP location id -> COARSE key: the region whose lock decides in-logic.
    /// `""` = always accessible.
    pub coarse: HashMap<u64, RegionId>,
    /// Coarse key -> its region-lock ITEM name, `"<coarse> Lock"`. Never contains `""`.
    pub lock_items: HashMap<RegionId, String>,
}

impl TrackerTables {
    /// The Region Lock item whose region owns EVERY one of `goal_locations`, when they agree
    /// (world#694).
    ///
    /// 🛑 THE COARSE KEY, NOT THE FINE REGION. `coarse` is by definition "the region whose lock
    /// decides in-logic", which is exactly the question being asked; the fine region is a visual
    /// grouping and the two legitimately differ. Resolving through the fine name would be a
    /// confidently-wrong answer of the kind [`crate::boss_fight_sample`]'s region note is about.
    ///
    /// `None` when the ids disagree, when any id is unknown to the tables, when the coarse key is
    /// `""` (always accessible -- no lock decides it), or when the tables are empty. Every one of
    /// those is "don't know", and the caller's only use for this is a NOTICE, so don't-know must
    /// read as "say nothing" rather than as a guess.
    pub fn goal_lock_item(&self, goal_locations: &[i64]) -> Option<&str> {
        let mut agreed: Option<&RegionId> = None;
        for &id in goal_locations {
            let key = self.coarse.get(&(id as u64))?;
            if key.is_empty() {
                return None; // always-accessible: no lock decides this location
            }
            match agreed {
                None => agreed = Some(key),
                Some(k) if k == key => {}
                Some(_) => return None, // two regions -> not one arena
            }
        }
        self.lock_items.get(agreed?).map(|s| s.as_str())
    }
}

/// Why the tables look the way they do. The caller logs this ONCE at connect: a feature is armed,
/// or it says why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TablesStatus {
    /// Built from slot_data. `coarse_defaulted` = `regionCoarseKeys` was absent, so each region was
    /// treated as its own coarse key (see [`build_tracker_tables`]).
    Armed {
        locations: usize,
        regions: usize,
        coarse_defaulted: bool,
    },
    /// `locationRegions` absent or unusable: an older apworld, or a foreign one. Tables are empty
    /// and the tracker groups nothing -- which is visible, unlike wrong grouping.
    NoRegions,
}

impl TablesStatus {
    /// A one-line arming log. `"armed with N entries"` or `"inert because X"`, per CONTRIBUTING.
    pub fn describe(&self) -> String {
        match self {
            TablesStatus::Armed {
                locations,
                regions,
                coarse_defaulted,
            } => format!(
                "tracker regions ARMED from slot_data: {locations} location(s) across {regions} \
                 region(s){}",
                if *coarse_defaulted {
                    " -- regionCoarseKeys ABSENT,each region treated as its own lock key"
                } else {
                    ""
                }
            ),
            TablesStatus::NoRegions => "tracker regions INERT: slot_data has no locationRegions \
                 (old or foreign apworld). The tracker will group NOTHING. There is deliberately no \
                 baked fallback -- a stale table would mis-group a num_regions seed instead of \
                 saying it does not know."
                .to_string(),
        }
    }
}

/// Build the tracker's region tables from a slot_data object.
///
/// `locationRegions` is `{fine region: [ap id, ...]}` (`LISTVAL_INT_MAP`) and `regionCoarseKeys` is
/// `{fine region: coarse key}` (`STR_MAP`), both per the contract.
///
/// When `regionCoarseKeys` is absent, each region becomes its OWN coarse key rather than being
/// treated as always-open. That is the conservative direction and it is also harmless for a region
/// that genuinely has no lock: the caller looks the key up in `regionOpenFlags`, finds nothing, and
/// treats it as unlocked -- the same answer `""` would have produced.
/// Takes the two slot_data values already looked up, `Option<&Value>` -- the same signature shape
/// as `scaling::parse_triple_ranges` and the other er-logic parsers. That is deliberate: the caller
/// writes `sd.get("locationRegions")`, which yields `Option<&Value>` whether `sd` is owned or
/// borrowed, so this cannot acquire a borrow mismatch against a crate that does not build on this
/// host.
pub fn build_tracker_tables(
    location_regions: Option<&Value>,
    region_coarse_keys: Option<&Value>,
) -> (TrackerTables, TablesStatus) {
    let Some(regions) = location_regions.and_then(Value::as_object) else {
        return (TrackerTables::default(), TablesStatus::NoRegions);
    };
    let coarse_src = region_coarse_keys.and_then(Value::as_object);
    let coarse_defaulted = coarse_src.is_none();

    let mut out = TrackerTables::default();
    for (fine, ids) in regions {
        let Some(ids) = ids.as_array() else { continue };
        // The coarse key: sent, or the region itself. `""` is a REAL value (always accessible), so
        // it is distinguished from "absent" rather than folded into it.
        let key = coarse_src
            .and_then(|m| m.get(fine.as_str()))
            .and_then(Value::as_str)
            .unwrap_or(fine.as_str())
            .to_string();
        for id in ids {
            let Some(id) = id.as_u64() else { continue };
            out.region.insert(id, fine.clone());
            out.coarse.insert(id, key.clone());
        }
        if !key.is_empty() {
            // `regionOpenFlags` is contractually keyed by exactly `"<Region> Lock"`.
            out.lock_items
                .entry(key.clone())
                .or_insert_with(|| format!("{key} Lock"));
        }
    }
    if out.region.is_empty() {
        return (TrackerTables::default(), TablesStatus::NoRegions);
    }
    let region_count = regions.len();
    let locations = out.region.len();
    (
        out,
        TablesStatus::Armed {
            locations,
            regions: region_count,
            coarse_defaulted,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tt() -> TrackerTables {
        let mut t = TrackerTables::default();
        for id in [10u64, 11, 12] {
            t.coarse.insert(id, "Enir Ilim".to_string());
        }
        t.coarse.insert(20, "Liurnia".to_string());
        t.coarse.insert(30, String::new()); // always accessible
        t.lock_items
            .insert("Enir Ilim".to_string(), "Enir Ilim Lock".to_string());
        t.lock_items
            .insert("Liurnia".to_string(), "Liurnia Lock".to_string());
        t
    }

    /// ⭐ world#694: goal locations that agree on one coarse region resolve to that region's Lock.
    #[test]
    fn agreeing_goal_locations_resolve_to_one_lock() {
        assert_eq!(tt().goal_lock_item(&[10, 11, 12]), Some("Enir Ilim Lock"));
    }

    /// 🛑 TWO REGIONS IS NOT ONE ARENA. Same rule `describe_goal` uses when it declines to name a
    /// region: a disagreeing set means say nothing, not pick the first.
    #[test]
    fn disagreeing_goal_locations_resolve_to_nothing() {
        assert_eq!(tt().goal_lock_item(&[10, 20]), None);
    }

    /// An id the tables do not know, an always-accessible coarse key, and empty tables are all
    /// don't-know -- and the caller only drives a NOTICE, so don't-know must be silence.
    #[test]
    fn unknown_always_accessible_and_empty_are_all_none() {
        assert_eq!(tt().goal_lock_item(&[10, 999]), None, "unknown id");
        assert_eq!(tt().goal_lock_item(&[30]), None, "always accessible");
        assert_eq!(
            TrackerTables::default().goal_lock_item(&[10]),
            None,
            "no tables"
        );
    }

    /// A coarse key with no lock item mapped is also don't-know rather than a fabricated name.
    #[test]
    fn a_coarse_key_with_no_lock_item_is_none() {
        let mut t = tt();
        t.lock_items.remove("Enir Ilim");
        assert_eq!(t.goal_lock_item(&[10]), None);
    }
    use serde_json::json;

    /// The exact shapes the contract declares, so a test passing here means the WIRE parses --
    /// not merely that some JSON does.
    fn seed() -> Value {
        json!({
            "locationRegions": {
                "Roundtable Hold": [7770017u64, 7770024u64],
                "Limgrave": [7770011u64, 7770012u64, 7770013u64],
                "Enir Ilim": [7770900u64],
            },
            "regionCoarseKeys": {
                "Roundtable Hold": "",
                "Limgrave": "Limgrave",
                "Enir Ilim": "Leyndell",
            },
        })
    }

    #[test]
    fn builds_fine_and_coarse_tables_from_slot_data() {
        let s = seed();
        let (t, st) = build_tracker_tables(s.get("locationRegions"), s.get("regionCoarseKeys"));
        assert_eq!(t.region.get(&7770011), Some(&"Limgrave".to_string()));
        assert_eq!(t.coarse.get(&7770011), Some(&"Limgrave".to_string()));
        assert_eq!(t.region.len(), 6);
        assert!(matches!(
            st,
            TablesStatus::Armed {
                locations: 6,
                regions: 3,
                coarse_defaulted: false
            }
        ));
    }

    #[test]
    fn the_hub_is_always_accessible_not_merely_lockless() {
        // `""` is a REAL coarse value meaning always-open. Folding it into "absent" would make the
        // hub look like a region whose lock we failed to find -- same answer today, different
        // reason, and the difference matters the moment a lookup failure becomes an error.
        let s = seed();
        let (t, _) = build_tracker_tables(s.get("locationRegions"), s.get("regionCoarseKeys"));
        assert_eq!(t.coarse.get(&7770017), Some(&String::new()));
        assert!(
            !t.lock_items.contains_key(""),
            "the always-accessible bucket must never be given a lock item"
        );
    }

    #[test]
    fn a_lockless_region_keys_off_its_host_not_itself() {
        // Enir Ilim (the finale) has no lock of its own; its physical space is gated by Leyndell's.
        // If this regressed to keying off itself, the client would look for an "Enir Ilim Lock",
        // find none, and call the finale permanently OPEN.
        let s = seed();
        let (t, _) = build_tracker_tables(s.get("locationRegions"), s.get("regionCoarseKeys"));
        assert_eq!(t.coarse.get(&7770900), Some(&"Leyndell".to_string()));
        assert_eq!(
            t.lock_items.get("Leyndell"),
            Some(&"Leyndell Lock".to_string())
        );
        assert!(!t.lock_items.contains_key("Enir Ilim"));
    }

    #[test]
    fn lock_item_names_match_the_region_open_flags_key_format() {
        let s = seed();
        let (t, _) = build_tracker_tables(s.get("locationRegions"), s.get("regionCoarseKeys"));
        assert_eq!(
            t.lock_items.get("Limgrave"),
            Some(&"Limgrave Lock".to_string())
        );
    }

    #[test]
    fn absent_location_regions_is_inert_and_says_so() {
        // The whole point of deleting the baked table: no stale fallback. Empty and LOUD.
        let (t, st) = build_tracker_tables(None, None);
        assert!(t.region.is_empty() && t.coarse.is_empty() && t.lock_items.is_empty());
        assert_eq!(st, TablesStatus::NoRegions);
        assert!(st.describe().contains("INERT"), "{}", st.describe());
    }

    #[test]
    fn an_empty_region_map_is_inert_not_armed_with_zero() {
        // "0 locations, armed" is the shape of a green run over nothing.
        let empty = json!({});
        let (_t, st) = build_tracker_tables(Some(&empty), None);
        assert_eq!(st, TablesStatus::NoRegions);
    }

    #[test]
    fn missing_coarse_keys_default_to_the_region_itself_and_report_it() {
        let r = json!({"Caelid": [1u64, 2u64]});
        let (t, st) = build_tracker_tables(Some(&r), None);
        assert_eq!(t.coarse.get(&1), Some(&"Caelid".to_string()));
        assert_eq!(t.lock_items.get("Caelid"), Some(&"Caelid Lock".to_string()));
        match st {
            TablesStatus::Armed {
                coarse_defaulted, ..
            } => assert!(
                coarse_defaulted,
                "the default must be REPORTED -- a silent default is indistinguishable from data"
            ),
            other => panic!("expected Armed, got {other:?}"),
        }
    }

    #[test]
    fn a_seed_that_kept_only_some_regions_yields_only_those() {
        // THE REASON THIS MODULE EXISTS. Under num_regions the apworld sends only the kept regions,
        // so a location in a dropped region must be ABSENT here. The baked table could not do this:
        // it always described the full default seed.
        let r = json!({"Limgrave": [7770011u64]});
        let c = json!({"Limgrave": "Limgrave"});
        let (t, st) = build_tracker_tables(Some(&r), Some(&c));
        assert_eq!(t.region.len(), 1);
        assert!(
            !t.region.contains_key(&7770900),
            "a location from a region this seed dropped must not appear"
        );
        assert!(matches!(st, TablesStatus::Armed { regions: 1, .. }));
    }

    #[test]
    fn junk_values_are_skipped_without_taking_the_table_down() {
        let r = json!({"Limgrave": [1u64, "not-an-id", -4], "Bad": 7});
        let (t, _) = build_tracker_tables(Some(&r), None);
        assert_eq!(t.region.len(), 1);
        assert_eq!(t.region.get(&1), Some(&"Limgrave".to_string()));
    }
}
