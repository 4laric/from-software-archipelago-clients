use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};

use crate::RUNTIME_BUILD;

pub const BRIDGE_PROTOCOL: &str = "BBGRANT1";
pub const HARNESS_VERSION: &str = "bb-native-grant-v7";

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
    session_started: SystemTime,
}

impl FileBridge {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            session_started: SystemTime::now(),
        }
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

    /// Whether a state for `tag` has a witness from this client session.
    ///
    /// A matching command is the durable crash-recovery witness. Without one,
    /// the harness must have rewritten the state file since this bridge was
    /// attached; otherwise a completed state left by a dead CE session could
    /// falsely acknowledge a later grant that happens to reuse the same tag.
    pub fn state_is_current_for(&self, tag: &str) -> Result<bool> {
        let command_modified = match fs::read_to_string(self.command_path()) {
            Ok(command) if command.split_whitespace().last() == Some(tag) => {
                Some(fs::metadata(self.command_path())?.modified()?)
            }
            Ok(_) => None,
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("reading bridge command {}", self.command_path().display())
                });
            }
        };
        let modified = match fs::metadata(self.state_path()) {
            Ok(metadata) => metadata.modified(),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "reading bridge state metadata {}",
                        self.state_path().display()
                    )
                });
            }
        }?;
        Ok(modified >= command_modified.unwrap_or(self.session_started))
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

    /// Withdraw a pending command the harness has NEVER reported touching
    /// (clients#296). Returns `true` when a file was removed.
    ///
    /// The bridge is asynchronous: a published command can execute against
    /// whatever save is loaded when the harness picks it up, not the save that
    /// was validated at publication. Until the harness can witness save
    /// identity at mutation time (bb-archipelago#56), the client-side half of
    /// the guarantee is that a command is never left lying around for an
    /// execution the client can no longer vouch for: on context loss, and at
    /// startup for a leftover from a previous process, the client withdraws.
    ///
    /// "Unwitnessed" is the load-bearing word. If the durable state names this
    /// tag AT ALL -- success, terminal failure, or an in-progress status --
    /// the harness owns the command: success must stay for the
    /// [`Self::acknowledge_command`] recovery path, failure stays for
    /// diagnosis, and an in-progress execution cannot be stopped by deleting a
    /// file it has already read. Withdrawing any of those changes nothing
    /// about safety and breaks the recovery semantics, so they are left alone.
    /// An unreadable/absent state means no witness exists, which is exactly
    /// the case that must be withdrawn.
    ///
    /// The durable item plan lives in the receive ledger, so a withdrawn
    /// command is not a lost item: the next poll under a validated context
    /// re-publishes it, and the harness-side `expected_before` check plus the
    /// state echo keep a race with an in-flight execution from double-granting.
    pub fn withdraw_unwitnessed_command(&self, tag: &str) -> Result<bool> {
        let path = self.command_path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        };
        // Same ownership rule as acknowledge_command: never touch a command
        // file whose tag is not the one the caller is responsible for.
        anyhow::ensure!(
            text.split_whitespace().last() == Some(tag),
            "refusing to withdraw a grant command for a different tag"
        );
        if let Ok(state) = self.read_state()
            && state.concerns_tag(tag)
        {
            return Ok(false); // witnessed: the harness owns this command now
        }
        fs::remove_file(&path)
            .with_context(|| format!("withdrawing unwitnessed {}", path.display()))?;
        Ok(true)
    }

    pub fn read_state(&self) -> Result<BridgeState> {
        parse_state_file(&self.state_path())
    }
}

/// The grant bridge has never been written: no state file exists at all
/// (clients#404).
///
/// This is a dedicated error type rather than a string the console has to
/// sniff, because it is the one bridge failure with a player-facing remedy
/// ("the grant harness is not running") rather than a diagnosis. Callers
/// classify it with [`missing_bridge_state`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeStateMissing {
    pub path: PathBuf,
}

impl std::fmt::Display for BridgeStateMissing {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "no grant bridge state at {}",
            self.path.display()
        )
    }
}

impl std::error::Error for BridgeStateMissing {}

/// Recover the [`BridgeStateMissing`] cause from anywhere in an error chain.
///
/// The condition is raised inside a grant attempt and reaches the console
/// wrapped in context, so the whole chain is searched.
pub fn missing_bridge_state(error: &anyhow::Error) -> Option<&BridgeStateMissing> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<BridgeStateMissing>())
}

