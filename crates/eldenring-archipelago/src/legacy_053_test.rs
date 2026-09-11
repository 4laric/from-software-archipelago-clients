#[test]
fn legacy_053_generated_profiles() {
    for name in ["SlotDataFixtureDefault", "SlotDataFixtureRich"] {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../er-logic/tests/fixtures/legacy_053/{name}.json")),
        )
        .unwrap();
        let sd: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(er_logic::client_features::is_legacy_contract_compatible(
            sd["versions"].as_str().unwrap()
        ));
        assert_eq!(sd["locationFlags"].as_object().unwrap().len(), 4909);
        assert_eq!(sd["regionOpenFlags"].as_object().unwrap().len(), 29);
        assert_eq!(sd["areaLockFlags"].as_array().unwrap().len(), 116);
        let errors = crate::contract_gen::validate(&sd);
        println!("{name}: validation={errors:?}");
        assert_eq!(errors, ["MISSING required key 'profile'"]);
        let selection = crate::profile::select(&sd).unwrap();
        assert!(!selection.profile.uses_key_resolver());
        let cfg = crate::region::parse(&sd);
        assert_eq!(
            cfg.region_open_flags.len(),
            sd["regionOpenFlags"].as_object().unwrap().len()
        );
        assert_eq!(
            cfg.area_lock_flags.len(),
            sd["areaLockFlags"].as_array().unwrap().len()
        );
        let required = er_logic::client_features::required_from_slot_data(&sd);
        assert!(er_logic::client_features::unsupported(&required).is_empty());
        let (_, status) = er_logic::tracker_tables::build_tracker_tables(
            sd.get("locationRegions"),
            sd.get("regionCoarseKeys"),
        );
        println!("tracker: {}", status.describe());
        assert!(matches!(
            status,
            er_logic::tracker_tables::TablesStatus::Armed { .. }
        ));
        assert_eq!(er_logic::options::parse_death_link_amnesty(&sd), (1, 1));
        assert!(!er_logic::options::parse_bool_option(
            &sd,
            "scale_rune_rewards"
        ));
        assert!(!er_logic::options::parse_bool_option(
            &sd,
            "reveal_sweep_boss_names"
        ));
        println!(
            "locations={} locks={} area_locks={} features={required:?}",
            sd["locationFlags"].as_object().unwrap().len(),
            cfg.region_open_flags.len(),
            cfg.area_lock_flags.len()
        );
    }
}
