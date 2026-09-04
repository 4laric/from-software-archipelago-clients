use anyhow::Result;
use std::collections::{HashMap, HashSet};

use crate::feed::EquipTarget;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocationContext {
    pub save_identity: String,
    pub gameplay_ready: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemGrant {
    pub raw_descriptor: u32,
    pub normalized_item_id: u32,
    pub item_category: u8,
    pub quantity: u32,
    pub expected_before: u32,
    pub reinforcement_level: Option<u8>,
    pub tag: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EquipRequest {
    pub normalized_item_id: u32,
    pub reinforcement_level: Option<u8>,
    pub target: EquipTarget,
    pub tag: String,
}

/// What a backend can say about the live quantity of one stack *before* a
/// grant runs (clients#427). This is the fresh-grant precondition: the number
/// the delivery machine will require the stack to hold, sampled from the game
/// rather than predicted from the ledger's lifetime delivered sum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StackObservation {
    /// This backend cannot read inventory: the caller keeps its
    /// pre-clients#427 ledger-derived baseline.
    Unsupported,
    /// Inventory geometry is not hydrated yet, or an absent stack has not
    /// survived the contract's absent-poll grace. Never "zero".
    NotReady,
    /// The observed quantity of the stack right now.
    Quantity(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationProgress {
    Pending,
    Complete,
}

/// The grant harness latched a terminal failure for this tag (clients#399).
/// Distinct from every other grant error so the delivery loop can park this
/// one item and keep delivering later ones, instead of the whole stream
/// wedging behind one verdict -- including a false one, which the harness's
/// whole-inventory verify could produce before bb-archipelago#106.
#[derive(Clone, Debug)]
pub struct GrantTerminalFailure {
    pub tag: String,
    pub status: String,
    pub detail: String,
}

impl std::fmt::Display for GrantTerminalFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The same wording the ensure! produced, so log watches still match.
        write!(
            f,
            "grant {} failed in harness: {} ({})",
            self.tag, self.status, self.detail
        )
    }
}

impl std::error::Error for GrantTerminalFailure {}

pub trait BloodborneBackend {
    /// Gated category-8 construction experiment used to close bb-archipelago#214.
    /// Implementations must run on the validated game-thread lane and must not
    /// insert the constructed instance into inventory.
    fn category8_generate(&mut self, _gem_gen_param: u32) -> Result<String> {
        anyhow::bail!("category-8 construction is unavailable")
    }
    fn category8_insert(&mut self, _variant: u8) -> Result<String> {
        anyhow::bail!("category-8 insertion is unavailable")
    }
    /// Observation-only seam for clients#510. Backends without the native
    /// diagnostic intentionally ignore it.
    fn record_location_checks(&mut self, _locations: &[i64]) {}
    /// Append a human observation beside the native pickup capture. Returns
    /// false when that optional capture is not armed.
    fn record_presentation_marker(&mut self, _note: &str) -> bool {
        false
    }
    /// Returns a validated live-play/save identity, or `None` when every game
    /// read and mutation must abstain. A process handle or raw event-flag read
    /// is not enough to prove that the intended character save is loaded.
    fn location_context(&mut self) -> Result<Option<LocationContext>>;
    /// `None` means the live accessor is not available, never "flag is false".
    fn read_event_flag(&mut self, event_flag: u32) -> Result<Option<bool>>;
    /// Contract-bounded rescue write. Callers must validate membership before
    /// invoking this; unsupported backends remain fail-closed.
    fn write_event_flag(&mut self, _event_flag: u32, _enabled: bool) -> Result<()> {
        anyhow::bail!("live event-flag writer is unavailable")
    }
    /// `None` preserves the received reinforcement level.
    fn target_weapon_level(&mut self) -> Result<Option<u8>>;
    fn grant_item(&mut self, grant: &ItemGrant) -> Result<OperationProgress>;
    /// The live quantity of a stack, for the fresh-grant baseline
    /// (clients#427).
    ///
    /// Required, deliberately: this trait is implemented once more by the
    /// `Backend` enum the shipped binary dispatches through, and a defaulted
    /// method that the enum forgets to forward silently swallows every real
    /// implementation beneath it. A backend that cannot read inventory answers
    /// [`StackObservation::Unsupported`] explicitly, which keeps it on the
    /// ledger-derived baseline it always used.
    fn observe_stack_quantity(
        &mut self,
        normalized_item_id: u32,
        reinforcement_level: Option<u8>,
    ) -> Result<StackObservation>;
    /// The live quantity of a stack sitting in the Hunter's Dream storage box
    /// (clients#618), independent of held inventory: a completed grant that
    /// did not fit into held inventory routes there instead, which is what
    /// lets a delivery token go "missing" from the player's perspective while
    /// still very much existing. Backends without a storage-box accessor
    /// answer [`StackObservation::Unsupported`] explicitly -- never "zero" --
    /// so a caller trying to prove a token absent before a rescue reissue can
    /// tell "confirmed empty" apart from "cannot check yet". The default
    /// keeps every backend that has not wired a storage read fail-closed.
    fn observe_storage_quantity(
        &mut self,
        _normalized_item_id: u32,
        _reinforcement_level: Option<u8>,
    ) -> Result<StackObservation> {
        Ok(StackObservation::Unsupported)
    }
    /// Whether the command published for `tag` may already have applied to the
    /// game (clients#427 follow-up).
    ///
    /// The recorded baseline is only binding while this is true. A command the
    /// harness is merely *retaining* -- the native machine's
    /// `awaiting_inventory`, which can wait minutes for the player to acquire
    /// a stack of the item -- cannot have moved the stack, so drift from the
    /// recorded number is the player's own doing and re-observing is both safe
    /// and required. Freezing the baseline there is what re-parked oz's
    /// requeued backlog as `quantity_mismatch`.
    ///
    /// Required for the same reason as [`Self::observe_stack_quantity`]: a
    /// default here is invisible to the enum wrapper the binary dispatches
    /// through.
    fn grant_may_have_applied(&mut self, tag: &str) -> Result<bool>;
    /// Whether the most recently completed grant for `tag` was observed
    /// landing in storage rather than held inventory (clients#617). Only
    /// meaningful for a category-8 token grant that just returned
    /// `OperationProgress::Complete`. The default is `false`, which is
    /// correct both when it did not happen and when this backend has no
    /// per-grant storage evidence to offer -- either way the stall diagnosis
    /// must not claim a token is in storage without proof.
    fn last_grant_went_to_storage(&mut self, _tag: &str) -> bool {
        false
    }
    fn equip_item(&mut self, request: &EquipRequest) -> Result<OperationProgress>;
    /// Kill the loaded player for an incoming DeathLink. `false` means the
    /// validated player HP pointer is not captured/gameplay-ready yet.
    fn death_link_kill(&mut self) -> Result<bool>;
    /// Retract a published-but-unexecuted grant command (clients#296). The
    /// client calls this when the validated context the command was published
    /// under is gone -- a save switch, a non-gameplay transition, or a process
    /// restart that finds a leftover command. Returns `true` when a command
    /// was actually withdrawn; `false` when there was nothing unwitnessed to
    /// withdraw.
    fn withdraw_unwitnessed_grant(&mut self, tag: &str) -> Result<bool>;
    /// Terminally retire a best-effort grant and release the single native lane. Unlike ordinary
    /// context-loss withdrawal, this grant will not be retried. Implementations should emit their
    /// normal terminal diagnostic with `reason`. The return value says whether an unexecuted
    /// native request was physically withdrawn before retirement.
    fn retire_grant(&mut self, tag: &str, reason: &str) -> Result<bool> {
        let _ = reason;
        self.withdraw_unwitnessed_grant(tag)
    }
    /// Read the save-resident receive watermark (bb-archipelago#77). `None`
    /// means no watermark is available: either the backend has no writable
    /// save field yet (the attested-mode status quo) or a previously recorded
    /// field is unreadable -- the client distinguishes the two from its own
    /// ledger. The default keeps live builds in attested mode.
    fn read_save_watermark(&mut self) -> Result<Option<u64>> {
        Ok(None)
    }
    /// Persist the receive cursor into the save after acknowledgement.
    /// Returns `true` only when the value was actually written; the default
    /// declines, which keeps the slot in attested mode. A write failure is
    /// non-fatal: the delivery is already acknowledged in the durable ledger.
    fn write_save_watermark(&mut self, _cursor: u64) -> Result<bool> {
        Ok(false)
    }
}

#[derive(Clone)]
pub struct MockBackend {
    pub set_flags: HashSet<u32>,
    pub location_context: Option<LocationContext>,
    pub upgrade_target_level: Option<u8>,
    pub inventory: HashMap<(u32, Option<u8>), u32>,
    pub grants: Vec<ItemGrant>,
    pub equips: Vec<EquipRequest>,
    /// Tags passed to `withdraw_unwitnessed_grant`, in call order. The loop only
    /// calls withdraw when its ledger holds an ungranted pending plan, so the
    /// mock answers `true`: from the loop's perspective an unwitnessed command
    /// existed and is now withdrawn.
    pub withdrawn: Vec<String>,
    /// Whether this mock simulates a save with a writable receive watermark
    /// (bb-archipelago#77). Off by default so the pre-watermark fixtures stay
    /// in attested mode.
    pub watermark_supported: bool,
    /// The mock save's watermark field. `None` while `watermark_supported` is
    /// on simulates a recorded field that has become unreadable.
    pub watermark: Option<u64>,
    /// clients#420: whether the live event-flag accessor is armed. Off models a
    /// native attach whose flag gate is still pending because the game has not
    /// finished loading a character -- reads answer `None` ("no accessor"),
    /// never `false`.
    pub event_flags_armed: bool,
    /// clients#427: whether this mock models a backend that can read the live
    /// stack quantity. On by default -- native can, and the fresh-grant
    /// baseline is observed. Off models the CE file bridge, which falls back
    /// to the ledger-derived baseline.
    pub stack_observation_supported: bool,
    /// Off models inventory geometry that has not hydrated yet: no baseline,
    /// so no grant this poll.
    pub stack_observation_ready: bool,
    /// clients#618: the mock's storage-box contents, checked by
    /// `observe_storage_quantity`. Separate from `inventory` because a token
    /// can sit in storage while held inventory reads zero.
    pub storage: HashMap<(u32, Option<u8>), u32>,
    /// Off models a backend with no storage-box accessor at all (the
    /// pre-clients#618 status quo, and every backend that has not wired a
    /// live storage read): `observe_storage_quantity` answers `Unsupported`.
    pub storage_observation_supported: bool,
    /// clients#427 follow-up: tags whose published command this mock models as
    /// *retained but never handed to the game* -- the native machine sitting in
    /// `awaiting_inventory`. Their recorded baseline is not binding, so the
    /// next publication re-observes the live stack.
    pub retained_unwitnessed: HashSet<String>,
    /// clients#617: tags whose grant should report as having landed in
    /// storage, for exercising the stall-diagnosis storage case without a
    /// live native engine.
    pub storage_routed: HashSet<String>,
    grant_delays: HashMap<String, u8>,
    pending_grants: HashSet<String>,
    equip_delays: HashMap<String, u8>,
    completed_grants: HashSet<String>,
    completed_equips: HashSet<String>,
    terminal_failures: HashMap<String, String>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self {
            set_flags: HashSet::new(),
            location_context: Some(LocationContext {
                save_identity: "mock-save".into(),
                gameplay_ready: true,
            }),
            upgrade_target_level: None,
            inventory: HashMap::new(),
            grants: Vec::new(),
            equips: Vec::new(),
            withdrawn: Vec::new(),
            watermark_supported: false,
            watermark: None,
            event_flags_armed: true,
            stack_observation_supported: true,
            stack_observation_ready: true,
            storage: HashMap::new(),
            storage_observation_supported: false,
            retained_unwitnessed: HashSet::new(),
            storage_routed: HashSet::new(),
            grant_delays: HashMap::new(),
            pending_grants: HashSet::new(),
            equip_delays: HashMap::new(),
            completed_grants: HashSet::new(),
            completed_equips: HashSet::new(),
            terminal_failures: HashMap::new(),
        }
    }
}

