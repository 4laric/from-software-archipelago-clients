use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::backend::{
    BloodborneBackend, EquipRequest, GrantTerminalFailure, ItemGrant, OperationProgress,
    StackObservation,
};
use crate::client_eprintln;
use crate::config::RuntimeConfig;
use crate::feed::{EquipTarget, ReceivedFact, equip_decisions};
use crate::ledger::{AcknowledgedItem, PendingItem, ReceiveLedger, WatermarkOutcome};
use crate::upgrades::{auto_upgrade_level, reinforced_descriptor_pair};

const LOCATION_RETRY_INITIAL: Duration = Duration::from_secs(1);
const LOCATION_RETRY_MAX: Duration = Duration::from_secs(30);
const QUICKSILVER_BULLET_GOODS_ID: u32 = 1_100;
const QUICKSILVER_BULLET_RAW_DESCRIPTOR: u32 = 0xB000_044C;
const GOODS_NORMALIZED_PREFIX: u32 = 0x4000_0000;

#[derive(Clone, Copy, Debug)]
struct LocationRetry {
    next_attempt: Instant,
    delay: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IncomingItem {
    pub index: u64,
    pub ap_item_id: i64,
}

/// The bridge tag of the grant command for an AP receive index. One helper so
/// the publisher, the withdrawal path, and the startup reconciler can never
/// disagree about which command a pending plan owns.
fn grant_tag(index: u64) -> String {
    format!("ap_{index}")
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
pub struct BlockedItem {
    pub index: u64,
    pub ap_item_id: i64,
    pub status: String,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ItemPollResult {
    Idle,
    Pending,
    /// A recorded save watermark is unreadable: no grants and no checks until
    /// it recovers (docs/SAVE-RECONCILIATION.md §5, invariant I3).
    Held,
    /// The save and ledger disagreed and were reconciled this poll; the
    /// outcome is reported and delivery resumes next poll.
    Reconciled(WatermarkOutcome),
    /// The grant for this index terminally failed in the harness. The item is
    /// acknowledged as blocked (never retried by the loop, never lost from the
    /// ledger) and the stream moves on to the next index; recovery is the
    /// operator-driven `bb-blocked` tool (clients#399).
    Blocked(BlockedItem),
    Completed(CompletedItem),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SustainPollResult {
    Idle,
    Pending,
    Completed(i64),
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
    location_retries: HashMap<i64, LocationRetry>,
    last_watermark_outcome: WatermarkOutcome,
    watermark_notice: Option<WatermarkOutcome>,
}

impl<B: BloodborneBackend> ClientLoop<B> {
    pub fn record_location_checks(&mut self, locations: &[i64]) {
        self.backend.record_location_checks(locations);
    }
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
            location_retries: HashMap::new(),
            last_watermark_outcome: WatermarkOutcome::Resume,
            watermark_notice: None,
        }
    }

    /// Outcome transitions of the save-watermark comparison, for the operator
    /// surface (docs/SAVE-RECONCILIATION.md §8). Each transition is reported
    /// once; `main` drains this every loop.
    pub fn take_watermark_notice(&mut self) -> Option<WatermarkOutcome> {
        self.watermark_notice.take()
    }

    /// Compare the save-resident watermark with the durable ledger cursor and
    /// apply the one defined outcome (bb-archipelago#77). Idempotent: the
    /// location and item polls both call it each loop, and the second call
    /// observes the reconciled state. Only valid under a validated context --
    /// identity refusal must precede any watermark comparison (§5).
    fn reconcile_watermark(&mut self) -> Result<WatermarkOutcome> {
        let active = self
            .ledger
            .slot(&self.seed_name, &self.slot_name)
            .is_some_and(|slot| slot.save_watermark.is_some());
        let observed = self.backend.read_save_watermark()?;
        if !active && observed.is_none() {
            // Attested mode: no watermark has ever been written or read for
            // this slot. Zero behavior change.
            return Ok(WatermarkOutcome::Resume);
        }
        let outcome = self
            .ledger
            .slot_mut(&self.seed_name, &self.slot_name)
            .reconcile_save_watermark(observed);
        if outcome != WatermarkOutcome::Resume {
            self.ledger.save(&self.ledger_path)?;
        }
        if outcome != self.last_watermark_outcome {
            self.last_watermark_outcome = outcome;
            self.watermark_notice = Some(outcome);
        }
        Ok(outcome)
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub fn death_link_enabled(&self) -> bool {
        self.config.death_link
    }

    /// Attempt one queued incoming DeathLink. `false` keeps it queued while
    /// the player is loading or before the HP capture hook has fired.
    pub fn receive_death_link(&mut self) -> Result<bool> {
        if !self.config.death_link {
            return Ok(false);
        }
        self.backend.death_link_kill()
    }

    /// The AP seed this runtime (and every ledger row it touches) is bound to.
    /// clients#423 compares it against a reconnect's slot data: landing on a
    /// regenerated seed must refuse, not continue on stale bindings.
    pub fn seed_name(&self) -> &str {
        &self.seed_name
    }

    pub fn ledger(&self) -> &ReceiveLedger {
        &self.ledger
    }

    /// Human-readable, non-mutating rescue-console status. This intentionally
    /// exposes stable contract/ledger facts rather than raw process addresses.
    pub fn rescue_status(&mut self) -> Result<String> {
        let context = self.backend.location_context()?;
        let slot = self.ledger.slot(&self.seed_name, &self.slot_name);
        let blocked = slot.map_or(0, |slot| slot.blocked_entries().count());
        let cursor = slot.and_then(|slot| slot.highest_processed_index);
        let context = match context {
            Some(context) => format!(
                "save={:?} gameplay_ready={}",
                context.save_identity, context.gameplay_ready
            ),
            None => "save=<unvalidated> gameplay_ready=false".to_string(),
        };
        Ok(format!(
            "seed={:?} slot={:?} {context} locations={} items={} receive_cursor={cursor:?} blocked={blocked}",
            self.seed_name,
            self.slot_name,
            self.config.locations.len(),
            self.config.items.len(),
        ))
    }

    pub fn rescue_read_flag(&mut self, event_flag: u32) -> Result<String> {
        let _ = self.require_runtime_context("rescue flag read")?;
        let mapped = self
            .config
            .locations
            .iter()
            .any(|binding| binding.event_flag == event_flag);
        anyhow::ensure!(
            mapped,
            "event flag {event_flag} is not in this seed contract"
        );
        let value = self
            .backend
            .read_event_flag(event_flag)?
            .context("live event-flag accessor is unavailable")?;
        Ok(format!("event flag {event_flag} = {value}"))
    }

    pub fn rescue_list_blocked(&self) -> String {
        let Some(slot) = self.ledger.slot(&self.seed_name, &self.slot_name) else {
            return "No receive ledger exists for this seed/slot yet.".to_string();
        };
        let rows = slot
            .blocked_entries()
            .map(|(index, item)| {
                format!(
                    "index={index} ap_item={} reason={}",
                    item.ap_item_id,
                    item.blocked.as_deref().unwrap_or("unknown")
                )
            })
            .collect::<Vec<_>>();
        if rows.is_empty() {
            "No parked deliveries.".to_string()
        } else {
            rows.join("\n")
        }
    }

    pub fn rescue_retry_blocked(&mut self, index: u64) -> Result<()> {
        let _ = self.require_runtime_context("rescue delivery retry")?;
        self.ledger
            .slot_mut(&self.seed_name, &self.slot_name)
            .requeue_blocked(index)?;
        self.ledger.save(&self.ledger_path)
    }

    pub fn rescue_export(&self) -> Result<PathBuf> {
        let output = self.ledger_path.with_file_name("rescue-diagnostics.json");
        let slot = self.ledger.slot(&self.seed_name, &self.slot_name);
        let document = json::json!({
            "format": "bb-rescue-diagnostics-v1",
            "runtime_build": crate::RUNTIME_BUILD,
            "seed": self.seed_name,
            "slot": self.slot_name,
            "location_count": self.config.locations.len(),
            "item_count": self.config.items.len(),
            "ledger": slot,
        });
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&output, json::to_vec_pretty(&document)?)?;
        Ok(output)
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

    pub fn reset_location_retry_backoff(&mut self) {
        self.location_retries.clear();
    }

    /// Queue the anti-farm bullet for newly sent randomized fixed checks.
    /// Historical server checks never enter this method: callers pass only
    /// transitions returned by `poll_locations`. It is persisted before the
    /// network send, closing the crash window between sending a check and
    /// recording its bonus; repeated flag polling is harmless because the
    /// location id is the durable idempotency key.
    pub fn queue_sustain_for_checks(&mut self, locations: &[i64]) -> Result<Vec<i64>> {
        let eligible = locations
            .iter()
            .copied()
            .filter(|location| {
                self.config.locations.iter().any(|binding| {
                    binding.ap_location_id == *location && binding.vanilla_award_suppressed
                })
            })
            .collect::<Vec<_>>();
        let slot = self.ledger.slot_mut(&self.seed_name, &self.slot_name);
        let mut queued = Vec::new();
        for location in eligible {
            if !slot.completed_sustain.contains(&location)
                && !slot.pending_sustain.contains_key(&location)
            {
                slot.pending_sustain.insert(location, None);
                queued.push(location);
            }
        }
        if !queued.is_empty() {
            self.ledger.save(&self.ledger_path)?;
        }
        Ok(queued)
    }

    /// Advance at most one replay-safe Quicksilver Bullet bonus. Received AP
    /// items retain priority: the binary calls this only when their delivery
    /// machine is idle, and errors here are reported independently.
    pub fn poll_sustain(&mut self) -> Result<SustainPollResult> {
        let Some((&location, &recorded_before)) = self
            .ledger
            .slot(&self.seed_name, &self.slot_name)
            .and_then(|slot| slot.pending_sustain.iter().next())
        else {
            return Ok(SustainPollResult::Idle);
        };
        let tag = format!("sustain_{location}");
        match self.require_runtime_context("pickup sustain delivery") {
            Ok(Some(_)) => {}
            Ok(None) => {
                if self.backend.withdraw_unwitnessed_grant(&tag)? {
                    self.ledger
                        .slot_mut(&self.seed_name, &self.slot_name)
                        .pending_sustain
                        .insert(location, None);
                    self.ledger.save(&self.ledger_path)?;
                }
                return Ok(SustainPollResult::Pending);
            }
            Err(error) => return Err(error),
        }
        if self.reconcile_watermark()? == WatermarkOutcome::Hold {
            return Ok(SustainPollResult::Pending);
        }

        let normalized = GOODS_NORMALIZED_PREFIX | QUICKSILVER_BULLET_GOODS_ID;
        let baseline_is_binding = match recorded_before {
            Some(_) => self.backend.grant_may_have_applied(&tag)?,
            None => false,
        };
        let expected_before = match recorded_before.filter(|_| baseline_is_binding) {
            Some(value) => value,
            None => match self.backend.observe_stack_quantity(normalized, None)? {
                StackObservation::Quantity(value) => {
                    self.ledger
                        .slot_mut(&self.seed_name, &self.slot_name)
                        .pending_sustain
                        .insert(location, Some(value));
                    self.ledger.save(&self.ledger_path)?;
                    value
                }
                StackObservation::NotReady => return Ok(SustainPollResult::Pending),
                StackObservation::Unsupported => 0,
            },
        };
        let grant = ItemGrant {
            raw_descriptor: QUICKSILVER_BULLET_RAW_DESCRIPTOR,
            normalized_item_id: normalized,
            item_category: 4,
            quantity: 1,
            expected_before,
            reinforcement_level: None,
            tag,
        };
        if self.backend.grant_item(&grant)? == OperationProgress::Pending {
            return Ok(SustainPollResult::Pending);
        }
        let slot = self.ledger.slot_mut(&self.seed_name, &self.slot_name);
        slot.pending_sustain.remove(&location);
        slot.completed_sustain.insert(location);
        self.ledger.save(&self.ledger_path)?;
        Ok(SustainPollResult::Completed(location))
    }

    pub fn poll_locations(&mut self, server_checked: &HashSet<i64>) -> Result<Vec<i64>> {
        self.poll_locations_at(server_checked, Instant::now())
    }

    fn poll_locations_at(
        &mut self,
        server_checked: &HashSet<i64>,
        now: Instant,
    ) -> Result<Vec<i64>> {
        let context_identity = match self.require_runtime_context("automatic location checks") {
            Ok(Some(identity)) => identity,
            Ok(None) => {
                self.location_true_streaks.clear();
                self.location_retries.clear();
                return Ok(Vec::new());
            }
            Err(error) => {
                self.location_true_streaks.clear();
                self.location_retries.clear();
                return Err(error);
            }
        };
        if self.config.location_check_debounce < 2 {
            self.location_true_streaks.clear();
            self.location_retries.clear();
            anyhow::bail!("location_check_debounce must be at least 2");
        }
        if self.location_identity.as_deref() != Some(&context_identity) {
            self.location_true_streaks.clear();
            self.location_retries.clear();
            self.location_identity = Some(context_identity);
        }
        // A held watermark silences checks as well as grants (§5): a save
        // whose delivery state cannot be verified is not read for checks
        // either. Reissue/adopt outcomes leave flag polling alone -- a
        // restored save simply reads false until the flags are earned again.
        if self.reconcile_watermark()? == WatermarkOutcome::Hold {
            self.location_true_streaks.clear();
            self.location_retries.clear();
            return Ok(Vec::new());
        }

        let mut newly_checked = Vec::new();
        for binding in &self.config.locations {
            if server_checked.contains(&binding.ap_location_id) {
                self.location_true_streaks.remove(&binding.ap_location_id);
                self.location_retries.remove(&binding.ap_location_id);
                continue;
            }
            let read = match self.backend.read_event_flag(binding.event_flag) {
                Ok(read) => read,
                Err(error) => {
                    self.location_true_streaks.clear();
                    self.location_retries.clear();
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
                        match self.location_retries.get_mut(&binding.ap_location_id) {
                            None => {
                                newly_checked.push(binding.ap_location_id);
                                self.location_retries.insert(
                                    binding.ap_location_id,
                                    LocationRetry {
                                        next_attempt: now + LOCATION_RETRY_INITIAL,
                                        delay: LOCATION_RETRY_INITIAL,
                                    },
                                );
                            }
                            Some(retry) if now >= retry.next_attempt => {
                                newly_checked.push(binding.ap_location_id);
                                retry.delay = retry.delay.saturating_mul(2).min(LOCATION_RETRY_MAX);
                                retry.next_attempt = now + retry.delay;
                            }
                            Some(_) => {}
                        }
                    }
                }
                Some(false) | None => {
                    self.location_true_streaks.remove(&binding.ap_location_id);
                    self.location_retries.remove(&binding.ap_location_id);
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
        let (raw_descriptor, normalized_item_id) =
            match (binding.reinforcement_level, delivered_level) {
                (Some(received), Some(delivered)) => reinforced_descriptor_pair(
                    binding.raw_descriptor,
                    binding.normalized_item_id,
                    received,
                    delivered,
                )
                .context("weapon reinforcement descriptor overflow or downgrade")?,
                _ => (binding.raw_descriptor, binding.normalized_item_id),
            };
        Ok(PendingItem {
            index: item.index,
            ap_item_id: item.ap_item_id,
            raw_descriptor,
            normalized_item_id,
            item_category: binding.item_category,
            quantity: binding.quantity,
            upgrade_target_level: target_level,
            reinforcement_level: delivered_level,
            equip_target: self.equip_target(indexed, item.index)?,
            grant_complete: false,
            equip_complete: false,
            observed_before: None,
        })
    }

    /// Startup reconciliation (clients#296): withdraw a grant command left over
    /// from a previous process BEFORE any context is validated. A leftover would
    /// otherwise execute against whatever save is loaded when the harness picks
    /// it up -- including a different character ("a full shad/CE/client restart
    /// cannot redirect a retained command to another character"). The durable
    /// pending plan is untouched: the next poll under a validated context
    /// re-publishes the command. Returns `true` when a command was withdrawn.
    pub fn reconcile_pending_command(&mut self) -> Result<bool> {
        let item_tag = self
            .ledger
            .slot(&self.seed_name, &self.slot_name)
            .and_then(|slot| slot.pending.as_ref())
            .filter(|pending| !pending.grant_complete)
            .map(|pending| grant_tag(pending.index));
        let sustain_tag = self
            .ledger
            .slot(&self.seed_name, &self.slot_name)
            .and_then(|slot| slot.pending_sustain.keys().next())
            .map(|location| format!("sustain_{location}"));
        let Some(tag) = item_tag.or(sustain_tag) else {
            return Ok(false);
        };
        self.backend.withdraw_unwitnessed_grant(&tag)
    }

    /// Drop the recorded observed baseline of the in-flight grant and persist
    /// that (clients#427). Only called when the backend proved the command was
    /// withdrawn unexecuted.
    fn forget_observed_baseline(&mut self) -> Result<()> {
        if self
            .ledger
            .slot_mut(&self.seed_name, &self.slot_name)
            .clear_observed_before()
        {
            self.ledger.save(&self.ledger_path)?;
        }
        Ok(())
    }

    /// Startup unpark (clients#427, clients#433): every entry whose park
    /// cause is known to be fixed re-enters the delivery queue --
    /// `quantity_mismatch` (the ledger's lifetime delivered sum no longer
    /// stands in for current inventory) and `write_error` detailed
    /// `quantity write failed` (the external guest-heap write shadPS4 refuses
    /// is gone). They now deliver, or fail for a real reason. Every other park
    /// stays parked for `bb-blocked`. Returns the requeued indices.
    pub fn requeue_fixed_cause_parks(&mut self) -> Result<Vec<u64>> {
        let indices = self
            .ledger
            .slot_mut(&self.seed_name, &self.slot_name)
            .requeue_fixed_cause_parks();
        if !indices.is_empty() {
            self.ledger.save(&self.ledger_path)?;
        }
        Ok(indices)
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
            // Nothing to deliver -- but a restore can still have happened
            // (every item delivered, then the save rolled back). Compare the
            // watermark whenever one is active. This path never arms a
            // binding and never refuses: a missing or mismatched context keeps
            // idle delivery exactly as silent as before.
            let watermark_active = self
                .ledger
                .slot(&self.seed_name, &self.slot_name)
                .is_some_and(|slot| slot.save_watermark.is_some());
            if watermark_active
                && matches!(
                    self.require_runtime_context("save-watermark reconciliation"),
                    Ok(Some(_))
                )
            {
                return Ok(match self.reconcile_watermark()? {
                    WatermarkOutcome::Resume => ItemPollResult::Idle,
                    WatermarkOutcome::Hold => ItemPollResult::Held,
                    outcome => ItemPollResult::Reconciled(outcome),
                });
            }
            return Ok(ItemPollResult::Idle);
        };
        let item = IncomingItem {
            index: next,
            ap_item_id,
        };
        // The in-flight grant's tag is read BEFORE the context check on
        // purpose: context loss is exactly when a published-but-unexecuted
        // command must be withdrawn (clients#296), and that duty does not
        // wait for a valid context. The durable plan stays in the ledger, so
        // a withdrawal is a held operation, never a lost item.
        let in_flight_tag = self
            .ledger
            .slot(&self.seed_name, &self.slot_name)
            .and_then(|slot| slot.pending.as_ref())
            .filter(|pending| !pending.grant_complete)
            .map(|pending| grant_tag(pending.index));
        match self.require_runtime_context("received-item delivery") {
            Ok(Some(_)) => {}
            Ok(None) => {
                // Not gameplay-ready (load transition, menu, character
                // screen): a command published earlier must not execute
                // against a game state we can no longer vouch for.
                if let Some(tag) = in_flight_tag
                    && self.backend.withdraw_unwitnessed_grant(&tag)?
                {
                    // The command is proven unexecuted, so the baseline it was
                    // sampled against is no longer binding: forget it, and the
                    // next publication observes the stack again (clients#427).
                    // A witnessed command keeps its baseline -- that is the
                    // replay-recovery number.
                    self.forget_observed_baseline()?;
                }
                return Ok(ItemPollResult::Pending);
            }
            Err(error) => {
                // Identity refused (a different save is loaded) or disarmed:
                // same withdrawal duty, and the operator sees that it happened.
                if let Some(tag) = in_flight_tag {
                    return Err(match self.backend.withdraw_unwitnessed_grant(&tag) {
                        Ok(true) => error.context("withdrew the unwitnessed pending grant command"),
                        Ok(false) => error,
                        Err(withdraw_error) => withdraw_error.context(format!(
                            "withdrawing the pending grant command after: {error:#}"
                        )),
                    });
                }
                return Err(error);
            }
        }

        // Context validated (identity refusal never reaches here): compare the
        // save watermark before any grant work. Hold abstains; reissue/adopt
        // are reported once and replay starts next poll.
        match self.reconcile_watermark()? {
            WatermarkOutcome::Resume => {}
            WatermarkOutcome::Hold => return Ok(ItemPollResult::Held),
            outcome => return Ok(ItemPollResult::Reconciled(outcome)),
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

        // Fail closed on a provenance this build has never heard of: refuse
        // THIS item, by name, and keep the session alive. The seed was
        // generated by a newer world; every other binding still delivers.
        if let Some(binding) = self.config.items.get(&item.ap_item_id)
            && !binding.descriptor_evidence.is_known()
        {
            let failure = GrantTerminalFailure {
                tag: grant_tag(item.index),
                status: "unknown_descriptor_evidence".to_string(),
                detail: format!(
                    "AP item {} carries descriptor evidence {:?}, which this client does not understand; this seed was generated by a newer world -- update the client to deliver it",
                    item.ap_item_id,
                    binding.descriptor_evidence.as_str()
                ),
            };
            return self.park_terminal_grant(item, &pending, failure);
        }

        if !pending.grant_complete {
            // clients#427: the delivery precondition.
            //
            //  * REPLAY -- this pending plan already carries the baseline its
            //    in-flight command was submitted against, so a restart compares
            //    against the same number and an already-applied stack is
            //    recognised (`recovered_complete`) instead of granted twice.
            //  * FRESH -- no baseline yet: observe the live stack, record it
            //    durably BEFORE the grant can execute, and use that. The
            //    ledger's lifetime delivered sum is NOT the current inventory
            //    of anything the player can spend, which is why every grant of
            //    a consumable parked once one was used.
            //  * RETAINED -- a baseline exists, but the backend reports the
            //    command it belongs to cannot have applied yet (it is still
            //    waiting on inventory, or on the player). It is re-observed
            //    below: a number sampled before a wait of unbounded length is
            //    not a precondition, it is the stale number that re-parked
            //    oz's requeued backlog.
            //
            // Double-delivery protection is unchanged and still lives in the
            // ledger's index cursor: an index is delivered at most once.
            // clients#451: for category-0 equipment this baseline is recorded
            // but is NOT used as an instance count -- the native machine takes
            // the insert lane for equipment unconditionally and ignores the
            // stack quantity, because `observe_stack_quantity` reports the
            // first matching RECORD, which says nothing about how many
            // instances the player holds. It stays observed and durable so the
            // ledger row keeps one shape for both categories.
            let recorded_baseline = pending.observed_before;
            let baseline_is_binding = match recorded_baseline {
                Some(_) => self
                    .backend
                    .grant_may_have_applied(&grant_tag(item.index))?,
                None => false,
            };
            let expected_before = if pending.item_category == 255 {
                0
            } else {
                match recorded_baseline.filter(|_| baseline_is_binding) {
                    Some(recorded) => recorded,
                    None => match self.backend.observe_stack_quantity(
                        pending.normalized_item_id,
                        pending.reinforcement_level,
                    )? {
                        StackObservation::Quantity(observed) => {
                            self.ledger
                                .slot_mut(&self.seed_name, &self.slot_name)
                                .record_observed_before(observed)?;
                            self.ledger.save(&self.ledger_path)?;
                            pending.observed_before = Some(observed);
                            observed
                        }
                        // No trustworthy reading yet: nothing is published.
                        StackObservation::NotReady => return Ok(ItemPollResult::Pending),
                        // A backend that cannot read inventory (the CE file bridge,
                        // whose harness ignores the field anyway) keeps the
                        // pre-clients#427 ledger-derived baseline.
                        StackObservation::Unsupported => self
                            .ledger
                            .slot(&self.seed_name, &self.slot_name)
                            .map_or(0, |slot| {
                                slot.delivered_quantity(
                                    pending.normalized_item_id,
                                    pending.reinforcement_level,
                                )
                            }),
                    },
                }
            };
            let grant = ItemGrant {
                raw_descriptor: pending.raw_descriptor,
                normalized_item_id: pending.normalized_item_id,
                item_category: pending.item_category,
                quantity: pending.quantity,
                expected_before,
                reinforcement_level: pending.reinforcement_level,
                tag: grant_tag(item.index),
            };
            let progress = match self.backend.grant_item(&grant) {
                Ok(progress) => progress,
                Err(error) => {
                    let failure = error.downcast::<GrantTerminalFailure>()?;
                    return self.park_terminal_grant(item, &pending, failure);
                }
            };
            if progress == OperationProgress::Pending {
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
                    raw_descriptor: pending.raw_descriptor,
                    normalized_item_id: pending.normalized_item_id,
                    item_category: pending.item_category,
                    quantity: pending.quantity,
                    reinforcement_level: pending.reinforcement_level,
                    equip_target: pending.equip_target,
                    blocked: None,
                },
            )?;
        // Record the receive cursor into the save when the backend supports
        // it (bb-archipelago#77). Declined or failed writes leave the slot in
        // attested mode; the delivery is already acknowledged, so a watermark
        // failure must never fail the poll.
        match self.backend.write_save_watermark(item.index) {
            Ok(true) => {
                self.ledger
                    .slot_mut(&self.seed_name, &self.slot_name)
                    .save_watermark = Some(item.index);
            }
            Ok(false) => {}
            Err(error) => client_eprintln!(
                "Save watermark write failed (the delivery stands; restore detection stays attested): {error:#}"
            ),
        }
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

    /// A terminally failed grant is parked: acknowledged in order with its
    /// failure detail, skipping the grant/equip completion ensures. The
    /// in-order acknowledge keeps every cursor and watermark invariant of the
    /// normal path (the ER pot-cap `Capped` path is the precedent), so the
    /// stream continues with the next index and the parked entry waits for
    /// the operator's `bb-blocked` tool. Never retried automatically:
    /// re-issuing an already-delivered item duplicates it (clients#399).
    fn park_terminal_grant(
        &mut self,
        item: IncomingItem,
        pending: &PendingItem,
        failure: GrantTerminalFailure,
    ) -> Result<ItemPollResult> {
        self.ledger
            .slot_mut(&self.seed_name, &self.slot_name)
            .acknowledge_blocked(
                item.index,
                AcknowledgedItem {
                    ap_item_id: item.ap_item_id,
                    raw_descriptor: pending.raw_descriptor,
                    normalized_item_id: pending.normalized_item_id,
                    item_category: pending.item_category,
                    quantity: pending.quantity,
                    reinforcement_level: pending.reinforcement_level,
                    equip_target: pending.equip_target,
                    blocked: Some(format!("{} ({})", failure.status, failure.detail)),
                },
            )?;
        // Mirror the normal acknowledge path's watermark write: without it a
        // future watermark-capable save would read the parked index as
        // regressed and reissue the item.
        match self.backend.write_save_watermark(item.index) {
            Ok(true) => {
                self.ledger
                    .slot_mut(&self.seed_name, &self.slot_name)
                    .save_watermark = Some(item.index);
            }
            Ok(false) => {}
            Err(error) => client_eprintln!(
                "Save watermark write failed for a parked grant (the parking stands): {error:#}"
            ),
        }
        self.ledger.save(&self.ledger_path)?;
        Ok(ItemPollResult::Blocked(BlockedItem {
            index: item.index,
            ap_item_id: item.ap_item_id,
            status: failure.status,
            detail: failure.detail,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{LocationContext, MockBackend};
    use crate::config::{
        DescriptorEvidence, FeedEffectBinding, LocationBinding, RuntimeItemBinding,
        TEST_PEBBLE_EVENT_FLAG,
    };
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

    fn goods() -> RuntimeItemBinding {
        RuntimeItemBinding {
            raw_descriptor: 0xB000_04CE,
            normalized_item_id: 0x4000_04CE,
            item_category: 4,
            descriptor_evidence: DescriptorEvidence::GoodsFormulaObserved,
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
            death_link: false,
            pickup_notification_probe: false,
            expected_save_identity: Some("mock-save".into()),
            suppression_manifest: None,
            installed_gameparam: None,
            suppression: crate::config::SuppressionRequirement::default(),
            location_check_debounce: 3,
            mock_set_flags: vec![],
            goal_location: None,
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

    /// clients#420: while the native flag gate is pending (the game has not
    /// finished loading a character), location checks must simply abstain --
    /// no checks, and no error either. Once the gate arms, the same loop starts
    /// checking with no other intervention. The gating shape is the existing
    /// one: a not-gameplay-ready context, plus `read_event_flag` answering
    /// `None` rather than `false`.
    #[test]
    fn location_checks_abstain_while_the_flag_gate_is_pending_then_arm() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.event_flags_armed = false;
        backend.location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: false,
        });
        backend.set_flags.insert(TEST_PEBBLE_EVENT_FLAG);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let none = HashSet::new();

        for _ in 0..5 {
            assert_eq!(
                client
                    .poll_locations(&none)
                    .expect("waiting is not an error"),
                Vec::<i64>::new()
            );
        }
        // Nothing was bound while waiting: no identity is claimed off a
        // not-ready context.
        assert!(
            client
                .ledger
                .slot("seed", "slot")
                .and_then(|slot| slot.bound_save_identity.clone())
                .is_none()
        );

        // The character finishes loading: the gate arms and the accessor works.
        client.backend.event_flags_armed = true;
        client.backend.location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: true,
        });
        // debounce is 3.
        assert!(client.poll_locations(&none).unwrap().is_empty());
        assert!(client.poll_locations(&none).unwrap().is_empty());
        assert_eq!(client.poll_locations(&none).unwrap(), vec![1000]);
        let _ = std::fs::remove_file(&ledger_path);
    }

    /// clients#423/#455: a check found while offline or sent into a silently
    /// dead socket must be re-sent. `newly_checked` is derived from the
    /// *server-confirmed* set handed in each tick, never the archipelago-rs
    /// optimistic `checked_locations()` cache. As long as the server does not
    /// know about a location whose flag reads true, the poll keeps reporting
    /// it with bounded exponential backoff. This test is the decision-layer
    /// witness: the first report is immediate after debounce, retries follow
    /// the 1/2/4-second schedule, and server acknowledgement silences them.
    #[test]
    fn a_check_the_server_does_not_know_about_is_reported_again() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: true,
        });
        backend.set_flags.insert(TEST_PEBBLE_EVENT_FLAG);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let none = HashSet::new();
        let start = Instant::now();

        // debounce is 3.
        assert!(client.poll_locations_at(&none, start).unwrap().is_empty());
        assert!(client.poll_locations_at(&none, start).unwrap().is_empty());
        assert_eq!(client.poll_locations_at(&none, start).unwrap(), vec![1000]);
        assert!(
            client
                .poll_locations_at(&none, start + Duration::from_millis(999))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            client
                .poll_locations_at(&none, start + Duration::from_secs(1))
                .unwrap(),
            vec![1000]
        );
        assert!(
            client
                .poll_locations_at(&none, start + Duration::from_secs(2))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            client
                .poll_locations_at(&none, start + Duration::from_secs(3))
                .unwrap(),
            vec![1000]
        );

        // The reconnect's fresh client now reports it as checked: quiet.
        let acknowledged = HashSet::from([1000]);
        assert!(
            client
                .poll_locations_at(&acknowledged, start + Duration::from_secs(7))
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_file(&ledger_path);
    }

    #[test]
    fn reconnect_resets_location_retry_backoff() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: true,
        });
        backend.set_flags.insert(TEST_PEBBLE_EVENT_FLAG);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let none = HashSet::new();
        let start = Instant::now();

        assert!(client.poll_locations_at(&none, start).unwrap().is_empty());
        assert!(client.poll_locations_at(&none, start).unwrap().is_empty());
        assert_eq!(client.poll_locations_at(&none, start).unwrap(), vec![1000]);
        assert!(
            client
                .poll_locations_at(&none, start + Duration::from_millis(500))
                .unwrap()
                .is_empty()
        );

        client.reset_location_retry_backoff();
        assert_eq!(
            client
                .poll_locations_at(&none, start + Duration::from_millis(500))
                .unwrap(),
            vec![1000]
        );
        let _ = std::fs::remove_file(&ledger_path);
    }

    #[test]
    fn location_retry_backoff_caps_at_thirty_seconds() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: true,
        });
        backend.set_flags.insert(TEST_PEBBLE_EVENT_FLAG);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let none = HashSet::new();
        let start = Instant::now();

