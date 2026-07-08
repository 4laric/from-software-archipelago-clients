// AUTO-GENERATED from eldenring_gf/contract.py -- do not edit by hand.
// The apworld<->client slot_data contract, mirrored so the client validates the same shapes.
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Any,
    Bool,
    BoolOrInt,
    Int,
    IntList,
    IntOrBool,
    ListvalIntMap,
    NestedGrants,
    Number,
    OptionsDict,
    ScalarIntMap,
    Str,
    StrMap,
    TripleList,
}

pub struct ContractKey {
    pub name: &'static str,
    pub shape: Shape,
    pub required: bool,
    pub greenfield: bool,
}

pub const CONTRACT: &[ContractKey] = &[
    ContractKey { name: "apIdsToItemIds", shape: Shape::ScalarIntMap, required: true, greenfield: true },
    ContractKey { name: "locationFlags", shape: Shape::ScalarIntMap, required: true, greenfield: true },
    ContractKey { name: "regionOpenFlags", shape: Shape::ScalarIntMap, required: true, greenfield: true },
    ContractKey { name: "options", shape: Shape::OptionsDict, required: true, greenfield: true },
    ContractKey { name: "regionSphereTargets", shape: Shape::ScalarIntMap, required: false, greenfield: true },
    ContractKey { name: "regionSphereTargetRanges", shape: Shape::TripleList, required: false, greenfield: true },
    ContractKey { name: "completionScalingBasis", shape: Shape::Int, required: false, greenfield: true },
    ContractKey { name: "areaLockFlags", shape: Shape::TripleList, required: false, greenfield: true },
    ContractKey { name: "lockRevealFlags", shape: Shape::ListvalIntMap, required: false, greenfield: true },
    ContractKey { name: "regionGraces", shape: Shape::ListvalIntMap, required: false, greenfield: true },
    ContractKey { name: "graceItems", shape: Shape::ScalarIntMap, required: false, greenfield: true },
    ContractKey { name: "startRegion", shape: Shape::Str, required: true, greenfield: true },
    ContractKey { name: "startGraces", shape: Shape::IntList, required: false, greenfield: true },
    ContractKey { name: "startItems", shape: Shape::IntList, required: false, greenfield: true },
    ContractKey { name: "reveal_all_maps", shape: Shape::Bool, required: false, greenfield: true },
    ContractKey { name: "goalLocations", shape: Shape::IntList, required: true, greenfield: true },
    ContractKey { name: "checkItemFlags", shape: Shape::ListvalIntMap, required: false, greenfield: true },
    ContractKey { name: "shopRowFlags", shape: Shape::ScalarIntMap, required: false, greenfield: true },
    ContractKey { name: "shopPreviewGoods", shape: Shape::ScalarIntMap, required: false, greenfield: true },
    ContractKey { name: "stoneswordVendorRow", shape: Shape::Int, required: false, greenfield: true },
    ContractKey { name: "dungeonSweepFlags", shape: Shape::ListvalIntMap, required: false, greenfield: true },
    ContractKey { name: "dungeonSweeps", shape: Shape::Any, required: false, greenfield: true },
    ContractKey { name: "sweepLockGates", shape: Shape::StrMap, required: false, greenfield: true },
    ContractKey { name: "progressiveGrants", shape: Shape::NestedGrants, required: false, greenfield: true },
    ContractKey { name: "death_link", shape: Shape::BoolOrInt, required: false, greenfield: true },
    ContractKey { name: "no_weapon_requirements", shape: Shape::BoolOrInt, required: false, greenfield: true },
    ContractKey { name: "enable_dlc", shape: Shape::BoolOrInt, required: false, greenfield: true },
    ContractKey { name: "completion_scaling", shape: Shape::IntOrBool, required: false, greenfield: true },
    ContractKey { name: "completion_scaling_floor", shape: Shape::Number, required: false, greenfield: true },
    ContractKey { name: "global_scadutree_blessing", shape: Shape::Int, required: false, greenfield: true },
    ContractKey { name: "versions", shape: Shape::Str, required: false, greenfield: true },
    ContractKey { name: "world_logic", shape: Shape::Str, required: false, greenfield: true },
    ContractKey { name: "region_count", shape: Shape::Any, required: false, greenfield: true },
    ContractKey { name: "ending_condition", shape: Shape::Any, required: false, greenfield: true },
    ContractKey { name: "great_runes_required", shape: Shape::Any, required: false, greenfield: true },
    ContractKey { name: "great_rune_items", shape: Shape::Any, required: false, greenfield: true },
    ContractKey { name: "bossLocations", shape: Shape::Any, required: false, greenfield: true },
    ContractKey { name: "bossLockItems", shape: Shape::Any, required: false, greenfield: true },
    ContractKey { name: "filler_foreign_localized", shape: Shape::Any, required: false, greenfield: true },
    ContractKey { name: "pool_builder", shape: Shape::Any, required: false, greenfield: true },
    ContractKey { name: "pool_builder_juice_added", shape: Shape::Any, required: false, greenfield: true },
    ContractKey { name: "pool_builder_intensity_floor", shape: Shape::Any, required: false, greenfield: true },
    ContractKey { name: "pool_builder_juice_candidates", shape: Shape::Any, required: false, greenfield: true },
    ContractKey { name: "pool_builder_juice_pct", shape: Shape::Any, required: false, greenfield: true },
    ContractKey { name: "locationIdsToKeys", shape: Shape::Any, required: true, greenfield: false },
    ContractKey { name: "itemCounts", shape: Shape::Any, required: true, greenfield: true },
    ContractKey { name: "naturalKeyTriggers", shape: Shape::Any, required: true, greenfield: false },
    ContractKey { name: "lockGrantItems", shape: Shape::Any, required: true, greenfield: false },
    ContractKey { name: "randomStartDoneFlag", shape: Shape::Any, required: true, greenfield: false },
    ContractKey { name: "randomStartWarpFlag", shape: Shape::Any, required: true, greenfield: false },
    ContractKey { name: "randomStartAreaId", shape: Shape::Any, required: true, greenfield: false },
    ContractKey { name: "randomStartGraceId", shape: Shape::Any, required: true, greenfield: false },
    ContractKey { name: "fogWalls", shape: Shape::Any, required: true, greenfield: false },
    ContractKey { name: "fogWallDebug", shape: Shape::Any, required: true, greenfield: false },
];

