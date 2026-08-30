//! Passive natural-award capture for category-8 blood-gem research.
//!
//! The game must remain the only writer. This module merely diffs the same
//! bounded inventory records the delivery verifier already reads and appends
//! evidence beside the receive ledger.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::guest::{GeneratedObjectProbe, InventoryEntry};

pub struct GemCapture {
    file: File,
    previous: Option<BTreeMap<u32, InventoryEntry>>,
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
            warned: false,
        })
    }

    pub fn observe(&mut self, entries: Option<Vec<InventoryEntry>>) -> Vec<InventoryEntry> {
        let Some(entries) = entries else {
            return Vec::new();
        };
        let current: BTreeMap<_, _> = entries
            .into_iter()
            .map(|entry| (entry.slot, entry))
            .collect();
        if self.previous.is_none() {
            let rows = current.values().map(entry_json).collect::<Vec<_>>();
            self.write(json::json!({
                "event": "baseline", "at_unix_ms": now_ms(),
                "occupied_slots": current.len(), "entries": rows,
            }));
            self.previous = Some(current);
            return Vec::new();
        }
        let previous = self.previous.as_ref().unwrap();
        let slots: BTreeSet<_> = previous.keys().chain(current.keys()).copied().collect();
        let deltas = slots
            .into_iter()
            .filter_map(|slot| {
                let before = previous.get(&slot);
                let after = current.get(&slot);
                (before != after).then(|| {
                    json::json!({
                        "slot": slot,
                        "before": before.map(entry_json),
                        "after": after.map(entry_json),
                    })
                })
            })
            .collect::<Vec<_>>();
        // Retain the concrete records before mutably borrowing the logger.
        let generated = current
            .iter()
            .filter(|(slot, entry)| {
                previous.get(slot) != Some(*entry) && entry.word(4) & 0xF000_0000 == 0x1000_0000
            })
            .map(|(_, entry)| entry.clone())
            .collect();
        if !deltas.is_empty() {
            self.write(json::json!({
                "event": "inventory_delta", "at_unix_ms": now_ms(), "deltas": deltas,
            }));
        }
        // Return concrete newly-added category-8-shaped records to the
        // game-thread resolver probe. Inventory JSON values above are for the
        // log only; retain the original records here.
        self.previous = Some(current);
        generated
    }

    pub fn record_generated_object(&mut self, probe: &GeneratedObjectProbe) {
        self.write(json::json!({
            "event": "generated_object",
            "at_unix_ms": now_ms(),
            "entry": entry_json(&probe.entry),
            "backing_address": format!("0x{:X}", probe.address),
            "backing_bytes": probe.bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "),
        }));
    }

    fn write(&mut self, value: json::Value) {
        if self.warned {
            return;
        }
        if writeln!(self.file, "{}", value)
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
        "handle_or_word0": format!("0x{:08X}", entry.word(0)),
        "normalized_id": format!("0x{:08X}", entry.word(4)),
        "word8": format!("0x{:08X}", entry.word(8)),
        "word12": format!("0x{:08X}", entry.word(12)),
        "bytes": entry.bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(slot: u32, id: u32) -> InventoryEntry {
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&(0xC080_0000 + slot).to_le_bytes());
        bytes[4..8].copy_from_slice(&id.to_le_bytes());
        InventoryEntry {
            slot,
            address: 0x9000 + u64::from(slot) * 16,
            bytes,
        }
    }

    #[test]
    fn a_natural_inventory_change_is_logged_without_filtering_category_eight() {
        let root = std::env::temp_dir().join(format!(
            "bb-gem-capture-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let ledger = root.join("ledger.json");
        let mut capture = GemCapture::beside_ledger(&ledger).unwrap();
        capture.observe(Some(vec![entry(4, 0x4000_03E8)]));
        let generated = capture.observe(Some(vec![entry(4, 0x4000_03E8), entry(9, 0x1001_E078)]));
        assert_eq!(generated.len(), 1);
        drop(capture);

        let text = std::fs::read_to_string(root.join("blood-gem-capture.jsonl")).unwrap();
        let rows = text
            .lines()
            .map(|line| json::from_str::<json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(rows[0]["event"], "baseline");
        assert_eq!(rows[1]["event"], "inventory_delta");
        assert_eq!(rows[1]["deltas"][0]["after"]["normalized_id"], "0x1001E078");
        std::fs::remove_dir_all(root).unwrap();
    }
}
