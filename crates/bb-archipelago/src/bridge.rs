use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};

use crate::RUNTIME_BUILD;

pub const BRIDGE_PROTOCOL: &str = "BBGRANT1";
pub const HARNESS_VERSION: &str = "bb-native-grant-v3";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrantCommand {
    pub raw_id: u32,
    pub normalized_id: u32,
    pub quantity: u32,
    /// `None` asks the harness to sample and durably record the live baseline
    /// before executing. That baseline makes restart recovery decidable even
    /// when vanilla inventory already contains the same consumable.
    pub expected_before: Option<u32>,
    pub tag: String,
}

impl GrantCommand {
    fn encode(&self) -> Result<String> {
        if self.quantity == 0 || self.quantity > 99 {
            bail!("grant quantity must be between 1 and 99");
        }
        if self.tag.is_empty() || self.tag.chars().any(char::is_whitespace) {
            bail!("grant tag must be one non-empty token");
        }
        let expected = self
            .expected_before
            .map_or_else(|| "AUTO".to_owned(), |value| value.to_string());
        Ok(format!(
            "{BRIDGE_PROTOCOL} GRANT 0x{:08X} 0x{:08X} {} {} {}",
            self.raw_id, self.normalized_id, self.quantity, expected, self.tag
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeState {
    pub build: Option<String>,
    pub protocol: Option<String>,
    pub harness: Option<String>,
    pub status: String,
    pub pid: Option<u32>,
    pub tag: Option<String>,
    pub detail: String,
}

impl BridgeState {
    pub fn is_success(&self) -> bool {
        matches!(self.status.as_str(), "completed" | "recovered_complete")
    }

    pub fn require_compatible(&self) -> Result<()> {
        anyhow::ensure!(
            self.build.as_deref() == Some(RUNTIME_BUILD),
            "Bloodborne runtime build mismatch: expected {RUNTIME_BUILD}, found {}",
            self.build.as_deref().unwrap_or("missing")
        );
        anyhow::ensure!(
            self.protocol.as_deref() == Some(BRIDGE_PROTOCOL),
            "grant bridge protocol mismatch: expected {BRIDGE_PROTOCOL}, found {}",
            self.protocol.as_deref().unwrap_or("missing")
        );
        anyhow::ensure!(
            self.harness.as_deref() == Some(HARNESS_VERSION),
            "grant harness mismatch: expected {HARNESS_VERSION}, found {}",
            self.harness.as_deref().unwrap_or("missing")
        );
        Ok(())
    }

    pub fn concerns_tag(&self, tag: &str) -> bool {
        self.tag.as_deref() == Some(tag) || {
            let wanted = format!("tag={tag}");
            self.detail.split_whitespace().any(|part| part == wanted)
        }
    }

    pub fn is_terminal_failure(&self) -> bool {
        matches!(
            self.status.as_str(),
            "failed" | "command_rejected" | "quantity_mismatch" | "setup_error" | "write_error"
        )
    }
}

#[derive(Clone, Debug)]
pub struct FileBridge {
    root: PathBuf,
}

impl FileBridge {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn command_path(&self) -> PathBuf {
        self.root.join("native-grant-command.txt")
    }

    pub fn state_path(&self) -> PathBuf {
        self.root.join("native-grant-state.txt")
    }

    /// Publishes one command without ever replacing an unacknowledged command.
    pub fn enqueue(&self, command: &GrantCommand) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("creating bridge directory {}", self.root.display()))?;
        let path = self.command_path();
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                bail!("a Bloodborne grant command is already pending")
            }
            Err(error) => {
                return Err(error).with_context(|| format!("creating {}", path.display()));
            }
        };
        let encoded = command.encode()?;
        if let Err(error) = file
            .write_all(encoded.as_bytes())
            .and_then(|_| file.sync_all())
        {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(error).with_context(|| format!("writing {}", path.display()));
        }
        Ok(())
    }

    pub fn command_pending(&self) -> bool {
        self.command_path().exists()
    }

