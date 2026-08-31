//! `feature_handshake` -- subtract what ARMED from what the seed DECLARED, and name the difference.
//!
//! # The bug this exists for
//!
//! er-archipelago#536. `merchant_bells_on_talk` shipped complete on both sides -- this client baked
//! its 38-row table, armed the ESD detour and listed the tag in `client_features::SUPPORTED`; the
//! apworld declared the sub-key in `contract.py` and documented it in `CONTRACT.md`. The one line
//! that puts the option's VALUE on the wire was never written. `parse_merchant_bells_on_talk` read
//! an absent key as `false` and the feature was dark for every seed that turned it on.
//!
//! Four gates were green while it was broken, and each for a defensible reason:
//!
//! * the contract validator reports MISSING only for `required=True`, and a client-gated option is
//!   `required=False` **on purpose** (an absent key must parse false on an older client) -- so
//!   absence and OFF are the same observation;
//! * `OPTIONS_SUBKEYS` is not folded into the contract hash, so `VERSION: OK` printed over the gap;
//! * the cross-repo path gate runs CLIENT-READ -> DECLARATION, and the declaration was the half
//!   that existed -- the mirror direction had no gate at all;
//! * `requiresClientFeatures` -- the one signal designed to stop a client silently ignoring an
//!   option -- was *satisfied*, because this client does support the tag.
//!
//! ⭐⭐⭐ **A handshake that succeeds over an empty payload is a false positive.** It is worse than a
//! refusal, because the refusal path is loud and this one reads as proof everything is fine.
//!
//! # Why the client can answer this at all
//!
//! It already holds both halves. `requiresClientFeatures` says what the seed depends on; the
//! feature modules know whether they are live. Nothing ever subtracted one from the other. That
//! subtraction is this module, and it is the only check in either repo that can see the gap --
//! world-side, the missing line is *a line that is not there*, and no runtime assertion sees an
//! absence.
//!
//! Two player reports five days apart both needed a source read to diagnose. Either would have been
//! one grep against this line.
//!
//! # 🛑 Why the probe is a READ-BACK
//!
//! Every probe below asks the feature module for the state its own behaviour gates on --
//! `merchant_bells::is_armed()` loads the same atomic the detour loads. A probe that instead
//! recorded "we called `set_enabled`" would be a receipt, and a receipt cannot witness the thing it
//! is receipting for: it would have reported ARMED throughout #536, because `set_enabled(false)`
//! was faithfully called with the value that never arrived.
//!
//! # 🛑 Why this is a REGISTRY with a gate, not a check per feature
//!
//! `auto_equip` already had a per-feature emission test, which is why auto_equip works. That test
//! did nothing for the next feature, because the next feature has to remember to copy it -- and did
//! not. So [`PROBES`] must cover [`SUPPORTED`] exactly, and
//! `every_supported_tag_has_a_probe` reds when it does not. Adding a tag without a probe is a
//! build failure, not a thing to remember.

use er_logic::client_features;

use crate::region::RegionConfig;

/// The borrowed state a probe may need. Most features keep their arming flag in a module static and
/// ignore this; `grace_attunement` lives on the per-connect [`RegionConfig`], which is owned by
/// `core` and cannot be a static.
pub struct ProbeCtx<'a> {
    pub region: Option<&'a RegionConfig>,
    pub armor_bundles: bool,
    pub region_completion_goal_gate: bool,
    pub reveal_sweep_boss_names: bool,
}

/// A tag paired with the read-back that decides whether it is live.
type Probe = fn(&ProbeCtx) -> bool;

