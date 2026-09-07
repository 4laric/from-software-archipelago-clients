//! BEDROCK-PROFILE slot_data contract test (er-logic half).
//!
//! Companion to `slot_data_fixture.rs`, which covers OUR apworld. This one drives the FOREIGN
//! (Bedrock / fswap `er`) profile: `tests/fixtures/slot_data_bedrock.json` is a real generated
//! seed, not hand-written keys. See `tests/fixtures/README.md` for provenance and how to refresh.
//!
//! What lives here is what er-logic owns: the options block, the version gate, the item maps, and
//! the BAKED region-lock table (`er_logic::region_lock::derive_region_locks`) that is the client's
//! only region enforcement on a seed like this one -- Bedrock's apworld emits neither
//! `areaLockFlags` nor `regionOpenFlags` even on a `world_logic: region_lock` roll. The matt key
//! resolver, the foreign `goal` key and `region::parse` live in the client crate, and their half of
//! this fixture is exercised by `crates/eldenring-archipelago/tests/slot_data_bedrock.rs`.
//!
//! Counts below are MEASURED on the recorded seed. They are exact on purpose: a change to a shared
//! parser that silently drops the Bedrock path shows up here as a number, not as a shrug.

use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn bedrock_slot_data() -> serde_json::Value {
    let p = fixtures().join("slot_data_bedrock.json");
    let raw = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("bedrock fixture {} unreadable: {e}", p.display()));
    serde_json::from_str(&raw).expect("bedrock fixture is not valid JSON")
}

fn bedrock_item_names() -> Vec<String> {
    let p = fixtures().join("slot_data_bedrock_items.json");
    let raw = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("bedrock item-name fixture {} unreadable: {e}", p.display()));
    serde_json::from_str(&raw).expect("bedrock item-name fixture is not valid JSON")
}

#[test]
fn bedrock_slot_data_shape_is_what_the_client_expects() {
    let sd = bedrock_slot_data();

    // The foreign world names itself the same way ours does.
    assert!(
        sd.get("options").and_then(|o| o.as_object()).is_some(),
        "bedrock slot_data.options missing or not an object"
    );
    let versions = sd
        .get("versions")
        .and_then(|v| v.as_str())
        .expect("bedrock slot_data.versions missing / not a string");
    assert!(!versions.is_empty(), "empty versions gate");

    // Tolerant option reads must not panic on a foreign options block (different key names,
    // different value shapes -- e.g. `world_logic` is an int enum over there).
    let _ = er_logic::options::parse_dlc(&sd);
    let _ = er_logic::options::parse_death_link(&sd);
    let _ = er_logic::options::parse_death_link_amnesty(&sd);
    let _ = er_logic::progressive::parse(&sd);

    // This is the region_lock roll: `world_logic` 0 = region_lock, 1 = open_world.
    assert_eq!(
        sd["options"]["world_logic"].as_i64(),
        Some(0),
        "recorded fixture is meant to be the world_logic: region_lock seed"
    );

    // apIdsToItemIds: stringified-int keys -> int values (core.rs item_map shape).
    let map = sd
        .get("apIdsToItemIds")
        .and_then(|v| v.as_object())
        .expect("apIdsToItemIds missing / not an object");
    assert_eq!(map.len(), 3083, "apIdsToItemIds row count moved");
    for (k, v) in map {
        k.parse::<i64>()
            .unwrap_or_else(|_| panic!("apIdsToItemIds key '{k}' not an int"));
        v.as_i64()
            .unwrap_or_else(|| panic!("apIdsToItemIds['{k}'] not an int"));
    }

    // itemCounts: same key shape, values >= 1 (core.rs clamps with .max(1)).
    let counts = sd
        .get("itemCounts")
        .and_then(|v| v.as_object())
        .expect("itemCounts missing / not an object");
    assert_eq!(counts.len(), 523, "itemCounts row count moved");
    for (k, v) in counts {
        k.parse::<i64>()
            .unwrap_or_else(|_| panic!("itemCounts key '{k}' not an int"));
        let n = v
            .as_i64()
            .unwrap_or_else(|| panic!("itemCounts['{k}'] not an int"));
        assert!(n >= 1, "itemCounts['{k}'] = {n} (< 1)");
    }

    // The tables our own profile speaks and this one does not. Their ABSENCE is load-bearing:
    // `region::foreign_seed_without_region_keys` keys the baked fallback off exactly this, and
    // `goal::parse` only consults the foreign `goal` key when `goalLocations` is missing.
    for absent in [
        "locationFlags",
        "areaLockFlags",
        "regionOpenFlags",
        "goalLocations",
        "lockGrantItems",
        "naturalKeyTriggers",
        "graceItems",
    ] {
        assert!(
            sd.get(absent).is_none(),
            "bedrock slot_data now emits `{absent}` -- the client's foreign-profile fallbacks key \
             off its absence; re-measure this fixture and the fallbacks before relaxing this"
        );
    }
}

#[test]
fn bedrock_seed_item_names_derive_the_baked_region_lock_table() {
    // Bedrock's region_lock seed mints "<Region> Lock" items but ships NO region geometry in
    // slot_data, so the ONLY thing that can gate those regions is the generated
    // `er_logic::region_locks` table driven by the item NAMES the client learns over the network.
    // This is the pure half of `region::prepare_baked_fallback`.
    let names = bedrock_item_names();
    assert_eq!(names.len(), 2449, "bedrock item-name roster size moved");

    let locks: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| n.ends_with(" Lock"))
        .collect();
    assert_eq!(
        locks.len(),
        27,
        "bedrock region_lock seed lock-item count moved: {locks:?}"
    );

    let derived = er_logic::region_lock::derive_region_locks(names.iter().map(String::as_str));
    assert!(
        !derived.is_empty(),
        "baked fallback derived NOTHING from a Bedrock region_lock seed -- that seed would be \
         completely ungated; unknown={:?} ungateable={:?}",
        derived.unknown,
        derived.ungateable
    );
    assert!(
        !derived.open_flags.is_empty() && !derived.ranges.is_empty(),
        "baked fallback derived open flags or kick ranges but not both: {} flags, {} ranges",
        derived.open_flags.len(),
        derived.ranges.len()
    );

    // Every lock name the seed mints is accounted for: mapped, or explicitly binned as
    // unknown/ungateable. Nothing may be silently dropped.
    let accounted = derived.open_flags.len() + derived.unknown.len() + derived.ungateable.len();
    assert_eq!(
        accounted,
        locks.len(),
        "lock names unaccounted for: {} mapped + {} unknown + {} ungateable != {} minted \
         (unknown={:?}, ungateable={:?})",
        derived.open_flags.len(),
        derived.unknown.len(),
        derived.ungateable.len(),
        locks.len(),
        derived.unknown,
        derived.ungateable
    );

    // Arming: the fallback goes live on the first RECEIVED scoped lock, and stays cold until then.
    let none = std::collections::HashSet::new();
    assert!(
        !er_logic::region_lock::fallback_armed(&derived, &none),
        "baked fallback armed with nothing received"
    );
    let one: std::collections::HashSet<String> = derived
        .open_flags
        .first()
        .map(|(n, _)| n.clone())
        .into_iter()
        .collect();
    assert!(
        er_logic::region_lock::fallback_armed(&derived, &one),
        "baked fallback did not arm on its own first mapped lock name ({one:?})"
    );
}