impl MockBackend {
    pub fn delay_grant(&mut self, tag: impl Into<String>, polls: u8) {
        self.grant_delays.insert(tag.into(), polls);
    }

    pub fn keep_grant_pending(&mut self, tag: impl Into<String>) {
        self.pending_grants.insert(tag.into());
    }

    /// Simulate the harness latching a terminal failure for a tag (clients#399).
    pub fn fail_grant_terminally(&mut self, tag: impl Into<String>) {
        self.fail_grant_terminally_with(tag, "failed");
    }

    /// The same, with the harness status that names the park reason -- which is
    /// what decides whether clients#427's startup unpark may retry it.
    pub fn fail_grant_terminally_with(
        &mut self,
        tag: impl Into<String>,
        status: impl Into<String>,
    ) {
        self.terminal_failures.insert(tag.into(), status.into());
    }

    pub fn delay_equip(&mut self, tag: impl Into<String>, polls: u8) {
        self.equip_delays.insert(tag.into(), polls);
    }

    /// Simulate a completed grant that the native engine observed landing in
    /// storage rather than held inventory (clients#617).
    pub fn route_grant_to_storage(&mut self, tag: impl Into<String>) {
        self.storage_routed.insert(tag.into());
    }

    /// Simulate a save restore (bb-archipelago#77): the game-side state rewinds
    /// to the restored snapshot -- inventory contents, the watermark field, and
    /// the memory of which bridge commands already executed -- while the
    /// client's durable ledger is untouched. Detection of that rewind is the
    /// watermark's job; this helper only arranges the game side.
    pub fn restore_save(
        &mut self,
        watermark: Option<u64>,
        inventory: HashMap<(u32, Option<u8>), u32>,
    ) {
        self.watermark = watermark;
        self.inventory = inventory;
        self.completed_grants.clear();
        self.completed_equips.clear();
    }

