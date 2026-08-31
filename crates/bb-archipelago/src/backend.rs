use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{Result, bail};

use crate::bridge::{FileBridge, GrantCommand};
use crate::event_flags::LiveEventFlags;
use crate::feed::EquipTarget;

const GRANT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const ASSUMED_CONTEXT_STABLE_READS: u8 = 3;

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
    /// This backend cannot read inventory (the CE file bridge): the caller
    /// keeps its pre-clients#427 ledger-derived baseline.
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
    /// Observation-only seam for clients#510. Backends without the native
    /// diagnostic intentionally ignore it.
    fn record_location_checks(&mut self, _locations: &[i64]) {}
    /// Returns a validated live-play/save identity, or `None` when every game
    /// read and mutation must abstain. A process handle or raw event-flag read
    /// is not enough to prove that the intended character save is loaded.
    fn location_context(&mut self) -> Result<Option<LocationContext>>;
    /// `None` means the live accessor is not available, never "flag is false".
    fn read_event_flag(&mut self, event_flag: u32) -> Result<Option<bool>>;
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
    /// through. A backend that cannot tell (the CE file bridge) answers `true`
    /// explicitly, the pre-existing freeze-on-first-observe behaviour.
    fn grant_may_have_applied(&mut self, tag: &str) -> Result<bool>;
    fn equip_item(&mut self, request: &EquipRequest) -> Result<OperationProgress>;
    /// Kill the loaded player for an incoming DeathLink. `false` means the
    /// validated player HP pointer is not captured/gameplay-ready yet.
    fn death_link_kill(&mut self) -> Result<bool>;
    /// Retract a published-but-unexecuted grant command (clients#296). The
    /// client calls this when the validated context the command was published
    /// under is gone -- a save switch, a non-gameplay transition, or a process
    /// restart that finds a leftover command. Returns `true` when a command
    /// was actually withdrawn; `false` when there was nothing unwitnessed to
    /// withdraw. See `FileBridge::withdraw_unwitnessed_command` for what
    /// "unwitnessed" means and why witnessed commands are left alone.
    fn withdraw_unwitnessed_grant(&mut self, tag: &str) -> Result<bool>;
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

pub struct FileBackend {
    bridge: FileBridge,
    event_flags: LiveEventFlags,
    assumed_context: Option<AssumedContextGate>,
}

#[derive(Clone, Debug)]
struct AssumedContextGate {
    identity: String,
    consecutive_ready: u8,
}

impl AssumedContextGate {
    fn new(identity: String) -> Self {
        Self {
            identity,
            consecutive_ready: 0,
        }
    }

    fn observe(&mut self, ready: bool) -> LocationContext {
        if ready {
            self.consecutive_ready = self.consecutive_ready.saturating_add(1);
        } else {
            self.consecutive_ready = 0;
        }
        LocationContext {
            save_identity: self.identity.clone(),
            gameplay_ready: self.consecutive_ready >= ASSUMED_CONTEXT_STABLE_READS,
        }
    }
}

impl FileBackend {
    pub fn new(bridge: FileBridge, event_flags: LiveEventFlags) -> Self {
        Self {
            bridge,
            event_flags,
            assumed_context: None,
        }
    }

    /// Enable the explicitly unsafe vertical-slice mode. The supplied identity
    /// is an operator attestation, not a value read from the game save.
    pub fn assuming_correct_save(
        bridge: FileBridge,
        event_flags: LiveEventFlags,
        assumed_identity: String,
    ) -> Self {
        Self {
            bridge,
            event_flags,
            assumed_context: Some(AssumedContextGate::new(assumed_identity)),
        }
    }
}

impl BloodborneBackend for FileBackend {
    fn location_context(&mut self) -> Result<Option<LocationContext>> {
        let Some(gate) = self.assumed_context.as_mut() else {
            // The normal live mode remains fail-closed until a real save
            // identity accessor is available.
            return Ok(None);
        };
        if let Err(error) = self.event_flags.probe_manager_resilient() {
            gate.observe(false);
            return Err(error);
        }
        Ok(Some(gate.observe(true)))
    }

    fn read_event_flag(&mut self, event_flag: u32) -> Result<Option<bool>> {
        self.event_flags.read_resilient(event_flag).map(Some)
    }

    fn target_weapon_level(&mut self) -> Result<Option<u8>> {
        // Weapon inventory/reinforcement state has not been resolved on v0.18.
        Ok(None)
    }

    fn death_link_kill(&mut self) -> Result<bool> {
        // The legacy file bridge has no HP command. DeathLink is native-only;
        // fail closed instead of inventing an unversioned side channel.
        Ok(false)
    }

