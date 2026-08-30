//! Read-only shop-enablement capture for Bloodborne shop randomization research.
//!
//! Static `ShopLineupParam` edits already power randomized starting weapons.
//! What the corpus cannot prove is the live seam around ordinary shops: which
//! inventory good unlocks stock, and what record a natural purchase creates.
//! This capture diffs the inventory walk and keeps a focused heartbeat for the
//! Emblem, workshop tools, and hunter badges. It never writes guest memory.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::guest::InventoryEntry;

const HEARTBEAT_MS: u128 = 5_000;

pub struct ShopCapture {
    file: File,
    previous: Option<Vec<InventoryEntry>>,
    last_sample_ms: u128,
    warned: bool,
}

impl ShopCapture {
    pub fn beside_ledger(ledger: &Path) -> std::io::Result<Self> {
        let path = ledger
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("shop-capture.jsonl");
        Ok(Self {
            file: OpenOptions::new().create(true).append(true).open(path)?,
            previous: None,
            last_sample_ms: 0,
            warned: false,
        })
    }

    pub fn observe(&mut self, entries: Option<Vec<InventoryEntry>>) {
        let Some(entries) = entries else { return };
        let now = now_ms();
        let first = self.previous.is_none();
        let changed = self.previous.as_ref() != Some(&entries);
        if first || changed || now.saturating_sub(self.last_sample_ms) >= HEARTBEAT_MS {
            let previous = self.previous.as_deref().unwrap_or_default();
            let (added, removed, modified) = inventory_diff(previous, &entries);
            self.write(json::json!({
                "event": if first { "baseline" } else if changed { "inventory_change" } else { "heartbeat" },
                "at_unix_ms": now,
                "inventory_rows": entries.len(),
                "shop_unlock_goods": entries.iter().filter(|entry| is_shop_unlock_good(entry)).map(entry_json).collect::<Vec<_>>(),
                "added": added.iter().map(|entry| entry_json(entry)).collect::<Vec<_>>(),
                "removed": removed.iter().map(|entry| entry_json(entry)).collect::<Vec<_>>(),
                "modified": modified.iter().map(|(before, after)| json::json!({
                    "before": entry_json(before),
                    "after": entry_json(after),
                })).collect::<Vec<_>>(),
            }));
            self.last_sample_ms = now;
        }
        self.previous = Some(entries);
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

fn inventory_diff<'a>(
    before: &'a [InventoryEntry],
    after: &'a [InventoryEntry],
) -> (
    Vec<&'a InventoryEntry>,
    Vec<&'a InventoryEntry>,
    Vec<(&'a InventoryEntry, &'a InventoryEntry)>,
) {
    let before_by_slot = before
        .iter()
        .map(|entry| (entry.slot, entry))
        .collect::<BTreeMap<_, _>>();
    let after_by_slot = after
        .iter()
        .map(|entry| (entry.slot, entry))
        .collect::<BTreeMap<_, _>>();
    let added = after_by_slot
        .iter()
        .filter(|(slot, _)| !before_by_slot.contains_key(slot))
        .map(|(_, entry)| *entry)
        .collect();
    let removed = before_by_slot
        .iter()
        .filter(|(slot, _)| !after_by_slot.contains_key(slot))
        .map(|(_, entry)| *entry)
        .collect();
    let modified = before_by_slot
        .iter()
        .filter_map(|(slot, before)| {
            after_by_slot
                .get(slot)
                .filter(|after| before.bytes != after.bytes)
                .map(|after| (*before, *after))
        })
        .collect();
    (added, removed, modified)
}

fn is_shop_unlock_good(entry: &InventoryEntry) -> bool {
    let full_id = entry.word(4);
    if full_id & 0xF000_0000 != 0x4000_0000 {
        return false;
    }
    matches!(full_id & 0x00FF_FFFF, 4011 | 4102..=4120)
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

    fn entry(slot: u32, full_id: u32, quantity: u32) -> InventoryEntry {
        let mut bytes = [0u8; 16];
        bytes[4..8].copy_from_slice(&full_id.to_le_bytes());
        bytes[8..12].copy_from_slice(&quantity.to_le_bytes());
        InventoryEntry {
            slot,
            address: 0x9000 + u64::from(slot) * 16,
            bytes,
        }
    }

    #[test]
    fn focuses_only_canonical_emblem_tools_and_badges() {
        assert!(is_shop_unlock_good(&entry(1, 0x4000_0FA0 + 11, 1)));
        assert!(is_shop_unlock_good(&entry(2, 0x4000_1006, 1)));
        assert!(is_shop_unlock_good(&entry(3, 0x4000_1018, 1)));
        assert!(!is_shop_unlock_good(&entry(4, 0xB000_1016, 1)));
        assert!(!is_shop_unlock_good(&entry(5, 0x4000_03E8, 20)));
    }

    #[test]
    fn purchase_transition_records_added_and_changed_rows() {
        let vial = entry(1, 0x4000_03E8, 1);
        let mut two_vials = vial.clone();
        two_vials.bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
        let pebble = entry(2, 0x4000_03F2, 1);
        let before = [vial];
        let after = [two_vials, pebble];
        let (added, removed, modified) = inventory_diff(&before, &after);
        assert_eq!(
            added.iter().map(|entry| entry.slot).collect::<Vec<_>>(),
            [2]
        );
        assert!(removed.is_empty());
        assert_eq!(modified.len(), 1);
        assert_eq!(modified[0].0.word(8), 1);
        assert_eq!(modified[0].1.word(8), 2);
    }
}
