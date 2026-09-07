//! BEDROCK-PROFILE slot_data contract test (client-crate half).
//!
//! Lives in `src/` rather than `tests/` because this crate is `crate-type = ["cdylib"]` with
//! private modules -- an integration test could not reach `key_resolver` / `goal` / `region`.
//!
//! The parsers that exist ONLY to serve Bedrock's apworld live in this crate, not in er-logic:
//! `key_resolver` (matt slot keys), the foreign `goal` key in `goal.rs`, and `region.rs`'s
//! foreign-seed detection. Until now their only end-to-end proof was the 2026-07-13 playtest and
//! a handful of hand-written keys in unit tests. This drives all of them from a REAL generated
//! seed: `../er-logic/tests/fixtures/slot_data_bedrock.json`.
//!
//! Provenance and refresh instructions: `crates/er-logic/tests/fixtures/README.md`.
//!
//! The counts are exact. If Bedrock's world moves and the numbers change, that is a REVIEWABLE
//! event -- re-record the fixture, and read the delta before you update the constants.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::{goal, key_resolver, region};

/// Fixtures live in er-logic (one place for both halves); this crate reaches across the workspace.
fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../er-logic/tests/fixtures")
}

fn bedrock_slot_data() -> serde_json::Value {
    let p = fixtures().join("slot_data_bedrock.json");
    let raw = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("bedrock fixture {} unreadable: {e}", p.display()));
    serde_json::from_str(&raw).expect("bedrock fixture is not valid JSON")
}

/// The shipped shop table, byte-copied from the world repo's
/// `greenfield/eldenring/shoplineup_flags.json` so this test exercises the real
/// `load_shoplineup_flags` reader against the real rows, not a stub.
fn shop_rows() -> HashMap<u32, u32> {
    key_resolver::load_shoplineup_flags(&fixtures().join("shoplineup_flags.json"))
}

#[test]
fn matt_slot_keys_resolve_the_measured_number_of_locations() {
    let sd = bedrock_slot_data();

    let keys = sd["locationIdsToKeys"]
        .as_object()
        .expect("locationIdsToKeys missing / not an object");
    assert_eq!(keys.len(), 4907, "bedrock location key count moved");

    let loc_flags = key_resolver::location_flags_from_keys(&sd);
    assert_eq!(
        loc_flags.len(),
        4423,
        "matt-key resolver: locations resolved to an acquisition flag moved"
    );
    // A flag of 0 would be polled forever and never fire.
    assert!(
        loc_flags.values().all(|&f| f != 0),
        "matt-key resolver emitted a zero flag"
    );
    // Distinct flags matter more than the raw count: a collapse (many locations onto one flag)
    // is exactly the class of bug the targets-first shop fix was written for.
    let distinct: HashSet<u32> = loc_flags.values().copied().collect();
    assert_eq!(
        distinct.len(),
        4040,
        "matt-key resolver: DISTINCT acquisition flags moved (a drop here means locations \
         collapsed onto shared flags)"
    );
}

#[test]
fn shop_slots_resolve_through_the_shipped_shoplineup_table() {
    let sd = bedrock_slot_data();

    let rows = shop_rows();
    assert_eq!(
        rows.len(),
        833,
        "shipped shoplineup_flags.json row count moved -- refresh the copy in \
         crates/er-logic/tests/fixtures/ from the world repo"
    );

    let shop_flags = key_resolver::shop_flags_from_keys(&sd, &rows);
    assert_eq!(
        shop_flags.len(),
        484,
        "shop rows resolved to stock flags moved (2026-07-13 playtest measured 476 of 583 on the \
         then-current world; the DLC shop targets landed 2026-08-31 and every shop slot in this \
         seed now resolves)"
    );
    let distinct: HashSet<u32> = shop_flags.values().copied().collect();
    assert_eq!(
        distinct.len(),
        483,
        "DISTINCT shop stock flags moved -- a large drop means the merchant-base-row collapse \
         regressed (see the TARGETS FIRST note in key_resolver.rs)"
    );

    // The two maps are disjoint by construction; core.rs merges them into ONE poll map and a
    // collision would mean one of the two is wrong about who owns the slot.
    let loc_flags = key_resolver::location_flags_from_keys(&sd);
    let overlap: Vec<i64> = shop_flags
        .keys()
        .filter(|k| loc_flags.contains_key(k))
        .copied()
        .collect();
    assert!(
        overlap.is_empty(),
        "flag-resolved and shop-resolved location sets overlap: {overlap:?}"
    );

    // Together they cover every location the seed ships.
    assert_eq!(
        loc_flags.len() + shop_flags.len(),
        4907,
        "flagged + shop-resolved no longer covers every bedrock location"
    );
}

