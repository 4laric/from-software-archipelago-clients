//! Real slot-data snapshots for every audited 0.5.x version; counts are recorded in the manifest.
#[test]
fn every_05_seed_contract_parses_without_losing_regions() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../er-logic/tests/fixtures/legacy_05");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest.as_array().unwrap().len(), 9);
    for release in manifest.as_array().unwrap() {
        assert_eq!(release["fixtures"].as_array().unwrap().len(), 2);
        for fixture in release["fixtures"].as_array().unwrap() {
            let sd: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(root.join(fixture["file"].as_str().unwrap())).unwrap(),
            )
            .unwrap();
            assert_eq!(sd["versions"], fixture["versions"]);
            assert!(er_logic::client_features::is_legacy_contract_compatible(
                sd["versions"].as_str().unwrap()
            ));
            assert_eq!(
                crate::contract_gen::validate(&sd),
                ["MISSING required key 'profile'"]
            );
            assert!(
                !crate::profile::select(&sd)
                    .unwrap()
                    .profile
                    .uses_key_resolver()
            );
            assert_eq!(
                sd["locationFlags"].as_object().unwrap().len() as u64,
                fixture["locations"].as_u64().unwrap()
            );
            let cfg = crate::region::parse(&sd);
            assert_eq!(
                cfg.region_open_flags.len() as u64,
                fixture["locks"].as_u64().unwrap()
            );
            assert_eq!(
                cfg.area_lock_flags.len() as u64,
                fixture["areas"].as_u64().unwrap()
            );
            for (name, flag) in sd["regionOpenFlags"].as_object().unwrap() {
                assert_eq!(cfg.region_open_flags[name] as u64, flag.as_u64().unwrap());
            }
            let (tables, status) = er_logic::tracker_tables::build_tracker_tables(
                sd.get("locationRegions"),
                sd.get("regionCoarseKeys"),
            );
            assert!(matches!(
                status,
                er_logic::tracker_tables::TablesStatus::Armed { .. }
            ));
            for (region, ids) in sd["locationRegions"].as_object().unwrap() {
                for id in ids.as_array().unwrap() {
                    assert_eq!(&tables.region[&id.as_u64().unwrap()], region);
                }
            }
            assert!(
                er_logic::client_features::unsupported(
                    &er_logic::client_features::required_from_slot_data(&sd)
                )
                .is_empty()
            );
            println!("{}: {}", fixture["file"], status.describe());
        }
    }
}
