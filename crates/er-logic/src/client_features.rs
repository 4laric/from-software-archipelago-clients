//! "This seed needs a client that understands X" — the connect-time feature handshake.
//!
//! # The gap this closes
//!
//! The apworld and the client ship SEPARATELY (Archipelago's Players folder vs a DLL from Nexus), so
//! a player running a stale client against a fresh apworld is the NORM, not an edge case. There are
//! already two guards, and neither covers a new *option*:
//!
//! * `versions` carries `contract/<hash>`, and `core.rs` shouts when it differs from the hash this
//!   binary was built against. But `contract.py::_contract_hash()` folds in `CONTRACT` only — NOT
//!   `OPTIONS_SUBKEYS`. An option added to the `options` sub-dict does not move the hash, so the
//!   skew check says "VERSION: OK" while the client silently cannot see the new key.
//! * The contract validator checks the keys it KNOWS. A key it has never heard of is not an error;
//!   it is simply not read.
//!
//! Net: add a client-consumed option, and an old client ignores it in total silence. The player set
//! a difficulty knob and the game did not do it — indistinguishable from the feature not existing.
//! That is CONTRIBUTING's "absence of behavior is indistinguishable from feature turned off".
//!
//! # Why not just hash the options sub-dict
//!
//! Because most option additions are genuinely harmless to an old client — it misses a new feature
//! it was never going to use. Folding `OPTIONS_SUBKEYS` into the contract hash would fire a loud
//! MISMATCH on every one of them, and a gate that cries wolf is a gate people stop reading (the
//! same reasoning as `gen_contract.py`'s idempotent-write note).
//!
//! So the apworld declares, per seed, only the features it ACTUALLY DEPENDS ON — a seed that leaves
//! the knob at its default declares nothing and connects to any client. You pay the compatibility
//! cost exactly when you opted into the thing that costs it.
//!
//! # Contract
//!
//! slot_data `requiresClientFeatures`: `["scaling_ceiling", ...]`, absent or `[]` when the seed needs
//! nothing special. Every string must be in `SUPPORTED`; anything else means this binary is older
//! than the apworld that rolled the seed.

/// Feature tags THIS build understands. Add a tag here in the same change that adds the behaviour,
/// never before it — the whole value of this list is that it cannot claim support it does not have.
pub const SUPPORTED: &[&str] = &[
    // options.completion_scaling_ceiling -> er_logic::scaling::ceiling_tier (2026-07-27).
    "scaling_ceiling",
];

/// Feature tags the seed requires that this build does not know.
///
/// Empty = safe to connect. Non-empty = the caller must REFUSE, loudly, naming the tags: the seed
/// was generated with settings this client cannot honour, and connecting anyway would silently
/// ignore whatever the player chose.
pub fn unsupported(required: &[String]) -> Vec<String> {
    required
        .iter()
        .filter(|f| !SUPPORTED.contains(&f.as_str()))
        .cloned()
        .collect()
}

/// Parse `requiresClientFeatures` out of slot_data. Absent / wrong-shaped -> empty (an older apworld
/// predates the handshake and by definition needs nothing from it).
pub fn required_from_slot_data(sd: &serde_json::Value) -> Vec<String> {
    sd.get("requiresClientFeatures")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The message to show the player. Names the tags AND what to do, because "unsupported feature
/// scaling_ceiling" alone tells a player nothing actionable.
pub fn refusal_message(missing: &[String]) -> String {
    format!(
        "This seed needs client feature(s) this build does not have: {}. \
         The apworld that generated it is NEWER than your client -- update the Elden Ring \
         Archipelago client, or regenerate the seed with those options left at their defaults. \
         (Connecting anyway would silently ignore settings the seed was built with.)",
        missing.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_seed_that_needs_nothing_connects() {
        assert!(unsupported(&[]).is_empty());
        assert!(required_from_slot_data(&json!({})).is_empty());
        assert!(required_from_slot_data(&json!({ "requiresClientFeatures": [] })).is_empty());
    }

    #[test]
    fn an_older_apworld_predates_the_handshake_and_is_not_penalised() {
        // No key at all -> nothing required. The handshake must never turn "old seed" into "refuse".
        let sd = json!({ "versions": "apworld/0.2.0 contract/abcd1234 data/deadbeef" });
        assert!(unsupported(&required_from_slot_data(&sd)).is_empty());
    }

    #[test]
    fn a_feature_this_build_has_is_accepted() {
        let sd = json!({ "requiresClientFeatures": ["scaling_ceiling"] });
        assert!(unsupported(&required_from_slot_data(&sd)).is_empty());
    }

    #[test]
    fn an_unknown_feature_is_refused_and_named() {
        // THE CASE THIS EXISTS FOR: a client older than the option. Before this handshake the key
        // was simply never read and the player's setting evaporated in silence.
        let sd = json!({ "requiresClientFeatures": ["scaling_ceiling", "some_future_thing"] });
        let missing = unsupported(&required_from_slot_data(&sd));
        assert_eq!(missing, vec!["some_future_thing".to_string()]);
        let msg = refusal_message(&missing);
        assert!(
            msg.contains("some_future_thing"),
            "the message must NAME the tag: {msg}"
        );
        assert!(
            msg.contains("update"),
            "the message must say what to do: {msg}"
        );
    }

    #[test]
    fn malformed_shapes_do_not_block_a_connect() {
        // Tolerant like every other slot_data parse here: a garbled key must not brick a session.
        for bad in [
            json!({"requiresClientFeatures": "scaling_ceiling"}),
            json!({"requiresClientFeatures": 7}),
            json!({"requiresClientFeatures": [1, 2]}),
        ] {
            assert!(
                unsupported(&required_from_slot_data(&bad)).is_empty(),
                "{bad}"
            );
        }
    }

    #[test]
    fn supported_has_no_duplicates_and_no_blanks() {
        let mut seen = std::collections::HashSet::new();
        for f in SUPPORTED {
            assert!(!f.trim().is_empty(), "blank feature tag");
            assert!(seen.insert(*f), "duplicate feature tag {f}");
        }
    }
}
