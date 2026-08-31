//! Read-only zero-Blood-Vial diagnostics for bb-archipelago#70.
//!
//! A shop-only first Vial can exist on the HUD without a canonical inventory
//! record.  Sample the bounded inventory walk every five seconds and record
//! every row whose low item id is 1000, plus immediate state transitions.  The
//! game remains the only writer.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::guest::InventoryEntry;

const VIAL_ID: u32 = 1000;
const CANONICAL_VIAL: u32 = 0x4000_03E8;
const HEARTBEAT_MS: u128 = 5_000;

pub struct VialCapture {
    file: File,
    previous: Option<Vec<InventoryEntry>>,
    last_sample_ms: u128,
    warned: bool,
}

impl VialCapture {
    pub fn beside_ledger(ledger: &Path) -> std::io::Result<Self> {
        let path = ledger
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("blood-vial-capture.jsonl");
        Ok(Self {
            file: OpenOptions::new().create(true).append(true).open(path)?,
            previous: None,
            last_sample_ms: 0,
            warned: false,
        })
    }

    /// Record canonical Vial rows and low-id collisions without passing this
    /// ordinary stackable good through the generated-instance resolver.
    pub fn observe(&mut self, entries: Option<Vec<InventoryEntry>>) {
        let Some(entries) = entries else { return };
        let suspects = entries
            .into_iter()
            .filter(|entry| {
                entry.word(0) & 0x00FF_FFFF == VIAL_ID || entry.word(4) & 0x00FF_FFFF == VIAL_ID
            })
            .collect::<Vec<_>>();
        let now = now_ms();
        let changed = self.previous.as_ref() != Some(&suspects);
        if changed || now.saturating_sub(self.last_sample_ms) >= HEARTBEAT_MS {
            self.write(json::json!({
                "event": if self.previous.is_none() { "baseline" } else if changed { "vial_state_change" } else { "heartbeat" },
                "at_unix_ms": now,
                "canonical_vial_present": suspects.iter().any(|entry| entry.word(4) == CANONICAL_VIAL),
                "suspect_rows": suspects.iter().map(entry_json).collect::<Vec<_>>(),
            }));
            self.last_sample_ms = now;
        }
        self.previous = Some(suspects);
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

fn entry_json(entry: &InventoryEntry) -> json::Value {
    json::json!({
        "slot": entry.slot,
        "address": format!("0x{:X}", entry.address),
        "word0": format!("0x{:08X}", entry.word(0)),
        "word4": format!("0x{:08X}", entry.word(4)),
        "word8": format!("0x{:08X}", entry.word(8)),
        "word12": format!("0x{:08X}", entry.word(12)),
        "bytes": entry.bytes.iter().map(|byte| format!("{byte:02X}")).collect::<Vec<_>>().join(" "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(slot: u32, word0: u32, word4: u32) -> InventoryEntry {
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&word0.to_le_bytes());
        bytes[4..8].copy_from_slice(&word4.to_le_bytes());
        InventoryEntry {
            slot,
            address: 0x9000 + u64::from(slot) * 16,
            bytes,
        }
    }

    #[test]
    fn collisions_and_canonical_rows_are_logged_without_an_object_probe() {
        let root = std::env::temp_dir().join(format!(
            "bb-vial-capture-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let ledger = root.join("ledger.json");
        let mut capture = VialCapture::beside_ledger(&ledger).unwrap();
        capture.observe(Some(vec![entry(2, 0x8080_02C0, 0x0000_03E8)]));
        let canonical = entry(14, 0xB000_03E8, CANONICAL_VIAL);
        capture.observe(Some(vec![canonical]));
        drop(capture);
        let text = std::fs::read_to_string(root.join("blood-vial-capture.jsonl")).unwrap();
        assert!(text.contains("0x808002C0"));
        assert!(text.contains("\"canonical_vial_present\":true"));
        assert!(!text.contains("backing_address"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
