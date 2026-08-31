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
    previous_suspects: Option<Vec<InventoryEntry>>,
    previous_entries: Option<Vec<InventoryEntry>>,
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
            previous_suspects: None,
            previous_entries: None,
            last_sample_ms: 0,
            warned: false,
        })
    }

    /// Record canonical Vial rows and low-id collisions without passing this
    /// ordinary stackable good through the generated-instance resolver.
    pub fn observe(&mut self, entries: Option<Vec<InventoryEntry>>) {
        let Some(entries) = entries else { return };
        let suspects = entries
            .iter()
            .filter(|entry| {
                entry.word(0) & 0x00FF_FFFF == VIAL_ID || entry.word(4) & 0x00FF_FFFF == VIAL_ID
            })
            .cloned()
            .collect::<Vec<_>>();
        let now = now_ms();
        let changed = self.previous_suspects.as_ref() != Some(&suspects);
        let previous_canonical = self
            .previous_suspects
            .as_ref()
            .is_some_and(|rows| rows.iter().any(|entry| entry.word(4) == CANONICAL_VIAL));
        let canonical = suspects
            .iter()
            .find(|entry| entry.word(4) == CANONICAL_VIAL);
        // A populated first observation is only a baseline. Without an earlier inventory
        // snapshot there is no witnessed transition and therefore no valid "before" window.
        let canonical_created =
            self.previous_suspects.is_some() && !previous_canonical && canonical.is_some();
        if changed || now.saturating_sub(self.last_sample_ms) >= HEARTBEAT_MS {
            let mut record = json::json!({
                "event": if self.previous_suspects.is_none() { "baseline" } else if canonical_created { "canonical_vial_created" } else if changed { "vial_state_change" } else { "heartbeat" },
                "at_unix_ms": now,
                "canonical_vial_present": canonical.is_some(),
                "suspect_rows": suspects.iter().map(entry_json).collect::<Vec<_>>(),
            });
            if canonical_created {
                let selected = canonical.expect("canonical row was just established");
                record["selected_slot"] = json::json!(selected.slot);
                record["before_window"] = json::json!(slot_window(
                    self.previous_entries.as_deref().unwrap_or(&[]),
                    selected.slot
                ));
                record["after_window"] = json::json!(slot_window(&entries, selected.slot));
                record["last_slot_before"] = json::json!(
                    self.previous_entries
                        .as_ref()
                        .and_then(|rows| rows.last())
                        .map(|row| row.slot)
                );
                record["last_slot_after"] = json::json!(entries.last().map(|row| row.slot));
            }
            self.write(record);
            self.last_sample_ms = now;
        }
        self.previous_suspects = Some(suspects);
        self.previous_entries = Some(entries);
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

fn slot_window(entries: &[InventoryEntry], selected_slot: u32) -> Vec<json::Value> {
    let first = selected_slot.saturating_sub(2);
    let last = selected_slot.saturating_add(2);
    (first..=last)
        .map(|slot| {
            entries
                .iter()
                .find(|entry| entry.slot == slot)
                .map(entry_json)
                .unwrap_or_else(|| json::json!({ "slot": slot, "present": false }))
        })
        .collect()
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
        assert!(text.contains("\"event\":\"canonical_vial_created\""));
        assert!(text.contains("\"selected_slot\":14"));
        assert!(text.contains("\"before_window\""));
        assert!(text.contains("\"after_window\""));
        assert!(!text.contains("backing_address"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn creation_window_keeps_before_bytes_and_marks_an_appended_slot_absent() {
        let root = std::env::temp_dir().join(format!(
            "bb-vial-capture-window-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let ledger = root.join("ledger.json");
        let mut capture = VialCapture::beside_ledger(&ledger).unwrap();
        capture.observe(Some(vec![entry(4, 0xB000_04CE, 0x4000_04CE)]));
        capture.observe(Some(vec![
            entry(4, 0xB000_04CE, 0x4000_04CE),
            entry(5, 0xB000_03E8, CANONICAL_VIAL),
        ]));
        drop(capture);
        let lines = std::fs::read_to_string(root.join("blood-vial-capture.jsonl")).unwrap();
        let created: json::Value = lines
            .lines()
            .map(|line| json::from_str(line).unwrap())
            .find(|record: &json::Value| record["event"] == "canonical_vial_created")
            .unwrap();
        assert_eq!(created["selected_slot"], 5);
        assert_eq!(created["last_slot_before"], 4);
        assert_eq!(created["last_slot_after"], 5);
        assert_eq!(created["before_window"][2]["slot"], 5);
        assert_eq!(created["before_window"][2]["present"], false);
        assert_eq!(created["after_window"][2]["word4"], "0x400003E8");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn populated_first_observation_remains_a_baseline_without_a_fake_before_window() {
        let root = std::env::temp_dir().join(format!(
            "bb-vial-capture-baseline-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let ledger = root.join("ledger.json");
        let mut capture = VialCapture::beside_ledger(&ledger).unwrap();
        capture.observe(Some(vec![entry(14, 0xB000_03E8, CANONICAL_VIAL)]));
        drop(capture);

        let line = std::fs::read_to_string(root.join("blood-vial-capture.jsonl")).unwrap();
        let baseline: json::Value = json::from_str(line.trim()).unwrap();
        assert_eq!(baseline["event"], "baseline");
        assert!(baseline.get("selected_slot").is_none());
        assert!(baseline.get("before_window").is_none());
        assert!(baseline.get("after_window").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
