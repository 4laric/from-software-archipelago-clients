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
    // options.auto_equip -> er_logic::auto_equip (routing) + eldenring_archipelago::auto_equip
    // (the four-rep equip, wired connect -> receive -> tick) (2026-08-02).
    "auto_equip",
    // dlc_blessing_catchup with scadutree_blessing_scope = dlc_only, i.e. blessing mode 3
    // -> er_logic::upgrades::{blessing_target, applies_globally} (2026-08-06). Only mode 3 needs
    // the tag: 0/1/2 mean exactly what they always meant, and a build without this arm clamps an
    // unrecognised 3 to 0 in set_global_scadu_blessing, so the catch-up would vanish in silence.
    "dlc_blessing_catchup",
    // graceAttunement -> eldenring_archipelago::region::tick_grace_attunement (2026-08-08). A seed
    // that gates graces MUST refuse an older client rather than connect: without this arm the key
    // is unread, so the player is handed one grace per region and the rest never light -- which
    // reads as a broken seed, not as an ignored setting.
    "grace_attunement",
    // options.merchant_bells_on_talk -> er_logic::merchant_bells + the ESD shop-open detour
    // (2026-08-10, er-archipelago#325). A seed with this on MUST refuse an older client: the key
    // would be unread, so the player would walk every merchant expecting their wares at the Twin
    // Maidens and find the hub empty -- which reads as a broken seed rather than an ignored option.
    "merchant_bells_on_talk",
    // options.no_equip_load = 2 (MEDIUM) -> er_logic::equip_load::RollMode (2026-08-11,
    // er-archipelago#548). ONLY MEDIUM NEEDS THE TAG, and the asymmetry is the whole point:
    // `no_equip_load` deliberately ships with no tag at all because the capability is older than
    // every client in circulation (features/body_tuning.py spells this out, and tagging `light`
    // would lock every existing player out of a seed whose feature they already implement).
    //
    // That reasoning stops dead at `medium`. An old client reads the new `2` through
    // parse_bool_option, sees a nonzero, and gives LIGHT -- so the player asked for the WEAKER
    // setting and silently got the strongest one. A difficulty option that can only fail in the
    // direction of "easier, and nothing said so" is exactly #536's shape, and a refusal is the only
    // honest answer.
    "no_equip_load_roll",
    // Spawn traps: the item NAME carries the ids (er-archipelago#581/#585, this crate's
    // `traps::SpawnSpec::from_item_name`) (2026-08-12, er-archipelago#595).
    //
    // 🛑 THE MOTIVATING CASE IS AN ITEM THAT EATS ITSELF. bobler's seed placed seven spawn traps
    // this build could not read. On pickup the item arrives, AP marks it delivered, the name does
    // not parse, and `enqueue_by_item_name` DROPS it -- no toast, no tracker row, nothing to
    // recover. The handshake was honoured in full that session and could not help, because spawn
    // traps declared nothing for it to check.
    //
    // 🛑🛑 THIS TAG VERSIONS THE NAME FORMAT, NOT THE CAPABILITY, which is why it is not called
    // something like "traps". A build that knows spawn traps but speaks the older
    // `Trap: <label> (<chr>/<npc>/<think> x<count>)` shape refuses the name exactly as an ignorant
    // build does, so a capability boolean would pass the handshake and still lose the item. When
    // `SpawnSpec::item_name`'s format changes, the apworld mints a NEW tag and this list gains it in
    // the same release that learns the new shape -- older builds then say CLIENT TOO OLD instead of
    // failing quietly. `traps.rs` pins the format literal; the apworld pins the pair in
    // `test_gf_spawn_traps`.
    "spawn_traps",
    // A seed that mints Blackout must refuse clients that would consume and ignore its fixed name.
    "blackout",
    // options.trap_link -> post-connect tag reconciliation plus exact-name Bounce delivery.
    "trap_link",
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

