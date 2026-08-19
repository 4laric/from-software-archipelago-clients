use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::feed::EquipTarget;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReceiveLedger {
    pub slots: BTreeMap<String, SlotLedger>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SlotLedger {
    pub highest_processed_index: Option<u64>,
    pub acknowledged: BTreeMap<u64, AcknowledgedItem>,
    #[serde(default)]
    pub pending: Option<PendingItem>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AcknowledgedItem {
    pub ap_item_id: i64,
    pub normalized_item_id: u32,
    pub quantity: u32,
    #[serde(default)]
    pub reinforcement_level: Option<u8>,
    #[serde(default)]
    pub equip_target: Option<EquipTarget>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PendingItem {
    pub index: u64,
    pub ap_item_id: i64,
    pub normalized_item_id: u32,
    pub quantity: u32,
    #[serde(default)]
    pub upgrade_target_level: Option<u8>,
    pub reinforcement_level: Option<u8>,
    pub equip_target: Option<EquipTarget>,
    #[serde(default)]
    pub grant_complete: bool,
    #[serde(default)]
    pub equip_complete: bool,
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

    pub fn delivered_quantity(
        &self,
        normalized_item_id: u32,
        reinforcement_level: Option<u8>,
    ) -> u32 {
        self.acknowledged
            .values()
            .filter(|item| {
                item.normalized_item_id == normalized_item_id
                    && item.reinforcement_level == reinforcement_level
            })
            .map(|item| item.quantity)
            .fold(0, u32::saturating_add)
    }

    pub fn begin(&mut self, pending: PendingItem) -> Result<()> {
        anyhow::ensure!(
            pending.index == self.next_index(),
            "pending item is out of order"
        );
        if let Some(existing) = &self.pending {
            anyhow::ensure!(
                existing == &pending,
                "pending item plan changed before acknowledgement"
            );
            return Ok(());
        }
        self.pending = Some(pending);
        Ok(())
    }

    pub fn pending_for(&self, index: u64, ap_item_id: i64) -> Result<Option<&PendingItem>> {
        let Some(pending) = self.pending.as_ref() else {
            return Ok(None);
        };
        anyhow::ensure!(
            pending.index == index && pending.ap_item_id == ap_item_id,
            "received item does not match the durable pending plan"
        );
        Ok(Some(pending))
    }

    pub fn mark_grant_complete(&mut self) -> Result<()> {
        let pending = self
            .pending
            .as_mut()
            .context("no pending item to mark granted")?;
        pending.grant_complete = true;
        Ok(())
    }

    pub fn mark_equip_complete(&mut self) -> Result<()> {
        let pending = self
            .pending
            .as_mut()
            .context("no pending item to mark equipped")?;
        anyhow::ensure!(
            pending.grant_complete,
            "cannot equip before grant completion"
        );
        pending.equip_complete = true;
        Ok(())
    }

    pub fn acknowledge(&mut self, index: u64, item: AcknowledgedItem) -> Result<()> {
        anyhow::ensure!(
            index == self.next_index(),
            "item acknowledgement is out of order"
        );
        let pending = self
            .pending
            .as_ref()
            .context("cannot acknowledge an item without a durable pending plan")?;
        anyhow::ensure!(pending.index == index, "pending item index changed");
        anyhow::ensure!(
            pending.grant_complete,
            "cannot acknowledge before grant completion"
        );
        anyhow::ensure!(
            pending.equip_target.is_none() || pending.equip_complete,
            "cannot acknowledge before equip completion"
        );
        self.acknowledged.insert(index, item);
        self.highest_processed_index = Some(index);
        self.pending = None;
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
                    reinforcement_level: None,
                    equip_target: None,
                },
            )
            .unwrap_err();
        ledger
            .slot_mut("seed-a", "hunter")
            .begin(PendingItem {
                index: 0,
                ap_item_id: 1,
                normalized_item_id: 0x4000_04CE,
                quantity: 1,
                reinforcement_level: None,
                equip_target: None,
                upgrade_target_level: None,
                grant_complete: false,
                equip_complete: false,
            })
            .unwrap();
        ledger
            .slot_mut("seed-a", "hunter")
            .mark_grant_complete()
            .unwrap();
        ledger
            .slot_mut("seed-a", "hunter")
            .acknowledge(
                0,
                AcknowledgedItem {
                    ap_item_id: 1,
                    normalized_item_id: 0x4000_04CE,
                    quantity: 1,
                    reinforcement_level: None,
                    equip_target: None,
                },
            )
            .unwrap();
        assert_eq!(ledger.slot("seed-a", "hunter").unwrap().next_index(), 1);
        assert!(ledger.slot("seed-a", "other").is_none());
        assert!(ledger.slot("seed-b", "hunter").is_none());
    }

    #[test]
    fn pending_plan_survives_serialization_and_cannot_drift() {
        let mut ledger = ReceiveLedger::default();
        let slot = ledger.slot_mut("seed", "slot");
        let pending = PendingItem {
            index: 0,
            ap_item_id: 9,
            normalized_item_id: 0x1000,
            quantity: 1,
            reinforcement_level: Some(6),
            equip_target: Some(EquipTarget::RightHand(0)),
            upgrade_target_level: Some(6),
            grant_complete: false,
            equip_complete: false,
        };
        slot.begin(pending.clone()).unwrap();
        let bytes = json::to_vec(&ledger).unwrap();
        let decoded: ReceiveLedger = json::from_slice(&bytes).unwrap();
        assert_eq!(
            decoded
                .slot("seed", "slot")
                .unwrap()
                .pending_for(0, 9)
                .unwrap(),
            Some(&pending)
        );
    }
}
