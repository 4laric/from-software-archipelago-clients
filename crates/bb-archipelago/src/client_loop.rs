use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::backend::{BloodborneBackend, GoodsGrant, GrantProgress};
use crate::config::RuntimeConfig;
use crate::ledger::{AcknowledgedItem, ReceiveLedger};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IncomingItem {
    pub index: u64,
    pub ap_item_id: i64,
}

pub struct ClientLoop<B> {
    backend: B,
    config: RuntimeConfig,
    ledger: ReceiveLedger,
    ledger_path: PathBuf,
    seed_name: String,
    slot_name: String,
}

impl<B: BloodborneBackend> ClientLoop<B> {
    pub fn new(
        backend: B,
        config: RuntimeConfig,
        ledger: ReceiveLedger,
        ledger_path: PathBuf,
        seed_name: impl Into<String>,
        slot_name: impl Into<String>,
    ) -> Self {
        Self {
            backend,
            config,
            ledger,
            ledger_path,
            seed_name: seed_name.into(),
            slot_name: slot_name.into(),
        }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub fn poll_locations(&mut self, server_checked: &HashSet<i64>) -> Result<Vec<i64>> {
        let mut newly_checked = Vec::new();
        for binding in &self.config.locations {
            if server_checked.contains(&binding.ap_location_id) {
                continue;
            }
            if self.backend.read_event_flag(binding.event_flag)? == Some(true) {
                newly_checked.push(binding.ap_location_id);
            }
        }
        Ok(newly_checked)
    }

    /// Processes at most one item, preserving AP index order and durable acknowledgement.
    pub fn poll_items(&mut self, received: &[IncomingItem]) -> Result<bool> {
        let next = self
            .ledger
            .slot(&self.seed_name, &self.slot_name)
            .map_or(0, |slot| slot.next_index());
        let Some(item) = received.iter().find(|item| item.index == next) else {
            return Ok(false);
        };
        let binding =
            self.config.items.get(&item.ap_item_id).with_context(|| {
                format!("AP item {} has no Bloodborne binding", item.ap_item_id)
            })?;
        let expected_before = self
            .ledger
            .slot(&self.seed_name, &self.slot_name)
            .map_or(0, |slot| {
                slot.delivered_quantity(binding.normalized_item_id)
            });
        let grant = GoodsGrant {
            normalized_item_id: binding.normalized_item_id,
            quantity: binding.quantity,
            expected_before,
            tag: format!("ap_{}", item.index),
        };
        if self.backend.grant_category4_goods(&grant)? == GrantProgress::Pending {
            return Ok(false);
        }
        self.ledger
            .slot_mut(&self.seed_name, &self.slot_name)
            .acknowledge(
                item.index,
                AcknowledgedItem {
                    ap_item_id: item.ap_item_id,
                    normalized_item_id: binding.normalized_item_id,
                    quantity: binding.quantity,
                },
            )?;
        self.ledger.save(&self.ledger_path)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MockBackend;
    use crate::config::{GoodsBinding, LocationBinding, TEST_PEBBLE_EVENT_FLAG};
    use std::collections::HashMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "bb-loop-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn loop_with(backend: MockBackend, ledger_path: PathBuf) -> ClientLoop<MockBackend> {
        ClientLoop::new(
            backend,
            RuntimeConfig {
                bridge_root: PathBuf::from("unused"),
                shad_log: None,
                locations: vec![LocationBinding {
                    ap_location_id: 1000,
                    event_flag: TEST_PEBBLE_EVENT_FLAG,
                    vanilla_award_suppressed: false,
                }],
                items: HashMap::from([(
                    2000,
                    GoodsBinding {
                        normalized_item_id: 0x4000_04CE,
                        quantity: 1,
                    },
                )]),
                mock_set_flags: vec![],
            },
            ReceiveLedger::default(),
            ledger_path,
            "seed",
            "slot",
        )
    }

    #[test]
    fn mock_loop_checks_location_and_never_regrants_acknowledged_item() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.set_flags.insert(TEST_PEBBLE_EVENT_FLAG);
        let mut client = loop_with(backend, ledger_path.clone());
        assert_eq!(client.poll_locations(&HashSet::new()).unwrap(), vec![1000]);

        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2000,
        }];
        assert!(client.poll_items(&received).unwrap());
        assert!(!client.poll_items(&received).unwrap());
        assert_eq!(client.backend().grants.len(), 1);

        let persisted = ReceiveLedger::load(&ledger_path).unwrap();
        let mut reloaded = ClientLoop::new(
            MockBackend::default(),
            client.config.clone(),
            persisted,
            ledger_path.clone(),
            "seed",
            "slot",
        );
        assert!(!reloaded.poll_items(&received).unwrap());
        assert!(reloaded.backend().grants.is_empty());
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn grants_strictly_in_index_order() {
        let ledger_path = path();
        let mut client = loop_with(MockBackend::default(), ledger_path.clone());
        let received = [
            IncomingItem {
                index: 1,
                ap_item_id: 2000,
            },
            IncomingItem {
                index: 0,
                ap_item_id: 2000,
            },
        ];
        assert!(client.poll_items(&received).unwrap());
        assert!(client.poll_items(&received).unwrap());
        assert_eq!(client.backend().grants[0].expected_before, 0);
        assert_eq!(client.backend().grants[1].expected_before, 1);
        std::fs::remove_file(ledger_path).unwrap();
    }
}
