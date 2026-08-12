use std::{fs, io, marker::PhantomData, path::PathBuf};

use anyhow::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::{Game, utils};

/// The on-disk shape of `apconfig.json`. Split out from [Config] so the parse/fallback logic is a
/// plain, host-testable function with no dependency on the generic [Game] type or the filesystem.
///
/// `url`, `slot`, and `seed` default to empty so a partial or hand-written file (e.g. one that only
/// has `slot`, or one written by an older client) still parses instead of erroring on a missing
/// field. The connect overlay / [Config::is_configured] treat an empty `url`/`slot` as "not set yet"
/// and prompt for it (`CoreBase::is_configured`), so an incomplete config is recoverable in-game
/// rather than fatal.
#[derive(Default, Debug, PartialEq, Deserialize, Serialize)]
struct RawConfig {
    /// Diagnostic probe flags, grouped under their own object so a reader can tell at a glance
    /// which keys are diagnostics. Unknown names are KEPT, not rejected: a config written for a
    /// newer client must not break an older one. See `crate::probes`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    probes: std::collections::BTreeMap<String, bool>,
    #[serde(default)]
    url: String,
    #[serde(default)]
    slot: String,
    #[serde(default)]
    seed: String,
    client_version: Option<String>,
    password: Option<String>,
}

/// Parses `apconfig.json` text into a [RawConfig]. Empty or whitespace-only text is treated as an
/// empty config (not an error). Non-empty text must be a well-formed JSON object; anything else
/// (malformed JSON, a JSON array/scalar, etc.) is rejected.
fn parse_config(text: &str) -> Result<RawConfig> {
    if text.trim().is_empty() {
        return Ok(RawConfig::default());
    }
    Ok(json::from_str(text)?)
}

/// Serialises a [RawConfig] for disk. PRETTY, and with a trailing newline.
///
/// WHY PRETTY. `apconfig.json` is a file we ask players to OPEN AND EDIT BY HAND -- it is how you
/// set the server url and the slot, and since the probe work it is also how you turn a diagnostic
/// on without touching an environment variable. A single 96-character line is a poor thing to hand
/// someone for that, and it is the shape `save()` produced the moment they first connected through
/// the overlay, silently reflowing whatever they had typed.
///
/// 🛑 THE SHIPPED TEMPLATE MUST MATCH THIS SHAPE. `package_release.ps1` in the apworld repo writes
/// the generic apconfig a release ships with. If that stays one line while this is pretty, the file
/// changes shape under the player on first connect -- which is the same surprise, just delayed. The
/// two are kept in step by convention and by `the_template_shape_is_what_we_ship` below.
///
/// Trailing newline because it is a text file a human opens; editors and `cat` both expect one.
fn serialize_config(raw: &RawConfig) -> Result<String> {
    Ok(format!("{}\n", json::to_string_pretty(raw)?))
}

/// Resolves the config from the result of reading the config file. A *missing* file yields an empty
/// config (the connect overlay then prompts for the details); a present-but-malformed file, or any
/// other IO error (permissions, etc.), is surfaced as an error rather than silently ignored.
fn resolve_config(read: io::Result<String>) -> Result<RawConfig> {
    match read {
        Ok(text) => parse_config(&text).map_err(|e| e.context("failed to parse apconfig.json")),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(RawConfig::default()),
        Err(err) => Err(Error::from(err).context("failed to read apconfig.json")),
    }
}

/// The configuration for the Archipelago connection.
pub struct Config<G: Game> {
    raw: RawConfig,

    /// Associates a [Game] with the config without adding any data.
    _marker: PhantomData<G>,
}

