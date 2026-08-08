//! Diagnostic probe gating -- environment variable OR `apconfig.json`, so a playtester never has to
//! set an environment variable.
//!
//! ## Why this exists
//!
//! Every probe in the client was gated on an `ER_*` env var, which is fine for a developer and
//! close to useless for the people whose machines the readings have to come from. A playtester
//! launches the game through a mod loader -- frequently a host randomizer's own loader, not a shell
//! -- so there is no natural place to hang a per-process variable. It becomes a wrapper `.bat` or a
//! system-wide setting: easy to get wrong, and impossible for us to confirm from the log they send
//! back.
//!
//! `apconfig.json` is a file they already have and have already edited (it holds the server URL),
//! and it sits beside the DLL. One line in a familiar file beats one environment variable in an
//! unfamiliar place.
//!
//! ## Precedence: the environment WINS
//!
//! A developer running a one-off with `ER_ESD_PROBE=1` must not have it silently ignored because a
//! config file says `false`. The env var is the override; the config is the persistent default.
//! [`resolve`] is that rule, kept pure so it can be tested without a filesystem or a process.
//!
//! ## The config shape
//!
//! ```json
//! { "url": "...", "slot": "...", "probes": { "esd": true } }
//! ```
//!
//! Probes are grouped under their own object rather than scattered across the top level: the rest
//! of `apconfig.json` is connection settings, and a reader should be able to tell at a glance which
//! keys are diagnostics they can safely delete. It also keeps a probe name from ever colliding with
//! a future connection key.
//!
//! Unknown keys are kept, not rejected -- a config written for a newer client must not break an
//! older one, and a typo'd probe name should leave the client working. 🛑 The cost is that a typo
//! is SILENT, which is exactly why [`log_active`] exists: the log states which probes are actually
//! on, so "I set it and nothing happened" is answerable from the log the player already sends.

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use log::info;

/// Probe flags parsed from `apconfig.json`, installed once at startup.
///
/// A map rather than named fields because `shared` is game-agnostic: the probe names belong to the
/// game crates, and `shared` has no business enumerating Elden Ring's diagnostics.
static CONFIG_PROBES: OnceLock<BTreeMap<String, bool>> = OnceLock::new();

/// Installs the config-file probe flags. Call once, at startup, after the config is loaded.
///
/// Later calls are ignored: the flags are read on a hot path via [`enabled`], so this is a
/// `OnceLock` rather than a lock, and a second install would mean two sources of truth for the same
/// question.
pub fn install(probes: BTreeMap<String, bool>) {
    let _ = CONFIG_PROBES.set(probes);
}

/// The resolution rule, pure: environment first, then config, then off.
///
/// `env_present` is "the variable is set to anything at all" -- matching the long-standing
/// `var_os(..).is_some()` convention in this codebase, where `ER_FOO=0` still means ON. That is
/// surprising in isolation but it is what every existing probe already does, and quietly changing
/// it here would alter the behaviour of flags people have in their notes.
pub fn resolve(env_present: bool, config: Option<bool>) -> bool {
    env_present || config.unwrap_or(false)
}

/// Is this probe on? `env_var` is the legacy variable, `key` the `probes` object key.
pub fn enabled(env_var: &str, key: &str) -> bool {
    let config = CONFIG_PROBES
        .get()
        .and_then(|probes| probes.get(key))
        .copied();
    resolve(std::env::var_os(env_var).is_some(), config)
}

/// WHICH gate turned this probe on, for a log line. `None` when it is off.
///
/// 🛑 EXISTS BECAUSE A BANNER THAT NAMES THE WRONG GATE IS WORSE THAN NO BANNER. Before this, the
/// ESD probe announced itself as "ACTIVE (ER_ESD_PROBE set)" unconditionally -- so a playtester who
/// turned it on the new way, from `apconfig.json`, would read a line crediting an environment
/// variable they had never set, and reasonably conclude the config key had done nothing and
/// something else was responsible. An instrument misreporting its own state has cost this project
/// a full triage cycle more than once.
pub fn source(env_var: &str, key: &str) -> Option<String> {
    if std::env::var_os(env_var).is_some() {
        return Some(format!("env {env_var}"));
    }
    let on = CONFIG_PROBES
        .get()
        .and_then(|probes| probes.get(key))
        .copied()
        .unwrap_or(false);
    on.then(|| format!("apconfig probes.{key}"))
}