fn parse_state_file(path: &Path) -> Result<BridgeState> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(BridgeStateMissing {
                path: path.to_path_buf(),
            }
            .into());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("reading bridge state {}", path.display()));
        }
    };
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

    /// clients#404: an absent state file is a classifiable condition, not a
    /// raw `os error 2` the console has to string-match.
    #[test]
    fn absent_state_file_is_a_typed_missing_bridge() {
        let root = temp_root("missing-state");
        let bridge = FileBridge::new(&root);
        let error = bridge.read_state().expect_err("no state file exists");
        let missing = missing_bridge_state(&error).expect("classified as a missing bridge");
        assert_eq!(missing.path, bridge.state_path());
        // The same classification survives the context a grant attempt adds.
        assert!(missing_bridge_state(&error.context("granting ap_7")).is_some());
    }

    #[test]
    fn a_malformed_state_file_is_not_a_missing_bridge() {
        let root = temp_root("malformed-state");
        let bridge = FileBridge::new(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(bridge.state_path(), "build=x\n").unwrap();
        let error = bridge.read_state().expect_err("no status key");
        assert!(missing_bridge_state(&error).is_none());
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
            "build=bb-0.1.0-r9\nprotocol=BBGRANT1\nharness=bb-native-grant-v7\nstatus=completed\npid=5040\ntag=received_17\ndetail=direct before=2 after=3\n",
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
    fn stale_success_without_a_session_witness_is_not_current() {
        let root = temp_root("stale-success");
        fs::create_dir_all(&root).unwrap();
        let state_path = root.join("native-grant-state.txt");
        fs::write(&state_path, "status=completed\ntag=received_17\n").unwrap();
        let modified = fs::metadata(&state_path).unwrap().modified().unwrap();
        let bridge = FileBridge {
            root: root.clone(),
            session_started: modified + Duration::from_secs(1),
        };
        assert!(!bridge.state_is_current_for("received_17").unwrap());

        // Keep the stale state and newly-created command on distinct filesystem
        // timestamps. Fast Windows runners can otherwise coalesce both writes,
        // making the test claim the old state was rewritten after enqueue.
        std::thread::sleep(Duration::from_millis(10));
        bridge.enqueue(&pebble()).unwrap();
        assert!(!bridge.state_is_current_for("received_17").unwrap());
        fs::write(&state_path, "status=completed\ntag=received_17\n").unwrap();
        assert!(bridge.state_is_current_for("received_17").unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn state_rewritten_during_this_session_is_current_without_a_command() {
        let root = temp_root("fresh-success");
        fs::create_dir_all(&root).unwrap();
        let bridge = FileBridge {
            root: root.clone(),
            session_started: SystemTime::UNIX_EPOCH,
        };
        fs::write(bridge.state_path(), "status=completed\ntag=received_17\n").unwrap();
        assert!(bridge.state_is_current_for("received_17").unwrap());
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

    #[test]
    fn withdraws_a_command_the_harness_never_witnessed() {
        // No state file at all: the harness never saw this command (e.g. it
        // was down at publication, or the client process died right after).
        let root = temp_root("withdraw-unwitnessed");
        let bridge = FileBridge::new(&root);
        bridge.enqueue(&pebble()).unwrap();
        assert!(bridge.withdraw_unwitnessed_command("received_17").unwrap());
        assert!(!bridge.command_pending());
        // A second withdrawal is a no-op, not an error.
        assert!(!bridge.withdraw_unwitnessed_command("received_17").unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_to_withdraw_a_witnessed_command() {
        for status in ["completed", "failed", "awaiting_inventory"] {
            let root = temp_root(&format!("withdraw-witnessed-{status}"));
            let bridge = FileBridge::new(&root);
            bridge.enqueue(&pebble()).unwrap();
            fs::write(
                bridge.state_path(),
                format!(
                    "build={RUNTIME_BUILD}\nprotocol={BRIDGE_PROTOCOL}\nharness={HARNESS_VERSION}\nstatus={status}\ntag=received_17\ndetail=\n"
                ),
            )
            .unwrap();
            assert!(!bridge.withdraw_unwitnessed_command("received_17").unwrap());
            assert!(bridge.command_pending());
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn withdraws_when_the_state_names_a_different_tag() {
        let root = temp_root("withdraw-other-tag");
        let bridge = FileBridge::new(&root);
        bridge.enqueue(&pebble()).unwrap();
        fs::write(
            bridge.state_path(),
            format!(
                "build={RUNTIME_BUILD}\nprotocol={BRIDGE_PROTOCOL}\nharness={HARNESS_VERSION}\nstatus=completed\ntag=received_16\ndetail=\n"
            ),
        )
        .unwrap();
        assert!(bridge.withdraw_unwitnessed_command("received_17").unwrap());
        assert!(!bridge.command_pending());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn never_withdraws_a_command_for_a_different_tag() {
        let root = temp_root("withdraw-tag-guard");
        let bridge = FileBridge::new(&root);
        bridge.enqueue(&pebble()).unwrap();
        assert!(bridge.withdraw_unwitnessed_command("received_18").is_err());
        assert!(bridge.command_pending());
        fs::remove_dir_all(root).unwrap();
    }
}