    pub fn command_is_stale(&self, timeout: Duration) -> Result<bool> {
        let path = self.command_path();
        let modified = match fs::metadata(&path) {
            Ok(metadata) => metadata.modified(),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error).with_context(|| format!("reading {} metadata", path.display()));
            }
        }?;
        Ok(SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default()
            >= timeout)
    }

    /// Remove only the command acknowledged by a matching durable state. This
    /// closes the crash window where the harness wrote `completed` but exited
    /// before it could unlink the command file.
    pub fn acknowledge_command(&self, tag: &str) -> Result<()> {
        let path = self.command_path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        };
        anyhow::ensure!(
            text.split_whitespace().last() == Some(tag),
            "refusing to acknowledge a grant command for a different tag"
        );
        fs::remove_file(&path).with_context(|| format!("removing acknowledged {}", path.display()))
    }

    pub fn read_state(&self) -> Result<BridgeState> {
        parse_state_file(&self.state_path())
    }
}

fn parse_state_file(path: &Path) -> Result<BridgeState> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading bridge state {}", path.display()))?;
    let mut status = None;
    let mut build = None;
    let mut protocol = None;
    let mut harness = None;
    let mut pid = None;
    let mut tag = None;
    let mut detail = String::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "build" => build = Some(value.to_owned()),
            "protocol" => protocol = Some(value.to_owned()),
            "harness" => harness = Some(value.to_owned()),
            "status" => status = Some(value.to_owned()),
            "pid" if !value.is_empty() => {
                pid = Some(value.parse().context("invalid bridge pid")?);
            }
            "tag" if !value.is_empty() => tag = Some(value.to_owned()),
            "detail" => detail = value.to_owned(),
            _ => {}
        }
    }
    Ok(BridgeState {
        build,
        protocol,
        harness,
        status: status.context("bridge state has no status")?,
        pid,
        tag,
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "bb-bridge-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn pebble() -> GrantCommand {
        GrantCommand {
            raw_id: 0xB000_04CE,
            normalized_id: 0x4000_04CE,
            quantity: 1,
            expected_before: None,
            tag: "received_17".into(),
        }
    }

    #[test]
    fn publishes_exact_runtime_contract_and_never_overwrites() {
        let root = temp_root("enqueue");
        let bridge = FileBridge::new(&root);
        bridge.enqueue(&pebble()).unwrap();
        assert_eq!(
            fs::read_to_string(bridge.command_path()).unwrap(),
            "BBGRANT1 GRANT 0xB00004CE 0x400004CE 1 AUTO received_17"
        );
        assert!(bridge.enqueue(&pebble()).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_success_state() {
        let root = temp_root("state");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("native-grant-state.txt"),
            "build=bb-0.1.0-r3\nprotocol=BBGRANT1\nharness=bb-native-grant-v3\nstatus=completed\npid=5040\ntag=received_17\ndetail=direct before=2 after=3\n",
        )
        .unwrap();
        let state = FileBridge::new(&root).read_state().unwrap();
        assert!(state.is_success());
        state.require_compatible().unwrap();
        assert!(state.concerns_tag("received_17"));
        assert_eq!(state.pid, Some(5040));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_an_old_or_unversioned_harness() {
        let state = BridgeState {
            build: None,
            protocol: None,
            harness: Some("bb-native-grant-v2".into()),
            status: "awaiting_inventory".into(),
            pid: Some(1),
            tag: None,
            detail: String::new(),
        };
        assert!(state.require_compatible().is_err());
    }

    #[test]
    fn detects_a_stale_pending_command() {
        let root = temp_root("stale");
        let bridge = FileBridge::new(&root);
        bridge.enqueue(&pebble()).unwrap();
        assert!(bridge.command_is_stale(Duration::ZERO).unwrap());
        assert!(!bridge.command_is_stale(Duration::from_secs(30)).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn acknowledges_only_the_matching_command() {
        let root = temp_root("ack");
        let bridge = FileBridge::new(&root);
        bridge.enqueue(&pebble()).unwrap();
        assert!(bridge.acknowledge_command("received_18").is_err());
        assert!(bridge.command_pending());
        bridge.acknowledge_command("received_17").unwrap();
        assert!(!bridge.command_pending());
        fs::remove_dir_all(root).unwrap();
    }
}
