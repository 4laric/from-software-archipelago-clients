use std::{fs, io, marker::PhantomData, path::PathBuf};

use anyhow::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::{Game, utils};

/// The configuration file for the Archipelago connection.
#[derive(Deserialize, Serialize)]
pub struct Config<G: Game> {
    url: String,
    slot: String,
    #[serde(default)]
    seed: String,
    client_version: Option<String>,
    password: Option<String>,
    #[serde(skip)]
    _marker: PhantomData<G>,
}

impl<G: Game> Config<G> {
    /// Loads the config from disk.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        match fs::read_to_string(&path) {
            Ok(text) => json::from_str(&text).map_err(|err| {
                Error::from(err).context(format!(
                    "Failed to parse config file {}",
                    path.to_string_lossy()
                ))
            }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                // Elden Ring has no static randomizer to write apconfig.json. Rather than treating a
                // missing file as fatal, start from an empty config and let the in-game connect
                // overlay collect server/slot/password and write it on first Connect. DS3/Sekiro keep
                // the hard error, since their static randomizer is responsible for the file.
                if matches!(G::TYPE, crate::GameType::EldenRing) {
                    Ok(Self::empty())
                } else {
                    Err(Error::from(err).context(format!(
                        "{} doesn't exist. Have you run randomizer\\{}?",
                        path.to_string_lossy(),
                        G::TYPE.static_randomizer_basename()
                    )))
                }
            }
            Err(err) => Err(Error::from(err).context(format!(
                "Failed to load config file {}",
                path.to_string_lossy()
            ))),
        }
    }

    /// An empty config with no connection info. Used as the starting point for Elden Ring, whose
    /// `apconfig.json` is written by the in-game connect overlay rather than a static randomizer.
    fn empty() -> Self {
        Self {
            url: String::new(),
            slot: String::new(),
            seed: String::new(),
            client_version: None,
            password: None,
            _marker: PhantomData,
        }
    }

    /// Saves the config file to disk.
    pub fn save(&self) -> Result<()> {
        Ok(fs::write(Self::path()?, json::to_string(self)?)?)
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

    /// Returns the Archipelago server URL defined in the config, or None if it
    /// doesn't contain a URL.
    pub fn url(&self) -> &str {
        self.url.as_str()
    }

    /// Sets the Archipelago server URL in the config file.
    pub fn set_url(&mut self, url: impl AsRef<str>) {
        self.url = url.as_ref().to_string()
    }

    /// Sets the Archipelago slot (player) name in the config file.
    pub fn set_slot(&mut self, slot: impl AsRef<str>) {
        self.slot = slot.as_ref().to_string()
    }

    /// Sets the Archipelago server password in the config file. `None` clears it.
    pub fn set_password(&mut self, password: Option<String>) {
        self.password = password;
    }

    /// Returns the slot that the config was created with.
    pub fn slot(&self) -> &str {
        self.slot.as_str()
    }

    /// Returns the seed that the config was created with.
    pub fn seed(&self) -> &str {
        self.seed.as_str()
    }

    /// Returns the version of the static randomizer that the config was created
    /// with, or None if it doesn't contain a version (such as for a local
    /// randomizer build).
    pub fn client_version(&self) -> Option<&str> {
        self.client_version.as_deref()
    }

    /// Returns the password that the config was created with, or None if it
    /// doesn't contain a password.
    pub fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }
}
