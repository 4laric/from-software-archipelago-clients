//! Tolerant slot_data option parsing, extracted from `net.rs` `build_slot_config`.
//!
//! The apworld ships booleans as ints (`enable_dlc: 1`), but a real JSON `true` must also work — a
//! strict typed deserialize would FAIL THE CONNECTION on the int form. These helpers accept either
//! and default to `false`, so a missing/garbage option is simply inert.

use serde_json::Value;

/// Read `options.<key>` as a bool, accepting JSON bool OR int (nonzero = true). Absent/garbage =>
/// false.
pub fn parse_bool_option(slot_data: &Value, key: &str) -> bool {
    slot_data
        .get("options")
        .and_then(|o| o.get(key))
        .map(|v| match v {
            Value::Bool(b) => *b,
            Value::Number(n) => n.as_i64().map(|i| i != 0).unwrap_or(false),
            _ => false,
        })
        .unwrap_or(false)
}

/// `options.enable_dlc` (int-or-bool).
pub fn parse_dlc(slot_data: &Value) -> bool {
    parse_bool_option(slot_data, "enable_dlc")
}

/// `options.death_link` (int-or-bool).
pub fn parse_death_link(slot_data: &Value) -> bool {
    parse_bool_option(slot_data, "death_link")
}

/// Weapon/spell requirement removal, under EITHER apworld's option name.
///
/// Our apworld emits `options.no_weapon_requirements`; Bedrock's fswap apworld emits
/// `options.remove_weapon_and_spell_requirements`. Same client feature — either name enables it.
pub fn parse_no_weapon_reqs(slot_data: &Value) -> bool {
    parse_bool_option(slot_data, "no_weapon_requirements")
        || parse_bool_option(slot_data, "remove_weapon_and_spell_requirements")
}

/// Regular smithing-stone upgrade-cost flatten cap, in stones/level (0 = off, 1..4 = cap).
///
/// Our apworld emits `options.flatten_regular_upgrades` as that INT directly. Bedrock's fswap
/// apworld emits `options.reduce_non_somber_upgrade_cost` as a BOOL toggle meaning "one stone per
/// weapon level" == cap 1. Our int wins when present and non-zero; otherwise fall back to Bedrock's
/// toggle mapped to cap 1. Absent/garbage => 0 (off).
pub fn parse_flatten_cap(slot_data: &Value) -> i64 {
    let own = slot_data
        .pointer("/options/flatten_regular_upgrades")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if own != 0 {
        own
    } else if parse_bool_option(slot_data, "reduce_non_somber_upgrade_cost") {
        1
    } else {
        0
    }
}

/// `options.no_equip_load` (int-or-bool). Same option name on both our apworld and Bedrock/fswap's.
pub fn parse_no_equip_load(slot_data: &Value) -> bool {
    parse_bool_option(slot_data, "no_equip_load")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_int_and_bool_forms() {
        assert!(parse_dlc(&json!({ "options": { "enable_dlc": 1 } })));
        assert!(parse_dlc(&json!({ "options": { "enable_dlc": true } })));
        assert!(!parse_dlc(&json!({ "options": { "enable_dlc": 0 } })));
        assert!(!parse_dlc(&json!({ "options": { "enable_dlc": false } })));
    }

    #[test]
    fn absent_option_or_options_block_is_false() {
        assert!(!parse_dlc(&json!({ "options": {} })));
        assert!(!parse_dlc(&json!({ "seed": "abc" })));
    }

    #[test]
    fn death_link_parses_independently() {
        let sd = json!({ "options": { "enable_dlc": 0, "death_link": 1 } });
        assert!(!parse_dlc(&sd));
        assert!(parse_death_link(&sd));
    }

    #[test]
    fn garbage_value_is_inert_not_fatal() {
        assert!(!parse_bool_option(
            &json!({ "options": { "x": "yes" } }),
            "x"
        ));
        assert!(!parse_bool_option(
            &json!({ "options": { "x": [1, 2] } }),
            "x"
        ));
    }

    #[test]
    fn no_weapon_reqs_accepts_either_apworld_name() {
        // our apworld's name
        assert!(parse_no_weapon_reqs(
            &json!({ "options": { "no_weapon_requirements": 1 } })
        ));
        // bedrock/fswap's name
        assert!(parse_no_weapon_reqs(
            &json!({ "options": { "remove_weapon_and_spell_requirements": true } })
        ));
        // neither present
        assert!(!parse_no_weapon_reqs(&json!({ "options": {} })));
    }

    #[test]
    fn flatten_cap_our_int_wins_then_bedrock_toggle_maps_to_one() {
        // our int form passes through unchanged
        assert_eq!(
            parse_flatten_cap(&json!({ "options": { "flatten_regular_upgrades": 3 } })),
            3
        );
        // bedrock toggle (int or bool) -> cap 1
        assert_eq!(
            parse_flatten_cap(&json!({ "options": { "reduce_non_somber_upgrade_cost": 1 } })),
            1
        );
        assert_eq!(
            parse_flatten_cap(&json!({ "options": { "reduce_non_somber_upgrade_cost": true } })),
            1
        );
        // off / absent
        assert_eq!(parse_flatten_cap(&json!({ "options": {} })), 0);
        assert_eq!(
            parse_flatten_cap(&json!({ "options": { "flatten_regular_upgrades": 0 } })),
            0
        );
    }

    #[test]
    fn no_equip_load_parses() {
        assert!(parse_no_equip_load(
            &json!({ "options": { "no_equip_load": 1 } })
        ));
        assert!(parse_no_equip_load(
            &json!({ "options": { "no_equip_load": true } })
        ));
        assert!(!parse_no_equip_load(
            &json!({ "options": { "no_equip_load": 0 } })
        ));
        assert!(!parse_no_equip_load(&json!({ "options": {} })));
    }
}