/// Every tag in [`SUPPORTED`], with the live-state read that answers "did it arm?".
///
/// ⭐ ORDER IS THE ORDER OF `SUPPORTED`, so the two lists diff by eye as well as by test.
pub const PROBES: &[(&str, Probe)] = &[
    // A ceiling is only DECLARED by a seed that actually caps, so ARMED must mean the same thing --
    // configured, and capping below the top rung.
    ("scaling_ceiling", |_| crate::scaling::ceiling_is_capped()),
    // The atomic the receive-queue drain gates on.
    ("auto_equip", |_| crate::auto_equip::is_armed()),
    // Blessing mode 3 specifically: 0/1/2 are old values no client misreads.
    ("dlc_blessing_catchup", |_| {
        crate::upgrades::dlc_blessing_catchup_armed()
    }),
    // Parsed per connect into RegionConfig. Non-empty = at least one region is gated, which is the
    // only case the apworld declares the tag for.
    ("grace_attunement", |c| {
        c.region.is_some_and(|r| !r.grace_attunement.is_empty())
    }),
    // #536's own tag: the atomic the ESD shop-open detour loads on every merchant open.
    ("merchant_bells_on_talk", |_| {
        crate::merchant_bells::is_armed()
    }),
    // MEDIUM specifically (er-archipelago#548). `off` and `light` are what every client in
    // circulation already does, so the apworld declares this tag only for a medium seed -- ARMED
    // must therefore mean "the mode is medium", not "the feature is on", or the subtraction would
    // be comparing two different questions and a light seed would look declared-but-dark.
    ("no_equip_load_roll", |_| {
        crate::no_equip_load::medium_armed()
    }),
    // 🛑 THIS TAG VERSIONS A FORMAT, SO THE PROBE PARSES ONE (er-archipelago#595). Every entry
    // above reads an arming flag; spawn traps have none to read. `enqueue_by_item_name` is always
    // live, and the tag does not claim the feature is ON -- it claims this build reads the CURRENT
    // name shape. So the read-back that honestly answers "did it arm?" is the parse itself.
    //
    // 🛑🛑 `|_| true` WOULD HAVE COMPILED AND PASSED THE GATE. It is a probe that cannot fail,
    // which is precisely the darkness this registry exists to see: a build whose parser had been
    // broken would still report ARMED, and the subtraction would go on agreeing with itself. The
    // literal mirrors the one in `client_features`'s own
    // `the_spawn_trap_tag_means_the_shape_this_build_actually_reads`, deliberately -- both pin the
    // same shape, and the apworld pins its half in `test_gf_spawn_traps`.
    ("spawn_traps", |_| {
        er_logic::traps::SpawnSpec::from_item_name("Trap: Basilisk x3 (4150/41500060)").is_some()
    }),
    // Exact parse is the read-back: this tag promises this fixed item is recognised, not that a
    // process-wide option happened to be enabled.
    ("blackout", |_| {
        er_logic::traps::Trap::from_item_name("Trap: Blackout")
            == Some(er_logic::traps::Trap::Blackout)
    }),
    // Read the same atomic that gates both outbound broadcasts and inbound queueing. The world
    // declares this tag only when the option is on, so `false` is a genuine dark feature.
    ("trap_link", |_| crate::traps::trap_link_enabled()),
    // Region Sync (er-archipelago#1005). Same shape as `trap_link`: read the ONE atomic that
    // gates both the outbound broadcast and the inbound apply, so ARMED means the link is really
    // live rather than merely compiled in. The world declares this tag only when the option is on,
    // so `false` here is a genuine dark feature and not a seed that never asked.
    ("region_sync", |_| crate::region_sync::is_enabled()),
    // The world declares this only when either cadence is above the compatibility default. Read
    // back the configured counters themselves so a missing options payload reports dark.
    ("death_link_amnesty", |_| {
        crate::deathlink::amnesty_configured()
    }),
    // Parsed per connect into Core. Non-empty means wrapper receipt has concrete members to apply.
    ("armor_bundles", |c| c.armor_bundles),
    ("region_completion_goal_gate", |c| {
        c.region_completion_goal_gate
    }),
    // Read the same atomic the write path consults before attempting an insert
    // (er-archipelago#937). The latch only flips when an insert has been CAUGHT wrong by the
    // post-swap read-back, so on first connect this is a capability claim -- "this build HAS the
    // INSERT arm" -- which is what the tag means; a reconnect after a failed insert reports the
    // genuine darkness. The SearchStringTable signature is deliberately NOT read here: that check
    // lives at the write path, where its failure names the real cause (see
    // fmg_inject::insert_path_live).
    ("shop_preview_fmg_insert", |_| {
        crate::fmg_inject::insert_path_live()
    }),
    // Progressive ability-lock (er-archipelago#980). The apworld declares this tag only for a
    // progressive seed, and the read-back that answers "did it arm?" is the parse itself: a
    // non-empty unlock map means this build turned abilityUnlockItems into concrete id->ability
    // bindings. Same shape as armor_bundles -- concrete members to apply, not a bare option flag.
    ("ability_unlock", |_| crate::ability_lock::has_unlock_map()),
    // Parsed directly from the top-level slot-data contract key into Core; true is the exact
    // state an opted-in seed declares.
    ("reveal_sweep_boss_names", |c| c.reveal_sweep_boss_names),
];