        assert!(client.poll_locations_at(&none, start).unwrap().is_empty());
        assert!(client.poll_locations_at(&none, start).unwrap().is_empty());
        assert_eq!(client.poll_locations_at(&none, start).unwrap(), vec![1000]);
        for elapsed in [1, 3, 7, 15, 31, 61] {
            assert_eq!(
                client
                    .poll_locations_at(&none, start + Duration::from_secs(elapsed))
                    .unwrap(),
                vec![1000]
            );
        }
        assert!(
            client
                .poll_locations_at(&none, start + Duration::from_secs(90))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            client
                .poll_locations_at(&none, start + Duration::from_secs(91))
                .unwrap(),
            vec![1000]
        );
        let _ = std::fs::remove_file(&ledger_path);
    }

    /// clients#423: the seed a runtime is bound to is readable, so a reconnect
    /// can compare it against the slot data it just received.
    #[test]
    fn the_runtime_reports_the_seed_it_is_bound_to() {
        let ledger_path = path();
        let client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        assert_eq!(client.seed_name(), "seed");
        let _ = std::fs::remove_file(&ledger_path);
    }

    /// A weapon whose param id is documented but not yet live-witnessed
    /// (bb-archipelago #208) delivers exactly like a live-witnessed one:
    /// provenance strength is bookkeeping, not behavior.
    #[test]
    fn a_param_id_inferred_weapon_delivers_like_any_other_binding() {
        let ledger_path = path();
        let mut runtime_config = config();
        runtime_config.items.insert(
            3100,
            RuntimeItemBinding {
                raw_descriptor: 0x806C_5660,
                normalized_item_id: 0x006C_5660,
                item_category: 0,
                descriptor_evidence: DescriptorEvidence::ParamIdInferred,
                quantity: 1,
                reinforcement_level: Some(0),
                feed_effect: FeedEffectBinding::RightHandWeapon,
            },
        );
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 3100,
        }];
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            runtime_config,
        );
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 0, .. })
        ));
        assert_eq!(client.backend().grants.len(), 1);
        assert_eq!(client.backend().grants[0].raw_descriptor, 0x806C_5660);
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// Forward compatibility, fail-closed per item: a binding whose evidence
    /// string this build has never seen is parked BY NAME, nothing is granted
    /// for it, and the very next index still delivers. The alternative -- the
    /// old strict parse -- killed the whole session at startup.
    #[test]
    fn an_unknown_evidence_binding_is_refused_by_name_and_the_stream_continues() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.items.insert(
            2100,
            RuntimeItemBinding {
                descriptor_evidence: DescriptorEvidence::Unknown(
                    "sigil_table_inferred".to_string(),
                ),
                ..goods()
            },
        );
        cfg.items.insert(
            2101,
            RuntimeItemBinding {
                raw_descriptor: 0xB000_04D2,
                normalized_item_id: 0x4000_04D2,
                ..goods()
            },
        );
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            cfg,
        );
        let received = [
            IncomingItem {
                index: 0,
                ap_item_id: 2100,
            },
            IncomingItem {
                index: 1,
                ap_item_id: 2101,
            },
        ];

        let first = client.poll_items(&received).unwrap();
        let ItemPollResult::Blocked(blocked) = first else {
            panic!("expected the unknown evidence to park, got {first:?}");
        };
        assert_eq!(blocked.index, 0);
        assert_eq!(blocked.status, "unknown_descriptor_evidence");
        assert!(
            blocked.detail.contains("sigil_table_inferred"),
            "the refusal names the evidence it did not understand: {}",
            blocked.detail
        );
        assert!(blocked.detail.contains("update the client"));
        assert!(
            client.backend.grants.is_empty(),
            "nothing was granted for the refused binding"
        );

        let second = client.poll_items(&received).unwrap();
        assert!(matches!(
            second,
            ItemPollResult::Completed(CompletedItem {
                index: 1,
                ap_item_id: 2101,
                ..
            })
        ));
        assert_eq!(client.backend.grants.len(), 1);
        assert_eq!(client.ledger.slot("seed", "slot").unwrap().next_index(), 2);
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn terminal_harness_failure_parks_and_the_stream_continues() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.fail_grant_terminally("ap_0");
        let mut cfg = config();
        cfg.items.insert(
            2001,
            RuntimeItemBinding {
                raw_descriptor: 0xB000_04D2,
                normalized_item_id: 0x4000_04D2,
                quantity: 3,
                ..goods()
            },
        );
        let mut client = loop_with(backend, ReceiveLedger::default(), ledger_path.clone(), cfg);
        let received = [
            IncomingItem {
                index: 0,
                ap_item_id: 2000,
            },
            IncomingItem {
                index: 1,
                ap_item_id: 2001,
            },
        ];

        let first = client.poll_items(&received).unwrap();
        let ItemPollResult::Blocked(blocked) = first else {
            panic!("expected the terminal failure to park, got {first:?}");
        };
        assert_eq!(blocked.index, 0);
        assert_eq!(blocked.ap_item_id, 2000);
        assert_eq!(blocked.status, "failed");
        // The failed grant never executed, and parking did not wedge the
        // stream: the next poll delivers index 1 instead of retrying index 0.
        assert!(client.backend.grants.is_empty());

        let second = client.poll_items(&received).unwrap();
        assert!(matches!(
            second,
            ItemPollResult::Completed(CompletedItem {
                index: 1,
                ap_item_id: 2001,
                ..
            })
        ));
        assert_eq!(client.backend.grants.len(), 1);
        assert_eq!(client.backend.grants[0].tag, "ap_1");

        let slot = client.ledger.slot("seed", "slot").unwrap();
        assert_eq!(slot.next_index(), 2);
        assert!(slot.pending.is_none());
        assert!(
            slot.acknowledged[&0]
                .blocked
                .as_deref()
                .unwrap()
                .contains("failed")
        );
        assert_eq!(slot.blocked_entries().count(), 1);
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// clients#427, the motivating case: deliver a consumable, let the player
    /// SPEND it, deliver another. Before the fix the precondition was the
    /// ledger's lifetime delivered sum (2), the live stack was 0, and the grant
    /// parked `quantity_mismatch` -- forever, for every later grant of that
    /// item. The precondition is now the observed stack.
    #[test]
    fn a_spent_consumable_still_delivers_against_the_observed_stack() {
        let ledger_path = path();
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received: Vec<IncomingItem> = (0..3)
            .map(|index| IncomingItem {
                index,
                ap_item_id: 2000,
            })
            .collect();
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 0, .. })
        ));
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 1, .. })
        ));
        assert_eq!(client.backend().inventory[&(0x4000_04CE, None)], 2);

        // The player uses both. The ledger still says two were delivered.
        client
            .backend_mut()
            .inventory
            .insert((0x4000_04CE, None), 0);
        assert_eq!(
            client
                .ledger()
                .slot("seed", "slot")
                .unwrap()
                .delivered_quantity(0x4000_04CE, None),
            2
        );

        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 2, .. })
        ));
        // Observed, not predicted.
        assert_eq!(client.backend().grants[2].expected_before, 0);
        assert_eq!(client.backend().inventory[&(0x4000_04CE, None)], 1);
        assert_eq!(
            client.ledger().slot("seed", "slot").unwrap().next_index(),
            3
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// clients#427: the fresh path samples, the replay path does NOT re-sample.
    /// The baseline is recorded durably before the grant can execute, and the
    /// in-flight command that survives a restart is compared against that same
    /// number -- which is exactly what lets the delivery machine answer
    /// `recovered_complete` for an already-applied stack instead of granting
    /// twice. The mock has no recovery model, so the witness here is the number
    /// the replayed grant carries: the recorded 1, never a fresh sample.
    #[test]
    fn a_fresh_grant_records_its_baseline_and_the_replay_reuses_it() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.delay_grant("ap_1", 1);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received: Vec<IncomingItem> = (0..2)
            .map(|index| IncomingItem {
                index,
                ap_item_id: 2000,
            })
            .collect();
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 0, .. })
        ));
        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );

        let persisted = ReceiveLedger::load(&ledger_path).unwrap();
        let pending = persisted
            .slot("seed", "slot")
            .unwrap()
            .pending
            .clone()
            .unwrap();
        assert_eq!(pending.index, 1);
        assert!(!pending.grant_complete);
        assert_eq!(pending.observed_before, Some(1));

        // Restart. The player spent the first item while the client was down;
        // a fresh sample would say 0, and re-granting on that baseline is the
        // double-delivery this recorded number exists to prevent.
        let mut reloaded = loop_with(
            MockBackend::default(),
            persisted,
            ledger_path.clone(),
            config(),
        );
        let error = reloaded.poll_items(&received).unwrap_err();
        assert!(
            format!("{error:#}").contains("expected 1, found 0"),
            "replay must use the recorded baseline, got: {error:#}"
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// clients#427: a proven-withdrawn command releases its baseline, so a
    /// player who spends during the load screen that withdrew it is not parked
    /// on a stale number when it re-publishes.
    #[test]
    fn a_withdrawn_command_releases_its_recorded_baseline() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.delay_grant("ap_0", 1);
        backend.inventory.insert((0x4000_04CE, None), 4);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2000,
        }];
        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        assert_eq!(
            client
                .ledger()
                .slot("seed", "slot")
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .observed_before,
            Some(4)
        );

        // A load transition: the context can no longer be vouched for, the
        // command is withdrawn unexecuted, and the player spends one.
        client.backend_mut().location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: false,
        });
        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        assert_eq!(client.backend().withdrawn, vec!["ap_0".to_string()]);
        assert_eq!(
            client
                .ledger()
                .slot("seed", "slot")
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .observed_before,
            None
        );

        client.backend_mut().location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: true,
        });
        client
            .backend_mut()
            .inventory
            .insert((0x4000_04CE, None), 3);
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 0, .. })
        ));
        assert_eq!(client.backend().grants[0].expected_before, 3);
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// clients#427: no grant is published off an unhydrated inventory -- a
    /// baseline of "not readable yet" is never recorded as zero.
    #[test]
    fn an_unreadable_stack_publishes_no_grant_and_records_no_baseline() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.stack_observation_ready = false;
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2000,
        }];
        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        assert!(client.backend().grants.is_empty());
        assert_eq!(
            client
                .ledger()
                .slot("seed", "slot")
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .observed_before,
            None
        );

        client.backend_mut().stack_observation_ready = true;
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 0, .. })
        ));
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// clients#427: a backend that cannot read inventory (the CE file bridge)
    /// keeps the ledger-derived baseline it always used -- the two-way witness
    /// is the same fixture answering 4 when it can observe and 0 when it
    /// cannot.
    #[test]
    fn a_backend_that_cannot_observe_keeps_the_ledger_baseline() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.inventory.insert((0x4000_04CE, None), 4);
        let mut client = loop_with(
            backend,
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
            ItemPollResult::Completed(CompletedItem { index: 0, .. })
        ));
        assert_eq!(client.backend().grants[0].expected_before, 4);
        std::fs::remove_file(&ledger_path).unwrap();

        let unobserving_path = path();
        let mut backend = MockBackend::default();
        backend.stack_observation_supported = false;
        backend.inventory.insert((0x4000_04CE, None), 4);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            unobserving_path.clone(),
            config(),
        );
        let error = client.poll_items(&received).unwrap_err();
        assert!(
            format!("{error:#}").contains("expected 0, found 4"),
            "the ledger-sum fallback must be unchanged, got: {error:#}"
        );
        std::fs::remove_file(unobserving_path).unwrap();
    }

    /// clients#427: the 21 parked items. A `quantity_mismatch` park was caused
    /// by the precondition this issue removed, so it re-enters the queue on
    /// startup and delivers. Every other park reason stays parked for
    /// bb-blocked -- its cause is not known to be fixed.
    #[test]
    fn quantity_mismatch_parks_requeue_on_startup_and_other_parks_stay() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.fail_grant_terminally_with("ap_0", "quantity_mismatch");
        backend.fail_grant_terminally_with("ap_1", "write_error");
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received: Vec<IncomingItem> = (0..3)
            .map(|index| IncomingItem {
                index,
                ap_item_id: 2000,
            })
            .collect();
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Blocked(BlockedItem { index: 0, .. })
        ));
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Blocked(BlockedItem { index: 1, .. })
        ));
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 2, .. })
        ));

        // Restart with the fix in place: the harness no longer mismatches.
        let persisted = ReceiveLedger::load(&ledger_path).unwrap();
        assert_eq!(
            persisted
                .slot("seed", "slot")
                .unwrap()
                .blocked_entries()
                .count(),
            2
        );
        let mut reloaded = loop_with(
            MockBackend::default(),
            persisted,
            ledger_path.clone(),
            config(),
        );
        assert_eq!(reloaded.requeue_fixed_cause_parks().unwrap(), vec![0]);

        assert!(matches!(
            reloaded.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 0, .. })
        ));
        assert_eq!(reloaded.backend().grants[0].tag, "ap_0");
        assert_eq!(reloaded.backend().inventory[&(0x4000_04CE, None)], 1);

        let slot = reloaded.ledger().slot("seed", "slot").unwrap();
        // The write_error park is untouched, the cursor never regressed, and
        // nothing already delivered is delivered again.
        assert_eq!(slot.blocked_entries().count(), 1);
        assert!(
            slot.acknowledged[&1]
                .blocked
                .as_deref()
                .unwrap()
                .contains("write_error")
        );
        assert_eq!(slot.highest_processed_index, Some(2));
        assert_eq!(slot.next_index(), 3);
        assert_eq!(
            reloaded.poll_items(&received).unwrap(),
            ItemPollResult::Idle
        );
        assert_eq!(reloaded.backend().grants.len(), 1);
        std::fs::remove_file(ledger_path).unwrap();
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
    fn sustain_is_queued_once_only_for_suppressed_new_checks() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.locations[0].vanilla_award_suppressed = true;
        cfg.locations.push(LocationBinding {
            ap_location_id: 2000,
            event_flag: 200,
            vanilla_award_suppressed: false,
        });
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            cfg,
        );

        assert_eq!(
            client.queue_sustain_for_checks(&[1000, 2000]).unwrap(),
            vec![1000]
        );
        assert!(client.queue_sustain_for_checks(&[1000]).unwrap().is_empty());
        let slot = client.ledger().slot("seed", "slot").unwrap();
        assert_eq!(
            slot.pending_sustain.keys().copied().collect::<Vec<_>>(),
            vec![1000]
        );
        assert!(slot.completed_sustain.is_empty());

        let persisted = ReceiveLedger::load(&ledger_path).unwrap();
        assert!(
            persisted
                .slot("seed", "slot")
                .unwrap()
                .pending_sustain
                .contains_key(&1000)
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn sustain_grant_is_witnessed_and_replay_safe() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.locations[0].vanilla_award_suppressed = true;
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            cfg.clone(),
        );
        client.queue_sustain_for_checks(&[1000]).unwrap();

        assert_eq!(
            client.poll_sustain().unwrap(),
            SustainPollResult::Completed(1000)
        );
        assert_eq!(client.backend().grants.len(), 1);
        let grant = &client.backend().grants[0];
        assert_eq!(grant.raw_descriptor, QUICKSILVER_BULLET_RAW_DESCRIPTOR);
        assert_eq!(grant.normalized_item_id, 0x4000_044c);
        assert_eq!(grant.quantity, 1);

        // Server packet replay/repeated flag polling cannot requeue a bonus
        // whose completion was durably witnessed.
        assert!(client.queue_sustain_for_checks(&[1000]).unwrap().is_empty());
        assert_eq!(client.poll_sustain().unwrap(), SustainPollResult::Idle);
        assert_eq!(client.backend().grants.len(), 1);

        let backend = client.backend().clone();
        let mut reloaded = loop_with(
            backend,
            ReceiveLedger::load(&ledger_path).unwrap(),
            ledger_path.clone(),
            cfg,
        );
        assert!(
            reloaded
                .queue_sustain_for_checks(&[1000])
                .unwrap()
                .is_empty()
        );
        assert_eq!(reloaded.poll_sustain().unwrap(), SustainPollResult::Idle);
        assert_eq!(reloaded.backend().grants.len(), 1);
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn interrupted_sustain_grant_reuses_its_durable_baseline() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.locations[0].vanilla_award_suppressed = true;
        let mut backend = MockBackend::default();
        backend.inventory.insert((0x4000_044c, None), 7);
        backend.delay_grant("sustain_1000", 1);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            cfg.clone(),
        );
        client.queue_sustain_for_checks(&[1000]).unwrap();
        assert_eq!(client.poll_sustain().unwrap(), SustainPollResult::Pending);
        assert_eq!(
            client
                .ledger()
                .slot("seed", "slot")
                .unwrap()
                .pending_sustain[&1000],
            Some(7)
        );

        let backend = client.backend().clone();
        let mut reloaded = loop_with(
            backend,
            ReceiveLedger::load(&ledger_path).unwrap(),
            ledger_path.clone(),
            cfg,
        );
        assert_eq!(
            reloaded.poll_sustain().unwrap(),
            SustainPollResult::Completed(1000)
        );
        assert_eq!(
            reloaded.backend().inventory.get(&(0x4000_044c, None)),
            Some(&8)
        );
        assert_eq!(reloaded.backend().grants.len(), 1);
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
    fn save_switch_withdraws_an_unwitnessed_pending_grant() {
        // clients#296: a command published under one validated save must not be
        // left for the harness to execute after the player switches characters.
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.delay_grant("ap_0", 1);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2000,
        }];
        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        assert!(client.backend().withdrawn.is_empty());

        // The save changes while the command is unexecuted: the poll refuses
        // AND withdraws; the durable plan survives.
        client.backend_mut().location_context = Some(LocationContext {
            save_identity: "other-save".into(),
            gameplay_ready: true,
        });
        let error = client.poll_items(&received).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("refused save identity"));
        assert!(message.contains("withdrew the unwitnessed pending grant command"));
        assert_eq!(client.backend().withdrawn, vec!["ap_0".to_string()]);
        assert!(
            client
                .ledger()
                .slot("seed", "slot")
                .is_some_and(|slot| slot.pending.is_some())
        );

        // Returning to the bound save permits one safe retry: the delay is
        // spent, the grant completes exactly once, and nothing regrants.
        client.backend_mut().location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: true,
        });
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));
        assert_eq!(client.backend().grants.len(), 1);
        assert_eq!(client.poll_items(&received).unwrap(), ItemPollResult::Idle);
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn a_non_gameplay_transition_holds_and_withdraws_a_pending_grant() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.delay_grant("ap_0", 1);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2000,
        }];
        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );

        // A load transition (same save, not gameplay-ready) holds the item and
        // withdraws the command; it never reports success for this window.
        client.backend_mut().location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: false,
        });
        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        assert_eq!(client.backend().withdrawn, vec!["ap_0".to_string()]);

        client.backend_mut().location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: true,
        });
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));
        assert_eq!(client.backend().grants.len(), 1);
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn startup_reconcile_withdraws_a_previous_sessions_command() {
        // Simulate a client that died with a grant in flight: the ledger holds
        // the durable plan, and the bridge still holds the command file.
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.delay_grant("ap_0", 60);
        let mut first = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2000,
        }];
        assert_eq!(
            first.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        drop(first);

        // The restarted client withdraws the leftover BEFORE any context is
        // validated, then re-publishes under the validated context and
        // completes exactly once.
        let persisted = ReceiveLedger::load(&ledger_path).unwrap();
        let mut restarted = loop_with(
            MockBackend::default(),
            persisted,
            ledger_path.clone(),
            config(),
        );
        assert!(restarted.reconcile_pending_command().unwrap());
        assert_eq!(restarted.backend().withdrawn, vec!["ap_0".to_string()]);
        assert!(matches!(
            restarted.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));
        assert_eq!(restarted.backend().grants.len(), 1);
        assert_eq!(
            restarted.poll_items(&received).unwrap(),
            ItemPollResult::Idle
        );
        std::fs::remove_file(ledger_path).unwrap();

        // No pending plan -> reconciliation is a no-op.
        let ledger_path = path();
        let mut idle = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        assert!(!idle.reconcile_pending_command().unwrap());
        assert!(idle.backend().withdrawn.is_empty());
        let _ = std::fs::remove_file(ledger_path);
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
    fn validated_saw_spear_grants_once_and_reloads_without_regrant() {
        let ledger_path = path();
        let mut runtime_config = config();
        runtime_config.items.insert(
            3000,
            RuntimeItemBinding {
                raw_descriptor: 0x806C_5660,
                normalized_item_id: 0x006C_5660,
                item_category: 0,
                descriptor_evidence: DescriptorEvidence::LiveGrantInventoryUi,
                quantity: 1,
                reinforcement_level: Some(0),
                feed_effect: FeedEffectBinding::RightHandWeapon,
            },
        );
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 3000,
        }];
        let mut first = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            runtime_config.clone(),
        );
        assert!(matches!(
            first.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));
        assert_eq!(first.backend().grants.len(), 1);
        assert_eq!(first.backend().grants[0].raw_descriptor, 0x806C_5660);
        assert_eq!(first.backend().grants[0].item_category, 0);

        let persisted = ReceiveLedger::load(&ledger_path).unwrap();
        let mut reloaded = loop_with(
            MockBackend::default(),
            persisted,
            ledger_path.clone(),
            runtime_config,
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
            RuntimeItemBinding {
                raw_descriptor: 0x8012_3400,
                normalized_item_id: 0x0012_3400,
                item_category: 0,
                descriptor_evidence: DescriptorEvidence::LiveGrantInventoryUi,
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
                RuntimeItemBinding {
                    raw_descriptor: 0x8012_3400,
                    normalized_item_id: 0x0012_3400,
                    item_category: 0,
                    descriptor_evidence: DescriptorEvidence::LiveGrantInventoryUi,
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
            RuntimeItemBinding {
                raw_descriptor: 0x8012_3400,
                normalized_item_id: 0x0012_3400,
                item_category: 0,
                descriptor_evidence: DescriptorEvidence::LiveGrantInventoryUi,
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
                RuntimeItemBinding {
                    raw_descriptor: 0xB000_1000 + offset as u32,
                    normalized_item_id: 0x4000_1000 + offset as u32,
                    item_category: 4,
                    descriptor_evidence: DescriptorEvidence::GoodsFormulaObserved,
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
            RuntimeItemBinding {
                raw_descriptor: 0x8012_3400,
                normalized_item_id: 0x0012_3400,
                item_category: 0,
                descriptor_evidence: DescriptorEvidence::LiveGrantInventoryUi,
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

    // docs/SAVE-RECONCILIATION.md §9: the six save-restore shapes, exercised
    // end-to-end against the mock save (bb-archipelago#77).

    #[test]
    fn restore_with_consumed_goods_replays_only_the_erased_tail() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.watermark_supported = true;
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received = [
            IncomingItem {
                index: 0,
                ap_item_id: 2000,
            },
            IncomingItem {
                index: 1,
                ap_item_id: 2000,
            },
        ];
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));
        assert_eq!(client.backend().watermark, Some(1));
        assert_eq!(client.backend().inventory[&(0x4000_04CE, None)], 2);

        // The player consumes one pebble and restores a save from before index
        // 1 was granted: game-side state rewinds, the client ledger does not.
        client
            .backend_mut()
            .restore_save(Some(0), HashMap::from([((0x4000_04CE, None), 1)]));
        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Reconciled(WatermarkOutcome::Reissue)
        );
        assert_eq!(
            client.take_watermark_notice(),
            Some(WatermarkOutcome::Reissue)
        );

        // The replay re-grants exactly the erased tail: expected_before comes
        // from the rewound ledger (1), matching the restored save -- never
        // from the pre-restore ledger (2), which the mock would reject, and
        // never from scanning the inventory (§4).
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 1, .. })
        ));
        assert_eq!(client.backend().grants.len(), 3);
        assert_eq!(client.backend().grants[2].expected_before, 1);
        assert_eq!(client.backend().watermark, Some(1));
        assert_eq!(client.poll_items(&received).unwrap(), ItemPollResult::Idle);
        assert_eq!(
            client.ledger().slot("seed", "slot").unwrap().next_index(),
            2
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn restore_across_an_equipment_ack_replays_grant_and_equip() {
        let ledger_path = path();
        let mut runtime_config = config();
        runtime_config.auto_upgrade = true;
        runtime_config.auto_equip = true;
        runtime_config.items.insert(
            3000,
            RuntimeItemBinding {
                raw_descriptor: 0x8012_3400,
                normalized_item_id: 0x0012_3400,
                item_category: 0,
                descriptor_evidence: DescriptorEvidence::LiveGrantInventoryUi,
                quantity: 1,
                reinforcement_level: Some(0),
                feed_effect: FeedEffectBinding::RightHandWeapon,
            },
        );
        let received = [
            IncomingItem {
                index: 0,
                ap_item_id: 3000,
            },
            IncomingItem {
                index: 1,
                ap_item_id: 3000,
            },
        ];
        let mut backend = MockBackend::default();
        backend.watermark_supported = true;
        backend.upgrade_target_level = Some(6);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            runtime_config,
        );
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));
        assert_eq!(client.backend().grants.len(), 2);
        assert_eq!(client.backend().equips.len(), 2);

        // Restore to before index 1: the save keeps the first weapon (granted
        // and auto-upgraded to +6) and forgets the second entirely.
        client.backend_mut().restore_save(
            Some(0),
            HashMap::from([((0x0012_3400 + 6 * 100, Some(6)), 1)]),
        );
        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Reconciled(WatermarkOutcome::Reissue)
        );

        // The replay re-executes BOTH halves of the erased acknowledgement:
        // the grant against the restored inventory, then the equip of the
        // re-planned target. Neither half consulted inventory contents to
        // decide whether the item was missing.
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 1, .. })
        ));
        assert_eq!(client.backend().grants.len(), 3);
        assert_eq!(client.backend().grants[2].expected_before, 1);
        assert_eq!(client.backend().equips.len(), 3);
        assert_eq!(client.poll_items(&received).unwrap(), ItemPollResult::Idle);
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn ledger_loss_adopts_the_save_cursor_without_regranting() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.watermark_supported = true;
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received = [
            IncomingItem {
                index: 0,
                ap_item_id: 2000,
            },
            IncomingItem {
                index: 1,
                ap_item_id: 2000,
            },
        ];
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));
        let backend_after_delivery = client.backend().clone();
        assert_eq!(backend_after_delivery.grants.len(), 2);

        // The durable ledger is lost (disk failure, fresh install): the save's
        // watermark is the only surviving cursor.
        std::fs::remove_file(&ledger_path).unwrap();
        let mut restarted = loop_with(
            backend_after_delivery,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        assert_eq!(
            restarted.poll_items(&received).unwrap(),
            ItemPollResult::Reconciled(WatermarkOutcome::AdoptSaveCursor)
        );
        assert_eq!(
            restarted.take_watermark_notice(),
            Some(WatermarkOutcome::AdoptSaveCursor)
        );
        // Nothing is re-granted (I1): the cursor jumps to the save's
        // watermark and delivery of anything new resumes from there.
        assert_eq!(
            restarted.poll_items(&received).unwrap(),
            ItemPollResult::Idle
        );
        assert_eq!(restarted.backend().grants.len(), 2);
        assert_eq!(
            restarted
                .ledger()
                .slot("seed", "slot")
                .unwrap()
                .next_index(),
            2
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn identity_refusal_precedes_watermark_comparison() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.watermark_supported = true;
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received = [
            IncomingItem {
                index: 0,
                ap_item_id: 2000,
            },
            IncomingItem {
                index: 1,
                ap_item_id: 2000,
            },
            IncomingItem {
                index: 2,
                ap_item_id: 2000,
            },
        ];
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));

        // A different character is loaded, and its save field happens to read
        // a lower watermark than this slot's ledger cursor: if the comparison
        // ran first it would rewind the ledger on the strength of a foreign
        // save. Identity refusal must precede any watermark comparison (§5).
        client.backend_mut().location_context = Some(LocationContext {
            save_identity: "other-save".into(),
            gameplay_ready: true,
        });
        client.backend_mut().watermark = Some(0);
        let error = client.poll_items(&received).unwrap_err();
        assert!(format!("{error:#}").contains("refused save identity"));
        let slot = client.ledger().slot("seed", "slot").unwrap();
        assert_eq!(slot.next_index(), 2);
        assert_eq!(slot.save_watermark, Some(1));
        assert_eq!(slot.acknowledged.len(), 2);
        assert_eq!(client.backend().grants.len(), 2);
        assert!(client.take_watermark_notice().is_none());
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn unreadable_watermark_holds_until_it_recovers() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.watermark_supported = true;
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received = [
            IncomingItem {
                index: 0,
                ap_item_id: 2000,
            },
            IncomingItem {
                index: 1,
                ap_item_id: 2000,
            },
        ];
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));

        // A watermark was recorded for this slot and is now unreadable: no
        // grants and no location checks until it recovers (I3) -- a save whose
        // delivery state cannot be verified is treated as unverifiable, not as
        // restored.
        client.backend_mut().watermark = None;
        assert_eq!(client.poll_items(&received).unwrap(), ItemPollResult::Held);
        assert_eq!(client.take_watermark_notice(), Some(WatermarkOutcome::Hold));
        assert_eq!(client.backend().grants.len(), 1);
        assert!(client.poll_locations(&HashSet::new()).unwrap().is_empty());

        // The field reads again and matches the ledger: delivery resumes
        // exactly where it stopped.
        client.backend_mut().watermark = Some(0);
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 1, .. })
        ));
        assert_eq!(
            client.take_watermark_notice(),
            Some(WatermarkOutcome::Resume)
        );
        assert_eq!(client.backend().grants.len(), 2);
        assert_eq!(client.poll_items(&received).unwrap(), ItemPollResult::Idle);
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn attested_restore_replays_once_and_is_fixed_point_across_restart() {
        // Attested mode is the MVP path (§5): no watermark support anywhere,
        // and the operator attests the restore out of band (bb-restored).
        let ledger_path = path();
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received = [
            IncomingItem {
                index: 0,
                ap_item_id: 2000,
            },
            IncomingItem {
                index: 1,
                ap_item_id: 2000,
            },
        ];
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(_)
        ));
        drop(client);

        // Operator attestation (bb-restored): "the save was restored to before
        // index 1". No watermark is recorded or observed anywhere.
        let mut ledger = ReceiveLedger::load(&ledger_path).unwrap();
        assert_eq!(ledger.slot_mut("seed", "slot").attest_restore(1), 1);
        ledger.save(&ledger_path).unwrap();

        // The restarted client replays exactly the attested tail against the
        // restored inventory, and the watermark machinery stays out of the way
        // (nothing recorded, nothing observed -> Resume, the status quo).
        let mut restored_backend = MockBackend::default();
        restored_backend.inventory.insert((0x4000_04CE, None), 1);
        let mut replayed = loop_with(
            restored_backend,
            ReceiveLedger::load(&ledger_path).unwrap(),
            ledger_path.clone(),
            config(),
        );
        assert!(matches!(
            replayed.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 1, .. })
        ));
        assert_eq!(replayed.backend().grants.len(), 1);
        assert_eq!(
            replayed.poll_items(&received).unwrap(),
            ItemPollResult::Idle
        );
        assert!(
            replayed
                .ledger()
                .slot("seed", "slot")
                .unwrap()
                .save_watermark
                .is_none()
        );

        // A second restart is a fixed point: nothing replays again.
        let mut settled = loop_with(
            MockBackend::default(),
            ReceiveLedger::load(&ledger_path).unwrap(),
            ledger_path.clone(),
            config(),
        );
        assert_eq!(settled.poll_items(&received).unwrap(), ItemPollResult::Idle);
        assert!(settled.backend().grants.is_empty());
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// clients#427 follow-up (clients#428 re-park): THE motivating case. The
    /// startup requeue hands the loop a backlog of many grants of the SAME
    /// item, and each delivery raises that stack. Every grant must observe the
    /// stack as it stands when its own command reaches the head of the queue --
    /// including after the player spends some mid-drain -- so the whole backlog
    /// drains and nothing parks.
    #[test]
    fn a_same_typed_backlog_drains_without_parking() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.inventory.insert((0x4000_04CE, None), 5);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received: Vec<IncomingItem> = (0..6)
            .map(|index| IncomingItem {
                index,
                ap_item_id: 2000,
            })
            .collect();

        for index in 0..6u64 {
            if index == 3 {
                // The player uses two of the item mid-drain.
                let current = client.backend().inventory[&(0x4000_04CE, None)];
                client
                    .backend_mut()
                    .inventory
                    .insert((0x4000_04CE, None), current - 2);
            }
            assert!(
                matches!(
                    client.poll_items(&received).unwrap(),
                    ItemPollResult::Completed(CompletedItem { index: done, .. }) if done == index
                ),
                "index {index} did not deliver"
            );
        }
        // Six deliveries of one each onto a stack of 5, minus the two spent.
        assert_eq!(client.backend().grants.len(), 6);
        assert_eq!(client.backend().inventory[&(0x4000_04CE, None)], 9);
        assert_eq!(
            client
                .ledger()
                .slot("seed", "slot")
                .unwrap()
                .blocked_entries()
                .count(),
            0
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// clients#427 follow-up: the defect oz reproduced within the hour. A
    /// published command that the harness is only RETAINING (native
    /// `awaiting_inventory` -- it can wait minutes, and its own operator
    /// message asks the player to go acquire the item) had its baseline frozen
    /// at publication. Everything the player did during that wait then read as
    /// a `quantity_mismatch`. The baseline is binding only while the command
    /// may have applied; while it is merely retained the next publication
    /// re-observes, and the fresh number is what is recorded and delivered
    /// against.
    #[test]
    fn a_retained_command_re_observes_before_it_publishes() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.inventory.insert((0x4000_04CE, None), 5);
        backend.delay_grant("ap_0", 1);
        backend.retained_unwitnessed.insert("ap_0".into());
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2000,
        }];

        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        assert_eq!(
            client
                .ledger()
                .slot("seed", "slot")
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .observed_before,
            Some(5)
        );

        // The wait is what the operator was told to do something about: the
        // player stocks up while the command sits retained.
        client
            .backend_mut()
            .inventory
            .insert((0x4000_04CE, None), 20);
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 0, .. })
        ));
        // Delivered against 20, not the stale 5 that parked oz's backlog.
        assert_eq!(client.backend().grants[0].expected_before, 20);
        assert_eq!(client.backend().inventory[&(0x4000_04CE, None)], 21);
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// clients#427 follow-up: the replay contract is unchanged for a command
    /// that may have applied. The same fixture, with the harness NOT reporting
    /// the command as merely retained, still freezes its baseline -- so a
    /// restart mid-grant replays against the recorded number instead of a
    /// fresh sample. (The paired witness is
    /// `a_restart_mid_grant_replays_against_the_recorded_baseline`.)
    #[test]
    fn a_command_that_may_have_applied_keeps_its_recorded_baseline() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.inventory.insert((0x4000_04CE, None), 5);
        backend.delay_grant("ap_0", 1);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2000,
        }];
        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        client
            .backend_mut()
            .inventory
            .insert((0x4000_04CE, None), 20);
        let error = client.poll_items(&received).unwrap_err();
        assert!(
            format!("{error:#}").contains("expected 5, found 20"),
            "a possibly-applied command must keep its baseline, got: {error:#}"
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// clients#427 follow-up, point 4: entries parked as `quantity_mismatch`
    /// by the clients#428 build itself re-enter the queue on startup. The park
    /// reason is recorded the same way, so the requeue that shipped with
    /// clients#428 covers today's re-parks -- and after the requeue they
    /// deliver against a freshly observed stack.
    #[test]
    fn a_quantity_mismatch_park_from_the_previous_build_requeues_and_delivers() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.inventory.insert((0x4000_04CE, None), 5);
        backend.fail_grant_terminally_with("ap_0", "quantity_mismatch");
        let mut client = loop_with(
            backend,
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
            ItemPollResult::Blocked(BlockedItem { index: 0, .. })
        ));

        // Restart: the park requeues, and the retry observes the stack as it
        // now stands (the player picked more up in the meantime).
        let persisted = ReceiveLedger::load(&ledger_path).unwrap();
        let mut backend = MockBackend::default();
        backend.inventory.insert((0x4000_04CE, None), 20);
        let mut client = loop_with(backend, persisted, ledger_path.clone(), config());
        assert_eq!(client.requeue_fixed_cause_parks().unwrap(), vec![0]);
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 0, .. })
        ));
        assert_eq!(client.backend().grants[0].expected_before, 20);
        std::fs::remove_file(ledger_path).unwrap();
    }
}
