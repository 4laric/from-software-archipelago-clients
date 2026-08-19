use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{Result, bail};

use crate::bridge::{FileBridge, GrantCommand};
use crate::event_flags::LiveEventFlags;
use crate::feed::EquipTarget;

const GRANT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationProgress {
    Pending,
    Complete,
}

pub trait BloodborneBackend {
    /// Returns a validated live-play/save identity, or `None` when every game
    /// read and mutation must abstain. A process handle or raw event-flag read
    /// is not enough to prove that the intended character save is loaded.
    fn location_context(&mut self) -> Result<Option<LocationContext>>;
    /// `None` means the live accessor is not available, never "flag is false".
    fn read_event_flag(&mut self, event_flag: u32) -> Result<Option<bool>>;
    /// `None` preserves the received reinforcement level.
    fn target_weapon_level(&mut self) -> Result<Option<u8>>;
    fn grant_item(&mut self, grant: &ItemGrant) -> Result<OperationProgress>;
    fn equip_item(&mut self, request: &EquipRequest) -> Result<OperationProgress>;
}

pub struct FileBackend {
    bridge: FileBridge,
    event_flags: LiveEventFlags,
}

impl FileBackend {
    pub fn new(bridge: FileBridge, event_flags: LiveEventFlags) -> Self {
        Self {
            bridge,
            event_flags,
        }
    }
}

impl BloodborneBackend for FileBackend {
    fn location_context(&mut self) -> Result<Option<LocationContext>> {
        // The direct reader validates the eboot build and re-resolves the flag
        // manager on every read, but it cannot yet identify the loaded save or
        // prove that gameplay is not transitioning. Fail closed until both are
        // available from a version-gated live accessor.
        Ok(None)
    }

    fn read_event_flag(&mut self, event_flag: u32) -> Result<Option<bool>> {
        self.event_flags.read_resilient(event_flag).map(Some)
    }

    fn target_weapon_level(&mut self) -> Result<Option<u8>> {
        // Weapon inventory/reinforcement state has not been resolved on v0.18.
        Ok(None)
    }

    fn grant_item(&mut self, grant: &ItemGrant) -> Result<OperationProgress> {
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
        if state.concerns_tag(&grant.tag) {
            if state.is_success() {
                self.bridge.acknowledge_command(&grant.tag)?;
                return Ok(OperationProgress::Complete);
            }
            anyhow::ensure!(
                !state.is_terminal_failure(),
                "grant {} failed in harness: {} ({})",
                grant.tag,
                state.status,
                state.detail
            );
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
}

pub struct MockBackend {
    pub set_flags: HashSet<u32>,
    pub location_context: Option<LocationContext>,
    pub upgrade_target_level: Option<u8>,
    pub inventory: HashMap<(u32, Option<u8>), u32>,
    pub grants: Vec<ItemGrant>,
    pub equips: Vec<EquipRequest>,
    grant_delays: HashMap<String, u8>,
    equip_delays: HashMap<String, u8>,
    completed_grants: HashSet<String>,
    completed_equips: HashSet<String>,
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
            grant_delays: HashMap::new(),
            equip_delays: HashMap::new(),
            completed_grants: HashSet::new(),
            completed_equips: HashSet::new(),
        }
    }
}

impl MockBackend {
    pub fn delay_grant(&mut self, tag: impl Into<String>, polls: u8) {
        self.grant_delays.insert(tag.into(), polls);
    }

    pub fn delay_equip(&mut self, tag: impl Into<String>, polls: u8) {
        self.equip_delays.insert(tag.into(), polls);
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
        Ok(Some(self.set_flags.contains(&event_flag)))
    }

    fn target_weapon_level(&mut self) -> Result<Option<u8>> {
        Ok(self.upgrade_target_level)
    }

    fn grant_item(&mut self, grant: &ItemGrant) -> Result<OperationProgress> {
        if self.completed_grants.contains(&grant.tag) {
            return Ok(OperationProgress::Complete);
        }
        if Self::delayed(&mut self.grant_delays, &grant.tag) {
            return Ok(OperationProgress::Pending);
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
}
