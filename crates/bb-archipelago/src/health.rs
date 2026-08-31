//! Small machine-readable liveness contract for the launcher.
//!
//! The file is a heartbeat, not a promise: consumers must reject it when it is
//! missing, malformed, or stale.  That makes a killed or wedged client fail
//! closed instead of leaving yesterday's `true` values on screen.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

pub const HEALTH_FILE_NAME: &str = "client-health.json";
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ClientHealth {
    pub format: &'static str,
    pub updated_unix_ms: u128,
    pub pid: u32,
    pub process_alive: bool,
    pub ap_connected: bool,
    pub delivery_armed: bool,
    pub detail: String,
}

impl ClientHealth {
    fn new(ap_connected: bool, delivery_armed: bool, detail: impl Into<String>) -> Self {
        Self {
            format: "bb-client-health-v1",
            updated_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            pid: std::process::id(),
            process_alive: true,
            ap_connected,
            delivery_armed,
            detail: detail.into(),
        }
    }
}

pub struct HealthReporter {
    path: PathBuf,
    last: Option<(bool, bool, String)>,
    last_write: Option<Instant>,
}

impl HealthReporter {
    pub fn beside_ledger(ledger: &Path) -> Self {
        Self {
            path: ledger
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(HEALTH_FILE_NAME),
            last: None,
            last_write: None,
        }
    }

    /// Publish on a state transition and periodically while unchanged.
    ///
    /// Health reporting is observational: inability to write it must never
    /// stop gameplay or delivery, so callers receive the error only to log.
    pub fn publish(
        &mut self,
        ap_connected: bool,
        delivery_armed: bool,
        detail: impl Into<String>,
    ) -> std::io::Result<bool> {
        let detail = detail.into();
        let state = (ap_connected, delivery_armed, detail.clone());
        let due = self.last.as_ref() != Some(&state)
            || self
                .last_write
                .is_none_or(|last| last.elapsed() >= HEARTBEAT_INTERVAL);
        if !due {
            return Ok(false);
        }
        let health = ClientHealth::new(ap_connected, delivery_armed, detail);
        write_atomic(&self.path, &health)?;
        self.last = Some(state);
        self.last_write = Some(Instant::now());
        Ok(true)
    }
}

fn write_atomic(path: &Path, health: &ClientHealth) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let bytes = json::to_vec_pretty(health)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    fs::write(&temporary, bytes)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_is_machine_readable_and_state_changes_write_immediately() {
        let root = std::env::temp_dir().join(format!("bb-health-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let ledger = root.join("ledger.json");
        let mut reporter = HealthReporter::beside_ledger(&ledger);
        assert!(reporter.publish(false, false, "starting").unwrap());
        assert!(!reporter.publish(false, false, "starting").unwrap());
        assert!(reporter.publish(true, true, "ready").unwrap());
        let value: json::Value = json::from_slice(
            &fs::read(root.join(HEALTH_FILE_NAME)).expect("health sidecar exists"),
        )
        .expect("health sidecar is JSON");
        assert_eq!(value["format"], "bb-client-health-v1");
        assert_eq!(value["process_alive"], true);
        assert_eq!(value["ap_connected"], true);
        assert_eq!(value["delivery_armed"], true);
        assert_eq!(value["detail"], "ready");
        let _ = fs::remove_dir_all(root);
    }
}