/// Has the active-probe line been emitted? See [`log_active`].
static ANNOUNCED: AtomicBool = AtomicBool::new(false);

/// Logs which probes are active, so "I turned it on and nothing happened" is answerable from the
/// log rather than from a conversation.
///
/// Takes the (env_var, key) pairs the caller cares about, because `shared` does not know them.
///
/// 🛑🛑 SELF-LATCHING, AND THAT IS NOT A CONVENIENCE. This shipped unlatched on 2026-08-08 and the
/// natural call site is `core::update_live`, which runs EVERY TICK -- so boblerrr's first session
/// on it logged `probes: none active` over two thousand times in ninety seconds. Its neighbours
/// there (`warp_hook::install`, `esd_probe::install`) are all self-guarded one-shots, which is
/// exactly why the call looked correct sitting among them.
///
/// The latch lives HERE rather than at the call site on purpose: a caller that has to remember is
/// a caller that will forget, and the next probe added to the list would reintroduce it.
pub fn log_active(pairs: &[(&str, &str)]) {
    if ANNOUNCED.swap(true, Ordering::Relaxed) {
        return;
    }
    let active: Vec<&str> = pairs
        .iter()
        .filter(|(env_var, key)| enabled(env_var, key))
        .map(|(_, key)| *key)
        .collect();
    if active.is_empty() {
        info!("probes: none active");
    } else {
        info!(
            "probes: ACTIVE -- {}. These are diagnostics; they change logging, and an ARM flag can \
             change behaviour.",
            active.join(", ")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE MOTIVATING CASE (CONTRIBUTING rule 11): a playtester who cannot set an environment
    /// variable turns a probe on from the file they already edit.
    #[test]
    fn a_config_flag_turns_a_probe_on_with_no_env_var() {
        assert!(resolve(false, Some(true)));
    }

    #[test]
    fn nothing_set_anywhere_is_off() {
        assert!(!resolve(false, None));
        assert!(!resolve(false, Some(false)));
    }

    /// The env var is an OVERRIDE, not a peer: a developer's one-off must not be silently cancelled
    /// by a config file left over from a previous session.
    #[test]
    fn the_environment_wins_over_a_false_config() {
        assert!(resolve(true, Some(false)));
        assert!(resolve(true, None));
        assert!(resolve(true, Some(true)));
    }

    /// The banner must credit the gate that ACTUALLY turned the probe on. Naming the env var when
    /// the config key did it is how a working probe gets reported as broken.
    #[test]
    fn source_is_none_when_the_probe_is_off() {
        // No env var set for a name nothing uses, and no config installed in this test binary.
        assert_eq!(source("ER_A_PROBE_THAT_DOES_NOT_EXIST", "nope"), None);
    }

    /// The regression that put this latch here: unlatched, at a per-tick call site, it emitted
    /// thousands of identical lines into a playtester's log.
    #[test]
    fn log_active_announces_at_most_once() {
        // Two calls; the second must be a no-op. Asserted on the latch rather than on captured
        // output, because the logger is process-global and a test must not depend on log capture.
        ANNOUNCED.store(false, Ordering::Relaxed);
        log_active(&[("ER_NOT_A_REAL_PROBE", "nope")]);
        assert!(
            ANNOUNCED.load(Ordering::Relaxed),
            "first call must announce and latch"
        );
        log_active(&[("ER_NOT_A_REAL_PROBE", "nope")]);
        assert!(ANNOUNCED.load(Ordering::Relaxed), "latch must stay set");
    }

    /// An absent key is absent, not false -- they are the same answer today, but the distinction is
    /// what would let a future "off unless explicitly set" default exist without a rewrite.
    #[test]
    fn an_absent_key_and_an_explicit_false_agree_today() {
        assert_eq!(resolve(false, None), resolve(false, Some(false)));
    }
}
