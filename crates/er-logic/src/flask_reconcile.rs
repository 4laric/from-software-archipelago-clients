//! flask_reconcile — pure, HISTORY-AGNOSTIC leveled flask state.
//!
//! The flask upgrade is a RECONCILED LEVELED state, NOT a consumed-goods grant (no ledger). Every
//! reconcile tick the Windows glue reads the character's CURRENT flask, computes the DESIRED flask
//! from how many "Progressive Flask Upgrade" AP items have been received (`received_count`), and
//! writes the HIGHER of the two. That makes it idempotent, upward-only, and self-healing across
//! reconnect / save-load with no per-item bookkeeping: AP replays the whole received set on connect,
//! so `received_count` is stable and the desired state is a pure function of that count.
//!
//! Two independent axes:
//!   * CHARGES (Golden Seeds): the flask ALLOCATION = `max_hp_flask + max_fp_flask` (Crimson +
//!     Cerulean). Vanilla total cap is [`MAX_CHARGES`] (14).
//!   * POTENCY (Sacred Tears, `0..=`[`MAX_POTENCY`]): the TIER of the held flask ITEM. Per Hexinton's
//!     "Set flask level" script a flask at potency `L` is the item `base + L*2` for each flask family
//!     base id (see [`FLASK_BASE_IDS`]).
//!
//! This module is PURE (only `serde_json` for the tolerant slot_data parse): it computes the desired
//! rung and the upward-only delta. The live read/write of `PlayerGameData` + the inventory item-tier
//! swap lives in the Windows-only glue (`eldenring-archipelago/src/flask.rs`).

use serde_json::Value;

/// Vanilla total flask-charge cap (`max_hp_flask + max_fp_flask`).
pub const MAX_CHARGES: u32 = 14;
/// Vanilla max flask potency level (12 Sacred Tears).
pub const MAX_POTENCY: u32 = 12;

/// The flask item family base ids (Hexinton "Set flask level"): a flask at potency level `L` is the
/// item `base + L*2`. Even-based families hold even item ids and odd-based families hold odd ids, so
/// a held item id classifies to at most one family (see [`classify_flask_item`]).
///
/// NOTE(windows-verify): these base ids and the `base + L*2` tier stride are taken from the Hexinton
/// CE table and NOT yet confirmed against a live set->readback. The item-tier swap is the UNCERTAIN
/// half of this feature; see the module doc on `eldenring-archipelago/src/flask.rs`.
pub const FLASK_BASE_IDS: [i32; 4] = [1000, 1001, 1050, 1051];

/// One rung of the flask ladder: the CUMULATIVE target after receiving `(i+1)` upgrade items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlaskTarget {
    /// Total charge allocation target (`2..=14`).
    pub charges: u32,
    /// Potency level target (`0..=12`).
    pub potency: u32,
}

/// The live flask state read from the character this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlaskState {
    /// Live total allocation (`max_hp_flask + max_fp_flask`).
    pub charges: u32,
    /// Live potency level (the max tier among held flask items).
    pub potency: u32,
}

/// The UPWARD-ONLY actions to move `current` toward `desired`. Every field is already clamped to the
/// vanilla caps and to `>= current` (the reconcile NEVER lowers either axis).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlaskActions {
    /// The total charge allocation to reach (`current.charges.max(desired.charges)`, capped).
    pub target_charges: u32,
    /// How many charges to ADD to `current.charges` to reach `target_charges` (0 if already `>=`).
    /// The glue adds this to `max_hp_flask` (see [`FlaskActions::is_noop`] and the glue docs).
    pub add_charges: u32,
    /// The potency level to reach (`current.potency.max(desired.potency)`, capped).
    pub target_potency: u32,
    /// Whether a flask-item-tier swap is needed (`target_potency > current.potency`).
    pub raise_potency: bool,
}