// ---------------------------------------------------------------------------------------------
// DECLARED vs ARMED -- the half this handshake was missing until 2026-08-11
// ---------------------------------------------------------------------------------------------
//
// Everything above answers "is this client NEW enough for the seed?". It cannot answer "did the
// feature the seed asked for actually TURN ON?", and those are different questions with the same
// symptom.
//
// er-archipelago#536 is the worked example. `merchant_bells_on_talk` shipped complete on both
// sides: this crate had the planner, the game crate had the ESD detour, and the tag was in
// `SUPPORTED`. The apworld declared the sub-key and documented it -- and never emitted its VALUE.
// So `parse_merchant_bells_on_talk` read an absent key as `false`, the detour stayed asleep, and
// every gate was green: `required=False` means the contract validator's MISSING arm never fires,
// and `OPTIONS_SUBKEYS` is not folded into the contract hash, so `VERSION: OK` printed straight
// over the gap. The seed declared the tag, this client accepted the tag, and nothing happened.
//
// ⭐⭐⭐ THE HANDSHAKE SUCCEEDING IS WORSE THAN A REFUSAL. `requiresClientFeatures` exists to stop a
// client silently ignoring an option; here the client implements the feature, accepts the tag, and
// is handed no payload -- so the one signal designed to catch this reads as PROOF IT IS FINE.
//
// The client already holds both halves of the answer. It just never subtracted them. That is all
// this does: DECLARED (from slot_data) minus ARMED (read back out of the live feature state), the
// difference logged by name. Two player reports, five days apart, both needed a source read to
// diagnose; either would have been one grep with this line in the log.
//
// 🛑 ARMED MUST BE A READ-BACK, NOT A RECEIPT. The probe has to ask the feature module what its
// state IS, not remember that `set_enabled` was called -- a guard whose subject cannot witness it
// is the same blindness one level down.

/// The three-way split between what the seed asked for and what this client turned on.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Handshake {
    /// The seed declared it, this build knows the tag, and the feature is NOT live. **This is the
    /// defect class**: the value never reached us, or an arming path failed. Always a WARN.
    pub declared_not_armed: Vec<String>,
    /// Declared and live. The happy path; logged so a reader can see the check ran at all.
    pub agreed: Vec<String>,
    /// Live but never declared. Not an error -- most options are legitimately undeclared, because
    /// a seed only declares what an OLDER client would break on. Logged at INFO as context.
    pub armed_not_declared: Vec<String>,
}

/// Subtract ARMED from DECLARED.
///
/// `armed` is the probe table: every tag in [`SUPPORTED`] paired with its live state. Tags the seed
/// declared that this build does not know are NOT reported here -- [`unsupported`] already refuses
/// those, and reporting the same tag twice under two names would train a reader to skim both.
pub fn reconcile(required: &[String], armed: &[(&str, bool)]) -> Handshake {
    let live = |tag: &str| armed.iter().any(|(t, on)| *t == tag && *on);
    let mut h = Handshake::default();
    for tag in required {
        // Unknown tags are `unsupported`'s business, not ours.
        if !SUPPORTED.contains(&tag.as_str()) {
            continue;
        }
        if live(tag) {
            h.agreed.push(tag.clone());
        } else {
            h.declared_not_armed.push(tag.clone());
        }
    }
    for (tag, on) in armed {
        if *on && !required.iter().any(|r| r == tag) {
            h.armed_not_declared.push((*tag).to_string());
        }
    }
    h
}

