//! Read-only inventory-manager snapshots for category-8 blood-gem research.
//!
//! ItemLot category 8 is a generation recipe, not a runtime descriptor prefix.
//! The former probe incorrectly classified category-1 armor ids beginning in
//! `0x1...` as gems and watched only the ordinary held-item arrays. Natural gem
//! acquisitions proved that view incomplete. Snapshot the owning manager and
//! its bounded guest-pointer blocks instead, so before/after captures can reveal
//! the separate generated-gem container without guessing its layout.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::guest::InventoryDiagnosticSnapshot;

const HEARTBEAT_MS: u128 = 5_000;

pub struct GemCapture {
    file: File,
    previous: Option<InventoryDiagnosticSnapshot>,
    last_sample_ms: u128,
    warned: bool,
}

impl GemCapture {
    pub fn beside_ledger(ledger: &Path) -> std::io::Result<Self> {
        let path = ledger
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("blood-gem-capture.jsonl");
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file,
            previous: None,
            last_sample_ms: 0,
            warned: false,
        })
    }

    pub fn observe(&mut self, snapshot: Option<InventoryDiagnosticSnapshot>) {
        let Some(snapshot) = snapshot else { return };
        let now = now_ms();
        let changed = self.previous.as_ref() != Some(&snapshot);
        if changed || now.saturating_sub(self.last_sample_ms) >= HEARTBEAT_MS {
            self.write(json::json!({
                "event": if self.previous.is_none() { "manager_baseline" } else if changed { "manager_delta" } else { "manager_heartbeat" },
                "at_unix_ms": now,
                "manager_address": format!("0x{:X}", snapshot.manager_address),
                "manager_bytes": hex(&snapshot.manager_bytes),
                "pointer_blocks": snapshot.pointer_blocks.iter().map(|block| json::json!({
                    "manager_offset": format!("0x{:X}", block.manager_offset),
                    "address": format!("0x{:X}", block.address),
                    "bytes": hex(&block.bytes),
                })).collect::<Vec<_>>(),
            }));
            self.last_sample_ms = now;
        }
        self.previous = Some(snapshot);
    }

    fn write(&mut self, value: json::Value) {
        if self.warned {
            return;
        }
        if writeln!(self.file, "{value}")
            .and_then(|_| self.file.flush())
            .is_err()
        {
            self.warned = true;
        }
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::guest::InventoryPointerBlock;

    #[test]
    fn manager_snapshots_never_claim_that_runtime_ids_are_gems() {
        let root = std::env::temp_dir().join(format!(
            "bb-gem-capture-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let ledger = root.join("ledger.json");
        let mut capture = GemCapture::beside_ledger(&ledger).unwrap();
        capture.observe(Some(InventoryDiagnosticSnapshot {
            manager_address: 0x2080_0000,
            manager_bytes: vec![0x70, 0x82, 0x03, 0x10],
            pointer_blocks: vec![InventoryPointerBlock {
                manager_offset: 0x48,
                address: 0x2080_1000,
                bytes: vec![1, 2, 3, 4],
            }],
        }));
        drop(capture);

        let text = std::fs::read_to_string(root.join("blood-gem-capture.jsonl")).unwrap();
        assert!(text.contains("\"event\":\"manager_baseline\""), "{text}");
        assert!(text.contains("\"manager_offset\":\"0x48\""), "{text}");
        assert!(!text.contains("normalized_id"), "{text}");
        assert!(!text.contains("generated_object"), "{text}");
        std::fs::remove_dir_all(root).unwrap();
    }
}