impl<G: Game> Config<G> {
    /// Loads the config from disk. A missing or partial file is tolerated (see [resolve_config]); the
    /// connect overlay fills in anything that's missing.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        let raw = resolve_config(fs::read_to_string(&path))
            .map_err(|e| e.context(format!("config file {}", path.to_string_lossy())))?;
        Ok(Self {
            raw,
            _marker: PhantomData,
        })
    }

    /// Saves the config file to disk. See [serialize_config] for why it is pretty-printed.
    pub fn save(&self) -> Result<()> {
        Ok(fs::write(Self::path()?, serialize_config(&self.raw)?)?)
    }

    /// The path to the configuration file.
    fn path() -> Result<PathBuf> {
        // Elden Ring (pure-runtime, no static randomizer) reads apconfig.json next to the client DLL
        // — the dir the me3 profile's `[[natives]]` path points at — rather than me3's install root.
        // DS3/Sekiro keep the upstream mod-directory, where their static randomizer writes the config.
        let dir = if matches!(G::TYPE, crate::GameType::EldenRing) {
            utils::current_module_directory()?
        } else {
            utils::mod_directory()?.to_path_buf()
        };
        Ok(dir.join("apconfig.json"))
    }

    /// The diagnostic probe flags from the config file. See `crate::probes`.
    pub fn probes(&self) -> &std::collections::BTreeMap<String, bool> {
        &self.raw.probes
    }

    /// Returns the Archipelago server URL defined in the config (empty if not set).
    pub fn url(&self) -> &str {
        self.raw.url.as_str()
    }

    /// Sets the Archipelago server URL in the config file.
    pub fn set_url(&mut self, url: impl AsRef<str>) {
        self.raw.url = url.as_ref().to_string()
    }

    /// Sets the Archipelago slot (player) name in the config file.
    pub fn set_slot(&mut self, slot: impl AsRef<str>) {
        self.raw.slot = slot.as_ref().to_string()
    }

    /// Sets the Archipelago server password in the config file. `None` clears it.
    pub fn set_password(&mut self, password: Option<String>) {
        self.raw.password = password;
    }

    /// Returns the slot that the config was created with (empty if not set).
    pub fn slot(&self) -> &str {
        self.raw.slot.as_str()
    }

    /// Returns the seed that the config was created with.
    pub fn seed(&self) -> &str {
        self.raw.seed.as_str()
    }

    /// Returns the version of the static randomizer that the config was created
    /// with, or None if it doesn't contain a version (such as for a local
    /// randomizer build).
    pub fn client_version(&self) -> Option<&str> {
        self.raw.client_version.as_deref()
    }

    /// Returns the password that the config was created with, or None if it
    /// doesn't contain a password.
    pub fn password(&self) -> Option<&str> {
        self.raw.password.as_deref()
    }
}