    fn delayed(delays: &mut HashMap<String, u8>, tag: &str) -> bool {
        let Some(remaining) = delays.get_mut(tag) else {
            return false;
        };
        if *remaining == 0 {
            delays.remove(tag);
            return false;
        }
        *remaining -= 1;
        true
    }
}

impl BloodborneBackend for MockBackend {
    fn location_context(&mut self) -> Result<Option<LocationContext>> {
        Ok(self.location_context.clone())
    }

    fn read_event_flag(&mut self, event_flag: u32) -> Result<Option<bool>> {
        if !self.event_flags_armed {
            return Ok(None);
        }
        Ok(Some(self.set_flags.contains(&event_flag)))
    }

    fn write_event_flag(&mut self, event_flag: u32, enabled: bool) -> Result<()> {
        if !self.event_flags_armed {
            anyhow::bail!("live event-flag accessor is unavailable");
        }
        if enabled {
            self.set_flags.insert(event_flag);
        } else {
            self.set_flags.remove(&event_flag);
        }
        Ok(())
    }

    fn target_weapon_level(&mut self) -> Result<Option<u8>> {
        Ok(self.upgrade_target_level)
    }

    fn death_link_kill(&mut self) -> Result<bool> {
        Ok(self
            .location_context
            .as_ref()
            .is_some_and(|context| context.gameplay_ready))
    }

