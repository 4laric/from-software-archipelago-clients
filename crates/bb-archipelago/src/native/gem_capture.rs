//! Bounded JSONL capture for the read-only native ItemGrant probe.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::guest::InventoryEntry;
use super::item_grant_probe::ItemGrantCallSnapshot;

pub struct GemCapture {
    file: File,
    previous_sequence: u64,
    previous_category_eight: Option<BTreeMap<u32, [u8; 16]>>,
    image_base: u64,
    warned: bool,
}

const AP_GRANT_CAVE_START_RVA: u64 = 0x50D_B800;
const AP_GRANT_CAVE_END_RVA: u64 = 0x50D_C000;

impl GemCapture {
    pub fn beside_ledger(ledger: &Path, image_base: u64) -> std::io::Result<Self> {
        let path = ledger
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("blood-gem-capture.jsonl");
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file,
            previous_sequence: 0,
            previous_category_eight: None,
            image_base,
            warned: false,
        })
    }

    /// Observe completed category-8 inventory records without touching guest
    /// code. Blood gems and Caryll runes share this representation: the first
    /// word is a generated instance handle (`0xC...`) and the second word is
    /// the source GemGenParam id (`0x8...`). The old ItemGrant-only probe sees
    /// the outer lot descriptor for most natural pickups and cannot recover
    /// either value.
    pub fn observe_inventory(&mut self, entries: Option<Vec<InventoryEntry>>) {
        let Some(entries) = entries else { return };
        let current = entries
            .into_iter()
            .filter(|entry| is_category_eight(entry))
            .map(|entry| (entry.slot, entry.bytes))
            .collect::<BTreeMap<_, _>>();
        let Some(previous) = self.previous_category_eight.replace(current.clone()) else {
            self.write(json::json!({
                "event": "category8_baseline",
                "at_unix_ms": now_ms(),
                "records": current.iter().map(|(&slot, bytes)| category_eight_json(slot, bytes)).collect::<Vec<_>>(),
            }));
            return;
        };
        for (&slot, bytes) in &current {
            if previous.get(&slot) == Some(bytes) {
                continue;
            }
            self.write(json::json!({
                "event": "category8_instance_change",
                "at_unix_ms": now_ms(),
                "slot": slot,
                "before": previous.get(&slot).map(|value| category_eight_json(slot, value)),
                "after": category_eight_json(slot, bytes),
            }));
        }
    }

    pub fn observe(&mut self, snapshots: Vec<ItemGrantCallSnapshot>) {
        for snapshot in snapshots {
            if snapshot.sequence <= self.previous_sequence {
                continue;
            }
            self.observe_new(snapshot);
        }
    }

    fn observe_new(&mut self, snapshot: ItemGrantCallSnapshot) {
        let caller_rva = snapshot.caller.checked_sub(self.image_base);
        let origin = match caller_rva {
            Some(AP_GRANT_CAVE_START_RVA..AP_GRANT_CAVE_END_RVA) => "ap_delivery",
            _ => "game",
        };
        let missed = snapshot
            .sequence
            .saturating_sub(self.previous_sequence)
            .saturating_sub(1);
        self.previous_sequence = snapshot.sequence;
        self.write(json::json!({
            "event": "item_grant_call",
            "at_unix_ms": now_ms(),
            "sequence": snapshot.sequence,
            "missed_calls_since_previous_sample": missed,
            "inventory": format!("0x{:X}", snapshot.inventory),
            "descriptor_address": format!("0x{:X}", snapshot.descriptor_address),
            "quantity": snapshot.quantity,
            "raw_id": format!("0x{:08X}", snapshot.raw_id),
            "internal_pointer": format!("0x{:X}", snapshot.internal_pointer),
            "normalized_id": format!("0x{:08X}", snapshot.normalized_id),
            "caller": format!("0x{:X}", snapshot.caller),
            "caller_rva": caller_rva.map(|rva| format!("0x{rva:X}")),
            "origin": origin,
            "natural_pickup_candidate": origin == "game",
        }));
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

fn is_category_eight(entry: &InventoryEntry) -> bool {
    entry.word(0) & 0xF000_0000 == 0xC000_0000 && entry.word(4) & 0xF000_0000 == 0x8000_0000
}

fn category_eight_json(slot: u32, bytes: &[u8; 16]) -> json::Value {
    let word = |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    json::json!({
        "slot": slot,
        "raw_descriptor": bytes.iter().map(|byte| format!("{byte:02X}")).collect::<String>(),
        "instance_handle": format!("0x{:08X}", word(0)),
        "normalized_param": format!("0x{:08X}", word(4)),
        "gem_gen_param_id": word(4) & 0x0FFF_FFFF,
        "quantity": word(8),
        "inventory_slot": word(12),
    })
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inventory_entry(slot: u32, words: [u32; 4]) -> InventoryEntry {
        let mut bytes = [0; 16];
        for (index, word) in words.into_iter().enumerate() {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        InventoryEntry {
            slot,
            address: 0x9000 + u64::from(slot) * 16,
            bytes,
        }
    }

    #[test]
    fn repeated_sequence_is_not_logged_twice() {
        let root = std::env::temp_dir().join(format!(
            "bb-item-grant-capture-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let ledger = root.join("ledger.json");
        let mut capture = GemCapture::beside_ledger(&ledger, 0).unwrap();
        let call = ItemGrantCallSnapshot {
            sequence: 2,
            inventory: 1,
            descriptor_address: 2,
            quantity: 3,
            raw_id: 4,
            internal_pointer: 5,
            normalized_id: 6,
            caller: 0x50D_BB44,
        };
        capture.observe(vec![call.clone()]);
        capture.observe(vec![call]);
        drop(capture);
        let text = std::fs::read_to_string(root.join("blood-gem-capture.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("\"event\":\"item_grant_call\""), "{text}");
        assert!(
            text.contains("\"missed_calls_since_previous_sample\":1"),
            "{text}"
        );
        assert!(text.contains("\"origin\":\"ap_delivery\""), "{text}");
        assert!(
            text.contains("\"natural_pickup_candidate\":false"),
            "{text}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn completed_category_eight_instance_reports_handle_and_param() {
        let root = std::env::temp_dir().join(format!(
            "bb-category8-capture-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let ledger = root.join("ledger.json");
        let mut capture = GemCapture::beside_ledger(&ledger, 0).unwrap();
        capture.observe_inventory(Some(vec![inventory_entry(
            7,
            [0xB000_03E8, 0x4000_03E8, 1, 7],
        )]));
        capture.observe_inventory(Some(vec![
            inventory_entry(7, [0xB000_03E8, 0x4000_03E8, 1, 7]),
            inventory_entry(102, [0xC080_0ED5, 0x8001_91F5, 1, 102]),
        ]));
        drop(capture);
        let text = std::fs::read_to_string(root.join("blood-gem-capture.jsonl")).unwrap();
        assert!(text.contains("category8_instance_change"), "{text}");
        assert!(
            text.contains("\"instance_handle\":\"0xC0800ED5\""),
            "{text}"
        );
        assert!(text.contains("\"gem_gen_param_id\":102901"), "{text}");
        assert!(text.contains("\"inventory_slot\":102"), "{text}");
        assert!(!text.contains("0xB00003E8"), "{text}");
        std::fs::remove_dir_all(root).unwrap();
    }
}