/// Declared sub-keys of the top-level `options` echo (validated when `options` is present).
pub const OPTIONS_SUBKEYS: &[ContractKey] = &[
    ContractKey { name: "death_link", shape: Shape::BoolOrInt, required: true, greenfield: true },
    ContractKey { name: "enable_dlc", shape: Shape::BoolOrInt, required: true, greenfield: true },
    ContractKey { name: "no_weapon_requirements", shape: Shape::BoolOrInt, required: true, greenfield: true },
    ContractKey { name: "completion_scaling", shape: Shape::IntOrBool, required: true, greenfield: true },
    ContractKey { name: "completion_scaling_floor", shape: Shape::Number, required: true, greenfield: true },
    ContractKey { name: "global_scadutree_blessing", shape: Shape::Int, required: true, greenfield: true },
    ContractKey { name: "auto_upgrade", shape: Shape::Int, required: true, greenfield: true },
    ContractKey { name: "flatten_regular_upgrades", shape: Shape::Int, required: true, greenfield: true },
];

fn is_int(v: &Value) -> bool { v.is_i64() || v.is_u64() }

fn shape_ok(shape: Shape, v: &Value) -> bool {
    match shape {
        Shape::ScalarIntMap => v.as_object().map_or(false, |o| o.values().all(is_int)),
        Shape::ListvalIntMap => v.as_object().map_or(false, |o| {
            o.values().all(|x| x.as_array().map_or(false, |a| a.iter().all(is_int)))
        }),
        Shape::StrMap => v.as_object().map_or(false, |o| o.values().all(|x| x.is_string())),
        Shape::TripleList => v.as_array().map_or(false, |a| {
            a.iter().all(|t| t.as_array().map_or(false, |t| t.len() == 3 && t.iter().all(is_int)))
        }),
        Shape::IntList => v.as_array().map_or(false, |a| a.iter().all(is_int)),
        Shape::Bool => v.is_boolean(),
        Shape::BoolOrInt => v.is_boolean() || v.as_i64().map_or(false, |n| n == 0 || n == 1),
        Shape::IntOrBool => v.is_boolean() || is_int(v),
        Shape::Int => is_int(v),
        Shape::Number => v.is_number(),
        Shape::Str => v.is_string(),
        Shape::NestedGrants => v.as_object().map_or(false, |o| {
            o.values().all(|l| l.as_array().map_or(false, |l| l.iter().all(|e| {
                e.get("goods").map_or(false, is_int)
                    && e.get("flags").and_then(|f| f.as_array())
                        .map_or(false, |f| f.iter().all(is_int))
            })))
        }),
        Shape::OptionsDict => v.is_object(),
        Shape::Any => true,
    }
}

/// Validate an assembled slot_data object against the greenfield contract. Returns the list of
/// problems (missing-required + shape mismatches, top-level and `options.*`); empty == clean.
/// Mirrors contract.py's missing/shape checks (unknown-key rejection stays gen-side only).
pub fn validate(sd: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for k in CONTRACT {
        if !k.greenfield { continue; }
        match sd.get(k.name) {
            None => if k.required { out.push(format!("MISSING required key '{}'", k.name)); },
            Some(v) => if !shape_ok(k.shape, v) {
                out.push(format!("SHAPE '{}' expected {:?}", k.name, k.shape));
            },
        }
    }
    if let Some(opts) = sd.get("options").and_then(|v| v.as_object()) {
        for k in OPTIONS_SUBKEYS {
            if !k.greenfield { continue; }
            match opts.get(k.name) {
                None => if k.required { out.push(format!("MISSING required sub-key 'options.{}'", k.name)); },
                Some(v) => if !shape_ok(k.shape, v) {
                    out.push(format!("SHAPE 'options.{}' expected {:?}", k.name, k.shape));
                },
            }
        }
    }
    out
}
