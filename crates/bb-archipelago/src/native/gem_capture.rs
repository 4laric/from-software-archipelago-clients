//! Bounded JSONL capture for the read-only native ItemGrant probe.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::item_grant_probe::ItemGrantCallSnapshot;

pub struct GemCapture {
    file: File,
    previous_sequence: u64,
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
            previous_sequence: 0,
            warned: false,
        })
    }

    pub fn observe(&mut self, snapshot: Option<ItemGrantCallSnapshot>) {
        let Some(snapshot) = snapshot else { return };
        if snapshot.sequence == self.previous_sequence {
            return;
        }
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

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_sequence_is_not_logged_twice() {
        let root = std::env::temp_dir().join(format!(
            "bb-item-grant-capture-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let ledger = root.join("ledger.json");
        let mut capture = GemCapture::beside_ledger(&ledger).unwrap();
        let call = ItemGrantCallSnapshot {
            sequence: 2,
            inventory: 1,
            descriptor_address: 2,
            quantity: 3,
            raw_id: 4,
            internal_pointer: 5,
            normalized_id: 6,
            caller: 7,
        };
        capture.observe(Some(call.clone()));
        capture.observe(Some(call));
        drop(capture);
        let text = std::fs::read_to_string(root.join("blood-gem-capture.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("\"event\":\"item_grant_call\""), "{text}");
        assert!(
            text.contains("\"missed_calls_since_previous_sample\":1"),
            "{text}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
