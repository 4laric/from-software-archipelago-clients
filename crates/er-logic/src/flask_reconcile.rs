//! flask_reconcile — pure, HISTORY-AGNOSTIC leveled flask state.
//!
//! The flask upgrade is a RECONCILED LEVELED state, NOT a consumed-goods grant (no ledger). Every
//! reconcile tick the Windows glue reads the character's CURRENT flask, computes the DESIRED flask
//! from how many "Progressive Flask Upgrade" AP items have been received (`received_count`), and
//! writes the HIGHER of the two. That makes it idempotent, upward-only, and self-healing across
//! reconnect / save-load with no per-item bookkeeping: AP replays the whole received set on connect,
//! so `received_count` is stable and the desired state is a pure function of that count.
//!
//! CHARGES ONLY. The flask charge ALLOCATION (`max_hp_flask + max_fp_flask`, Crimson + Cerulean;
//! vanilla cap [`MAX_CHARGES`] = 14) is the one axis this module reconciles -- a single-field direct
//! write, confirmed safe. POTENCY is NOT handled here: it is delivered as granted Sacred Tears via
//! `progressiveGrants` (the player upgrades potency at a grace the vanilla way). An earlier build
//! raised potency by an in-place flask item-id swap (`base + L*2`); that CTD'd on death -- ER mirrors
//! the flask tier across the inventory entry, the equipped/quickslot reference, AND the global GaItem,
//! and death's refill crashed on the half-updated state (archipelago20260719.log) -- so it was
//! removed. The ladder still CARRIES a `potency` field (documentation / the gen derives the tear
//! schedule from it); this module ignores it.
//!
//! This module is PURE (only `serde_json` for the tolerant slot_data parse): it computes the desired
//! rung and the upward-only CHARGE delta. The live read/write of `PlayerGameData` lives in the
//! Windows-only glue (`eldenring-archipelago/src/flask.rs`).

use serde_json::Value;

/// Vanilla total flask-charge cap (`max_hp_flask + max_fp_flask`).
pub const MAX_CHARGES: u32 = 14;
/// Vanilla max flask potency level. Retained only to CLAMP the parsed (documentation) `potency`
/// field; the client no longer acts on the potency axis (it is delivered via granted Sacred Tears).
pub const MAX_POTENCY: u32 = 12;

/// One rung of the flask ladder: the CUMULATIVE target after receiving `(i+1)` upgrade items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlaskTarget {
    /// Total charge allocation target (`2..=14`).
    pub charges: u32,
    /// Potency level target (`0..=12`).
    pub potency: u32,
}

/// Parse `flaskLadder` from slot_data. Tolerant, in the spirit of [`crate::progressive::parse`]: an
/// ordered array of `{"charges": int, "potency": int}` rungs where entry `i` is the cumulative
/// target after receiving `(i+1)` upgrades. Absent / non-array / empty => empty vec (feature OFF, a
/// hard no-op). A non-object rung is skipped; a rung missing a field defaults that field to 0; both
/// fields are clamped to the vanilla caps.
///
/// The contract promises a monotonic non-decreasing ladder, but this parser does NOT enforce or
/// reorder it — [`reconcile`] is upward-only against the LIVE flask, so even a malformed
/// (non-monotonic) ladder can never LOWER the player's flask.
pub fn parse(slot_data: &Value) -> Vec<FlaskTarget> {
    let Some(arr) = slot_data.get("flaskLadder").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in arr {
        let Some(obj) = e.as_object() else {
            continue;
        };
        let charges = obj.get("charges").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let potency = obj.get("potency").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        out.push(FlaskTarget {
            charges: charges.min(MAX_CHARGES),
            potency: potency.min(MAX_POTENCY),
        });
    }
    out
}

/// The desired flask target after receiving `received_count` "Progressive Flask Upgrade" items.
/// HISTORY-AGNOSTIC: a pure function of the COUNT (order-independent, reconnect-stable). `None`
/// (feature no-op) when the ladder is empty OR the count is 0. A count past the ladder end clamps to
/// the LAST rung (`min(count, len) - 1`).
pub fn desired(ladder: &[FlaskTarget], received_count: usize) -> Option<FlaskTarget> {
    if ladder.is_empty() || received_count == 0 {
        return None;
    }
    let idx = received_count.min(ladder.len()) - 1;
    Some(ladder[idx])
}

