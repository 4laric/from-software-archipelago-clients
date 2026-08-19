use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrantCommand {
    pub raw_id: u32,
    pub normalized_id: u32,
    pub quantity: u32,
    pub expected_before: u32,
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
        Ok(format!(
            "GRANT 0x{:08X} 0x{:08X} {} {} {}",
            self.raw_id, self.normalized_id, self.quantity, self.expected_before, self.tag
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeState {
    pub status: String,
    pub pid: Option<u32>,
    pub detail: String,
}

impl BridgeState {
    pub fn is_success(&self) -> bool {
        matches!(self.status.as_str(), "completed" | "recovered_complete")
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

    pub fn read_state(&self) -> Result<BridgeState> {
        parse_state_file(&self.state_path())
    }
}

fn parse_state_file(path: &Path) -> Result<BridgeState> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading bridge state {}", path.display()))?;
    let mut status = None;
    let mut pid = None;
    let mut detail = String::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "status" => status = Some(value.to_owned()),
            "pid" if !value.is_empty() => {
                pid = Some(value.parse().context("invalid bridge pid")?);
            }
            "detail" => detail = value.to_owned(),
            _ => {}
        }
    }
    Ok(BridgeState {
        status: status.context("bridge state has no status")?,
        pid,
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
            expected_before: 2,
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
            "GRANT 0xB00004CE 0x400004CE 1 2 received_17"
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
            "status=completed\npid=5040\ndetail=tag=received_17 direct before=2 after=3\n",
        )
        .unwrap();
        let state = FileBridge::new(&root).read_state().unwrap();
        assert!(state.is_success());
        assert_eq!(state.pid, Some(5040));
        fs::remove_dir_all(root).unwrap();
    }
}
