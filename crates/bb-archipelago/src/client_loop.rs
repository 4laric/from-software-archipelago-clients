use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::backend::{BloodborneBackend, EquipRequest, ItemGrant, OperationProgress};
use crate::config::RuntimeConfig;
use crate::feed::{EquipTarget, ReceivedFact, equip_decisions};
use crate::ledger::{AcknowledgedItem, PendingItem, ReceiveLedger};
use crate::upgrades::auto_upgrade_level;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IncomingItem {
    pub index: u64,
    pub ap_item_id: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletedItem {
    pub index: u64,
    pub ap_item_id: i64,
    pub received_level: Option<u8>,
    pub target_level: Option<u8>,
    pub delivered_level: Option<u8>,
    pub equip_target: Option<EquipTarget>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ItemPollResult {
    Idle,
    Pending,
    Completed(CompletedItem),
}

pub struct ClientLoop<B> {
    backend: B,
    config: RuntimeConfig,
    ledger: ReceiveLedger,
    ledger_path: PathBuf,
    seed_name: String,
    slot_name: String,
    location_identity: Option<String>,
    location_true_streaks: HashMap<i64, u8>,
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
            location_identity: None,
            location_true_streaks: HashMap::new(),
        }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub fn ledger(&self) -> &ReceiveLedger {
        &self.ledger
    }

    /// Validate and durably bind the game context shared by every read or
    /// mutation. `Ok(None)` is a normal non-gameplay transition; missing or
    /// mismatched identity is an actionable refusal.
    fn require_runtime_context(&mut self, operation: &str) -> Result<Option<String>> {
        let context = match self.backend.location_context() {
            Ok(Some(context)) => context,
            Ok(None) => {
                anyhow::bail!("{operation} is disarmed: no validated gameplay/save identity");
            }
            Err(error) => return Err(error),
        };
        if !context.gameplay_ready {
            return Ok(None);
        }
        let Some(expected) = self.config.expected_save_identity.as_deref() else {
            anyhow::bail!("{operation} is disarmed: expected_save_identity is not configured");
        };
        if context.save_identity != expected {
            anyhow::bail!(
                "{operation} refused save identity {:?}; expected {:?}",
                context.save_identity,
                expected
            );
        }
        let bound = self
            .ledger
            .slot(&self.seed_name, &self.slot_name)
            .and_then(|slot| slot.bound_save_identity.as_deref());
        if let Some(bound) = bound {
            anyhow::ensure!(
                bound == context.save_identity,
                "{operation} refused save identity {:?}; AP slot is durably bound to {:?}",
                context.save_identity,
                bound
            );
        } else {
            self.ledger
                .slot_mut(&self.seed_name, &self.slot_name)
                .bound_save_identity = Some(context.save_identity.clone());
            self.ledger.save(&self.ledger_path)?;
        }
        Ok(Some(context.save_identity))
    }

    pub fn poll_locations(&mut self, server_checked: &HashSet<i64>) -> Result<Vec<i64>> {
        let context_identity = match self.require_runtime_context("automatic location checks") {
            Ok(Some(identity)) => identity,
            Ok(None) => {
                self.location_true_streaks.clear();
                return Ok(Vec::new());
            }
            Err(error) => {
                self.location_true_streaks.clear();
                return Err(error);
            }
        };
        if self.config.location_check_debounce < 2 {
            self.location_true_streaks.clear();
            anyhow::bail!("location_check_debounce must be at least 2");
        }
        if self.location_identity.as_deref() != Some(&context_identity) {
            self.location_true_streaks.clear();
            self.location_identity = Some(context_identity);
        }

        let mut newly_checked = Vec::new();
        for binding in &self.config.locations {
            if server_checked.contains(&binding.ap_location_id) {
                self.location_true_streaks.remove(&binding.ap_location_id);
                continue;
            }
            let read = match self.backend.read_event_flag(binding.event_flag) {
                Ok(read) => read,
                Err(error) => {
                    self.location_true_streaks.clear();
                    return Err(error);
                }
            };
            match read {
                Some(true) => {
                    let streak = self
                        .location_true_streaks
                        .entry(binding.ap_location_id)
                        .or_default();
                    *streak = streak.saturating_add(1);
                    if *streak >= self.config.location_check_debounce {
                        newly_checked.push(binding.ap_location_id);
                    }
                }
                Some(false) | None => {
                    self.location_true_streaks.remove(&binding.ap_location_id);
                }
            }
        }
        Ok(newly_checked)
    }

    fn indexed_received(received: &[IncomingItem]) -> Result<BTreeMap<u64, i64>> {
        let mut indexed = BTreeMap::new();
        for item in received {
            anyhow::ensure!(
                indexed.insert(item.index, item.ap_item_id).is_none(),
                "received item index {} appears more than once",
                item.index
            );
        }
        Ok(indexed)
    }

    fn equip_target(
        &self,
        indexed: &BTreeMap<u64, i64>,
        current_index: u64,
    ) -> Result<Option<EquipTarget>> {
        if !self.config.auto_equip {
            return Ok(None);
        }
        let mut facts = Vec::new();
        for (&index, &ap_item_id) in indexed.range(..=current_index) {
            let binding =
                self.config.items.get(&ap_item_id).with_context(|| {
                    format!("AP item {ap_item_id} has no Bloodborne feed binding")
                })?;
            facts.push(ReceivedFact {
                index,
                effect: binding.feed_effect.effect(),
            });
        }
        let decision = equip_decisions(facts)?
            .into_iter()
            .find(|decision| decision.received_index == current_index);
        Ok(decision.map(|decision| decision.target))
    }

    fn plan_item(
        &mut self,
        item: IncomingItem,
        indexed: &BTreeMap<u64, i64>,
    ) -> Result<PendingItem> {
        let binding = self
            .config
            .items
            .get(&item.ap_item_id)
            .with_context(|| format!("AP item {} has no Bloodborne binding", item.ap_item_id))?
            .clone();
        let target_level = if self.config.auto_upgrade && binding.reinforcement_level.is_some() {
            self.backend.target_weapon_level()?
        } else {
            None
        };
        let delivered_level = binding
            .reinforcement_level
            .map(|received| auto_upgrade_level(self.config.auto_upgrade, received, target_level));
        Ok(PendingItem {
            index: item.index,
            ap_item_id: item.ap_item_id,
            normalized_item_id: binding.normalized_item_id,
            quantity: binding.quantity,
            upgrade_target_level: target_level,
            reinforcement_level: delivered_level,
            equip_target: self.equip_target(indexed, item.index)?,
            grant_complete: false,
            equip_complete: false,
        })
    }

    /// Processes at most one item, preserving AP index order and durable state
    /// across the grant -> optional upgrade -> optional equip sequence.
    pub fn poll_items(&mut self, received: &[IncomingItem]) -> Result<ItemPollResult> {
        let next = self
            .ledger
            .slot(&self.seed_name, &self.slot_name)
            .map_or(0, |slot| slot.next_index());
        let indexed = Self::indexed_received(received)?;
        let Some(&ap_item_id) = indexed.get(&next) else {
            return Ok(ItemPollResult::Idle);
        };
        let item = IncomingItem {
            index: next,
            ap_item_id,
        };
        if self
            .require_runtime_context("received-item delivery")?
            .is_none()
        {
            return Ok(ItemPollResult::Pending);
        }

        let mut pending = match self
            .ledger
            .slot(&self.seed_name, &self.slot_name)
            .and_then(|slot| slot.pending.clone())
        {
            Some(existing) => {
                anyhow::ensure!(
                    existing.index == item.index && existing.ap_item_id == item.ap_item_id,
                    "received item does not match the durable pending plan"
                );
                existing
            }
            None => {
                let planned = self.plan_item(item, &indexed)?;
                self.ledger
                    .slot_mut(&self.seed_name, &self.slot_name)
                    .begin(planned.clone())?;
                self.ledger.save(&self.ledger_path)?;
                planned
            }
        };

        if !pending.grant_complete {
            let grant = ItemGrant {
                normalized_item_id: pending.normalized_item_id,
                quantity: pending.quantity,
                expected_before: self.ledger.slot(&self.seed_name, &self.slot_name).map_or(
                    0,
                    |slot| {
                        slot.delivered_quantity(
                            pending.normalized_item_id,
                            pending.reinforcement_level,
                        )
                    },
                ),
                reinforcement_level: pending.reinforcement_level,
                tag: format!("ap_{}", item.index),
            };
            if self.backend.grant_item(&grant)? == OperationProgress::Pending {
                return Ok(ItemPollResult::Pending);
            }
            self.ledger
                .slot_mut(&self.seed_name, &self.slot_name)
                .mark_grant_complete()?;
            self.ledger.save(&self.ledger_path)?;
            pending.grant_complete = true;
        }

        if let Some(target) = pending.equip_target
            && !pending.equip_complete
        {
            let request = EquipRequest {
                normalized_item_id: pending.normalized_item_id,
                reinforcement_level: pending.reinforcement_level,
                target,
                tag: format!("ap_{}_equip", item.index),
            };
            if self.backend.equip_item(&request)? == OperationProgress::Pending {
                return Ok(ItemPollResult::Pending);
            }
            self.ledger
                .slot_mut(&self.seed_name, &self.slot_name)
                .mark_equip_complete()?;
            self.ledger.save(&self.ledger_path)?;
            pending.equip_complete = true;
        }

        self.ledger
            .slot_mut(&self.seed_name, &self.slot_name)
            .acknowledge(
                item.index,
                AcknowledgedItem {
                    ap_item_id: item.ap_item_id,
                    normalized_item_id: pending.normalized_item_id,
                    quantity: pending.quantity,
                    reinforcement_level: pending.reinforcement_level,
                    equip_target: pending.equip_target,
                },
            )?;
        self.ledger.save(&self.ledger_path)?;
        let received_level = self
            .config
            .items
            .get(&item.ap_item_id)
            .and_then(|binding| binding.reinforcement_level);
        Ok(ItemPollResult::Completed(CompletedItem {
            index: item.index,
            ap_item_id: item.ap_item_id,
            received_level,
            target_level: pending.upgrade_target_level,
            delivered_level: pending.reinforcement_level,
            equip_target: pending.equip_target,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{LocationContext, MockBackend};
    use crate::config::{FeedEffectBinding, GoodsBinding, LocationBinding, TEST_PEBBLE_EVENT_FLAG};
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

    fn goods() -> GoodsBinding {
        GoodsBinding {
            normalized_item_id: 0x4000_04CE,
            quantity: 1,
            reinforcement_level: None,
            feed_effect: FeedEffectBinding::NotEquippable,
        }
    }

    fn config() -> RuntimeConfig {
        RuntimeConfig {
            bridge_root: PathBuf::from("unused"),
            shad_log: None,
            locations: vec![LocationBinding {
                ap_location_id: 1000,
                event_flag: TEST_PEBBLE_EVENT_FLAG,
                vanilla_award_suppressed: false,
            }],
            items: HashMap::from([(2000, goods())]),
            auto_upgrade: false,
            auto_equip: false,
            expected_save_identity: Some("mock-save".into()),
            suppression_manifest: None,
            installed_gameparam: None,
            suppression: crate::config::SuppressionRequirement::default(),
            location_check_debounce: 3,
            mock_set_flags: vec![],
        }
    }

    fn loop_with(
        backend: MockBackend,
        ledger: ReceiveLedger,
        ledger_path: PathBuf,
        config: RuntimeConfig,
    ) -> ClientLoop<MockBackend> {
        ClientLoop::new(backend, config, ledger, ledger_path, "seed", "slot")
    }

    #[test]
    fn locations_require_bound_ready_context_and_debounce() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.set_flags.insert(TEST_PEBBLE_EVENT_FLAG);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        assert!(client.poll_locations(&HashSet::new()).unwrap().is_empty());
        assert!(client.poll_locations(&HashSet::new()).unwrap().is_empty());
        assert_eq!(client.poll_locations(&HashSet::new()).unwrap(), vec![1000]);

        client.backend_mut().location_context = Some(LocationContext {
            save_identity: "other-save".into(),
            gameplay_ready: true,
        });
        let error = client.poll_locations(&HashSet::new()).unwrap_err();
        assert!(format!("{error:#}").contains("refused save identity"));
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn live_style_missing_context_disarms_checks_before_flag_reads() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.location_context = None;
        backend.set_flags.insert(TEST_PEBBLE_EVENT_FLAG);
        let mut client = loop_with(backend, ReceiveLedger::default(), ledger_path, config());
        let error = client.poll_locations(&HashSet::new()).unwrap_err();
        assert!(format!("{error:#}").contains("disarmed"));
    }

    #[test]
    fn unavailable_context_breaks_the_consecutive_true_streak() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.set_flags.insert(TEST_PEBBLE_EVENT_FLAG);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        assert!(client.poll_locations(&HashSet::new()).unwrap().is_empty());
        client.backend_mut().location_context = None;
        assert!(client.poll_locations(&HashSet::new()).is_err());
        client.backend_mut().location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: true,
        });
        assert!(client.poll_locations(&HashSet::new()).unwrap().is_empty());
        assert!(client.poll_locations(&HashSet::new()).unwrap().is_empty());
        assert_eq!(client.poll_locations(&HashSet::new()).unwrap(), vec![1000]);
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn item_delivery_refuses_missing_or_mismatched_save_context_before_grant() {
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2000,
        }];
        for context in [
            None,
            Some(LocationContext {
                save_identity: "wrong-save".into(),
                gameplay_ready: true,
            }),
        ] {
            let ledger_path = path();
            let mut backend = MockBackend::default();
            backend.location_context = context;
            let mut client = loop_with(
                backend,
                ReceiveLedger::default(),
                ledger_path.clone(),
                config(),
            );
            assert!(client.poll_items(&received).is_err());
            assert!(client.backend().grants.is_empty());
            assert!(
                client
                    .ledger()
                    .slot("seed", "slot")
                    .is_none_or(|slot| slot.pending.is_none())
            );
            let _ = std::fs::remove_file(ledger_path);
        }
    }

    #[test]
    fn item_delivery_waits_without_mutation_outside_gameplay() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: false,
        });
        let mut client = loop_with(backend, ReceiveLedger::default(), ledger_path, config());
        assert_eq!(
            client
                .poll_items(&[IncomingItem {
                    index: 0,
                    ap_item_id: 2000,
                }])
                .unwrap(),
            ItemPollResult::Pending
        );
        assert!(client.backend().grants.is_empty());
        assert!(client.ledger().slot("seed", "slot").is_none());
    }

    #[test]
    fn durable_slot_binding_cannot_be_changed_by_config() {
        let ledger_path = path();
        let mut ledger = ReceiveLedger::default();
        ledger.slot_mut("seed", "slot").bound_save_identity = Some("first-save".into());
        let mut client = loop_with(MockBackend::default(), ledger, ledger_path, config());
        let error = client
            .poll_items(&[IncomingItem {
                index: 0,
                ap_item_id: 2000,
            }])
            .unwrap_err();
        assert!(format!("{error:#}").contains("durably bound"));
        assert!(client.backend().grants.is_empty());
    }

    #[test]
    fn item_delivery_binds_save_identity_before_planning_or_granting() {
        let ledger_path = path();
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2000,
        }];
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));
        assert_eq!(
            client
                .ledger()
                .slot("seed", "slot")
                .unwrap()
                .bound_save_identity
                .as_deref(),
            Some("mock-save")
        );
        let persisted = ReceiveLedger::load(&ledger_path).unwrap();
        assert_eq!(
            persisted
                .slot("seed", "slot")
                .unwrap()
                .bound_save_identity
                .as_deref(),
            Some("mock-save")
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn mock_loop_never_regrants_an_acknowledged_item() {
        let ledger_path = path();
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2000,
        }];
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));
        assert_eq!(client.poll_items(&received).unwrap(), ItemPollResult::Idle);
        assert_eq!(client.backend().grants.len(), 1);

        let persisted = ReceiveLedger::load(&ledger_path).unwrap();
        let mut reloaded = loop_with(
            MockBackend::default(),
            persisted,
            ledger_path.clone(),
            config(),
        );
        assert_eq!(
            reloaded.poll_items(&received).unwrap(),
            ItemPollResult::Idle
        );
        assert!(reloaded.backend().grants.is_empty());
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn grants_strictly_in_index_order() {
        let ledger_path = path();
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
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
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 0, .. })
        ));
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 1, .. })
        ));
        assert_eq!(client.backend().grants[0].expected_before, 0);
        assert_eq!(client.backend().grants[1].expected_before, 1);
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn auto_upgrade_and_auto_equip_share_the_delivered_weapon_identity() {
        let ledger_path = path();
        let mut runtime_config = config();
        runtime_config.auto_upgrade = true;
        runtime_config.auto_equip = true;
        runtime_config.items.insert(
            3000,
            GoodsBinding {
                normalized_item_id: 0x0012_3400,
                quantity: 1,
                reinforcement_level: Some(0),
                feed_effect: FeedEffectBinding::RightHandWeapon,
            },
        );
        let mut backend = MockBackend::default();
        backend.upgrade_target_level = Some(6);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            runtime_config,
        );
        let result = client
            .poll_items(&[IncomingItem {
                index: 0,
                ap_item_id: 3000,
            }])
            .unwrap();
        assert_eq!(
            result,
            ItemPollResult::Completed(CompletedItem {
                index: 0,
                ap_item_id: 3000,
                received_level: Some(0),
                target_level: Some(6),
                delivered_level: Some(6),
                equip_target: Some(EquipTarget::RightHand(0)),
            })
        );
        assert_eq!(client.backend().grants[0].reinforcement_level, Some(6));
        assert_eq!(client.backend().equips[0].reinforcement_level, Some(6));
        assert_eq!(client.backend().equips[0].target, EquipTarget::RightHand(0));
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn production_loop_applies_every_auto_upgrade_boundary() {
        for (enabled, received_level, target_level, expected_level) in [
            (false, 3, Some(8), 3),
            (true, 3, None, 3),
            (true, 8, Some(4), 8),
            (true, 0, Some(99), 10),
        ] {
            let ledger_path = path();
            let mut runtime_config = config();
            runtime_config.auto_upgrade = enabled;
            runtime_config.items.insert(
                3000,
                GoodsBinding {
                    normalized_item_id: 0x0012_3400,
                    quantity: 1,
                    reinforcement_level: Some(received_level),
                    feed_effect: FeedEffectBinding::RightHandWeapon,
                },
            );
            let mut backend = MockBackend::default();
            backend.upgrade_target_level = target_level;
            let mut client = loop_with(
                backend,
                ReceiveLedger::default(),
                ledger_path.clone(),
                runtime_config,
            );
            let result = client
                .poll_items(&[IncomingItem {
                    index: 0,
                    ap_item_id: 3000,
                }])
                .unwrap();
            let ItemPollResult::Completed(completed) = result else {
                panic!("item did not complete: {result:?}");
            };
            assert_eq!(completed.delivered_level, Some(expected_level));
            assert_eq!(
                client.backend().grants[0].reinforcement_level,
                Some(expected_level)
            );
            std::fs::remove_file(ledger_path).unwrap();
        }
    }

    #[test]
    fn pending_grant_keeps_its_original_upgrade_plan() {
        let ledger_path = path();
        let mut runtime_config = config();
        runtime_config.auto_upgrade = true;
        runtime_config.items.insert(
            3000,
            GoodsBinding {
                normalized_item_id: 0x0012_3400,
                quantity: 1,
                reinforcement_level: Some(0),
                feed_effect: FeedEffectBinding::RightHandWeapon,
            },
        );
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 3000,
        }];
        let mut backend = MockBackend::default();
        backend.upgrade_target_level = Some(4);
        backend.delay_grant("ap_0", 1);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            runtime_config,
        );
        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        client.backend_mut().upgrade_target_level = Some(9);
        let ItemPollResult::Completed(completed) = client.poll_items(&received).unwrap() else {
            panic!("pending grant did not complete");
        };
        assert_eq!(completed.target_level, Some(4));
        assert_eq!(completed.delivered_level, Some(4));
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn production_feed_drives_hand_attire_and_rune_targets() {
        let ledger_path = path();
        let mut runtime_config = config();
        runtime_config.auto_equip = true;
        let rows = [
            (3000, FeedEffectBinding::RightHandWeapon),
            (3001, FeedEffectBinding::RightHandWeapon),
            (3002, FeedEffectBinding::AttireChest),
            (3003, FeedEffectBinding::CaryllRune),
            (3004, FeedEffectBinding::RuneWorkshopTool),
            (3005, FeedEffectBinding::CaryllRune),
            (3006, FeedEffectBinding::OathRune),
        ];
        for (offset, (ap_item_id, feed_effect)) in rows.into_iter().enumerate() {
            runtime_config.items.insert(
                ap_item_id,
                GoodsBinding {
                    normalized_item_id: 0x4000_1000 + offset as u32,
                    quantity: 1,
                    reinforcement_level: None,
                    feed_effect,
                },
            );
        }
        let received = rows
            .into_iter()
            .enumerate()
            .map(|(index, (ap_item_id, _))| IncomingItem {
                index: index as u64,
                ap_item_id,
            })
            .collect::<Vec<_>>();
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            runtime_config,
        );
        for _ in &received {
            assert!(matches!(
                client.poll_items(&received).unwrap(),
                ItemPollResult::Completed(_)
            ));
        }
        assert_eq!(
            client
                .backend()
                .equips
                .iter()
                .map(|request| request.target)
                .collect::<Vec<_>>(),
            [
                EquipTarget::RightHand(0),
                EquipTarget::RightHand(1),
                EquipTarget::Attire(crate::feed::AttireSlot::Chest),
                EquipTarget::CaryllRune(1),
                EquipTarget::OathRune,
            ]
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn restart_after_grant_does_not_regrant_before_pending_equip() {
        let ledger_path = path();
        let mut runtime_config = config();
        runtime_config.auto_upgrade = true;
        runtime_config.auto_equip = true;
        runtime_config.items.insert(
            3000,
            GoodsBinding {
                normalized_item_id: 0x0012_3400,
                quantity: 1,
                reinforcement_level: Some(0),
                feed_effect: FeedEffectBinding::RightHandWeapon,
            },
        );
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 3000,
        }];
        let mut backend = MockBackend::default();
        backend.upgrade_target_level = Some(4);
        backend.delay_equip("ap_0_equip", 1);
        let mut first = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            runtime_config.clone(),
        );
        assert_eq!(
            first.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        assert_eq!(first.backend().grants.len(), 1);
        let inventory = first.backend().inventory.clone();

        let persisted = ReceiveLedger::load(&ledger_path).unwrap();
        assert!(
            persisted
                .slot("seed", "slot")
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .grant_complete
        );
        let mut restarted_backend = MockBackend::default();
        restarted_backend.inventory = inventory;
        restarted_backend.upgrade_target_level = Some(9);
        let mut restarted = loop_with(
            restarted_backend,
            persisted,
            ledger_path.clone(),
            runtime_config,
        );
        assert!(matches!(
            restarted.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));
        assert!(restarted.backend().grants.is_empty());
        assert_eq!(restarted.backend().equips.len(), 1);
        assert_eq!(restarted.backend().equips[0].reinforcement_level, Some(4));
        std::fs::remove_file(ledger_path).unwrap();
    }
}
