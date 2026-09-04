use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::backend::{
    BloodborneBackend, EquipRequest, GrantTerminalFailure, ItemGrant, LocationContext,
    OperationProgress, StackObservation,
};
use crate::client_eprintln;
use crate::config::RuntimeConfig;
use crate::feed::{EquipTarget, ReceivedFact, equip_decisions};
use crate::ledger::{
    AcknowledgedItem, FixedCauseRequeue, OperatorAction, PendingItem, ReceiveLedger, SlotLedger,
    VictoryRecord, WatermarkOutcome,
};
use crate::upgrades::{auto_upgrade_level, reinforced_descriptor_pair};

/// Nothing has reached a character through this slot yet: no acknowledged
/// or pending AP item, no operator grant, and no per-check bonus. While this
/// holds the character binding is provisional.
fn slot_is_pristine(slot: &SlotLedger) -> bool {
    slot.acknowledged.is_empty()
        && slot.highest_processed_index.is_none()
        && slot.pending.is_none()
        && slot.operator_grants.is_empty()
        && slot.pending_sustain.is_empty()
        && slot.completed_sustain.is_empty()
}

/// One step of a rescue recipe. Each variant maps onto exactly one existing
/// audited primitive; there is no raw flag or descriptor escape hatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RescueStep {
    /// Set one contract *location* flag. This sends that check.
    SetLocationFlag(u32),
    /// Set the flag of the seed's `goal_location`. This sends the goal.
    SetGoalFlag,
    /// Queue the contract item whose normalized id and category match, on the
    /// idempotent operator lane. For category 255 this is an event-flag write.
    GrantItem {
        normalized_item_id: u32,
        item_category: u8,
    },
}

#[derive(Clone, Copy, Debug)]
enum ResolvedRescueStep {
    Flag { flag: u32, location: i64 },
    Item { ap_item_id: i64 },
}

/// A named repair for a failure the beta is expected to hit. `when` is the
/// symptom a host matches against; `after` is what the player should see.
#[derive(Clone, Copy, Debug)]
pub struct RescueRecipe {
    pub name: &'static str,
    pub when: &'static str,
    pub after: &'static str,
    pub steps: &'static [RescueStep],
}

/// The shipped recipe table. Ids here are Bloodborne 01.09 event flags and
/// normalized descriptors owned by the apworld's `runtime_bindings.py`; every
/// one is re-validated against the live seed contract before use.
pub const RESCUE_RECIPES: &[RescueRecipe] = &[
    RescueRecipe {
        name: "laurence-skull",
        when: "You inspected Laurence's Skull at the Grand Cathedral altar (after Vicar Amelia), the cutscene played, but no check was sent.",
        after: "the Grand Cathedral altar check is sent. The Forbidden Woods password is NOT granted by this; it arrives as its own AP item.",
        steps: &[RescueStep::SetLocationFlag(12_401_898)],
    },
    RescueRecipe {
        name: "forbidden-woods-password",
        when: "AP shows you received \"Fear the Old Blood\" but the Forbidden Woods gate at the bottom of Cathedral Ward still refuses the password.",
        after: "the password flag is written again (idempotent); talk to the gate once more.",
        steps: &[RescueStep::GrantItem {
            normalized_item_id: 12_401_803,
            item_category: 255,
        }],
    },
    RescueRecipe {
        name: "goal",
        when: "You watched your seed's ending (Mergo's Wet Nurse, Gehrman, or Moon Presence, per your YAML goal) but AP never marked you as finished.",
        after: "the goal location flag is written; the client sends goal completion on its next poll. Only run this after the ending actually played.",
        steps: &[RescueStep::SetGoalFlag],
    },
];

pub fn rescue_recipe_names() -> Vec<&'static str> {
    RESCUE_RECIPES.iter().map(|recipe| recipe.name).collect()
}

/// One line per recipe for `rescue` with no arguments.
pub fn rescue_recipe_listing() -> String {
    RESCUE_RECIPES
        .iter()
        .map(|recipe| format!("{}: {}", recipe.name, recipe.when))
        .collect::<Vec<_>>()
        .join("\n")
}

const LOCATION_RETRY_INITIAL: Duration = Duration::from_secs(1);
const LOCATION_RETRY_MAX: Duration = Duration::from_secs(30);
/// Sustain is a best-effort anti-farm replacement, never an AP-owned item. Once its native command
/// has occupied the single delivery lane for this many 50 ms polls, retire it so it cannot starve
/// the authoritative receive stream. This is deliberately longer than the native hydration verify
/// budget (240 polls / 12 seconds).
const SUSTAIN_PENDING_POLL_LIMIT: u32 = 600;
const QUICKSILVER_BULLET_GOODS_ID: u32 = 900;
const QUICKSILVER_BULLET_RAW_DESCRIPTOR: u32 = 0xB000_0384;
const GOODS_NORMALIZED_PREFIX: u32 = 0x4000_0000;