/// The WARN body for a declared-but-unarmed feature. Names the tags AND the two things that cause
/// it, because "merchant_bells_on_talk not armed" tells a triager nothing actionable.
///
/// ASCII only: this string can reach the in-game toast deck.
pub fn not_armed_message(tags: &[String]) -> String {
    format!(
        "feature-handshake: DECLARED but NOT ARMED: {}. The seed asked this client for the \
         feature(s) above and this build has them, but they did not turn on -- so the option will \
         do nothing and the log will otherwise look healthy. Almost always the apworld sent the \
         TAG without the VALUE (the option's key is missing from slot_data `options`), which no \
         other check can see: an optional sub-key's absence is identical to its OFF state, and \
         OPTIONS_SUBKEYS is not folded into the contract hash. Regenerate the seed with an apworld \
         that emits the value; updating this client will NOT help.",
        tags.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 🛑 er-archipelago#595. The tag is only worth anything if it means "this build reads the
    /// CURRENT spawn-trap name shape". Pins both halves together: if `SpawnSpec::item_name`'s format
    /// moves and this build keeps claiming `spawn_traps`, seeds minted for the new shape would
    /// handshake clean here and then have every spawn trap consume itself -- the exact defect the
    /// tag exists to stop. Changing the format means minting a new tag, in both repos.
    #[test]
    fn the_spawn_trap_tag_means_the_shape_this_build_actually_reads() {
        assert!(SUPPORTED.contains(&"spawn_traps"));
        let spec = crate::traps::SpawnSpec::from_item_name("Trap: Basilisk x3 (4150/41500060)")
            .expect("this build must read the shape its tag claims");
        assert_eq!(spec.chr_id, 4150);
        assert_eq!(spec.count, 3);
        // The shape the tag REPLACES. A build that read this too would not need a new tag, and the
        // versioning argument above would be wrong.
        assert!(
            crate::traps::SpawnSpec::from_item_name("Trap: Basilisk (4150/41500060/41500000 x3)")
                .is_none(),
            "if the old shape parsed, the tag would not be versioning anything"
        );
    }
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

    /// THE CASE THIS TAG WAS ADDED FOR: a seed rolled with `auto_equip` on declares the tag, and
    /// this build carries the behaviour (`er_logic::auto_equip` routing + the game-side module
    /// driven from the receive loop), so it must connect. If the tag is ever dropped from
    /// `SUPPORTED` while the behaviour still ships, this reds -- which is the point: the list is
    /// only worth having if something asserts it matches reality in BOTH directions.
    #[test]
    fn a_seed_requiring_auto_equip_is_accepted() {
        let sd = json!({ "requiresClientFeatures": ["auto_equip"] });
        assert!(
            unsupported(&required_from_slot_data(&sd)).is_empty(),
            "this build implements auto_equip, so the tag must be in SUPPORTED"
        );
        // Alongside the other tag, too -- a seed may need several.
        let sd = json!({ "requiresClientFeatures": ["auto_equip", "scaling_ceiling"] });
        assert!(unsupported(&required_from_slot_data(&sd)).is_empty());
    }

    /// THE CASE THIS TAG WAS ADDED FOR: a seed rolled with `merchant_bells_on_talk` on declares the
    /// tag, and this build carries the behaviour (`er_logic::merchant_bells` planning +
    /// `merchant_bell_table` + the game-side ESD shop-open detour). The option is an
    /// OPTIONS_SUBKEYS bool, and the contract hash folds CONTRACT and NOT OPTIONS_SUBKEYS, so
    /// nothing else in the handshake would have caught an older client here.
    #[test]
    fn a_seed_requiring_merchant_bells_on_talk_is_accepted() {
        let sd = json!({ "requiresClientFeatures": ["merchant_bells_on_talk"] });
        assert!(
            unsupported(&required_from_slot_data(&sd)).is_empty(),
            "this build implements merchant_bells_on_talk, so the tag must be in SUPPORTED"
        );
    }

    /// THE CASE THIS TAG WAS ADDED FOR: blessing mode 3 (`dlc_only` scope + `dlc_blessing_catchup`)
    /// is the only NEW wire value the 2026-08-06 option split introduced. A build without the arm
    /// clamps an unrecognised 3 to 0 in `set_global_scadu_blessing`, so the player's catch-up would
    /// evaporate while the version check still said OK -- the contract hash folds CONTRACT, not
    /// OPTIONS_SUBKEYS, so nothing else would have noticed. This build HAS the arm
    /// (`blessing_target` mode 3 + `applies_globally`), so the tag must be accepted.
    #[test]
    fn a_seed_requiring_dlc_blessing_catchup_is_accepted() {
        let sd = json!({ "requiresClientFeatures": ["dlc_blessing_catchup"] });
        assert!(
            unsupported(&required_from_slot_data(&sd)).is_empty(),
            "this build implements blessing mode 3, so the tag must be in SUPPORTED"
        );
        // A seed may cap the enemy ceiling AND ask for catch-up; both must pass together.
        let sd = json!({ "requiresClientFeatures": ["scaling_ceiling", "dlc_blessing_catchup"] });
        assert!(unsupported(&required_from_slot_data(&sd)).is_empty());
    }

    /// ...and knowing `auto_equip` must not make the handshake soft: an unknown tag sitting next to
    /// a known one is still refused and still named.
    #[test]
    fn an_unknown_tag_beside_auto_equip_is_still_refused() {
        let sd = json!({ "requiresClientFeatures": ["auto_equip", "auto_equip_v2"] });
        let missing = unsupported(&required_from_slot_data(&sd));
        assert_eq!(missing, vec!["auto_equip_v2".to_string()]);
        assert!(refusal_message(&missing).contains("auto_equip_v2"));
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

    // ---- DECLARED vs ARMED ------------------------------------------------------------------

    /// ⭐⭐⭐ THE MOTIVATING CASE, verbatim: er-archipelago#536. The seed declares
    /// `merchant_bells_on_talk`, this build supports it (so `unsupported` is EMPTY and the old
    /// handshake is happy), and the feature is off because the value never arrived. Before this
    /// function that combination was indistinguishable from a healthy session.
    #[test]
    fn a_declared_feature_that_never_armed_is_named() {
        let sd = json!({ "requiresClientFeatures": ["merchant_bells_on_talk"] });
        let required = required_from_slot_data(&sd);
        assert!(
            unsupported(&required).is_empty(),
            "precondition: the OLD handshake sees nothing wrong here -- that is the whole bug"
        );

        let armed = [("merchant_bells_on_talk", false), ("auto_equip", false)];
        let h = reconcile(&required, &armed);
        assert_eq!(
            h.declared_not_armed,
            vec!["merchant_bells_on_talk".to_string()]
        );
        assert!(h.agreed.is_empty());

        let msg = not_armed_message(&h.declared_not_armed);
        assert!(
            msg.contains("merchant_bells_on_talk"),
            "must NAME the tag: {msg}"
        );
        assert!(
            msg.contains("Regenerate"),
            "must say what actually fixes it: {msg}"
        );
        assert!(
            msg.is_ascii(),
            "reaches the in-game toast deck, which is ASCII-only: {msg}"
        );
    }

    /// The same seed once the apworld emits the value: declared AND armed, nothing to warn about.
    #[test]
    fn a_declared_feature_that_armed_is_silent() {
        let required = required_from_slot_data(&json!({
            "requiresClientFeatures": ["merchant_bells_on_talk"]
        }));
        let h = reconcile(&required, &[("merchant_bells_on_talk", true)]);
        assert!(h.declared_not_armed.is_empty());
        assert_eq!(h.agreed, vec!["merchant_bells_on_talk".to_string()]);
    }

    /// An option that is ON but undeclared is the NORMAL case, not a defect: a seed only declares
    /// what would break an older client. It must never reach the WARN arm.
    #[test]
    fn armed_but_undeclared_is_context_not_a_defect() {
        let h = reconcile(&[], &[("auto_equip", true), ("scaling_ceiling", false)]);
        assert!(h.declared_not_armed.is_empty());
        assert_eq!(h.armed_not_declared, vec!["auto_equip".to_string()]);
    }

    /// A tag this build does not know is `unsupported`'s job. Reporting it here as well would put
    /// the same tag in two WARN lines under two different names.
    #[test]
    fn an_unknown_tag_is_left_to_the_refusal_path() {
        let required = required_from_slot_data(&json!({
            "requiresClientFeatures": ["some_future_thing"]
        }));
        let h = reconcile(&required, &[("auto_equip", false)]);
        assert!(h.declared_not_armed.is_empty(), "not ours to report");
        assert!(h.agreed.is_empty());
        assert_eq!(
            unsupported(&required),
            vec!["some_future_thing".to_string()]
        );
    }

    /// Mixed: one honoured, one dark, one unknown -- each lands in exactly one bucket.
    #[test]
    fn each_tag_lands_in_exactly_one_bucket() {
        let required = required_from_slot_data(&json!({
            "requiresClientFeatures": ["auto_equip", "merchant_bells_on_talk", "some_future_thing"]
        }));
        let h = reconcile(
            &required,
            &[
                ("auto_equip", true),
                ("merchant_bells_on_talk", false),
                ("scaling_ceiling", true),
            ],
        );
        assert_eq!(h.agreed, vec!["auto_equip".to_string()]);
        assert_eq!(
            h.declared_not_armed,
            vec!["merchant_bells_on_talk".to_string()]
        );
        assert_eq!(h.armed_not_declared, vec!["scaling_ceiling".to_string()]);
        assert_eq!(
            unsupported(&required),
            vec!["some_future_thing".to_string()]
        );
    }

    /// A seed that declares nothing and a client with nothing on: the check must be completely
    /// silent rather than emitting an empty banner every connect.
    #[test]
    fn a_quiet_seed_produces_a_wholly_empty_handshake() {
        let h = reconcile(&[], &[("auto_equip", false), ("scaling_ceiling", false)]);
        assert_eq!(h, Handshake::default());
    }
}