/// The number of charges to ADD to reach the ladder's charge target from `current_charges`. Upward
/// only (never negative), clamped to [`MAX_CHARGES`]: `0` when already at/above the target. This is
/// the ENTIRE reconcile now -- potency is delivered via granted Sacred Tears, not computed here.
pub fn charge_deficit(current_charges: u32, desired: FlaskTarget) -> u32 {
    let target = current_charges.max(desired.charges).min(MAX_CHARGES);
    target.saturating_sub(current_charges)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ladder() -> Vec<FlaskTarget> {
        // A representative monotonic ladder: charges climb 2->6, potency 0->2.
        vec![
            FlaskTarget { charges: 2, potency: 0 },
            FlaskTarget { charges: 4, potency: 1 },
            FlaskTarget { charges: 6, potency: 2 },
        ]
    }

    // ---- parse ---------------------------------------------------------------------------

    #[test]
    fn parse_absent_key_is_empty_off() {
        assert!(parse(&serde_json::json!({ "seed": "x" })).is_empty());
        assert!(parse(&serde_json::json!({ "flaskLadder": "nope" })).is_empty());
        assert!(parse(&serde_json::json!({ "flaskLadder": [] })).is_empty());
    }

    #[test]
    fn parse_reads_rungs_tolerantly_and_clamps() {
        let sd = serde_json::json!({ "flaskLadder": [
            { "charges": 2, "potency": 0 },
            { "charges": 8 },                  // missing potency -> 0
            { "potency": 3 },                  // missing charges -> 0
            "junk",                             // non-object -> skipped
            { "charges": 99, "potency": 99 }   // clamped to caps
        ]});
        let l = parse(&sd);
        assert_eq!(l.len(), 4, "the non-object rung is dropped");
        assert_eq!(l[0], FlaskTarget { charges: 2, potency: 0 });
        assert_eq!(l[1], FlaskTarget { charges: 8, potency: 0 });
        assert_eq!(l[2], FlaskTarget { charges: 0, potency: 3 });
        assert_eq!(l[3], FlaskTarget { charges: MAX_CHARGES, potency: MAX_POTENCY });
    }

    // ---- desired -------------------------------------------------------------------------

    #[test]
    fn desired_absent_ladder_is_none() {
        assert_eq!(desired(&[], 5), None);
    }

    #[test]
    fn desired_count_zero_is_none() {
        assert_eq!(desired(&ladder(), 0), None);
    }

    #[test]
    fn desired_indexes_by_count() {
        assert_eq!(desired(&ladder(), 1), Some(FlaskTarget { charges: 2, potency: 0 }));
        assert_eq!(desired(&ladder(), 2), Some(FlaskTarget { charges: 4, potency: 1 }));
        assert_eq!(desired(&ladder(), 3), Some(FlaskTarget { charges: 6, potency: 2 }));
    }

    #[test]
    fn desired_past_end_clamps_to_last_rung() {
        let last = FlaskTarget { charges: 6, potency: 2 };
        assert_eq!(desired(&ladder(), 4), Some(last));
        assert_eq!(desired(&ladder(), 999), Some(last));
    }

    // ---- charge_deficit (the whole reconcile now -- charges only) ------------------------

    #[test]
    fn charge_deficit_raises_up_to_target() {
        assert_eq!(charge_deficit(2, FlaskTarget { charges: 6, potency: 2 }), 4);
    }

    #[test]
    fn charge_deficit_zero_at_or_above_target_never_lowers() {
        assert_eq!(charge_deficit(6, FlaskTarget { charges: 6, potency: 2 }), 0);
        // upward-only: live ALREADY exceeds the rung (hand-allocated / a prior higher rung) -> 0.
        assert_eq!(charge_deficit(10, FlaskTarget { charges: 6, potency: 2 }), 0);
    }

    #[test]
    fn charge_deficit_clamps_to_cap() {
        // A ladder charge past the cap is clamped to MAX_CHARGES.
        assert_eq!(
            charge_deficit(13, FlaskTarget { charges: MAX_CHARGES + 5, potency: 0 }),
            MAX_CHARGES - 13
        );
    }

    #[test]
    fn charge_deficit_ignores_potency() {
        // Potency is delivered via granted Sacred Tears; the ladder's potency field must NOT move
        // charges (this is what stopped the per-frame potency reconcile + its "SKIPPED" spam).
        assert_eq!(charge_deficit(4, FlaskTarget { charges: 4, potency: 12 }), 0);
    }

    #[test]
    fn end_to_end_desired_then_charge_deficit_no_op_after_convergence() {
        let l = ladder();
        // Received 3 upgrades -> last rung; live charges already there -> nothing to add.
        let d = desired(&l, 3).unwrap();
        assert_eq!(charge_deficit(d.charges, d), 0);
    }
}