    fn observe_stack_quantity(
        &mut self,
        normalized_item_id: u32,
        reinforcement_level: Option<u8>,
    ) -> Result<StackObservation> {
        if !self.stack_observation_supported {
            return Ok(StackObservation::Unsupported);
        }
        if !self.stack_observation_ready {
            return Ok(StackObservation::NotReady);
        }
        Ok(StackObservation::Quantity(
            self.inventory
                .get(&(normalized_item_id, reinforcement_level))
                .copied()
                .unwrap_or(0),
        ))
    }

    fn observe_storage_quantity(
        &mut self,
        normalized_item_id: u32,
        reinforcement_level: Option<u8>,
    ) -> Result<StackObservation> {
        if !self.storage_observation_supported {
            return Ok(StackObservation::Unsupported);
        }
        Ok(StackObservation::Quantity(
            self.storage
                .get(&(normalized_item_id, reinforcement_level))
                .copied()
                .unwrap_or(0),
        ))
    }

    /// Review finding C4: this default (`true` for every tag the test did not
    /// explicitly mark retained) is the SAME answer the native machine gives
    /// for a tag it has no record of, which is what a restart looks like.
    /// While the native side answered `false` there, the restart tests in
    /// `client_loop` passed against a mock that disagreed with the shipped
    /// backend; keep the two aligned so those tests stay honest.
    fn grant_may_have_applied(&mut self, tag: &str) -> Result<bool> {
        Ok(!self.retained_unwitnessed.contains(tag))
    }