    /// The CE file bridge cannot read inventory, so the caller keeps its
    /// ledger-derived baseline (clients#427).
    fn observe_stack_quantity(
        &mut self,
        _normalized_item_id: u32,
        _reinforcement_level: Option<u8>,
    ) -> Result<StackObservation> {
        Ok(StackObservation::Unsupported)
    }

    /// The bridge cannot tell whether a published command already reached the
    /// game, so it keeps the conservative freeze-on-first-observe answer.
    fn grant_may_have_applied(&mut self, _tag: &str) -> Result<bool> {
        Ok(true)
    }

    fn grant_item(&mut self, grant: &ItemGrant) -> Result<OperationProgress> {
        if grant.item_category == 255 {
            self.event_flags
                .write_resilient(grant.normalized_item_id, true)?;
            return Ok(OperationProgress::Complete);
        }
        match grant.item_category {
            4 => anyhow::ensure!(
                grant.normalized_item_id & 0xF000_0000 == 0x4000_0000
                    && grant.raw_descriptor & 0xF000_0000 == 0xB000_0000
                    && (grant.normalized_item_id & 0x0FFF_FFFF)
                        == (grant.raw_descriptor & 0x0FFF_FFFF),
                "grant {} has an invalid category-4 raw/normalized descriptor pair",
                grant.tag
            ),
            0 => anyhow::ensure!(
                grant.normalized_item_id & 0xF000_0000 == 0
                    && grant.raw_descriptor & 0xF000_0000 == 0x8000_0000
                    && (grant.normalized_item_id & 0x0FFF_FFFF)
                        == (grant.raw_descriptor & 0x0FFF_FFFF),
                "grant {} has an invalid category-0 raw/normalized descriptor pair",
                grant.tag
            ),
            category => bail!(
                "grant {} uses unsupported Bloodborne item category {category}",
                grant.tag
            ),
        }
        let state = self.bridge.read_state()?;
        state.require_compatible()?;
        if state.concerns_tag(&grant.tag) && self.bridge.state_is_current_for(&grant.tag)? {
            if state.is_success() {
                self.bridge.acknowledge_command(&grant.tag)?;
                return Ok(OperationProgress::Complete);
            }
            if state.is_terminal_failure() {
                return Err(GrantTerminalFailure {
                    tag: grant.tag.clone(),
                    status: state.status.clone(),
                    detail: state.detail.clone(),
                }
                .into());
            }
        }
        if self.bridge.command_pending() {
            anyhow::ensure!(
                !self.bridge.command_is_stale(GRANT_COMMAND_TIMEOUT)?,
                "grant {} timed out after {} seconds; command left in place for diagnosis",
                grant.tag,
                GRANT_COMMAND_TIMEOUT.as_secs()
            );
            return Ok(OperationProgress::Pending);
        }
        self.bridge.enqueue(&GrantCommand {
            raw_id: grant.raw_descriptor,
            normalized_id: grant.normalized_item_id,
            quantity: grant.quantity,
            expected_before: None,
            tag: grant.tag.clone(),
        })?;
        Ok(OperationProgress::Pending)
    }

    fn equip_item(&mut self, request: &EquipRequest) -> Result<OperationProgress> {
        bail!(
            "live auto-equip is not armed for {:?}; item {} remains durably pending",
            request.target,
            request.tag
        )
    }

    fn withdraw_unwitnessed_grant(&mut self, tag: &str) -> Result<bool> {
        self.bridge.withdraw_unwitnessed_command(tag)
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
    /// clients#427 follow-up: tags whose published command this mock models as
    /// *retained but never handed to the game* -- the native machine sitting in
    /// `awaiting_inventory`. Their recorded baseline is not binding, so the
    /// next publication re-observes the live stack.
    pub retained_unwitnessed: HashSet<String>,
    grant_delays: HashMap<String, u8>,
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
            retained_unwitnessed: HashSet::new(),
            grant_delays: HashMap::new(),
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

    fn grant_may_have_applied(&mut self, tag: &str) -> Result<bool> {
        Ok(!self.retained_unwitnessed.contains(tag))
    }

    fn grant_item(&mut self, grant: &ItemGrant) -> Result<OperationProgress> {
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

#[cfg(test)]
mod assumed_context_tests {
    use super::*;

    #[test]
    fn assumed_context_arms_after_three_reads_and_disarms_immediately() {
        let mut gate = AssumedContextGate::new("unsafe-test".into());
        assert!(!gate.observe(true).gameplay_ready);
        assert!(!gate.observe(true).gameplay_ready);
        assert!(gate.observe(true).gameplay_ready);
        assert!(!gate.observe(false).gameplay_ready);
        assert!(!gate.observe(true).gameplay_ready);
        assert!(!gate.observe(true).gameplay_ready);
        assert!(gate.observe(true).gameplay_ready);
    }
}