/// Build the `(tag, live)` table this connect.
fn armed(ctx: &ProbeCtx) -> Vec<(&'static str, bool)> {
    PROBES.iter().map(|(tag, p)| (*tag, p(ctx))).collect()
}

/// Reconcile and log. Call ONCE per connect, **after every feature has been configured** -- a probe
/// run mid-arming would report a false negative and cry wolf, which is how a gate stops being read.
///
/// Returns the declared-but-unarmed tags so the caller can also surface them on screen: a player
/// who never opens the log is exactly the one who would otherwise conclude the option they chose
/// simply does nothing.
pub fn log_and_report(required: &[String], ctx: &ProbeCtx) -> Vec<String> {
    let armed = armed(ctx);
    let h = client_features::reconcile(required, &armed);

    if !h.declared_not_armed.is_empty() {
        log::warn!(
            "{}",
            client_features::not_armed_message(&h.declared_not_armed)
        );
    }
    // Say the check RAN even when it is happy. A gate that is silent on success is a gate you
    // cannot distinguish from a gate that was never called -- which is the whole failure mode this
    // module was written about.
    log::info!(
        "feature-handshake: {} declared, {} armed as declared, {} declared-but-dark, {} armed \
         without being declared (normal: a seed only declares what an older client would break on)",
        required.len(),
        h.agreed.len(),
        h.declared_not_armed.len(),
        h.armed_not_declared.len()
    );
    if !h.agreed.is_empty() {
        log::info!("feature-handshake: honoured {:?}", h.agreed);
    }
    if !h.armed_not_declared.is_empty() {
        log::info!(
            "feature-handshake: on but undeclared {:?}",
            h.armed_not_declared
        );
    }
    h.declared_not_armed
}

#[cfg(test)]
mod tests {
    use super::*;
    use er_logic::client_features::SUPPORTED;

    /// ⭐⭐⭐ THE GATE. A new entry in `SUPPORTED` with no probe would make this module quietly
    /// blind to exactly the feature most likely to have the #536 bug -- the newest one. A per-
    /// feature guard is not a gate; this is the gate.
    #[test]
    fn every_supported_tag_has_a_probe() {
        let mut probed: Vec<&str> = PROBES.iter().map(|(t, _)| *t).collect();
        let mut supported: Vec<&str> = SUPPORTED.to_vec();
        probed.sort_unstable();
        supported.sort_unstable();
        // 🛑 THE WITNESS. This gate is an `assert_eq!` between two lists, and two EMPTY lists
        // compare equal -- so if `SUPPORTED` were ever emptied (or this module's `PROBES` lost its
        // entries in a bad merge), the assertion below would pass while checking nothing. Shipping
        // a vacuous coverage gate inside a change that exists to stop vacuous gates would be a
        // joke at our own expense. Pin the floor and pin a known member.
        assert!(
            supported.len() >= 5,
            "vacuous: SUPPORTED has {} entries, so the equality below proves nothing",
            supported.len()
        );
        assert!(
            probed.contains(&"merchant_bells_on_talk"),
            "vacuous: the tag this whole module was written for is not in PROBES"
        );
        assert_eq!(
            probed, supported,
            "client_features::SUPPORTED and feature_handshake::PROBES must match EXACTLY. \
             A tag in SUPPORTED with no probe is a feature whose darkness nothing can see; a probe \
             for a tag that is not SUPPORTED is a claim of support this build does not make."
        );
    }

