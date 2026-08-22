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

/// `options.trap_link` (int-or-bool).
pub fn parse_trap_link(slot_data: &Value) -> bool {
    parse_bool_option(slot_data, "trap_link")
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

/// `options.no_equip_load` as a plain on/off. Same option name on both our apworld and
/// Bedrock/fswap's.
///
/// 🛑 THIS IS THE LOSSY READ. Since er-archipelago#548 the key carries a ROLL MODE, not a bool
/// (`0` off / `1` light / `2` medium), and [`crate::equip_load::parse`] is the one that can tell
/// `light` from `medium`. This wrapper survives for callers that only need "is the feature on",
/// and it DELEGATES rather than re-deriving so the two can never disagree about what an
/// unrecognised value means.
pub fn parse_no_equip_load(slot_data: &Value) -> bool {
    crate::equip_load::parse(slot_data).mode.is_on()
}

/// `options.auto_equip` (int-or-bool). Same option name on both our apworld and Bedrock/fswap's.
/// When on, a received weapon is auto-equipped into a hand slot (see [`crate::auto_equip`]).
pub fn parse_auto_equip(slot_data: &Value) -> bool {
    parse_bool_option(slot_data, "auto_equip")
}

/// `options.merchant_bells_on_talk` (int-or-bool). When on, opening a merchant's buy menu sets the
/// event flag the Twin Maiden Husks would have set had you handed them that merchant's Bell
/// Bearing, so their wares are available at the hub from then on (see [`crate::merchant_bells`]).
/// An absent key parses false, which is the off default -- so a seed rolled by an older apworld,
/// or by a foreign one, is unaffected.
pub fn parse_merchant_bells_on_talk(slot_data: &Value) -> bool {
    parse_bool_option(slot_data, "merchant_bells_on_talk")
}

/// `options.no_fall_damage` (int-or-bool). When on, the player never takes fall damage (the
/// spirit-spring `fallDamageRate=0` trick, applied permanently -- see [`crate::no_fall_damage`]).
pub fn parse_no_fall_damage(slot_data: &Value) -> bool {
    parse_bool_option(slot_data, "no_fall_damage")
}

/// `options.locked_abilities`: a JSON array of ability names (`["roll","r1"]`) -> the
/// `er_logic::ability_lock` u8 locked set. Tolerant like the rest of this module -- a non-array,
/// a non-string element, or an unknown name is skipped, never fatal, so a garbage option is simply
/// inert rather than failing the connection. Absent/empty => 0 (the client then falls back to the
/// `ER_ABILITY_LOCK_TEST` env var, or does nothing).
pub fn parse_ability_lock(slot_data: &Value) -> u8 {
    let Some(arr) = slot_data
        .get("options")
        .and_then(|o| o.get("locked_abilities"))
        .and_then(|v| v.as_array())
    else {
        return 0;
    };
    arr.iter()
        .filter_map(|v| v.as_str())
        .filter_map(crate::ability_lock::Ability::from_name)
        .fold(0u8, |set, a| set | a.bit())
}

/// `abilityUnlockItems`: a JSON object {ap_item_id_string: ability_name} (progressive ability lock,
/// er-archipelago#980) -> a map from AP item id to the `ability_lock` ability BIT. Receiving one of
/// these ids unlocks that ability. Tolerant: a non-object, an unparseable id, or an unknown ability
/// name is skipped, never fatal, so a garbage map is simply inert. Empty/absent => empty map (the
/// client then never drives the stream unlock path and the abilities stay statically locked).
pub fn parse_ability_unlock_items(slot_data: &Value) -> std::collections::HashMap<i64, u8> {
    let mut out = std::collections::HashMap::new();
    let Some(obj) = slot_data
        .get("abilityUnlockItems")
        .and_then(|v| v.as_object())
    else {
        return out;
    };
    for (id_str, name) in obj {
        let (Ok(id), Some(a)) = (
            id_str.parse::<i64>(),
            name.as_str()
                .and_then(crate::ability_lock::Ability::from_name),
        ) else {
            continue;
        };
        out.insert(id, a.bit());
    }
    out
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
    fn trap_link_parses_independently() {
        let sd = json!({ "options": { "death_link": 0, "trap_link": true } });
        assert!(!parse_death_link(&sd));
        assert!(parse_trap_link(&sd));
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

    #[test]
    fn auto_equip_parses() {
        assert!(parse_auto_equip(&json!({ "options": { "auto_equip": 1 } })));
        assert!(parse_auto_equip(
            &json!({ "options": { "auto_equip": true } })
        ));
        assert!(!parse_auto_equip(
            &json!({ "options": { "auto_equip": 0 } })
        ));
        assert!(!parse_auto_equip(&json!({ "options": {} })));
    }

    #[test]
    fn no_fall_damage_parses() {
        assert!(parse_no_fall_damage(
            &json!({ "options": { "no_fall_damage": 1 } })
        ));
        assert!(parse_no_fall_damage(
            &json!({ "options": { "no_fall_damage": true } })
        ));
        assert!(!parse_no_fall_damage(
            &json!({ "options": { "no_fall_damage": 0 } })
        ));
        assert!(!parse_no_fall_damage(&json!({ "options": {} })));
    }

    #[test]
    fn ability_lock_folds_names_and_is_tolerant() {
        use crate::ability_lock::Ability;
        let sd = json!({ "options": { "locked_abilities": ["roll", "r1", "l2"] } });
        assert_eq!(
            parse_ability_lock(&sd),
            Ability::Roll.bit() | Ability::R1.bit() | Ability::L2.bit()
        );
        // unknown names + non-strings are skipped, not fatal
        let messy = json!({ "options": { "locked_abilities": ["roll", "nope", 7, "jump"] } });
        assert_eq!(
            parse_ability_lock(&messy),
            Ability::Roll.bit() | Ability::Jump.bit()
        );
        // absent / wrong-typed => 0
        assert_eq!(parse_ability_lock(&json!({ "options": {} })), 0);
        assert_eq!(
            parse_ability_lock(&json!({ "options": { "locked_abilities": "roll" } })),
            0
        );
    }

    #[test]
    fn ability_unlock_items_maps_ids_to_bits_and_is_tolerant() {
        use crate::ability_lock::Ability;
        let sd = json!({ "abilityUnlockItems": { "7900002": "roll", "7900003": "r1" } });
        let m = parse_ability_unlock_items(&sd);
        assert_eq!(m.get(&7900002).copied(), Some(Ability::Roll.bit()));
        assert_eq!(m.get(&7900003).copied(), Some(Ability::R1.bit()));
        // garbage id / unknown ability / wrong container -> skipped, never fatal
        let messy = json!({ "abilityUnlockItems": { "nope": "roll", "7900004": "wat", "7900005": "jump" } });
        let mm = parse_ability_unlock_items(&messy);
        assert_eq!(mm.len(), 1);
        assert_eq!(mm.get(&7900005).copied(), Some(Ability::Jump.bit()));
        assert!(parse_ability_unlock_items(&json!({ "abilityUnlockItems": [] })).is_empty());
        assert!(parse_ability_unlock_items(&json!({})).is_empty());
    }
}
