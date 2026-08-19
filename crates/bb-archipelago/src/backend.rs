use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::bridge::{FileBridge, GrantCommand};
use crate::event_flags::LiveEventFlags;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoodsGrant {
    pub normalized_item_id: u32,
    pub quantity: u32,
    pub expected_before: u32,
    pub tag: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GrantProgress {
    Pending,
    Complete,
}

pub trait BloodborneBackend {
    /// `None` means the live accessor is not available, never "flag is false".
    fn read_event_flag(&mut self, event_flag: u32) -> Result<Option<bool>>;
    fn grant_category4_goods(&mut self, grant: &GoodsGrant) -> Result<GrantProgress>;
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
    fn read_event_flag(&mut self, event_flag: u32) -> Result<Option<bool>> {
        self.event_flags.read_resilient(event_flag).map(Some)
    }

    fn grant_category4_goods(&mut self, grant: &GoodsGrant) -> Result<GrantProgress> {
        if self.bridge.command_pending() {
            return Ok(GrantProgress::Pending);
        }
        if let Ok(state) = self.bridge.read_state()
            && state.is_success()
            && state.detail.contains(&format!("tag={}", grant.tag))
        {
            return Ok(GrantProgress::Complete);
        }
        // Observed category-4 goods use normalized 0x4....... and raw 0xB....... .
        let raw_id = grant.normalized_item_id | 0x7000_0000;
        self.bridge.enqueue(&GrantCommand {
            raw_id,
            normalized_id: grant.normalized_item_id,
            quantity: grant.quantity,
            expected_before: grant.expected_before,
            tag: grant.tag.clone(),
        })?;
        Ok(GrantProgress::Pending)
    }
}

#[derive(Default)]
pub struct MockBackend {
    pub set_flags: HashSet<u32>,
    pub goods: HashMap<u32, u32>,
    pub grants: Vec<GoodsGrant>,
    completed_tags: HashSet<String>,
}

impl BloodborneBackend for MockBackend {
    fn read_event_flag(&mut self, event_flag: u32) -> Result<Option<bool>> {
        Ok(Some(self.set_flags.contains(&event_flag)))
    }

    fn grant_category4_goods(&mut self, grant: &GoodsGrant) -> Result<GrantProgress> {
        if self.completed_tags.contains(&grant.tag) {
            return Ok(GrantProgress::Complete);
        }
        let current = self
            .goods
            .get(&grant.normalized_item_id)
            .copied()
            .unwrap_or(0);
        anyhow::ensure!(
            current == grant.expected_before,
            "mock quantity mismatch: expected {}, found {}",
            grant.expected_before,
            current
        );
        self.goods.insert(
            grant.normalized_item_id,
            current.saturating_add(grant.quantity),
        );
        self.grants.push(grant.clone());
        self.completed_tags.insert(grant.tag.clone());
        Ok(GrantProgress::Complete)
    }
}
