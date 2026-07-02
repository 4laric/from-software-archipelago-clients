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

    /// Saves the config file to disk.
    pub fn save(&self) -> Result<()> {
        Ok(fs::write(Self::path()?, json::to_string(&self.raw)?)?)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error as IoError, ErrorKind};

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
        for bad in ["{", r#"{ "slot": }"#, "not json", "[]", "null", "\"a string\"", "42"] {
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
}