/// Whether `url` is worth opening a socket to.
///
/// A url is NOT connectable when it is empty, or when it carries a port that is not a number.
///
/// 🛑 THE SECOND CASE EXISTS BECAUSE THE SHIPPED DEFAULT USES ONE. `package_release.ps1` writes
/// `{"url":"archipelago.gg:PORT", ...}`: archipelago.gg gives every room its OWN port at creation,
/// so no number could be correct there, and `38281` -- the local default -- beside `archipelago.gg`
/// would be a plausible-looking lie. `PORT` cannot be mistaken for a setting. Without this check it
/// would also not be SAFE: `Socket::connect` hands `wss://archipelago.gg:PORT` to tungstenite,
/// which fails to parse the uri, and the player gets a retry loop of parser errors instead of the
/// connect form. That is the same failure the empty-url guard above was written for, and the same
/// remedy applies -- treat it as "not configured yet" and let the overlay ask.
///
/// Deliberately narrow: it answers "could this port ever be a port", not "is this host real". A
/// bracketed IPv6 literal (`[::1]:38281`) still ends in digits and passes; a scheme's own colon is
/// stripped first so `wss://archipelago.gg` (no port at all) is connectable, as it always was.
pub fn is_connectable(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() {
        return false;
    }
    let host_part = url
        .split_once("://")
        .map(|(_scheme, rest)| rest)
        .unwrap_or(url);
    // Only the LAST colon can introduce a port; anything before it is IPv6 or noise we do not judge.
    match host_part.rsplit_once(':') {
        // A trailing bare colon ("host:") is as unusable as a bad port, and for the same reason.
        Some((_host, port)) => !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error as IoError, ErrorKind};

    /// THE MOTIVATING CASE (CONTRIBUTING rule 11): a playtester who cannot set an environment
    /// variable turns a probe on by adding one key to the file they already edit.
    #[test]
    fn probes_are_read_from_the_config_file() {
        let raw = parse_config(r#"{"url":"x","probes":{"esd":true}}"#).expect("parses");
        assert_eq!(raw.probes.get("esd"), Some(&true));
    }

    /// 🛑 THE REAL HAZARD IN THIS CHANGE. The connect overlay calls `save()`, which re-serialises
    /// the WHOLE config -- so a probes key that did not survive a round-trip would be silently
    /// deleted the first time the player connected, and the probe would stop working for reasons
    /// no log could explain.
    #[test]
    fn a_probe_flag_survives_a_save_round_trip() {
        let raw = parse_config(r#"{"url":"u","slot":"s","probes":{"esd":true}}"#).expect("parses");
        let written = serialize_config(&raw).expect("serialises");
        let reread = parse_config(&written).expect("re-parses");
        assert_eq!(reread.probes.get("esd"), Some(&true));
        assert_eq!(reread, raw);
    }

    /// An unknown probe name is KEPT, not dropped: a config written for a newer client that is
    /// opened (and saved) by an older one must not lose the newer client's settings.
    #[test]
    fn an_unknown_probe_name_is_preserved() {
        let raw = parse_config(r#"{"probes":{"not_a_real_probe_yet":true}}"#).expect("parses");
        let reread = parse_config(&serialize_config(&raw).unwrap()).expect("re-parses");
        assert_eq!(reread.probes.get("not_a_real_probe_yet"), Some(&true));
    }

    /// Nobody who never touched a probe should find `"probes":{}` appearing in their config.
    #[test]
    fn an_empty_probe_map_is_not_written_out() {
        let raw = parse_config(r#"{"url":"u"}"#).expect("parses");
        assert!(!serialize_config(&raw).unwrap().contains("probes"));
    }

    /// THE MOTIVATING CASE (CONTRIBUTING rule 11): Alaric, 2026-08-12 -- "the default apconfig,
    /// can we write that so it's formatted across multiple lines?" It is a file we ask people to
    /// hand-edit, and it was one 96-character line.
    #[test]
    fn the_config_is_written_across_multiple_lines() {
        let raw = parse_config(r#"{"url":"u","slot":"s"}"#).expect("parses");
        let out = serialize_config(&raw).expect("serialises");
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() > 3, "config is still one line: {out:?}");
        assert_eq!(lines[0], "{", "the object should open on its own line");
        assert!(
            out.ends_with("}\n"),
            "want a closing brace and a newline: {out:?}"
        );
        // Every key on its own indented line -- the property a hand-editor cares about.
        for key in ["url", "slot", "seed", "client_version", "password"] {
            let want = format!("  \"{key}\":");
            let found = lines.iter().any(|l| l.starts_with(&want));
            assert!(found, "no indented line for {key} in {out:?}");
        }
    }

    /// 🛑 CROSS-REPO: this is the exact text `package_release.ps1` (apworld repo) ships as the
    /// generic apconfig. Pinned here because the client is what would REWRITE it -- if the two
    /// shapes drift, a player's file silently reflows the first time they connect through the
    /// overlay -- the surprise the pretty-printing exists to remove. Change one, change both.
    #[test]
    fn the_template_shape_is_what_we_ship() {
        let shipped = concat!(
            r#"{"url":"archipelago.gg:PORT","slot":"Player1","seed":"","#,
            r#""client_version":null,"password":null}"#,
        );
        let raw = parse_config(shipped).expect("the shipped template parses");
        let want = concat!(
            "{\n",
            "  \"url\": \"archipelago.gg:PORT\",\n",
            "  \"slot\": \"Player1\",\n",
            "  \"seed\": \"\",\n",
            "  \"client_version\": null,\n",
            "  \"password\": null\n",
            "}\n",
        );
        assert_eq!(serialize_config(&raw).expect("serialises"), want);
    }

    /// A config with no `probes` key at all is ordinary, not an error -- every config in the wild
    /// today is one of these.
    #[test]
    fn a_config_without_probes_still_parses() {
        let raw = parse_config(r#"{"url":"u","slot":"s"}"#).expect("parses");
        assert!(raw.probes.is_empty());
    }

    fn missing() -> io::Result<String> {
        Err(IoError::from(ErrorKind::NotFound))
    }

    fn ok(text: &str) -> io::Result<String> {
        Ok(text.to_string())
    }

    #[test]
    fn missing_file_is_empty_config() {
        // The whole point of the pure-runtime flow: no file yet -> empty config, overlay prompts.
        assert_eq!(resolve_config(missing()).unwrap(), RawConfig::default());
    }

    #[test]
    fn full_config_parses_every_field() {
        let cfg = resolve_config(ok(
            r#"{ "url": "archipelago.gg:38281", "slot": "Alaric", "seed": "abc", "password": "hunter2", "client_version": "1.2.3" }"#,
        ))
        .unwrap();
        assert_eq!(cfg.url, "archipelago.gg:38281");
        assert_eq!(cfg.slot, "Alaric");
        assert_eq!(cfg.seed, "abc");
        assert_eq!(cfg.password.as_deref(), Some("hunter2"));
        assert_eq!(cfg.client_version.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn partial_slot_only_parses_with_empty_url() {
        // The exact file that hard-crashed the old client: slot present, url missing.
        let cfg = resolve_config(ok(r#"{ "slot": "MigTest" }"#)).unwrap();
        assert_eq!(cfg.slot, "MigTest");
        assert!(cfg.url.is_empty());
        assert!(cfg.password.is_none());
    }

    #[test]
    fn partial_url_only_parses_with_empty_slot() {
        let cfg = resolve_config(ok(r#"{ "url": "localhost:38281" }"#)).unwrap();
        assert_eq!(cfg.url, "localhost:38281");
        assert!(cfg.slot.is_empty());
    }

    #[test]
    fn empty_object_is_empty_config() {
        assert_eq!(resolve_config(ok("{}")).unwrap(), RawConfig::default());
    }

    #[test]
    fn empty_and_whitespace_text_is_empty_config() {
        for text in ["", "   ", "\n\t  \r\n"] {
            assert_eq!(
                resolve_config(ok(text)).unwrap(),
                RawConfig::default(),
                "text {text:?} should parse as empty"
            );
        }
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // Old/forward-compat keys (e.g. bake-era location_flags, or a stray death_link) must not
        // break parsing.
        let cfg = resolve_config(ok(
            r#"{ "url": "x:1", "slot": "s", "death_link": true, "location_flags": {"1": 2}, "bogus": 5 }"#,
        ))
        .unwrap();
        assert_eq!(cfg.url, "x:1");
        assert_eq!(cfg.slot, "s");
    }

    #[test]
    fn malformed_or_non_object_json_is_rejected() {
        for bad in [
            "{",
            r#"{ "slot": }"#,
            "not json",
            "[]",
            "null",
            "\"a string\"",
            "42",
        ] {
            assert!(
                resolve_config(ok(bad)).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn non_notfound_io_error_is_surfaced() {
        // A missing file is fine; a permissions error (or anything else) must NOT be swallowed.
        let read = Err(IoError::from(ErrorKind::PermissionDenied));
        assert!(resolve_config(read).is_err());
    }

    #[test]
    fn a_placeholder_port_is_not_connectable() {
        // The shipped default, verbatim. If this ever returns true the release template starts a
        // retry loop of tungstenite parse errors on every fresh install.
        assert!(!is_connectable("archipelago.gg:PORT"));
        assert!(!is_connectable("archipelago.gg:<port>"));
        assert!(!is_connectable("archipelago.gg:12345x"));
        assert!(!is_connectable("archipelago.gg:"));
        assert!(!is_connectable(""));
        assert!(!is_connectable("   "));
    }

    #[test]
    fn real_urls_are_still_connectable() {
        // WITNESS for the test above: a guard that rejects everything would pass it and break the
        // client. These are the shapes players actually have.
        assert!(is_connectable("archipelago.gg:12345"));
        assert!(is_connectable("localhost:38281"));
        assert!(is_connectable("wss://archipelago.gg:12345"));
        assert!(is_connectable("ws://localhost:38281"));
        assert!(is_connectable("archipelago.gg")); // port defaults to 38281 downstream
        assert!(is_connectable("wss://archipelago.gg"));
        assert!(is_connectable("[::1]:38281"));
        assert!(is_connectable("192.168.0.10:38281"));
    }
}