impl FlaskActions {
    /// Nothing to do this tick: no charges to add AND no potency raise (idempotent fixpoint).
    pub fn is_noop(&self) -> bool {
        self.add_charges == 0 && !self.raise_potency
    }
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

/// Compute the UPWARD-ONLY delta from the `current` live flask toward `desired`. Never lowers either
/// axis, clamps both to the vanilla caps, and is idempotent: `current >= desired` on both axes =>
/// [`FlaskActions::is_noop`].
pub fn reconcile(current: FlaskState, desired: FlaskTarget) -> FlaskActions {
    let target_charges = current.charges.max(desired.charges).min(MAX_CHARGES);
    let target_potency = current.potency.max(desired.potency).min(MAX_POTENCY);
    FlaskActions {
        target_charges,
        add_charges: target_charges.saturating_sub(current.charges),
        target_potency,
        raise_potency: target_potency > current.potency,
    }
}

/// The flask item id for family `base` at potency `level` (`base + level*2`, Hexinton "Set flask
/// level"). The glue writes this into the held inventory slot to raise potency.
pub fn flask_item_id(base: i32, level: u32) -> i32 {
    base + (level as i32) * 2
}

/// Classify a held GOODS row id into its flask family + potency level, if it is a flask item.
/// Returns `(base, level)` for the matching family, or `None` when the id is not a flask (or its
/// implied level exceeds [`MAX_POTENCY`]). Because the families are `{even, odd}` interleaved and
/// range-bounded, a valid flask id matches exactly one family.
pub fn classify_flask_item(item_id: i32) -> Option<(i32, u32)> {
    for &base in &FLASK_BASE_IDS {
        if item_id < base {
            continue;
        }
        let d = item_id - base;
        if d % 2 != 0 {
            continue;
        }
        let level = (d / 2) as u32;
        if level <= MAX_POTENCY {
            return Some((base, level));
        }
    }
    None
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

    // ---- reconcile -----------------------------------------------------------------------

    #[test]
    fn reconcile_raises_both_axes() {
        let a = reconcile(
            FlaskState { charges: 2, potency: 0 },
            FlaskTarget { charges: 6, potency: 2 },
        );
        assert_eq!(a.target_charges, 6);
        assert_eq!(a.add_charges, 4);
        assert_eq!(a.target_potency, 2);
        assert!(a.raise_potency);
        assert!(!a.is_noop());
    }

    #[test]
    fn reconcile_is_idempotent_when_already_at_target() {
        let a = reconcile(
            FlaskState { charges: 6, potency: 2 },
            FlaskTarget { charges: 6, potency: 2 },
        );
        assert_eq!(a.add_charges, 0);
        assert!(!a.raise_potency);
        assert!(a.is_noop());
    }

    #[test]
    fn reconcile_is_upward_only_never_lowers() {
        // Live flask ALREADY exceeds the desired rung (e.g. player hand-allocated more, or a prior
        // higher rung): the reconcile must not touch anything.
        let a = reconcile(
            FlaskState { charges: 10, potency: 5 },
            FlaskTarget { charges: 6, potency: 2 },
        );
        assert_eq!(a.target_charges, 10, "keeps the higher live charges");
        assert_eq!(a.add_charges, 0);
        assert_eq!(a.target_potency, 5, "keeps the higher live potency");
        assert!(!a.raise_potency);
        assert!(a.is_noop());
    }

    #[test]
    fn reconcile_mixed_axes_upward_only() {
        // Charges below target but potency above: only charges move.
        let a = reconcile(
            FlaskState { charges: 3, potency: 4 },
            FlaskTarget { charges: 6, potency: 2 },
        );
        assert_eq!(a.add_charges, 3);
        assert_eq!(a.target_potency, 4);
        assert!(!a.raise_potency);
    }

    #[test]
    fn reconcile_clamps_to_caps() {
        let a = reconcile(
            FlaskState { charges: 13, potency: 11 },
            FlaskTarget { charges: MAX_CHARGES + 5, potency: MAX_POTENCY + 5 },
        );
        assert_eq!(a.target_charges, MAX_CHARGES);
        assert_eq!(a.add_charges, MAX_CHARGES - 13);
        assert_eq!(a.target_potency, MAX_POTENCY);
        assert!(a.raise_potency);
    }

    // ---- item-tier helpers ---------------------------------------------------------------

    #[test]
    fn flask_item_id_is_base_plus_two_level() {
        assert_eq!(flask_item_id(1000, 0), 1000);
        assert_eq!(flask_item_id(1000, 1), 1002);
        assert_eq!(flask_item_id(1000, 12), 1024);
        assert_eq!(flask_item_id(1050, 2), 1054);
    }

    #[test]
    fn classify_flask_item_roundtrips_each_family() {
        for &base in &FLASK_BASE_IDS {
            for level in 0..=MAX_POTENCY {
                let id = flask_item_id(base, level);
                assert_eq!(
                    classify_flask_item(id),
                    Some((base, level)),
                    "id {id} must classify to family {base} level {level}"
                );
            }
        }
    }

    #[test]
    fn classify_rejects_non_flask_and_out_of_range() {
        assert_eq!(classify_flask_item(9500), None, "Cracked Pot is not a flask");
        assert_eq!(classify_flask_item(999), None, "below every base");
        // base 1000 at level 13 (= 1026) is past MAX_POTENCY and belongs to no family.
        assert_eq!(classify_flask_item(1026), None);
    }

    #[test]
    fn end_to_end_desired_then_reconcile_no_op_after_convergence() {
        let l = ladder();
        // Received 3 upgrades -> last rung; live flask already there -> nothing to do.
        let d = desired(&l, 3).unwrap();
        let a = reconcile(FlaskState { charges: d.charges, potency: d.potency }, d);
        assert!(a.is_noop());
    }
}