    fn last_grant_went_to_storage(&mut self, tag: &str) -> bool {
        self.storage_routed.contains(tag)
    }

    fn grant_item(&mut self, grant: &ItemGrant) -> Result<OperationProgress> {
        if self.pending_grants.contains(&grant.tag) {
            return Ok(OperationProgress::Pending);
        }
        if let Some(status) = self.terminal_failures.get(&grant.tag) {
            return Err(GrantTerminalFailure {
                tag: grant.tag.clone(),
                status: status.clone(),
                detail: "mock terminal harness failure".into(),
            }
            .into());
        }
        if self.completed_grants.contains(&grant.tag) {
            return Ok(OperationProgress::Complete);
        }
        if Self::delayed(&mut self.grant_delays, &grant.tag) {
            return Ok(OperationProgress::Pending);
        }
        if grant.item_category == 255 {
            self.set_flags.insert(grant.normalized_item_id);
            self.grants.push(grant.clone());
            self.completed_grants.insert(grant.tag.clone());
            return Ok(OperationProgress::Complete);
        }
        let key = (grant.normalized_item_id, grant.reinforcement_level);
        let current = self.inventory.get(&key).copied().unwrap_or(0);
        anyhow::ensure!(
            current == grant.expected_before,
            "mock quantity mismatch: expected {}, found {}",
            grant.expected_before,
            current
        );
        self.inventory
            .insert(key, current.saturating_add(grant.quantity));
        self.grants.push(grant.clone());
        self.completed_grants.insert(grant.tag.clone());
        Ok(OperationProgress::Complete)
    }

    fn equip_item(&mut self, request: &EquipRequest) -> Result<OperationProgress> {
        if self.completed_equips.contains(&request.tag) {
            return Ok(OperationProgress::Complete);
        }
        if Self::delayed(&mut self.equip_delays, &request.tag) {
            return Ok(OperationProgress::Pending);
        }
        anyhow::ensure!(
            self.inventory
                .contains_key(&(request.normalized_item_id, request.reinforcement_level)),
            "mock equip target is not present in inventory"
        );
        self.equips.push(request.clone());
        self.completed_equips.insert(request.tag.clone());
        Ok(OperationProgress::Complete)
    }

    fn withdraw_unwitnessed_grant(&mut self, tag: &str) -> Result<bool> {
        self.withdrawn.push(tag.to_owned());
        Ok(true)
    }

    fn retire_grant(&mut self, tag: &str, _reason: &str) -> Result<bool> {
        self.pending_grants.remove(tag);
        self.withdrawn.push(tag.to_owned());
        Ok(true)
    }

    fn read_save_watermark(&mut self) -> Result<Option<u64>> {
        Ok(self.watermark_supported.then_some(self.watermark).flatten())
    }

    fn write_save_watermark(&mut self, cursor: u64) -> Result<bool> {
        if !self.watermark_supported {
            return Ok(false);
        }
        self.watermark = Some(cursor);
        Ok(true)
    }
}