    /// The probe table must not carry duplicates: `reconcile`'s `any(live)` would then let one
    /// stale `true` mask a real `false`.
    #[test]
    fn the_probe_table_has_no_duplicate_tags() {
        let mut seen: Vec<&str> = PROBES.iter().map(|(t, _)| *t).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(before, seen.len(), "duplicate tag in PROBES");
    }

    /// Probes must be callable with no game attached and no region config -- this runs on a CI
    /// runner with no Elden Ring in sight, and a probe that dereferenced a game pointer would
    /// panic there long before a player ever saw the line.
    ///
    /// 🛑 ASSERTS NO VALUE, DELIBERATELY. Every probe reads a process-wide static, and the other
    /// tests in this binary run in the same process in parallel -- so "nothing is configured,
    /// therefore false" is true only until some unrelated test calls `set_enabled`. That is a
    /// draw-dependent assertion, and it would fail once a week and be re-run until green. The
    /// VALUE logic is tested where it is deterministic: `client_features::reconcile` in er-logic.
    #[test]
    fn probes_are_host_safe() {
        let ctx = ProbeCtx {
            region: None,
            armor_bundles: false,
            region_completion_goal_gate: false,
            reveal_sweep_boss_names: false,
        };
        for (_tag, p) in PROBES {
            let _ = p(&ctx);
        }
    }

    /// `grace_attunement` is the one probe that reads BORROWED state rather than a static, so it
    /// is the one that can be pinned deterministically: no config -> not armed, gated region ->
    /// armed. This is the shape a future probe should copy.
    #[test]
    fn the_grace_attunement_probe_follows_its_config() {
        let probe = PROBES
            .iter()
            .find(|(t, _)| *t == "grace_attunement")
            .expect("covered by every_supported_tag_has_a_probe")
            .1;
        assert!(
            !probe(&ProbeCtx {
                region: None,
                armor_bundles: false,
                region_completion_goal_gate: false,
                reveal_sweep_boss_names: false,
            }),
            "no config -> not armed"
        );

        let mut cfg = RegionConfig::default();
        assert!(
            !probe(&ProbeCtx {
                region: Some(&cfg),
                armor_bundles: false,
                region_completion_goal_gate: false,
                reveal_sweep_boss_names: false,
            }),
            "a seed that gates no region must not report the feature armed"
        );
        cfg.grace_attunement.insert(
            "Limgrave Lock".to_string(),
            crate::region::GraceGate::default(),
        );
        assert!(
            probe(&ProbeCtx {
                region: Some(&cfg),
                armor_bundles: false,
                region_completion_goal_gate: false,
                reveal_sweep_boss_names: false,
            }),
            "one gated region is what the apworld declares the tag for"
        );
    }

    /// End to end on the shape that shipped broken (#536): the seed declares the bell option and
    /// the probe table says it is not live, so the reconciliation names it. Driven through
    /// `reconcile` with an explicit table rather than through `log_and_report`, for the same
    /// parallel-statics reason as above.
    #[test]
    fn the_536_shape_is_reported() {
        let required = vec!["merchant_bells_on_talk".to_string()];
        let armed: Vec<(&str, bool)> = PROBES.iter().map(|(t, _)| (*t, false)).collect();
        let h = client_features::reconcile(&required, &armed);
        assert_eq!(
            h.declared_not_armed,
            vec!["merchant_bells_on_talk".to_string()]
        );
    }
}
