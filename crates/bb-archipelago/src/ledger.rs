use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReceiveLedger {
    pub slots: BTreeMap<String, SlotLedger>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SlotLedger {
    pub highest_processed_index: Option<u64>,
    pub acknowledged: BTreeMap<u64, AcknowledgedItem>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AcknowledgedItem {
    pub ap_item_id: i64,
    pub normalized_item_id: u32,
    pub quantity: u32,
}

impl ReceiveLedger {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let temporary = path.with_extension("tmp");
        let backup = path.with_extension("bak");
        let bytes = json::to_vec_pretty(self)?;
        fs::write(&temporary, bytes).with_context(|| format!("writing {}", temporary.display()))?;
        if path.exists() {
            let _ = fs::remove_file(&backup);
            fs::rename(path, &backup).with_context(|| format!("backing up {}", path.display()))?;
        }
        if let Err(error) = fs::rename(&temporary, path) {
            if backup.exists() {
                let _ = fs::rename(&backup, path);
            }
            return Err(error).with_context(|| format!("publishing {}", path.display()));
        }
        let _ = fs::remove_file(backup);
        Ok(())
    }
}

impl ReceiveLedger {
    pub fn slot_key(seed_name: &str, slot_name: &str) -> String {
        format!("{seed_name}\u{1f}{slot_name}")
    }

    pub fn slot_mut(&mut self, seed_name: &str, slot_name: &str) -> &mut SlotLedger {
        self.slots
            .entry(Self::slot_key(seed_name, slot_name))
            .or_default()
    }

    pub fn slot(&self, seed_name: &str, slot_name: &str) -> Option<&SlotLedger> {
        self.slots.get(&Self::slot_key(seed_name, slot_name))
    }
}

impl SlotLedger {
    pub fn next_index(&self) -> u64 {
        self.highest_processed_index.map_or(0, |index| index + 1)
    }

    pub fn delivered_quantity(&self, normalized_item_id: u32) -> u32 {
        self.acknowledged
            .values()
            .filter(|item| item.normalized_item_id == normalized_item_id)
            .map(|item| item.quantity)
            .fold(0, u32::saturating_add)
    }

    pub fn acknowledge(&mut self, index: u64, item: AcknowledgedItem) -> Result<()> {
        anyhow::ensure!(
            index == self.next_index(),
            "item acknowledgement is out of order"
        );
        self.acknowledged.insert(index, item);
        self.highest_processed_index = Some(index);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_ledger_starts_at_zero() {
        let path = std::env::temp_dir().join(format!(
            "bb-ledger-missing-{}-{}.json",
            std::process::id(),
            u64::MAX
        ));
        let _ = fs::remove_file(&path);
        assert_eq!(
            ReceiveLedger::load(&path).unwrap(),
            ReceiveLedger::default()
        );
    }

    #[test]
    fn ledgers_are_isolated_by_seed_and_slot() {
        let mut ledger = ReceiveLedger::default();
        ledger
            .slot_mut("seed-a", "hunter")
            .acknowledge(
                0,
                AcknowledgedItem {
                    ap_item_id: 1,
                    normalized_item_id: 0x4000_04CE,
                    quantity: 1,
                },
            )
            .unwrap();
        assert_eq!(ledger.slot("seed-a", "hunter").unwrap().next_index(), 1);
        assert!(ledger.slot("seed-a", "other").is_none());
        assert!(ledger.slot("seed-b", "hunter").is_none());
    }
}