fn rescue_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            elapsed.as_millis().min(u128::from(u64::MAX)) as u64
        })
}

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OperatorGrantPoll {
    Idle,
    Pending,
    /// An operator grant is queued but the AP lane owns a durable pending plan
    /// (review finding C1). The rescue cannot start until that plan retires,
    /// and only `poll_items` retires it, so this is explicitly NOT `Pending`:
    /// the caller must keep polling items. The rescue regains priority on the
    /// first poll after `slot.pending` clears.
    WaitingForItems,
    Completed(i64),
    /// The harness latched a terminal verdict for this rescue grant (review
    /// finding C3). The row is parked durably, the lane is released, and the
    /// caller reports it once instead of re-raising it twenty times a second.
    Parked {
        ap_item_id: i64,
        status: String,
        detail: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SustainPollResult {
    Idle,
    Pending,
    Completed(i64),
    Retired {
        location: i64,
        command_withdrawn: bool,
        reason: &'static str,
    },
}

/// Seed-owned outbound DeathLink policy decision. Detection and broadcast are
/// intentionally separate from this durable state machine: Bloodborne does
/// not enable either until its local-death signal is live-validated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeathLinkAmnestyDecision {
    Disabled,
    Forgiven { used: u32, allowance: u32 },
    Send,
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
    sustain_pending_polls: Option<(i64, u32)>,
    sustain_notice: Option<SustainPollResult>,
}

impl<B: BloodborneBackend> ClientLoop<B> {
    pub fn record_location_checks(&mut self, locations: &[i64]) {
        self.backend.record_location_checks(locations);
    }

    pub fn record_presentation_marker(&mut self, note: &str) -> bool {
        self.backend.record_presentation_marker(note)
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
            sustain_pending_polls: None,
            sustain_notice: None,
        }
    }

    pub fn take_sustain_notice(&mut self) -> Option<SustainPollResult> {
        self.sustain_notice.take()
    }

    fn sustain_command_in_flight(&self) -> bool {
        self.ledger
            .slot(&self.seed_name, &self.slot_name)
            .is_some_and(|slot| slot.pending_sustain.values().any(Option::is_some))
    }

    fn next_sustain(&self) -> Option<(i64, Option<u32>)> {
        let pending = &self
            .ledger
            .slot(&self.seed_name, &self.slot_name)?
            .pending_sustain;
        // A recorded baseline means this command may already own the single native lane. New
        // locations can sort ahead of it by AP id while the player keeps collecting checks; always
        // finish or retire the published command before considering a merely queued sustain.
        pending
            .iter()
            .find(|(_, baseline)| baseline.is_some())
            .or_else(|| pending.iter().next())
            .map(|(&location, &baseline)| (location, baseline))
    }

    fn retire_sustain(
        &mut self,
        location: i64,
        command_withdrawn: bool,
        reason: &'static str,
    ) -> Result<SustainPollResult> {
        let slot = self.ledger.slot_mut(&self.seed_name, &self.slot_name);
        slot.pending_sustain.remove(&location);
        // Retired bonuses are intentionally not replayed. If the cave ran just before a timeout,
        // treating this best-effort bullet as still owed could duplicate it after restart.
        slot.completed_sustain.insert(location);
        self.ledger.save(&self.ledger_path)?;
        self.sustain_pending_polls = None;
        let result = SustainPollResult::Retired {
            location,
            command_withdrawn,
            reason,
        };
        self.sustain_notice = Some(result);
        Ok(result)
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

    pub fn death_link_amnesty(&self) -> u32 {
        self.config.death_link_amnesty
    }

    /// Record one qualifying *local* death and persist the decision before it
    /// can be reported or broadcast. Incoming DeathLinks never call this
    /// method and therefore cannot consume local amnesty.
    pub fn record_qualifying_local_death(&mut self) -> Result<DeathLinkAmnestyDecision> {
        if !self.config.death_link {
            return Ok(DeathLinkAmnestyDecision::Disabled);
        }
        let allowance = self.config.death_link_amnesty;
        let slot = self.ledger.slot_mut(&self.seed_name, &self.slot_name);
        let decision = if slot.death_link_amnesty_used < allowance {
            slot.death_link_amnesty_used += 1;
            DeathLinkAmnestyDecision::Forgiven {
                used: slot.death_link_amnesty_used,
                allowance,
            }
        } else {
            slot.death_link_amnesty_used = 0;
            DeathLinkAmnestyDecision::Send
        };
        self.ledger.save(&self.ledger_path)?;
        Ok(decision)
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

    pub fn victory(&self) -> Option<&VictoryRecord> {
        self.ledger
            .slot(&self.seed_name, &self.slot_name)
            .and_then(|slot| slot.victory.as_ref())
    }

    /// Persist the first authoritative goal witness for this exact seed/slot.
    /// A different goal location is refused instead of rewriting history.
    pub fn record_victory(&mut self, record: VictoryRecord) -> Result<bool> {
        let slot = self.ledger.slot_mut(&self.seed_name, &self.slot_name);
        if let Some(existing) = &slot.victory {
            anyhow::ensure!(
                existing.goal_location == record.goal_location,
                "victory record belongs to goal location {}; refusing replacement with {}",
                existing.goal_location,
                record.goal_location
            );
            return Ok(false);
        }
        slot.victory = Some(record);
        self.ledger.save(&self.ledger_path)?;
        Ok(true)
    }

    /// Best-effort presentation artifact. The caller deliberately invokes
    /// this only after Goal has been sent and the ledger record persisted.
    pub fn write_victory_summary(&self) -> Result<PathBuf> {
        let record = self.victory().context("victory is not recorded")?;
        let output = self.ledger_path.with_file_name("victory-summary.txt");
        std::fs::write(&output, crate::victory::summary_text(record))
            .with_context(|| format!("writing {}", output.display()))?;
        Ok(output)
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
        self.require_runtime_context("rescue flag read")?
            .context("rescue flag read is waiting for gameplay")?;
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

    pub fn rescue_category8_generate(&mut self, gem_gen_param: u32) -> Result<String> {
        self.require_runtime_context("category-8 construction")?
            .context("category-8 construction is waiting for gameplay")?;
        self.backend.category8_generate(gem_gen_param)
    }

    pub fn rescue_category8_insert(&mut self, variant: u8) -> Result<String> {
        self.require_runtime_context("category-8 insertion")?
            .context("category-8 insertion is waiting for gameplay")?;
        self.backend.category8_insert(variant)
    }

    pub fn rescue_list_blocked(&self) -> String {
        self.rescue_list_blocked_with_names(|ap_item_id| format!("item #{ap_item_id}"))
    }

    pub fn rescue_list_blocked_with_names(&self, mut resolve: impl FnMut(i64) -> String) -> String {
        let Some(slot) = self.ledger.slot(&self.seed_name, &self.slot_name) else {
            return "No receive ledger exists for this seed/slot yet.".to_string();
        };
        let rows = slot
            .blocked_entries()
            .map(|(index, item)| {
                format!(
                    "index={index} item={:?} ap_item={} reason={}",
                    resolve(item.ap_item_id),
                    item.ap_item_id,
                    item.blocked.as_deref().unwrap_or("unknown")
                )
            })
            .collect::<Vec<_>>();
        // review finding C3: a parked rescue grant has no AP index, so it is
        // not in `acknowledged`. List it here anyway, or the operator's only
        // evidence would be a single console line they may have scrolled past.
        let rows = rows
            .into_iter()
            .chain(
                slot.operator_grant_parks
                    .iter()
                    .map(|(ap_item_id, reason)| {
                        format!(
                            "index=rescue item={:?} ap_item={ap_item_id} reason={reason}",
                            resolve(*ap_item_id)
                        )
                    }),
            )
            .collect::<Vec<_>>();
        if rows.is_empty() {
            "No parked deliveries.".to_string()
        } else {
            rows.join("\n")
        }
    }

    /// Structured parked rows for UI projection. Name resolution deliberately
    /// stays in the worker, where the Archipelago datapackage is available.
    pub fn rescue_blocked_entries(&self) -> Vec<(u64, i64, String)> {
        self.ledger
            .slot(&self.seed_name, &self.slot_name)
            .into_iter()
            .flat_map(|slot| slot.blocked_entries())
            .map(|(index, item)| {
                (
                    index,
                    item.ap_item_id,
                    item.blocked.as_deref().unwrap_or("unknown").to_owned(),
                )
            })
            .collect()
    }

    /// Safe session facts for the renderer; never exposes process addresses.
    pub fn rescue_context(&mut self) -> Result<Option<LocationContext>> {
        self.backend.location_context()
    }

    pub fn rescue_retry_blocked(&mut self, index: u64) -> Result<()> {
        self.require_runtime_context("rescue delivery retry")?
            .context("rescue delivery retry is waiting for gameplay")?;
        if let Some(pending) = self
            .ledger
            .slot(&self.seed_name, &self.slot_name)
            .and_then(|slot| slot.pending.as_ref())
        {
            anyhow::bail!(
                "parked index {index} is waiting behind active delivery index {}; resolve the Delivery Stalled guidance first. If this client supports the parked binding now, it will retry automatically as soon as that delivery finishes",
                pending.index
            );
        }
        self.ledger
            .slot_mut(&self.seed_name, &self.slot_name)
            .requeue_blocked(index)?;
        self.ledger.save(&self.ledger_path)
    }

    /// Reissue the pending delivery at `index` whose token has been proven
    /// gone (clients#618): the grant completed (`grant_complete: true`) but
    /// the physical item never reached the character -- typically a reload
    /// to an older save. `retry`/`requeue_blocked` refuse this case ("a
    /// delivery is already pending" -- `pending`, not `acknowledged`), and
    /// until now nothing else in the console could clear it; the only
    /// recourse was hand-editing `ledger.json`.
    ///
    /// This requires gameplay context like `retry`, refuses unless `index`
    /// names the current pending entry with its ack flag off (a completed
    /// acknowledgement would mean the item is not actually stuck), and
    /// refuses if the token is present ANYWHERE -- held inventory or the
    /// storage box. A token sitting in storage is exactly the case this must
    /// not reissue over: the bridge only awards once more, so a second token
    /// would strand the first one behind it forever. Only once both reads
    /// come back proven-zero does it reset the pending plan so the normal
    /// ordered pipeline re-grants the same index from the seed contract.
    pub fn rescue_reissue_pending(&mut self, index: u64) -> Result<()> {
        self.require_runtime_context("rescue delivery reissue")?
            .context("rescue delivery reissue is waiting for gameplay")?;
        let (ap_item_id, normalized_item_id, reinforcement_level, grant_complete) = {
            let slot = self
                .ledger
                .slot(&self.seed_name, &self.slot_name)
                .context("no receive ledger exists for this seed/slot yet")?;
            let pending = slot
                .pending
                .as_ref()
                .context("no pending delivery to reissue")?;
            anyhow::ensure!(
                pending.index == index,
                "index {index} is not the pending delivery (the front of the queue is index {})",
                pending.index
            );
            (
                pending.ap_item_id,
                pending.normalized_item_id,
                pending.reinforcement_level,
                pending.grant_complete,
            )
        };
        anyhow::ensure!(
            grant_complete,
            "entry at index {index} has not completed its grant yet; this is not the \
             lost-token case (leave menus and wait, or restart the client)"
        );
        match self
            .backend
            .observe_stack_quantity(normalized_item_id, reinforcement_level)?
        {
            StackObservation::Quantity(0) => {}
            StackObservation::Quantity(held) => anyhow::bail!(
                "refused: the token is held in inventory ({held}); it is not lost, so \
                 reissuing would grant a duplicate"
            ),
            StackObservation::NotReady => {
                anyhow::bail!("refused: held inventory has not hydrated yet; try again shortly")
            }
            StackObservation::Unsupported => anyhow::bail!(
                "refused: this backend cannot read held inventory, so the token's absence \
                 cannot be confirmed"
            ),
        }
        match self
            .backend
            .observe_storage_quantity(normalized_item_id, reinforcement_level)?
        {
            StackObservation::Quantity(0) => {}
            StackObservation::Quantity(stored) => anyhow::bail!(
                "refused: the token is in the Hunter's Dream storage box ({stored}); withdraw \
                 it instead of reissuing, or a second token would leave one stranded there"
            ),
            StackObservation::NotReady => {
                anyhow::bail!("refused: the storage box has not hydrated yet; try again shortly")
            }
            StackObservation::Unsupported => anyhow::bail!(
                "refused: this backend cannot read the storage box, so the token's absence \
                 cannot be confirmed there; withdraw/inspect it manually before trying again \
                 once a storage read is available"
            ),
        }
        self.ledger
            .slot_mut(&self.seed_name, &self.slot_name)
            .reissue_pending(index)?;
        self.ledger
            .slot_mut(&self.seed_name, &self.slot_name)
            .operator_actions
            .push(OperatorAction {
                timestamp_ms: rescue_timestamp_ms(),
                command: "reissue".into(),
                argument: ap_item_id,
                resolved_name: format!("index {index}"),
            });
        self.ledger.save(&self.ledger_path)
    }

    /// The AP index at the front of the queue, if a durable pending plan exists.
    pub fn pending_index(&self) -> Option<u64> {
        self.ledger
            .slot(&self.seed_name, &self.slot_name)
            .and_then(|slot| slot.pending.as_ref().map(|pending| pending.index))
    }

    /// How many parked (blocked) entries this slot's ledger carries.
    pub fn parked_count(&self) -> usize {
        self.ledger
            .slot(&self.seed_name, &self.slot_name)
            .map_or(0, |slot| slot.blocked_entries().count())
    }

    /// Why the item at the front of the queue is not moving, in one sentence a
    /// player can act on. Read from the ledger and the runtime gate, so it
    /// names the actual wait rather than a generic "pending".
    pub fn pending_diagnosis(&mut self) -> Option<String> {
        let (index, ap_item_id, grant_complete, category8_award, token_routed_to_storage) = {
            let slot = self.ledger.slot(&self.seed_name, &self.slot_name)?;
            let pending = slot.pending.as_ref()?;
            (
                pending.index,
                pending.ap_item_id,
                pending.grant_complete,
                self.config
                    .category8_awards
                    .get(&pending.ap_item_id)
                    .cloned(),
                pending.token_routed_to_storage,
            )
        };
        let head = format!(
            "AP item index {index} (id {ap_item_id}) is at the front of the queue and has not moved"
        );
        match self.require_runtime_context("stall diagnosis") {
            Err(error) => return Some(format!("{head}: {error:#}")),
            Ok(None) => {
                return Some(format!(
                    "{head}: waiting for gameplay (load your character and leave menus and loading screens)"
                ));
            }
            Ok(Some(_)) => {}
        }
        if self.reconcile_watermark().ok()? == WatermarkOutcome::Hold {
            return Some(format!(
                "{head}: the save watermark is on hold after a rollback"
            ));
        }
        // clients#617: a category-8 grant that has completed is not, by
        // itself, evidence of a stall -- the ack flag and the ledger-recorded
        // storage-routing evidence must be checked before naming a case,
        // instead of always repeating the storage-box guess regardless of
        // whether it is true.
        Some(
            if let Some(award) = category8_award.filter(|_| grant_complete) {
                if token_routed_to_storage {
                    format!(
                        "{head}: its delivery token was granted but landed in the Hunter's Dream \
                     storage box instead of held inventory. Withdraw it from storage so the \
                     bridge event can consume it"
                    )
                } else {
                    match self.backend.read_event_flag(award.ack_flag) {
                        Ok(Some(true)) => format!(
                            "{head}: its acknowledgement flag is already set -- the award landed \
                         and the client simply has not caught up yet. This should clear on the \
                         next poll; restart the client if it does not"
                        ),
                        Ok(_) => match self
                            .backend
                            .observe_stack_quantity(0x4000_0000 | award.token_goods_id, None)
                        {
                            Ok(StackObservation::Quantity(0)) => format!(
                                "{head}: its delivery token is not held and its acknowledgement \
                             flag is not set -- the token is likely gone (for example, lost to \
                             a reload onto an older save). Confirm it is absent from storage too, \
                             then run 'reissue {index}' to re-grant it -- only once you have \
                             confirmed it is truly gone, since a second token would strand the \
                             first one if it is still sitting in storage"
                            ),
                            _ => format!(
                                "{head}: its delivery token was granted and is still on hand, but \
                             the game has not consumed it yet. Leave menus and wait a few seconds"
                            ),
                        },
                        Err(error) => {
                            format!("{head}: could not read its acknowledgement flag: {error:#}")
                        }
                    }
                }
            } else if !grant_complete {
                format!(
                    "{head}: the native grant has not completed. Leave menus and loading screens; \
                 if it stays here, restart the client (the plan is durable and will not double-grant)"
                )
            } else {
                format!("{head}: waiting for its equip or acknowledgement step")
            },
        )
    }

    /// Release the durable character binding so the next validated gameplay
    /// observation binds afresh. Only permitted while the slot's ledger is
    /// pristine: nothing delivered, nothing pending, no operator grants. Once
    /// an item has reached a character, that character is the seed's.
    pub fn rescue_rebind(&mut self) -> Result<String> {
        let slot = self.ledger.slot_mut(&self.seed_name, &self.slot_name);
        let Some(previous) = slot.bound_save_identity.clone() else {
            return Ok(
                "No character is bound yet; the next validated gameplay observation binds.".into(),
            );
        };
        anyhow::ensure!(
            slot_is_pristine(slot),
            "rebind refused: items have already been delivered to {previous}; a seed follows the character that received its first item"
        );
        slot.bound_save_identity = None;
        slot.operator_actions.push(OperatorAction {
            timestamp_ms: rescue_timestamp_ms(),
            command: "rebind".into(),
            argument: 0,
            resolved_name: previous.clone(),
        });
        self.ledger.save(&self.ledger_path)?;
        Ok(format!(
            "released binding to {previous}; load the character you mean to play and the client will bind to it"
        ))
    }

    /// Set one seed-owned location flag. No raw or unmapped flag can cross
    /// this boundary; the caller receives the AP location id for naming.
    pub fn rescue_location_for_flag(&self, event_flag: u32) -> Result<i64> {
        self.config
            .locations
            .iter()
            .find(|binding| binding.event_flag == event_flag)
            .map(|binding| binding.ap_location_id)
            .with_context(|| format!("event flag {event_flag} is not in this seed contract"))
    }

    pub fn rescue_set_flag(&mut self, event_flag: u32, location_name: &str) -> Result<i64> {
        self.require_runtime_context("rescue setflag")?
            .context("rescue setflag is waiting for gameplay")?;
        let location = self.rescue_location_for_flag(event_flag)?;
        self.backend.write_event_flag(event_flag, true)?;
        self.ledger
            .slot_mut(&self.seed_name, &self.slot_name)
            .operator_actions
            .push(OperatorAction {
                timestamp_ms: rescue_timestamp_ms(),
                command: "setflag".into(),
                argument: i64::from(event_flag),
                resolved_name: location_name.into(),
            });
        self.ledger.save(&self.ledger_path)?;
        Ok(location)
    }

    /// Queue one contract-known item id on an isolated durable lane. It never
    /// changes the AP receive cursor and a repeated command is a fixed point.
    pub fn rescue_give(&mut self, ap_item_id: i64, item_name: &str) -> Result<bool> {
        self.require_runtime_context("rescue give")?
            .context("rescue give is waiting for gameplay")?;
        let binding = self
            .config
            .items
            .get(&ap_item_id)
            .with_context(|| format!("item index {ap_item_id} is not in this seed contract"))?
            .clone();
        anyhow::ensure!(
            binding.descriptor_evidence.is_known(),
            "item index {ap_item_id} has no proven named mapping"
        );
        let slot = self.ledger.slot_mut(&self.seed_name, &self.slot_name);
        if slot.operator_grants.contains_key(&ap_item_id)
            // review finding C3: a parked rescue stays recorded, so a retyped
            // `give` does not re-run a command that may have half applied.
            || slot.operator_grant_parks.contains_key(&ap_item_id)
            || slot
                .acknowledged
                .values()
                .any(|item| item.ap_item_id == ap_item_id)
        {
            return Ok(false);
        }
        slot.operator_grants.insert(
            ap_item_id,
            PendingItem {
                index: 0,
                ap_item_id,
                raw_descriptor: binding.raw_descriptor,
                normalized_item_id: binding.normalized_item_id,
                item_category: binding.item_category,
                quantity: 1,
                upgrade_target_level: None,
                reinforcement_level: binding.reinforcement_level,
                equip_target: None,
                grant_complete: false,
                equip_complete: true,
                observed_before: None,
                token_routed_to_storage: false,
            },
        );
        slot.operator_actions.push(OperatorAction {
            timestamp_ms: rescue_timestamp_ms(),
            command: "give".into(),
            argument: ap_item_id,
            resolved_name: item_name.into(),
        });
        self.ledger.save(&self.ledger_path)?;
        Ok(true)
    }

    /// Queue every contract-known weapon and attire binding on the durable
    /// operator lane. Polling remains strictly serial: this only removes the
    /// need to paste roughly ninety `give` commands by hand.
    pub fn rescue_equipment_census(
        &mut self,
        mut resolve_item: impl FnMut(i64) -> String,
    ) -> Result<(usize, usize)> {
        self.require_runtime_context("equipment census")?
            .context("equipment census is waiting for gameplay")?;
        let mut candidates = self
            .config
            .items
            .iter()
            .filter(|(_, binding)| {
                matches!(binding.item_category, 0 | 1) && binding.descriptor_evidence.is_known()
            })
            .map(|(&ap_item_id, binding)| (ap_item_id, binding.clone()))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(ap_item_id, _)| *ap_item_id);

        let slot = self.ledger.slot_mut(&self.seed_name, &self.slot_name);
        let mut queued = 0;
        let mut skipped = 0;
        for (ap_item_id, binding) in candidates {
            if slot.operator_grants.contains_key(&ap_item_id)
                || slot.operator_grant_parks.contains_key(&ap_item_id)
                || slot
                    .acknowledged
                    .values()
                    .any(|item| item.ap_item_id == ap_item_id)
            {
                skipped += 1;
                continue;
            }
            let item_name = resolve_item(ap_item_id);
            slot.operator_grants.insert(
                ap_item_id,
                PendingItem {
                    index: 0,
                    ap_item_id,
                    raw_descriptor: binding.raw_descriptor,
                    normalized_item_id: binding.normalized_item_id,
                    item_category: binding.item_category,
                    quantity: 1,
                    upgrade_target_level: None,
                    reinforcement_level: binding.reinforcement_level,
                    equip_target: None,
                    grant_complete: false,
                    equip_complete: true,
                    observed_before: None,
                    token_routed_to_storage: false,
                },
            );
            slot.operator_actions.push(OperatorAction {
                timestamp_ms: rescue_timestamp_ms(),
                command: "census".into(),
                argument: ap_item_id,
                resolved_name: item_name,
            });
            queued += 1;
        }
        self.ledger.save(&self.ledger_path)?;
        Ok((queued, skipped))
    }

    /// Apply one named rescue recipe. Every step reuses an audited primitive
    /// (`rescue_set_flag`, `rescue_give`); the recipe adds a stable name, a
    /// description of the failure it repairs, and up-front validation so a
    /// half-applied repair cannot start. Returns one audit line per step.
    pub fn rescue_recipe(
        &mut self,
        name: &str,
        mut resolve_item: impl FnMut(i64) -> String,
        mut resolve_location: impl FnMut(i64) -> String,
    ) -> Result<Vec<String>> {
        let recipe = RESCUE_RECIPES
            .iter()
            .find(|recipe| recipe.name.eq_ignore_ascii_case(name))
            .with_context(|| {
                format!(
                    "unknown rescue recipe {name:?}; known recipes: {}",
                    rescue_recipe_names().join(", ")
                )
            })?;
        self.require_runtime_context("rescue recipe")?
            .context("rescue recipe is waiting for gameplay")?;

        // Resolve and validate every step before the first mutation.
        let mut plan = Vec::with_capacity(recipe.steps.len());
        for step in recipe.steps {
            plan.push(match *step {
                RescueStep::SetLocationFlag(flag) => {
                    let location = self.rescue_location_for_flag(flag)?;
                    ResolvedRescueStep::Flag { flag, location }
                }
                RescueStep::SetGoalFlag => {
                    let goal = self
                        .config
                        .goal_location
                        .context("this seed contract carries no goal_location")?;
                    let flag = self
                        .config
                        .locations
                        .iter()
                        .find(|binding| binding.ap_location_id == goal)
                        .map(|binding| binding.event_flag)
                        .with_context(|| format!("goal location {goal} has no runtime flag"))?;
                    ResolvedRescueStep::Flag {
                        flag,
                        location: goal,
                    }
                }
                RescueStep::GrantItem {
                    normalized_item_id,
                    item_category,
                } => {
                    let ap_item_id = self
                        .config
                        .items
                        .iter()
                        .find(|(_, binding)| {
                            binding.normalized_item_id == normalized_item_id
                                && binding.item_category == item_category
                                && binding.descriptor_evidence.is_known()
                        })
                        .map(|(id, _)| *id)
                        .with_context(|| {
                            format!(
                                "no contract item with normalized id {normalized_item_id:#x} in category {item_category}"
                            )
                        })?;
                    ResolvedRescueStep::Item { ap_item_id }
                }
            });
        }

        let mut lines = Vec::with_capacity(plan.len() + 1);
        for step in plan {
            match step {
                ResolvedRescueStep::Flag { flag, location } => {
                    let label = resolve_location(location);
                    self.rescue_set_flag(flag, &label)?;
                    lines.push(format!(
                        "AUDIT rescue {} setflag flag={flag} ({label:?}): written; that check is now sent.",
                        recipe.name
                    ));
                }
                ResolvedRescueStep::Item { ap_item_id } => {
                    let label = resolve_item(ap_item_id);
                    let queued = self.rescue_give(ap_item_id, &label)?;
                    lines.push(if queued {
                        format!(
                            "AUDIT rescue {} give index={ap_item_id} ({label:?}): queued through normal delivery.",
                            recipe.name
                        )
                    } else {
                        format!(
                            "AUDIT rescue {} give index={ap_item_id} ({label:?}): already recorded; no second grant queued.",
                            recipe.name
                        )
                    });
                }
            }
        }
        self.ledger
            .slot_mut(&self.seed_name, &self.slot_name)
            .operator_actions
            .push(OperatorAction {
                timestamp_ms: rescue_timestamp_ms(),
                command: "rescue".into(),
                argument: 0,
                resolved_name: recipe.name.into(),
            });
        self.ledger.save(&self.ledger_path)?;
        lines.push(format!(
            "AUDIT rescue {} complete: {}",
            recipe.name, recipe.after
        ));
        Ok(lines)
    }

    pub fn poll_operator_grant(&mut self) -> Result<OperatorGrantPoll> {
        let Some((&ap_item_id, mut pending)) = self
            .ledger
            .slot(&self.seed_name, &self.slot_name)
            .and_then(|slot| {
                slot.operator_grants
                    .iter()
                    .find(|(_, row)| !row.grant_complete)
            })
            .map(|(id, row)| (id, row.clone()))
        else {
            return Ok(OperatorGrantPoll::Idle);
        };
        // review finding C1: an AP plan owns the native lane, and only
        // `poll_items` ever clears `slot.pending`. Reporting `Pending` here
        // made the caller skip `poll_items`, so the AP plan could never
        // retire and the rescue could never start -- durably, because both
        // rows survive a restart. Say "waiting for items" instead, so the AP
        // lane keeps advancing; the rescue takes the lane on the next poll
        // after the plan completes.
        if self
            .ledger
            .slot(&self.seed_name, &self.slot_name)
            .is_some_and(|slot| slot.pending.is_some())
        {
            return Ok(OperatorGrantPoll::WaitingForItems);
        }
        // The main loop gives an explicit rescue grant priority over ordinary AP delivery. If a
        // sustain already owns the native lane, merely reporting the rescue as pending would also
        // stop the idle-only sustain poll and deadlock both operations. Advance the expendable
        // sustain here until it completes or reaches its bounded retirement, then rescue.
        if self.sustain_command_in_flight() && self.poll_sustain()? == SustainPollResult::Pending {
            return Ok(OperatorGrantPoll::Pending);
        }
        let tag = format!("operator_grant_{ap_item_id}");
        match self.require_runtime_context("rescue give") {
            Ok(Some(_)) => {}
            Ok(None) => {
                if self.backend.withdraw_unwitnessed_grant(&tag)? {
                    self.ledger
                        .slot_mut(&self.seed_name, &self.slot_name)
                        .operator_grants
                        .get_mut(&ap_item_id)
                        .expect("operator grant remains durable while polling")
                        .observed_before = None;
                    self.ledger.save(&self.ledger_path)?;
                }
                return Ok(OperatorGrantPoll::Pending);
            }
            Err(error) => {
                if self.backend.withdraw_unwitnessed_grant(&tag)? {
                    self.ledger
                        .slot_mut(&self.seed_name, &self.slot_name)
                        .operator_grants
                        .get_mut(&ap_item_id)
                        .expect("operator grant remains durable while polling")
                        .observed_before = None;
                    self.ledger.save(&self.ledger_path)?;
                }
                return Err(error);
            }
        }
        let expected_before = match pending.observed_before {
            Some(value) => value,
            None => match self
                .backend
                .observe_stack_quantity(pending.normalized_item_id, pending.reinforcement_level)?
            {
                StackObservation::Quantity(value) => {
                    pending.observed_before = Some(value);
                    self.ledger
                        .slot_mut(&self.seed_name, &self.slot_name)
                        .operator_grants
                        .insert(ap_item_id, pending.clone());
                    self.ledger.save(&self.ledger_path)?;
                    value
                }
                StackObservation::NotReady => return Ok(OperatorGrantPoll::Pending),
                StackObservation::Unsupported => 0,
            },
        };
        let grant = ItemGrant {
            raw_descriptor: pending.raw_descriptor,
            normalized_item_id: pending.normalized_item_id,
            item_category: pending.item_category,
            quantity: 1,
            expected_before,
            reinforcement_level: pending.reinforcement_level,
            tag,
        };
        // review finding C3: the harness latches a terminal verdict for the
        // tag, so propagating it would re-raise the same error on every 50 ms
        // poll, hold the lane, and stop `poll_items` for the rest of the
        // session. Park it the way `park_terminal_grant` parks an AP item.
        let progress = match self.backend.grant_item(&grant) {
            Ok(progress) => progress,
            Err(error) => {
                let failure = error.downcast::<GrantTerminalFailure>()?;
                return self.park_operator_grant(ap_item_id, failure);
            }
        };
        if progress == OperationProgress::Pending {
            return Ok(OperatorGrantPoll::Pending);
        }
        self.ledger
            .slot_mut(&self.seed_name, &self.slot_name)
            .operator_grants
            .get_mut(&ap_item_id)
            .expect("operator grant remains durable while polling")
            .grant_complete = true;
        self.ledger.save(&self.ledger_path)?;
        Ok(OperatorGrantPoll::Completed(ap_item_id))
    }

    pub fn rescue_export(&self) -> Result<PathBuf> {
        let output = self.ledger_path.with_file_name("rescue-diagnostics.json");
        let slot = self.ledger.slot(&self.seed_name, &self.slot_name);
        let capture_files = collect_capture_files(&self.ledger_path)?;
        let document = json::json!({
            "format": "bb-rescue-diagnostics-v2",
            "runtime_build": crate::RUNTIME_BUILD,
            "seed": self.seed_name,
            "slot": self.slot_name,
            "location_count": self.config.locations.len(),
            "item_count": self.config.items.len(),
            "ledger": slot,
            "operator_actions": slot.map(|slot| &slot.operator_actions),
            "capture_files": capture_files,
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
        if let Some(expected) = self.config.expected_save_identity.as_deref()
            && context.save_identity != expected
        {
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
            if bound != context.save_identity {
                // A fresh shadPS4 profile writes several userdata slots while
                // it initialises save data, and a player may load the wrong
                // character first. Until something has actually reached a
                // character the binding is provisional and follows the game.
                let previous = bound.to_owned();
                let slot = self.ledger.slot_mut(&self.seed_name, &self.slot_name);
                anyhow::ensure!(
                    slot_is_pristine(slot),
                    "{operation} refused save identity {:?}; AP slot is durably bound to {:?}",
                    context.save_identity,
                    previous
                );
                slot.bound_save_identity = Some(context.save_identity.clone());
                slot.operator_actions.push(OperatorAction {
                    timestamp_ms: rescue_timestamp_ms(),
                    command: "rebind-auto".into(),
                    argument: 0,
                    resolved_name: format!("{previous} -> {}", context.save_identity),
                });
                self.ledger.save(&self.ledger_path)?;
                client_eprintln!(
                    "Rebound AP slot {:?} from {previous} to loaded Bloodborne character {}: nothing had been delivered yet.",
                    self.slot_name,
                    context.save_identity
                );
            }
        } else {
            self.ledger
                .slot_mut(&self.seed_name, &self.slot_name)
                .bound_save_identity = Some(context.save_identity.clone());
            self.ledger.save(&self.ledger_path)?;
            client_eprintln!(
                "Bound AP slot {:?} to loaded Bloodborne character {}.",
                self.slot_name,
                context.save_identity
            );
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

    /// The per-check bonus good as (raw descriptor, normalized id): the seed's
    /// published `sustain_item` when the contract carries one, else the
    /// Quicksilver Bullet constant for older contracts.
    fn sustain_descriptor(&self) -> (u32, u32) {
        match &self.config.sustain_item {
            Some(item) => (item.raw_descriptor, item.normalized_item_id),
            None => (
                QUICKSILVER_BULLET_RAW_DESCRIPTOR,
                GOODS_NORMALIZED_PREFIX | QUICKSILVER_BULLET_GOODS_ID,
            ),
        }
    }

    /// Advance at most one replay-safe Quicksilver Bullet bonus. Received AP
    /// items retain priority: the binary calls this only when their delivery
    /// machine is idle, and errors here are reported independently.
    pub fn poll_sustain(&mut self) -> Result<SustainPollResult> {
        let Some((location, recorded_before)) = self.next_sustain() else {
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

        let (raw_descriptor, normalized) = self.sustain_descriptor();
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
            raw_descriptor,
            normalized_item_id: normalized,
            item_category: 4,
            quantity: 1,
            expected_before,
            reinforcement_level: None,
            tag,
        };
        match self.backend.grant_item(&grant) {
            Ok(OperationProgress::Pending) => {
                let polls = match self.sustain_pending_polls {
                    Some((same, polls)) if same == location => polls.saturating_add(1),
                    _ => 1,
                };
                self.sustain_pending_polls = Some((location, polls));
                if polls < SUSTAIN_PENDING_POLL_LIMIT {
                    return Ok(SustainPollResult::Pending);
                }
                let withdrawn = self
                    .backend
                    .retire_grant(&grant.tag, "native grant timed out")?;
                return self.retire_sustain(location, withdrawn, "native grant timed out");
            }
            Ok(OperationProgress::Complete) => {}
            Err(error) => {
                if error.downcast_ref::<GrantTerminalFailure>().is_some() {
                    return self.retire_sustain(location, false, "native grant failed terminally");
                }
                return Err(error);
            }
        }
        let slot = self.ledger.slot_mut(&self.seed_name, &self.slot_name);
        slot.pending_sustain.remove(&location);
        slot.completed_sustain.insert(location);
        self.ledger.save(&self.ledger_path)?;
        self.sustain_pending_polls = None;
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
            token_routed_to_storage: false,
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
        let operator_tag = self
            .ledger
            .slot(&self.seed_name, &self.slot_name)
            .and_then(|slot| {
                slot.operator_grants
                    .iter()
                    .find(|(_, pending)| !pending.grant_complete)
            })
            .map(|(ap_item_id, _)| format!("operator_grant_{ap_item_id}"));
        let Some(tag) = item_tag.or(sustain_tag).or(operator_tag) else {
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
    /// stays parked for `bb-blocked`. Returns the requeued indices, plus the
    /// ones deferred because a durable pending plan still owns the cursor
    /// (review finding C5) -- the caller reports those so the operator knows
    /// why a park it expected to see unparked is still parked.
    pub fn requeue_fixed_cause_parks(&mut self) -> Result<FixedCauseRequeue> {
        let compatible = self
            .config
            .items
            .iter()
            .filter(|(_, binding)| binding.descriptor_evidence.is_known())
            .map(|(ap_item_id, _)| *ap_item_id)
            .collect::<HashSet<_>>();
        let slot = self.ledger.slot_mut(&self.seed_name, &self.slot_name);
        let mut outcome = slot.requeue_fixed_cause_parks();
        let upgraded = slot.requeue_now_compatible_parks(|id| compatible.contains(&id));
        outcome.requeued.extend(upgraded.requeued);
        outcome.deferred.extend(upgraded.deferred);
        if !outcome.requeued.is_empty() {
            self.ledger.save(&self.ledger_path)?;
        }
        Ok(outcome)
    }

    fn requeue_now_compatible_parks(&mut self) -> Result<FixedCauseRequeue> {
        if self.ledger.slot(&self.seed_name, &self.slot_name).is_none() {
            return Ok(FixedCauseRequeue::default());
        }
        let compatible = self
            .config
            .items
            .iter()
            .filter(|(_, binding)| binding.descriptor_evidence.is_known())
            .map(|(ap_item_id, _)| *ap_item_id)
            .collect::<HashSet<_>>();
        let outcome = self
            .ledger
            .slot_mut(&self.seed_name, &self.slot_name)
            .requeue_now_compatible_parks(|id| compatible.contains(&id));
        if !outcome.requeued.is_empty() {
            self.ledger.save(&self.ledger_path)?;
        }
        Ok(outcome)
    }

    /// Processes at most one item, preserving AP index order and durable state
    /// across the grant -> optional upgrade -> optional equip sequence.
    pub fn poll_items(&mut self, received: &[IncomingItem]) -> Result<ItemPollResult> {
        // A park deferred at startup because another delivery owned the lane
        // must recover without asking the player to restart or use Rescue.
        // This is cheap once there is nothing eligible and persists only when
        // an entry actually moves back into the ordered queue.
        let recovered = self.requeue_now_compatible_parks()?;
        if !recovered.requeued.is_empty() {
            client_eprintln!(
                "Automatically re-queued {} parked item(s) now supported by this client (indices {}).",
                recovered.requeued.len(),
                recovered
                    .requeued
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        // A sustain command already published to the single native lane must be advanced before an
        // AP item tries to use that lane. The old main-loop ordering stopped polling sustain as
        // soon as an AP item arrived, so each side waited forever for the other. Bounded retirement
        // in `poll_sustain` means AP waits at most one sustain budget, never indefinitely.
        if self.sustain_command_in_flight() && self.poll_sustain()? == SustainPollResult::Pending {
            return Ok(ItemPollResult::Pending);
        }
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
            let evidence = binding.descriptor_evidence.as_str();
            let failure = match evidence.strip_prefix("unreviewed_attire_") {
                // Downgraded at contract load: the world published attire this
                // build has not reviewed for its category-1 lane. Say that,
                // not "unknown evidence", so the player and the log agree.
                Some(protector_id) => GrantTerminalFailure {
                    tag: grant_tag(item.index),
                    status: "unreviewed_attire".to_string(),
                    detail: format!(
                        "AP item {} is attire (protector {protector_id}) that this client has not reviewed for delivery; it is parked, not lost, and delivers once a client that has reviewed it is installed",
                        item.ap_item_id
                    ),
                },
                None => GrantTerminalFailure {
                    tag: grant_tag(item.index),
                    status: "unknown_descriptor_evidence".to_string(),
                    detail: format!(
                        "AP item {} carries descriptor evidence {evidence:?}, which this client does not understand; this seed was generated by a newer world -- update the client to deliver it",
                        item.ap_item_id
                    ),
                },
            };
            return self.park_terminal_grant(item, &pending, failure);
        }

        let category8_award = self.config.category8_awards.get(&item.ap_item_id).cloned();
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
            let expected_before = if pending.item_category == 255 || category8_award.is_some() {
                // Category-8 bridge goods are reserved synthetic tokens. An
                // absent token has no inventory record for the ordinary stack
                // observer to find, so observing it can remain NotReady
                // forever. The false acknowledgement flag above plus the
                // durable AP pending row establish the one-shot precondition;
                // the native goods lane can therefore insert from zero.
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
            // clients#617: capture, at the one moment the backend can actually
            // see it, whether this category-8 grant landed in storage instead
            // of held inventory. The stall diagnosis below has no independent
            // way to observe the storage box, so this durable flag is the only
            // honest source for the "withdraw it from storage" verdict.
            let routed_to_storage =
                category8_award.is_some() && self.backend.last_grant_went_to_storage(&grant.tag);
            let slot = self.ledger.slot_mut(&self.seed_name, &self.slot_name);
            slot.mark_grant_complete()?;
            if routed_to_storage {
                slot.mark_token_routed_to_storage()?;
            }
            self.ledger.save(&self.ledger_path)?;
            pending.grant_complete = true;
            pending.token_routed_to_storage = routed_to_storage;
        }

        if let Some(award) = &category8_award {
            // The token's ordinary grant acknowledgement is only the first
            // half. Completion belongs to the game's event: it consumes the
            // token, awards the lot, then raises this seed-owned flag.
            if self.backend.read_event_flag(award.ack_flag)? != Some(true) {
                return Ok(ItemPollResult::Pending);
            }
            match self
                .backend
                .observe_stack_quantity(0x4000_0000 | award.token_goods_id, None)?
            {
                StackObservation::Quantity(0) => {}
                StackObservation::NotReady | StackObservation::Unsupported => {
                    return Ok(ItemPollResult::Pending);
                }
                // A true flag can be stale from an earlier seed using this
                // save. The event clears it after seeing the new token, so do
                // not turn that short hand-off window into a client failure.
                StackObservation::Quantity(_) => return Ok(ItemPollResult::Pending),
            }
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
            // Review finding C2: an equip failure must never wedge the stream.
            // The grant above is already complete and durable, so the item IS
            // in the player's inventory. Propagating the error with `?` was
            // fatal in practice because the native backend's `equip_item` is an
            // unconditional bail and the error is not a `GrantTerminalFailure`,
            // so it was never parked either: with the world's "Auto Equip
            // Received Gear" option on, the first weapon or attire piece
            // blocked every later AP item forever. Equipping is a convenience
            // on top of a delivery that already happened, so say so once and
            // acknowledge the item as delivered but not equipped.
            match self.backend.equip_item(&request) {
                Ok(OperationProgress::Complete) => {}
                Ok(OperationProgress::Pending) => return Ok(ItemPollResult::Pending),
                Err(error) => client_eprintln!(
                    "Auto Equip Received Gear could not equip AP item {} ({target:?}); it was delivered to your inventory, so equip it yourself: {error:#}",
                    item.ap_item_id
                ),
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
    /// Park a terminally failed operator grant (review finding C3). The row
    /// leaves the active lane so `poll_operator_grant` reports `Idle` on the
    /// next poll and AP delivery resumes; the harness verdict is kept durably
    /// so `blocked` can show it and a repeated `give` stays a fixed point. It
    /// is never retried automatically: the failed command may have applied.
    fn park_operator_grant(
        &mut self,
        ap_item_id: i64,
        failure: GrantTerminalFailure,
    ) -> Result<OperatorGrantPoll> {
        let slot = self.ledger.slot_mut(&self.seed_name, &self.slot_name);
        slot.operator_grants.remove(&ap_item_id);
        slot.operator_grant_parks.insert(
            ap_item_id,
            format!("{} ({})", failure.status, failure.detail),
        );
        self.ledger.save(&self.ledger_path)?;
        Ok(OperatorGrantPoll::Parked {
            ap_item_id,
            status: failure.status,
            detail: failure.detail,
        })
    }

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

fn collect_capture_files(ledger: &Path) -> Result<std::collections::BTreeMap<String, String>> {
    let folder = ledger.parent().unwrap_or_else(|| Path::new("."));
    let mut captures = std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let include = name.ends_with("-capture.jsonl")
            || name == "boss-flag-census.jsonl"
            || name == crate::health::HEALTH_FILE_NAME;
        if include {
            captures.insert(name, std::fs::read_to_string(entry.path())?);
        }
    }
    Ok(captures)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rescue_capture_collection_includes_every_probe_sidecar() {
        let root = std::env::temp_dir().join(format!("bb-export-probes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for name in [
            "rune-capture.jsonl",
            "pickup-notification-capture.jsonl",
            "boss-flag-census.jsonl",
            crate::health::HEALTH_FILE_NAME,
        ] {
            std::fs::write(root.join(name), name).unwrap();
        }
        std::fs::write(root.join("unrelated.txt"), "no").unwrap();
        let files = collect_capture_files(&root.join("ledger.json")).unwrap();
        assert_eq!(files.len(), 4);
        assert!(!files.contains_key("unrelated.txt"));
        let _ = std::fs::remove_dir_all(root);
    }
    use crate::backend::{
        EquipRequest, ItemGrant, LocationContext, MockBackend, OperationProgress, StackObservation,
    };
    use crate::config::{
        Category8AwardBinding, DescriptorEvidence, FeedEffectBinding, LocationBinding,
        RuntimeItemBinding, TEST_PEBBLE_EVENT_FLAG,
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

    fn equipment(category: u8, normalized_item_id: u32) -> RuntimeItemBinding {
        RuntimeItemBinding {
            raw_descriptor: 0x8000_0000 | normalized_item_id,
            normalized_item_id,
            item_category: category,
            descriptor_evidence: DescriptorEvidence::ParamIdInferred,
            quantity: 1,
            reinforcement_level: Some(0),
            feed_effect: FeedEffectBinding::NotEquippable,
        }
    }

    fn config() -> RuntimeConfig {
        RuntimeConfig {
            shad_log: None,
            locations: vec![LocationBinding {
                ap_location_id: 1000,
                event_flag: TEST_PEBBLE_EVENT_FLAG,
                vanilla_award_suppressed: false,
                region: None,
            }],
            items: HashMap::from([(2000, goods())]),
            category8_awards: HashMap::new(),
            auto_upgrade: false,
            auto_equip: false,
            death_link: false,
            pickup_notification_probe: false,
            boss_flag_census: false,
            rune_capture: false,
            insight_probe: false,
            readiness_durations: false,
            death_link_amnesty: 0,
            expected_save_identity: Some("mock-save".into()),
            suppression_manifest: None,
            installed_gameparam: None,
            suppression: crate::config::SuppressionRequirement::default(),
            location_check_debounce: 3,
            mock_set_flags: vec![],
            goal_location: None,
            sustain_item: None,
        }
    }

    fn category8_config() -> RuntimeConfig {
        let mut cfg = config();
        cfg.items.insert(
            2900,
            RuntimeItemBinding {
                raw_descriptor: 0xB000_2648,
                normalized_item_id: 0x4000_2648,
                item_category: 4,
                descriptor_evidence: DescriptorEvidence::GoodsFormulaObserved,
                quantity: 1,
                reinforcement_level: None,
                feed_effect: FeedEffectBinding::NotEquippable,
            },
        );
        cfg.category8_awards.insert(
            2900,
            Category8AwardBinding {
                item_key: "caryll_rune_communion_1".into(),
                token_goods_id: 9800,
                item_lot_id: 98_000_000,
                gemgen_id: 102_901,
                ack_flag: 98_001_000,
                source_lot_id: 2_400_640,
            },
        );
        cfg
    }

    #[test]
    fn category8_delivery_waits_for_event_ack_and_consumed_token() {
        let ledger_path = path();
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2900,
        }];
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            category8_config(),
        );

        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        assert_eq!(client.backend().grants.len(), 1);
        assert_eq!(
            client.backend().inventory.get(&(0x4000_2648, None)),
            Some(&1),
        );

        client
            .backend_mut()
            .inventory
            .insert((0x4000_2648, None), 0);
        client.backend_mut().set_flags.insert(98_001_000);
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem {
                index: 0,
                ap_item_id: 2900,
                ..
            })
        ));
        assert_eq!(client.backend().grants.len(), 1);
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn category8_stale_ack_from_previous_seed_does_not_skip_delivery() {
        let ledger_path = path();
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2900,
        }];
        let mut backend = MockBackend::default();
        backend.set_flags.insert(98_001_000);
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            category8_config(),
        );
        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        assert_eq!(client.backend().grants.len(), 1);
        assert_eq!(
            client.backend().inventory.get(&(0x4000_2648, None)),
            Some(&1),
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// clients#617: a category-8 grant that lands in storage must be named
    /// as such -- and only when the backend actually observed that, never as
    /// the default guess.
    #[test]
    fn category8_stall_diagnosis_names_storage_only_when_observed() {
        let ledger_path = path();
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2900,
        }];
        let mut backend = MockBackend::default();
        backend.route_grant_to_storage(grant_tag(0));
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            category8_config(),
        );
        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        let diagnosis = client.pending_diagnosis().unwrap();
        assert!(
            diagnosis.contains("storage box"),
            "expected the storage case, got: {diagnosis}"
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// clients#618: a category-8 token whose native grant completed but was
    /// never consumed (a reload to an older save is the usual cause) leaves
    /// the ledger's `pending` set forever. Once the operator has proven the
    /// token absent from BOTH held inventory and the storage box, `reissue`
    /// resets the stuck plan so the ordinary ordered pipeline re-grants it --
    /// and never leaves a second token behind.
    fn stuck_pending_ledger() -> ReceiveLedger {
        let mut ledger = ReceiveLedger::default();
        let slot = ledger.slot_mut("seed", "slot");
        slot.begin(PendingItem {
            index: 0,
            ap_item_id: 2000,
            raw_descriptor: 0xB000_04CE,
            normalized_item_id: 0x4000_04CE,
            item_category: 4,
            quantity: 1,
            reinforcement_level: None,
            equip_target: None,
            upgrade_target_level: None,
            grant_complete: true,
            equip_complete: false,
            observed_before: Some(0),
            token_routed_to_storage: false,
        })
        .unwrap();
        ledger
    }

    fn gameplay_backend() -> MockBackend {
        let mut backend = MockBackend::default();
        backend.location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: true,
        });
        backend
    }

    #[test]
    fn rescue_reissue_pending_resets_a_lost_token_confirmed_absent_everywhere() {
        let ledger_path = path();
        let mut backend = gameplay_backend();
        backend.storage_observation_supported = true;
        let mut client = loop_with(
            backend,
            stuck_pending_ledger(),
            ledger_path.clone(),
            config(),
        );

        client.rescue_reissue_pending(0).unwrap();

        let slot = client.ledger().slot("seed", "slot").unwrap();
        assert!(slot.pending.is_none());
        assert!(slot.redeliver.contains(&0));
        assert_eq!(slot.operator_actions.last().unwrap().command, "reissue");
        assert_eq!(slot.operator_actions.last().unwrap().argument, 2000);

        let reloaded = ReceiveLedger::load(&ledger_path).unwrap();
        assert!(reloaded.slot("seed", "slot").unwrap().pending.is_none());
        assert!(
            reloaded
                .slot("seed", "slot")
                .unwrap()
                .redeliver
                .contains(&0)
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// A grant that did NOT land in storage must never be told to withdraw
    /// from storage -- the false positive this issue exists to close.
    #[test]
    fn category8_stall_diagnosis_does_not_claim_storage_when_token_is_held() {
        let ledger_path = path();
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2900,
        }];
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            category8_config(),
        );
        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        // Token was granted and is held (clients#617 fixture default);
        // ack flag is still unset.
        assert_eq!(
            client.backend().inventory.get(&(0x4000_2648, None)),
            Some(&1)
        );
        let diagnosis = client.pending_diagnosis().unwrap();
        assert!(
            !diagnosis.contains("storage box"),
            "must not guess storage without evidence, got: {diagnosis}"
        );
        assert!(
            diagnosis.contains("has not consumed it yet"),
            "expected the generic waiting case, got: {diagnosis}"
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn rescue_reissue_pending_refuses_when_the_token_is_held() {
        let ledger_path = path();
        let mut backend = gameplay_backend();
        backend.storage_observation_supported = true;
        backend.inventory.insert((0x4000_04CE, None), 1);
        let mut client = loop_with(
            backend,
            stuck_pending_ledger(),
            ledger_path.clone(),
            config(),
        );

        let error = client.rescue_reissue_pending(0).unwrap_err();
        assert!(error.to_string().contains("held in inventory"), "{error}");
        // Refused: nothing about the stuck plan moved.
        assert!(
            client
                .ledger()
                .slot("seed", "slot")
                .unwrap()
                .pending
                .is_some()
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// The ack flag reading true means the award already landed; the
    /// diagnosis must say so rather than repeat the generic "not consumed"
    /// line.
    #[test]
    fn category8_stall_diagnosis_recognizes_a_true_ack_flag_as_landed() {
        let ledger_path = path();
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2900,
        }];
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            category8_config(),
        );
        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        client.backend_mut().set_flags.insert(98_001_000);
        let diagnosis = client.pending_diagnosis().unwrap();
        assert!(
            diagnosis.contains("award landed"),
            "expected the ack-on case, got: {diagnosis}"
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn rescue_reissue_pending_refuses_when_the_token_is_in_storage() {
        let ledger_path = path();
        let mut backend = gameplay_backend();
        backend.storage_observation_supported = true;
        backend.storage.insert((0x4000_04CE, None), 1);
        let mut client = loop_with(
            backend,
            stuck_pending_ledger(),
            ledger_path.clone(),
            config(),
        );

        let error = client.rescue_reissue_pending(0).unwrap_err();
        assert!(error.to_string().contains("storage box"), "{error}");
        assert!(
            client
                .ledger()
                .slot("seed", "slot")
                .unwrap()
                .pending
                .is_some()
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// A false negative in the other direction: the token absent AND the ack
    /// flag off means the token is genuinely gone (e.g. lost to a reload),
    /// and the diagnosis must point at reissue rather than at storage.
    #[test]
    fn category8_stall_diagnosis_reports_a_token_gone_when_absent_and_unacked() {
        let ledger_path = path();
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2900,
        }];
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            category8_config(),
        );
        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        // The token disappeared without the game ever raising the ack flag --
        // clients#617's "reload to an older save" case.
        client
            .backend_mut()
            .inventory
            .insert((0x4000_2648, None), 0);
        let diagnosis = client.pending_diagnosis().unwrap();
        assert!(
            !diagnosis.contains("storage box"),
            "must not claim storage for a token that is gone, got: {diagnosis}"
        );
        assert!(
            diagnosis.contains("reissue"),
            "expected the token-gone case to point at reissue, got: {diagnosis}"
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn rescue_reissue_pending_refuses_when_storage_cannot_be_confirmed() {
        // No native storage-box accessor is wired yet (the pre-clients#618
        // status quo): `Unsupported` must fail closed rather than assume
        // absence, or a token quietly sitting in storage gets duplicated.
        let ledger_path = path();
        let backend = gameplay_backend();
        let mut client = loop_with(
            backend,
            stuck_pending_ledger(),
            ledger_path.clone(),
            config(),
        );

        let error = client.rescue_reissue_pending(0).unwrap_err();
        assert!(
            error.to_string().contains("cannot read the storage box"),
            "{error}"
        );
        assert!(
            client
                .ledger()
                .slot("seed", "slot")
                .unwrap()
                .pending
                .is_some()
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn rescue_reissue_pending_refuses_before_the_grant_completes() {
        let ledger_path = path();
        let mut backend = gameplay_backend();
        backend.storage_observation_supported = true;
        let mut ledger = ReceiveLedger::default();
        ledger
            .slot_mut("seed", "slot")
            .begin(PendingItem {
                index: 0,
                ap_item_id: 2000,
                raw_descriptor: 0xB000_04CE,
                normalized_item_id: 0x4000_04CE,
                item_category: 4,
                quantity: 1,
                reinforcement_level: None,
                equip_target: None,
                upgrade_target_level: None,
                grant_complete: false,
                equip_complete: false,
                observed_before: None,
                token_routed_to_storage: false,
            })
            .unwrap();
        let mut client = loop_with(backend, ledger, ledger_path.clone(), config());

        let error = client.rescue_reissue_pending(0).unwrap_err();
        assert!(
            error.to_string().contains("has not completed its grant"),
            "{error}"
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn rescue_reissue_pending_refuses_without_gameplay_context() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: false,
        });
        let mut client = loop_with(
            backend,
            stuck_pending_ledger(),
            ledger_path.clone(),
            config(),
        );

        assert!(
            client
                .rescue_reissue_pending(0)
                .unwrap_err()
                .to_string()
                .contains("waiting for gameplay")
        );
        // No successful save ever happened here, so the file may not exist.
        let _ = std::fs::remove_file(ledger_path);
    }

    /// Production-loop regression backend for clients#291. Only the target
    /// census differs from `MockBackend`: it resolves actual Bloodborne
    /// EquipParamWeapon ids through the same classifier as the native guest.
    struct InventoryTargetBackend {
        inner: MockBackend,
        held_weapon_ids: Vec<u32>,
    }

    impl BloodborneBackend for InventoryTargetBackend {
        fn location_context(&mut self) -> Result<Option<LocationContext>> {
            self.inner.location_context()
        }

        fn read_event_flag(&mut self, event_flag: u32) -> Result<Option<bool>> {
            self.inner.read_event_flag(event_flag)
        }

        fn target_weapon_level(&mut self) -> Result<Option<u8>> {
            Ok(self
                .held_weapon_ids
                .iter()
                .filter_map(|id| crate::native::guest::weapon_reinforcement_level(*id))
                .max())
        }

        fn grant_item(&mut self, grant: &ItemGrant) -> Result<OperationProgress> {
            self.inner.grant_item(grant)
        }

        fn observe_stack_quantity(
            &mut self,
            normalized_item_id: u32,
            reinforcement_level: Option<u8>,
        ) -> Result<StackObservation> {
            self.inner
                .observe_stack_quantity(normalized_item_id, reinforcement_level)
        }

        fn grant_may_have_applied(&mut self, tag: &str) -> Result<bool> {
            self.inner.grant_may_have_applied(tag)
        }

        fn equip_item(&mut self, request: &EquipRequest) -> Result<OperationProgress> {
            self.inner.equip_item(request)
        }

        fn death_link_kill(&mut self) -> Result<bool> {
            self.inner.death_link_kill()
        }

        fn withdraw_unwitnessed_grant(&mut self, tag: &str) -> Result<bool> {
            self.inner.withdraw_unwitnessed_grant(tag)
        }

        fn retire_grant(&mut self, tag: &str, reason: &str) -> Result<bool> {
            self.inner.retire_grant(tag, reason)
        }

        fn read_save_watermark(&mut self) -> Result<Option<u64>> {
            self.inner.read_save_watermark()
        }

        fn write_save_watermark(&mut self, cursor: u64) -> Result<bool> {
            self.inner.write_save_watermark(cursor)
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
    fn rescue_setflag_is_context_and_contract_bounded_and_exported() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.event_flags_armed = true;
        backend.location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: true,
        });
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        assert!(client.rescue_set_flag(999_999, "bad").is_err());
        assert_eq!(
            client
                .rescue_set_flag(TEST_PEBBLE_EVENT_FLAG, "Pebble")
                .unwrap(),
            1000
        );
        assert!(client.backend.set_flags.contains(&TEST_PEBBLE_EVENT_FLAG));
        let export = client.rescue_export().unwrap();
        let value: json::Value = json::from_slice(&std::fs::read(&export).unwrap()).unwrap();
        assert_eq!(value["operator_actions"][0]["resolved_name"], "Pebble");
        std::fs::remove_file(export).unwrap();
    }

    #[test]
    fn rescue_mutations_refuse_without_gameplay_context() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.event_flags_armed = true;
        backend.location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: false,
        });
        let mut client = loop_with(backend, ReceiveLedger::default(), ledger_path, config());
        assert!(
            client
                .rescue_set_flag(TEST_PEBBLE_EVENT_FLAG, "Pebble")
                .unwrap_err()
                .to_string()
                .contains("waiting for gameplay")
        );
        assert!(client.rescue_give(2000, "Pebble").is_err());
    }

    #[test]
    fn operator_give_is_durable_and_never_touches_the_ap_cursor_or_double_grants() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.event_flags_armed = true;
        backend.location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: true,
        });
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        assert!(client.rescue_give(2000, "Pebble").unwrap());
        assert_eq!(
            client.poll_operator_grant().unwrap(),
            OperatorGrantPoll::Completed(2000)
        );
        let slot = client.ledger().slot("seed", "slot").unwrap();
        assert_eq!(slot.highest_processed_index, None);
        assert!(slot.operator_grants[&2000].grant_complete);
        assert_eq!(client.backend.grants.len(), 1);
        assert!(!client.rescue_give(2000, "Pebble").unwrap());
        assert_eq!(
            client.poll_operator_grant().unwrap(),
            OperatorGrantPoll::Idle
        );
        assert_eq!(client.backend.grants.len(), 1);

        let reloaded = ReceiveLedger::load(&ledger_path).unwrap();
        assert!(reloaded.slot("seed", "slot").unwrap().operator_grants[&2000].grant_complete);
        assert_eq!(
            reloaded.slot("seed", "slot").unwrap().operator_actions[0].command,
            "give"
        );
        assert_eq!(
            reloaded
                .slot("seed", "slot")
                .unwrap()
                .highest_processed_index,
            None
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn equipment_census_queues_only_known_weapons_and_attire_and_is_idempotent() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.event_flags_armed = true;
        backend.location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: true,
        });
        let mut cfg = config();
        cfg.items.insert(2001, equipment(0, 7_100_000));
        cfg.items.insert(2002, equipment(1, 1_000_000));
        cfg.items.insert(
            2003,
            RuntimeItemBinding {
                descriptor_evidence: DescriptorEvidence::Unknown("unverified".into()),
                ..equipment(0, 7_200_000)
            },
        );
        let mut client = loop_with(backend, ReceiveLedger::default(), ledger_path.clone(), cfg);

        assert_eq!(
            client
                .rescue_equipment_census(|id| format!("item-{id}"))
                .unwrap(),
            (2, 0)
        );
        let slot = client.ledger().slot("seed", "slot").unwrap();
        assert_eq!(slot.operator_grants.len(), 2);
        assert!(slot.operator_grants.contains_key(&2001));
        assert!(slot.operator_grants.contains_key(&2002));
        assert!(!slot.operator_grants.contains_key(&2000));
        assert!(!slot.operator_grants.contains_key(&2003));
        assert!(
            slot.operator_actions
                .iter()
                .all(|action| action.command == "census")
        );

        assert_eq!(
            client
                .rescue_equipment_census(|id| format!("item-{id}"))
                .unwrap(),
            (0, 2)
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn operator_give_advances_and_retires_a_wedged_sustain_before_rescue() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.locations[0].vanilla_award_suppressed = true;
        let mut backend = MockBackend::default();
        backend.event_flags_armed = true;
        backend.location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: true,
        });
        backend.keep_grant_pending("sustain_1000");
        let mut client = loop_with(backend, ReceiveLedger::default(), ledger_path.clone(), cfg);
        client.queue_sustain_for_checks(&[1000]).unwrap();
        assert_eq!(client.poll_sustain().unwrap(), SustainPollResult::Pending);
        assert!(client.rescue_give(2000, "Pebble").unwrap());

        for _ in 1..(SUSTAIN_PENDING_POLL_LIMIT - 1) {
            assert_eq!(
                client.poll_operator_grant().unwrap(),
                OperatorGrantPoll::Pending
            );
        }
        assert_eq!(
            client.poll_operator_grant().unwrap(),
            OperatorGrantPoll::Completed(2000)
        );
        assert_eq!(client.backend().withdrawn, vec!["sustain_1000"]);
        assert_eq!(client.backend().grants.len(), 1);
        assert_eq!(client.backend().grants[0].tag, "operator_grant_2000");
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// Review finding C1: the operator lane used to report `Pending` while an
    /// AP plan was durable, which made the caller skip `poll_items` -- the only
    /// code that ever clears that plan. Both lanes then waited on each other
    /// forever, across restarts. Now the AP lane keeps advancing and the
    /// rescue takes the native lane on the next poll after the plan retires.
    #[test]
    fn an_operator_grant_queued_while_an_ap_item_is_pending_lets_both_complete() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.items.insert(
            2001,
            RuntimeItemBinding {
                raw_descriptor: 0xB000_04D2,
                normalized_item_id: 0x4000_04D2,
                ..goods()
            },
        );
        let mut backend = MockBackend::default();
        // The AP grant spans two polls, exactly as a native delivery does.
        backend.delay_grant("ap_0", 1);
        let mut client = loop_with(backend, ReceiveLedger::default(), ledger_path.clone(), cfg);
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2000,
        }];

        assert_eq!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Pending
        );
        assert!(
            client
                .ledger()
                .slot("seed", "slot")
                .unwrap()
                .pending
                .is_some(),
            "the AP plan must be durable for this regression to bite"
        );
        assert!(client.rescue_give(2001, "Pebble").unwrap());

        // The rescue defers to the in-flight AP plan without holding the lane.
        assert_eq!(
            client.poll_operator_grant().unwrap(),
            OperatorGrantPoll::WaitingForItems
        );
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 0, .. })
        ));
        assert!(
            client
                .ledger()
                .slot("seed", "slot")
                .unwrap()
                .pending
                .is_none()
        );

        // With the plan retired the rescue takes priority immediately.
        assert_eq!(
            client.poll_operator_grant().unwrap(),
            OperatorGrantPoll::Completed(2001)
        );
        let tags = client
            .backend()
            .grants
            .iter()
            .map(|grant| grant.tag.clone())
            .collect::<Vec<_>>();
        assert_eq!(tags, vec!["ap_0".to_string(), "operator_grant_2001".into()]);
        std::fs::remove_file(ledger_path).unwrap();
    }

    /// Review finding C3: the harness latches its terminal verdict, so
    /// propagating it re-raised the same error on every 50 ms poll, held the
    /// operator lane, and stopped AP delivery for the session while flooding
    /// the console. The grant is parked durably instead.
    #[test]
    fn a_terminally_failed_operator_grant_parks_and_the_next_ap_item_delivers() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.items.insert(
            2001,
            RuntimeItemBinding {
                raw_descriptor: 0xB000_04D2,
                normalized_item_id: 0x4000_04D2,
                ..goods()
            },
        );
        let mut backend = MockBackend::default();
        backend.fail_grant_terminally_with("operator_grant_2001", "quantity_mismatch");
        let mut client = loop_with(backend, ReceiveLedger::default(), ledger_path.clone(), cfg);
        assert!(client.rescue_give(2001, "Pebble").unwrap());

        let parked = client.poll_operator_grant().unwrap();
        let OperatorGrantPoll::Parked {
            ap_item_id,
            status,
            detail,
        } = parked
        else {
            panic!("expected the terminal failure to park, got {parked:?}");
        };
        assert_eq!(ap_item_id, 2001);
        assert_eq!(status, "quantity_mismatch");
        assert_eq!(detail, "mock terminal harness failure");

        // The lane is released: the very next poll is idle, not another raise.
        assert_eq!(
            client.poll_operator_grant().unwrap(),
            OperatorGrantPoll::Idle
        );
        assert!(
            client.rescue_list_blocked().contains("quantity_mismatch"),
            "the operator must be able to see the park: {}",
            client.rescue_list_blocked()
        );
        // A retyped give is a fixed point: the failed command may have applied.
        assert!(!client.rescue_give(2001, "Pebble").unwrap());

        // AP delivery keeps running behind the parked rescue.
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2000,
        }];
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 0, .. })
        ));

        let reloaded = ReceiveLedger::load(&ledger_path).unwrap();
        let slot = reloaded.slot("seed", "slot").unwrap();
        assert!(
            !slot.operator_grants.contains_key(&2001),
            "the parked row must leave the active lane"
        );
        assert_eq!(
            slot.operator_grant_parks.get(&2001).map(String::as_str),
            Some("quantity_mismatch (mock terminal harness failure)")
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn operator_give_refuses_an_out_of_contract_item() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.event_flags_armed = true;
        backend.location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: true,
        });
        let mut client = loop_with(backend, ReceiveLedger::default(), ledger_path, config());
        assert!(client.rescue_give(9_999, "bad").is_err());
    }

    #[test]
    fn operator_give_does_not_duplicate_an_ap_delivered_item() {
        let ledger_path = path();
        let mut ledger = ReceiveLedger::default();
        ledger.slot_mut("seed", "slot").acknowledged.insert(
            0,
            AcknowledgedItem {
                ap_item_id: 2000,
                raw_descriptor: goods().raw_descriptor,
                normalized_item_id: goods().normalized_item_id,
                item_category: 4,
                quantity: 1,
                reinforcement_level: None,
                equip_target: None,
                blocked: None,
            },
        );
        let mut backend = MockBackend::default();
        backend.event_flags_armed = true;
        backend.location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: true,
        });
        let mut client = loop_with(backend, ledger, ledger_path, config());
        assert!(!client.rescue_give(2000, "Pebble").unwrap());
        assert_eq!(
            client.poll_operator_grant().unwrap(),
            OperatorGrantPoll::Idle
        );
        assert!(client.backend.grants.is_empty());
    }

    fn recipe_config() -> RuntimeConfig {
        let mut config = config();
        config.locations.push(LocationBinding {
            ap_location_id: 1001,
            event_flag: 12_401_898,
            vanilla_award_suppressed: false,
            region: None,
        });
        config.locations.push(LocationBinding {
            ap_location_id: 1002,
            event_flag: 12_101_850,
            vanilla_award_suppressed: false,
            region: None,
        });
        config.goal_location = Some(1002);
        config.items.insert(
            2001,
            RuntimeItemBinding {
                raw_descriptor: 12_401_803,
                normalized_item_id: 12_401_803,
                item_category: 255,
                descriptor_evidence: DescriptorEvidence::EventFlagEffect,
                quantity: 1,
                reinforcement_level: None,
                feed_effect: FeedEffectBinding::NotEquippable,
            },
        );
        config
    }

    fn ready_backend() -> MockBackend {
        let mut backend = MockBackend::default();
        backend.event_flags_armed = true;
        backend.location_context = Some(LocationContext {
            save_identity: "mock-save".into(),
            gameplay_ready: true,
        });
        backend
    }

    #[test]
    fn rescue_recipes_reuse_the_audited_primitives_and_are_exported() {
        let ledger_path = path();
        let mut client = loop_with(
            ready_backend(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            recipe_config(),
        );
        let lines = client
            .rescue_recipe(
                "Laurence-Skull",
                |id| format!("item {id}"),
                |id| format!("loc {id}"),
            )
            .unwrap();
        assert!(client.backend.set_flags.contains(&12_401_898));
        assert!(!client.backend.set_flags.contains(&12_401_803));
        assert!(lines[0].starts_with("AUDIT rescue laurence-skull setflag flag=12401898"));
        assert!(
            lines
                .last()
                .unwrap()
                .starts_with("AUDIT rescue laurence-skull complete")
        );

        let lines = client
            .rescue_recipe(
                "forbidden-woods-password",
                |id| format!("item {id}"),
                |id| format!("loc {id}"),
            )
            .unwrap();
        assert!(lines[0].contains("give index=2001"));
        let slot = client.ledger.slot("seed", "slot").unwrap();
        assert!(slot.operator_grants.contains_key(&2001));
        assert_eq!(slot.highest_processed_index, None);
        // Repeating is a fixed point on the operator lane.
        let lines = client
            .rescue_recipe(
                "forbidden-woods-password",
                |id| format!("item {id}"),
                |id| format!("loc {id}"),
            )
            .unwrap();
        assert!(lines[0].contains("already recorded"));

        let lines = client
            .rescue_recipe("goal", |id| format!("item {id}"), |id| format!("loc {id}"))
            .unwrap();
        assert!(lines[0].contains("flag=12101850"));
        assert!(client.backend.set_flags.contains(&12_101_850));

        let export = client.rescue_export().unwrap();
        let value: json::Value = json::from_slice(&std::fs::read(&export).unwrap()).unwrap();
        let commands: Vec<&str> = value["operator_actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|action| action["command"].as_str().unwrap())
            .collect();
        assert_eq!(
            commands,
            [
                "setflag", "rescue", "give", "rescue", "rescue", "setflag", "rescue"
            ]
        );
        std::fs::remove_file(export).unwrap();
        let _ = std::fs::remove_file(ledger_path);
    }

    #[test]
    fn rescue_recipe_refuses_unknown_names_and_mutates_nothing_when_a_step_is_off_contract() {
        let ledger_path = path();
        // Base config has neither the Laurence witness nor a goal location.
        let mut client = loop_with(
            ready_backend(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        let error = client
            .rescue_recipe("nope", |id| format!("{id}"), |id| format!("{id}"))
            .unwrap_err();
        assert!(error.to_string().contains("laurence-skull"));
        assert!(
            client
                .rescue_recipe("laurence-skull", |id| format!("{id}"), |id| format!("{id}"))
                .is_err()
        );
        assert!(
            client
                .rescue_recipe("goal", |id| format!("{id}"), |id| format!("{id}"))
                .is_err()
        );
        assert!(
            client
                .rescue_recipe(
                    "forbidden-woods-password",
                    |id| format!("{id}"),
                    |id| format!("{id}")
                )
                .is_err()
        );
        assert!(client.backend.set_flags.is_empty());
        assert!(client.ledger.slot("seed", "slot").is_none_or(|slot| {
            slot.operator_grants.is_empty() && slot.operator_actions.is_empty()
        }));
        let _ = std::fs::remove_file(ledger_path);
    }

    #[test]
    fn a_pristine_binding_follows_the_loaded_character_and_becomes_final_after_delivery() {
        let ledger_path = path();
        let mut backend = ready_backend();
        backend.location_context = Some(LocationContext {
            save_identity: "shad-save-slot:0008".into(),
            gameplay_ready: true,
        });
        let mut config = recipe_config();
        config.expected_save_identity = None;
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config,
        );
        // The fresh-profile initialisation write binds first.
        client.rescue_read_flag(TEST_PEBBLE_EVENT_FLAG).unwrap();
        assert_eq!(
            client
                .ledger
                .slot("seed", "slot")
                .unwrap()
                .bound_save_identity
                .as_deref(),
            Some("shad-save-slot:0008")
        );
        // The player's actual character appears: no refusal, the binding moves.
        client.backend.location_context = Some(LocationContext {
            save_identity: "shad-save-slot:0000".into(),
            gameplay_ready: true,
        });
        client.rescue_read_flag(TEST_PEBBLE_EVENT_FLAG).unwrap();
        let slot = client.ledger.slot("seed", "slot").unwrap();
        assert_eq!(
            slot.bound_save_identity.as_deref(),
            Some("shad-save-slot:0000")
        );
        assert_eq!(slot.operator_actions.len(), 1);
        assert_eq!(slot.operator_actions[0].command, "rebind-auto");

        // Once an item has reached this character, a switch is refused.
        client.ledger.slot_mut("seed", "slot").acknowledged.insert(
            0,
            AcknowledgedItem {
                ap_item_id: 2000,
                raw_descriptor: goods().raw_descriptor,
                normalized_item_id: goods().normalized_item_id,
                item_category: 4,
                quantity: 1,
                reinforcement_level: None,
                equip_target: None,
                blocked: None,
            },
        );
        client.backend.location_context = Some(LocationContext {
            save_identity: "shad-save-slot:0003".into(),
            gameplay_ready: true,
        });
        let refused = client.rescue_read_flag(TEST_PEBBLE_EVENT_FLAG).unwrap_err();
        assert!(format!("{refused:#}").contains("durably bound"));
        assert_eq!(
            client
                .ledger
                .slot("seed", "slot")
                .unwrap()
                .bound_save_identity
                .as_deref(),
            Some("shad-save-slot:0000")
        );
        let _ = std::fs::remove_file(ledger_path);
    }

    #[test]
    fn rescue_rebind_releases_a_pristine_binding_and_refuses_after_delivery() {
        let ledger_path = path();
        let mut backend = ready_backend();
        backend.location_context = Some(LocationContext {
            save_identity: "shad-save-slot:0008".into(),
            gameplay_ready: true,
        });
        let mut config = recipe_config();
        config.expected_save_identity = None;
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            config,
        );
        // A read binds the first character seen.
        client.rescue_read_flag(TEST_PEBBLE_EVENT_FLAG).unwrap();
        assert_eq!(
            client
                .ledger
                .slot("seed", "slot")
                .unwrap()
                .bound_save_identity
                .as_deref(),
            Some("shad-save-slot:0008")
        );
        // The player loads the character they meant to play.
        client.backend.location_context = Some(LocationContext {
            save_identity: "shad-save-slot:0000".into(),
            gameplay_ready: true,
        });

        let message = client.rescue_rebind().unwrap();
        assert!(message.contains("shad-save-slot:0008"));
        assert!(
            client
                .ledger
                .slot("seed", "slot")
                .unwrap()
                .bound_save_identity
                .is_none()
        );
        client.rescue_read_flag(TEST_PEBBLE_EVENT_FLAG).unwrap();
        assert_eq!(
            client
                .ledger
                .slot("seed", "slot")
                .unwrap()
                .bound_save_identity
                .as_deref(),
            Some("shad-save-slot:0000")
        );
        let actions: Vec<&str> = client
            .ledger
            .slot("seed", "slot")
            .unwrap()
            .operator_actions
            .iter()
            .map(|action| action.command.as_str())
            .collect();
        assert_eq!(actions, ["rebind"]);

        // Once something has been delivered the binding is final.
        client.ledger.slot_mut("seed", "slot").acknowledged.insert(
            0,
            AcknowledgedItem {
                ap_item_id: 2000,
                raw_descriptor: goods().raw_descriptor,
                normalized_item_id: goods().normalized_item_id,
                item_category: 4,
                quantity: 1,
                reinforcement_level: None,
                equip_target: None,
                blocked: None,
            },
        );
        let refused = client.rescue_rebind().unwrap_err();
        assert!(format!("{refused:#}").contains("already been delivered"));
        assert_eq!(
            client
                .ledger
                .slot("seed", "slot")
                .unwrap()
                .bound_save_identity
                .as_deref(),
            Some("shad-save-slot:0000")
        );
        let _ = std::fs::remove_file(ledger_path);
    }

    #[test]
    fn unreviewed_attire_parks_with_its_own_reason() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.items.insert(
            3000,
            RuntimeItemBinding {
                raw_descriptor: 0x9005_10F0,
                normalized_item_id: 0x1005_10F0,
                item_category: 1,
                descriptor_evidence: DescriptorEvidence::Unknown("unreviewed_attire_332000".into()),
                quantity: 1,
                reinforcement_level: None,
                feed_effect: FeedEffectBinding::AttireHands,
            },
        );
        let mut client = loop_with(
            ready_backend(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            cfg,
        );
        let first = client
            .poll_items(&[IncomingItem {
                index: 0,
                ap_item_id: 3000,
            }])
            .unwrap();
        let ItemPollResult::Blocked(blocked) = first else {
            panic!("expected the unreviewed attire to park, got {first:?}");
        };
        assert_eq!(blocked.status, "unreviewed_attire");
        assert!(
            blocked.detail.contains("protector 332000"),
            "{}",
            blocked.detail
        );
        assert!(
            blocked.detail.contains("parked, not lost"),
            "{}",
            blocked.detail
        );
        assert!(
            !blocked.detail.contains("newer world"),
            "{}",
            blocked.detail
        );
        assert!(client.backend.grants.is_empty());
        let _ = std::fs::remove_file(ledger_path);
    }

    #[test]
    fn rescue_recipe_refuses_without_gameplay_context() {
        let ledger_path = path();
        let mut backend = ready_backend();
        backend.location_context = None;
        let mut client = loop_with(
            backend,
            ReceiveLedger::default(),
            ledger_path.clone(),
            recipe_config(),
        );
        assert!(
            client
                .rescue_recipe("laurence-skull", |id| format!("{id}"), |id| format!("{id}"))
                .is_err()
        );
        assert!(client.backend.set_flags.is_empty());
        let _ = std::fs::remove_file(ledger_path);
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
        assert_eq!(
            reloaded.requeue_fixed_cause_parks().unwrap().requeued,
            vec![0]
        );

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
            region: None,
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
    fn sustain_grant_uses_the_seed_published_descriptor_when_present() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.locations[0].vanilla_award_suppressed = true;
        cfg.sustain_item = Some(goods());
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            cfg,
        );
        client.queue_sustain_for_checks(&[1000]).unwrap();
        assert_eq!(
            client.poll_sustain().unwrap(),
            SustainPollResult::Completed(1000)
        );
        let grant = &client.backend().grants[0];
        assert_eq!(grant.raw_descriptor, goods().raw_descriptor);
        assert_eq!(grant.normalized_item_id, goods().normalized_item_id);
        assert_eq!(grant.quantity, 1);
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
        assert_eq!(grant.normalized_item_id, 0x4000_0384);
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
        backend.inventory.insert((0x4000_0384, None), 7);
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
            reloaded.backend().inventory.get(&(0x4000_0384, None)),
            Some(&8)
        );
        assert_eq!(reloaded.backend().grants.len(), 1);
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn wedged_sustain_is_bounded_and_cannot_starve_an_ap_item() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.locations[0].vanilla_award_suppressed = true;
        let mut backend = MockBackend::default();
        backend.keep_grant_pending("sustain_1000");
        let mut client = loop_with(backend, ReceiveLedger::default(), ledger_path.clone(), cfg);
        client.queue_sustain_for_checks(&[1000]).unwrap();

        // First poll records the durable baseline and publishes the sustain command.
        assert_eq!(client.poll_sustain().unwrap(), SustainPollResult::Pending);
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2000,
        }];
        for _ in 1..(SUSTAIN_PENDING_POLL_LIMIT - 1) {
            assert_eq!(
                client.poll_items(&received).unwrap(),
                ItemPollResult::Pending
            );
        }

        // The limit retires only the expendable bonus; the authoritative AP item uses the freed
        // lane in the same poll and is acknowledged normally.
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 0, .. })
        ));
        let slot = client.ledger().slot("seed", "slot").unwrap();
        assert!(!slot.pending_sustain.contains_key(&1000));
        assert!(slot.completed_sustain.contains(&1000));
        assert_eq!(client.backend().withdrawn, vec!["sustain_1000"]);
        assert_eq!(client.backend().grants.len(), 1);
        assert_eq!(client.backend().grants[0].tag, "ap_0");
        assert_eq!(
            client.take_sustain_notice(),
            Some(SustainPollResult::Retired {
                location: 1000,
                command_withdrawn: true,
                reason: "native grant timed out",
            })
        );
        assert_eq!(client.take_sustain_notice(), None);
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn published_sustain_keeps_priority_when_a_lower_location_id_is_queued() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.locations[0].vanilla_award_suppressed = true;
        let mut backend = MockBackend::default();
        backend.keep_grant_pending("sustain_1000");
        let mut client = loop_with(backend, ReceiveLedger::default(), ledger_path.clone(), cfg);
        client.queue_sustain_for_checks(&[1000]).unwrap();
        assert_eq!(client.poll_sustain().unwrap(), SustainPollResult::Pending);

        // Reproduce oz's session: a later pickup with a lower AP location id sorts ahead of the
        // command that already owns the native lane. It must not replace the active poll target.
        client
            .ledger
            .slot_mut("seed", "slot")
            .pending_sustain
            .insert(500, None);
        let received = [IncomingItem {
            index: 0,
            ap_item_id: 2000,
        }];
        for _ in 1..(SUSTAIN_PENDING_POLL_LIMIT - 1) {
            assert_eq!(
                client.poll_items(&received).unwrap(),
                ItemPollResult::Pending
            );
        }
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 0, .. })
        ));
        let slot = client.ledger().slot("seed", "slot").unwrap();
        assert!(slot.pending_sustain.contains_key(&500));
        assert!(slot.completed_sustain.contains(&1000));
        assert_eq!(client.backend().withdrawn, vec!["sustain_1000"]);
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn terminal_sustain_failure_is_retired_without_blocking_the_next_item() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.locations[0].vanilla_award_suppressed = true;
        let mut backend = MockBackend::default();
        backend.fail_grant_terminally_with("sustain_1000", "write_error");
        let mut client = loop_with(backend, ReceiveLedger::default(), ledger_path.clone(), cfg);
        client.queue_sustain_for_checks(&[1000]).unwrap();
        assert_eq!(
            client.poll_sustain().unwrap(),
            SustainPollResult::Retired {
                location: 1000,
                command_withdrawn: false,
                reason: "native grant failed terminally",
            }
        );
        assert!(matches!(
            client.take_sustain_notice(),
            Some(SustainPollResult::Retired { location: 1000, .. })
        ));
        assert!(matches!(
            client
                .poll_items(&[IncomingItem {
                    index: 0,
                    ap_item_id: 2000,
                }])
                .unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 0, .. })
        ));
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
        // A completed per-check bonus means something reached "first-save":
        // the binding is final, and neither config nor the loaded character
        // can move it.
        ledger
            .slot_mut("seed", "slot")
            .completed_sustain
            .insert(1000);
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
    fn first_verified_live_slot_is_trusted_once_then_the_ledger_refuses_switches() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.expected_save_identity = None;
        let mut backend = MockBackend::default();
        backend.location_context = Some(LocationContext {
            save_identity: "shad-save-slot:0004".into(),
            gameplay_ready: true,
        });
        let mut client = loop_with(backend, ReceiveLedger::default(), ledger_path.clone(), cfg);
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
            Some("shad-save-slot:0004")
        );

        client.backend_mut().location_context = Some(LocationContext {
            save_identity: "shad-save-slot:0005".into(),
            gameplay_ready: true,
        });
        let error = client.poll_locations(&HashSet::new()).unwrap_err();
        assert!(format!("{error:#}").contains("durably bound"));
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
    fn held_whirligig_plus_seven_raises_a_different_weapon_family() {
        let ledger_path = path();
        let mut runtime_config = config();
        runtime_config.auto_upgrade = true;
        runtime_config.items.insert(
            3000,
            RuntimeItemBinding {
                // Ludwig's Holy Blade +0: deliberately unrelated to Whirligig.
                raw_descriptor: 0x807B_98A0,
                normalized_item_id: 8_100_000,
                item_category: 0,
                descriptor_evidence: DescriptorEvidence::LiveGrantInventoryUi,
                quantity: 1,
                reinforcement_level: Some(0),
                feed_effect: FeedEffectBinding::RightHandWeapon,
            },
        );
        let backend = InventoryTargetBackend {
            inner: MockBackend::default(),
            held_weapon_ids: vec![31_000_700], // Whirligig Saw +7
        };
        let mut client = ClientLoop::new(
            backend,
            runtime_config,
            ReceiveLedger::default(),
            ledger_path.clone(),
            "seed",
            "slot",
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
                target_level: Some(7),
                delivered_level: Some(7),
                equip_target: None,
            })
        );
        assert_eq!(
            client.backend().inner.grants[0].reinforcement_level,
            Some(7)
        );
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

    /// Review finding C2: the equip step sits after the grant is complete and
    /// durable, and the only shipped backend's `equip_item` always errors. The
    /// error used to propagate out of `poll_items`, was not a
    /// `GrantTerminalFailure` so it never parked, and the index never advanced:
    /// one weapon wedged every later item in an auto_equip seed. It must now
    /// acknowledge the item as delivered but not equipped and carry on.
    #[test]
    fn an_equip_failure_after_a_complete_grant_still_acknowledges_and_delivers_the_next_item() {
        let ledger_path = path();
        let mut runtime_config = config();
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
                ap_item_id: 2000,
            },
        ];
        let mut backend = MockBackend::default();
        // Hold the equip for one poll so the grant is durably complete first,
        // then make the equip fail the way the native backend always does.
        backend.delay_equip("ap_0_equip", 1);
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
        assert_eq!(client.backend().grants.len(), 1);
        client
            .backend_mut()
            .inventory
            .remove(&(0x0012_3400, Some(0)));

        let ItemPollResult::Completed(completed) = client
            .poll_items(&received)
            .expect("an equip failure must not fail the poll")
        else {
            panic!("the item was not acknowledged after the equip failed");
        };
        assert_eq!(completed.index, 0);
        assert_eq!(completed.ap_item_id, 3000);
        assert!(
            client.backend().equips.is_empty(),
            "the equip failed, so nothing was equipped"
        );
        let acknowledged = ReceiveLedger::load(&ledger_path).unwrap();
        let slot = acknowledged.slot("seed", "slot").unwrap();
        assert_eq!(
            slot.acknowledged
                .get(&0)
                .and_then(|entry| entry.blocked.clone()),
            None,
            "the item is delivered, not parked: it is in the inventory"
        );

        // The stream continues: the next index delivers.
        let ItemPollResult::Completed(next) = client.poll_items(&received).unwrap() else {
            panic!("the item after a failed equip was blocked");
        };
        assert_eq!(next.index, 1);
        assert_eq!(next.ap_item_id, 2000);
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

    /// Review finding C4, the paired witness named above: a restart is the
    /// case where the delivery machine has NO process memory of the tag. The
    /// baseline lives in the ledger, not in the machine, so the reloaded loop
    /// must replay against the recorded number and must not re-sample the live
    /// stack -- the cave may already have applied the delta on the game thread
    /// before the kill, and re-sampling would grant it a second time.
    #[test]
    fn a_restart_mid_grant_replays_against_the_recorded_baseline() {
        let ledger_path = path();
        let mut backend = MockBackend::default();
        backend.inventory.insert((0x4000_04CE, None), 3);
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
        let persisted = ReceiveLedger::load(&ledger_path).unwrap();
        assert_eq!(
            persisted
                .slot("seed", "slot")
                .and_then(|slot| slot.pending.as_ref())
                .and_then(|pending| pending.observed_before),
            Some(3),
            "the baseline must be durable before the grant can execute"
        );

        // The cave applied the delta on the game thread; the client is killed
        // before it can mark the grant complete. A fresh process has no
        // `finished` entry and no session state for ap_0.
        let mut backend = MockBackend::default();
        backend.inventory.insert((0x4000_04CE, None), 5);
        let mut client = loop_with(backend, persisted, ledger_path.clone(), config());
        let error = client.poll_items(&received).unwrap_err();
        assert!(
            format!("{error:#}").contains("expected 3, found 5"),
            "the restart must replay against the recorded 3, not re-sample 5; got: {error:#}"
        );
        assert_eq!(
            client.backend().inventory[&(0x4000_04CE, None)],
            5,
            "nothing may be granted a second time on top of the applied delta"
        );
        assert_eq!(
            ReceiveLedger::load(&ledger_path)
                .unwrap()
                .slot("seed", "slot")
                .and_then(|slot| slot.pending.as_ref())
                .and_then(|pending| pending.observed_before),
            Some(3),
            "the durable baseline must not be overwritten by a fresh sample"
        );
        std::fs::remove_file(&ledger_path).unwrap();
        let _ = std::fs::remove_file(ledger_path.with_extension("bak"));
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
        assert_eq!(
            client.requeue_fixed_cause_parks().unwrap().requeued,
            vec![0]
        );
        assert!(matches!(
            client.poll_items(&received).unwrap(),
            ItemPollResult::Completed(CompletedItem { index: 0, .. })
        ));
        assert_eq!(client.backend().grants[0].expected_before, 20);
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn death_link_amnesty_zero_sends_every_qualifying_local_death() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.death_link = true;
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            cfg,
        );
        assert_eq!(
            client.record_qualifying_local_death().unwrap(),
            DeathLinkAmnestyDecision::Send
        );
        assert_eq!(
            client.record_qualifying_local_death().unwrap(),
            DeathLinkAmnestyDecision::Send
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn death_link_amnesty_cycles_and_survives_a_restart() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.death_link = true;
        cfg.death_link_amnesty = 2;
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            cfg.clone(),
        );
        assert_eq!(
            client.record_qualifying_local_death().unwrap(),
            DeathLinkAmnestyDecision::Forgiven {
                used: 1,
                allowance: 2
            }
        );

        let persisted = ReceiveLedger::load(&ledger_path).unwrap();
        let mut reloaded = loop_with(MockBackend::default(), persisted, ledger_path.clone(), cfg);
        assert_eq!(
            reloaded.record_qualifying_local_death().unwrap(),
            DeathLinkAmnestyDecision::Forgiven {
                used: 2,
                allowance: 2
            }
        );
        assert_eq!(
            reloaded.record_qualifying_local_death().unwrap(),
            DeathLinkAmnestyDecision::Send
        );
        assert_eq!(
            reloaded.record_qualifying_local_death().unwrap(),
            DeathLinkAmnestyDecision::Forgiven {
                used: 1,
                allowance: 2
            }
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn death_link_amnesty_one_forgives_then_sends() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.death_link = true;
        cfg.death_link_amnesty = 1;
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            cfg,
        );
        assert_eq!(
            client.record_qualifying_local_death().unwrap(),
            DeathLinkAmnestyDecision::Forgiven {
                used: 1,
                allowance: 1
            }
        );
        assert_eq!(
            client.record_qualifying_local_death().unwrap(),
            DeathLinkAmnestyDecision::Send
        );
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn incoming_death_link_does_not_consume_local_amnesty() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.death_link = true;
        cfg.death_link_amnesty = 1;
        let mut ledger = ReceiveLedger::default();
        ledger.slot_mut("seed", "slot").death_link_amnesty_used = 1;
        let mut client = loop_with(MockBackend::default(), ledger, ledger_path, cfg);
        client.receive_death_link().unwrap();
        assert_eq!(
            client
                .ledger()
                .slot("seed", "slot")
                .unwrap()
                .death_link_amnesty_used,
            1
        );
    }

    #[test]
    fn disabled_death_link_never_mutates_or_persists_amnesty() {
        let ledger_path = path();
        let mut cfg = config();
        cfg.death_link_amnesty = 3;
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            cfg,
        );
        assert_eq!(
            client.record_qualifying_local_death().unwrap(),
            DeathLinkAmnestyDecision::Disabled
        );
        assert!(!ledger_path.exists());
    }

    #[test]
    fn victory_is_idempotent_and_survives_restart() {
        let ledger_path = path();
        let record = VictoryRecord {
            goal_location: 12_259_363,
            goal_name: "Moon Presence".into(),
            completed_at_ms: 123,
            elapsed_seconds: Some(3_661),
            checks_completed: Some(166),
            checks_total: Some(166),
            received_items: Some(120),
            sent_items: Some(166),
            deaths: None,
            death_links: None,
        };
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        assert!(client.record_victory(record.clone()).unwrap());
        assert!(!client.record_victory(record.clone()).unwrap());

        let persisted = ReceiveLedger::load(&ledger_path).unwrap();
        let restarted = loop_with(
            MockBackend::default(),
            persisted,
            ledger_path.clone(),
            config(),
        );
        assert_eq!(restarted.victory(), Some(&record));
        std::fs::remove_file(ledger_path).unwrap();
    }

    #[test]
    fn victory_cannot_be_replaced_by_a_different_goal() {
        let ledger_path = path();
        let make = |goal_location| VictoryRecord {
            goal_location,
            goal_name: format!("goal {goal_location}"),
            completed_at_ms: 1,
            elapsed_seconds: None,
            checks_completed: None,
            checks_total: None,
            received_items: None,
            sent_items: None,
            deaths: None,
            death_links: None,
        };
        let mut client = loop_with(
            MockBackend::default(),
            ReceiveLedger::default(),
            ledger_path.clone(),
            config(),
        );
        client.record_victory(make(1)).unwrap();
        assert!(client.record_victory(make(2)).is_err());
        std::fs::remove_file(ledger_path).unwrap();
    }
}