#[test]
fn foreign_goal_key_yields_a_non_empty_goal() {
    let sd = bedrock_slot_data();
    let loc_flags = key_resolver::location_flags_from_keys(&sd);

    // Bedrock emits no `goalLocations`; `goal` carries boss DEFEAT FLAGS directly. Without the
    // foreign fallback in goal.rs this parses empty and the slot is silently unwinnable.
    let cfg = goal::parse(&sd, &loc_flags);
    assert!(
        !cfg.is_empty(),
        "bedrock goal parsed EMPTY -- this slot could never send Goal"
    );
    assert_eq!(
        cfg.flag_goals,
        vec![20010800u32, 19000800],
        "bedrock goal boss flags moved (Final Boss + DLC Final Boss)"
    );
    assert!(
        cfg.checked_goals.is_empty(),
        "bedrock goal should be purely flag-detected"
    );
    assert!(cfg.rune_goals.is_empty() && cfg.runes_required == 0);
    assert!(
        cfg.item_goals.is_empty(),
        "bedrock emits no goalRequiredItems"
    );

    // Not met with nothing set; met once both flags are on.
    assert!(!goal::is_met(&cfg, |_| false, |_| false, |_| false));
    assert!(goal::is_met(&cfg, |_| true, |_| false, |_| false));
}

#[test]
fn region_parse_degrades_to_the_baked_fallback_on_a_bedrock_region_lock_seed() {
    let sd = bedrock_slot_data();

    // The fixture is a `world_logic: region_lock` roll, and Bedrock's apworld STILL emits no
    // region geometry. That is the whole reason the baked fallback exists.
    assert!(
        region::foreign_seed_without_region_keys(&sd),
        "bedrock seed now speaks a region-lock key -- the baked fallback would stop applying"
    );

    let mut cfg = region::parse(&sd);
    assert!(cfg.area_lock_flags.is_empty(), "areaLockFlags from nowhere");
    assert!(
        cfg.region_open_flags.is_empty(),
        "regionOpenFlags from nowhere"
    );
    assert_eq!(
        cfg.lock_grant_items.len(),
        0,
        "lockGrantItems parsed non-empty -- bedrock does not emit this key"
    );
    assert_eq!(
        cfg.natural_key_triggers.len(),
        0,
        "naturalKeyTriggers parsed non-empty -- bedrock does not emit this key"
    );
    assert_eq!(cfg.random_start_done_flag, 0);
    assert_eq!(cfg.leyndell_runes_required, 0);
    assert!(
        cfg.baked_fallback.is_none(),
        "parse must not arm the fallback itself"
    );

    // The client learns item NAMES over the network; feed it the seed's real roster.
    let names: Vec<String> = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("slot_data_bedrock_items.json"))
            .expect("bedrock item-name fixture unreadable"),
    )
    .expect("bedrock item-name fixture is not valid JSON");
    region::prepare_baked_fallback(&mut cfg, names.iter().map(String::as_str));
    let derived = cfg
        .baked_fallback
        .as_ref()
        .expect("baked fallback not prepared from a seed that mints 27 region locks");
    assert_eq!(
        derived.open_flags.len(),
        18,
        "baked fallback mapped-lock count moved"
    );
    assert_eq!(
        derived.ranges.len(),
        72,
        "baked fallback kick-range count moved"
    );

    // MEASURED COVERAGE GAP, not a bug this test papers over. Bedrock's region granularity is
    // finer than ours in nine places, so nine of his 27 lock items name no baked region and are
    // dropped (logged, never fatal): those regions go UNGATED on a Bedrock region_lock seed and
    // the player can walk into them. Pinning the exact list here means the gap is visible, and
    // that closing it -- or Bedrock renaming a lock -- shows up as a reviewable diff.
    assert_eq!(
        derived.unknown,
        vec![
            "Ashen Lock",
            "Charo's Lock",
            "Deeproot & Ainsel Main Lock",
            "Ellac Lock",
            "Recluses' Lock",
            "Sewer Lock",
            "Siofra & Ainsel Lock",
            "Stone Coffin Lock",
            "Volcano Lock",
        ],
        "the set of Bedrock lock names this client cannot map moved"
    );
    assert!(
        derived.ungateable.is_empty(),
        "baked regions matched but with no open flag: {:?}",
        derived.ungateable
    );
    assert_eq!(
        derived.open_flags.len() + derived.unknown.len() + derived.ungateable.len(),
        27,
        "lock names unaccounted for -- something was dropped silently"
    );

    // Cold until a scoped lock is actually received; then it merges into the live fields.
    let first = derived.open_flags[0].0.clone();
    assert!(!region::tick_baked_fallback(&mut cfg, &HashSet::new()));
    let received: HashSet<String> = std::iter::once(first).collect();
    assert!(
        region::tick_baked_fallback(&mut cfg, &received),
        "baked fallback did not arm on a received scoped lock"
    );
    assert!(
        cfg.baked_fallback.is_none(),
        "arming must consume the stash"
    );
    assert_eq!(cfg.region_open_flags.len(), 18);
    assert_eq!(cfg.area_lock_flags.len(), 72);
}
