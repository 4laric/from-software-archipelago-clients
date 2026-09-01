//! Observation-only correlation stream for the native pickup-banner research.
//!
//! This deliberately does not guess at or call a message function. It joins
//! the safe facts the client already owns (checks and delivery attempts) to the
//! live ItemGrant call boundary so a capture can identify callers and timing.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::backend::ItemGrant;

use super::item_grant_probe::ItemGrantCallSnapshot;
use super::pickup_presentation_probe::PickupPresentationSnapshot;

const MAX_RECORDS: usize = 4096;

pub struct PickupNotificationCapture {
    file: File,
    records: usize,
    previous_native_sequence: u64,
    presentation_sequences: HashMap<&'static str, u64>,
    image_base: u64,
    grant_states: HashMap<String, &'static str>,
    warned: bool,
}

impl PickupNotificationCapture {
    pub fn beside_ledger(ledger: &Path, image_base: u64) -> std::io::Result<Self> {
        let path = ledger
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("pickup-notification-capture.jsonl");
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut capture = Self {
            file,
            records: 0,
            previous_native_sequence: 0,
            presentation_sequences: HashMap::new(),
            image_base,
            grant_states: HashMap::new(),
            warned: false,
        };
        capture.write(json::json!({
            "format": "bb-pickup-notification-capture-v2",
            "event": "session_start",
            "at_unix_ms": now_ms(),
            "mode": "observation_only",
            "item_grant_rva": "0x14DA0A0",
            "native_notification_queue": "unmapped",
            "presentation_probe_callers": ["0x17D93FE", "0x14DA9FF"]
        }));
        Ok(capture)
    }

    pub fn location_checks(&mut self, locations: &[i64]) {
        if locations.is_empty() {
            return;
        }
        self.write(json::json!({
            "event": "ap_location_checks",
            "at_unix_ms": now_ms(),
            "locations": locations,
        }));
    }

    pub fn marker(&mut self, note: &str) {
        self.write(json::json!({
            "event": "operator_marker",
            "at_unix_ms": now_ms(),
            "note": note,
        }));
    }

    pub fn grant_state(&mut self, grant: &ItemGrant, state: &'static str) {
        if let Some(previous) = self.grant_states.get(&grant.tag)
            && grant_state_rank(state) <= grant_state_rank(previous)
        {
            return;
        }
        self.grant_states.insert(grant.tag.clone(), state);
        self.write(json::json!({
            "event": "ap_delivery",
            "at_unix_ms": now_ms(),
            "state": state,
            "tag": grant.tag,
            "raw_descriptor": format!("0x{:08X}", grant.raw_descriptor),
            "normalized_item_id": format!("0x{:08X}", grant.normalized_item_id),
            "item_category": grant.item_category,
            "quantity": grant.quantity,
            "reinforcement_level": grant.reinforcement_level,
        }));
    }

    pub fn observe_native_call(&mut self, snapshot: Option<ItemGrantCallSnapshot>) {
        let Some(snapshot) = snapshot else { return };
        if snapshot.sequence == self.previous_native_sequence {
            return;
        }
        let missed = snapshot
            .sequence
            .saturating_sub(self.previous_native_sequence)
            .saturating_sub(1);
        self.previous_native_sequence = snapshot.sequence;
        self.write(json::json!({
            "event": "native_item_grant_call",
            "at_unix_ms": now_ms(),
            "sequence": snapshot.sequence,
            "missed_calls_since_previous_sample": missed,
            "quantity": snapshot.quantity,
            "raw_id": format!("0x{:08X}", snapshot.raw_id),
            "normalized_id": format!("0x{:08X}", snapshot.normalized_id),
            "caller": format!("0x{:X}", snapshot.caller),
            "caller_rva": snapshot.caller.checked_sub(self.image_base).map(|rva| format!("0x{rva:X}")),
            "descriptor_address": format!("0x{:X}", snapshot.descriptor_address),
        }));
    }

    pub fn observe_presentation_calls(&mut self, snapshots: Vec<PickupPresentationSnapshot>) {
        for snapshot in snapshots {
            let previous = self
                .presentation_sequences
                .get(snapshot.site)
                .copied()
                .unwrap_or(0);
            if snapshot.sequence == previous {
                continue;
            }
            self.presentation_sequences
                .insert(snapshot.site, snapshot.sequence);
            let descriptor = snapshot.descriptor.as_deref().map(hex_bytes);
            self.write(json::json!({
                "event": "vanilla_pickup_call",
                "at_unix_ms": now_ms(),
                "site": snapshot.site,
                "caller_rva": format!("0x{:X}", snapshot.caller_rva),
                "sequence": snapshot.sequence,
                "missed_calls_since_previous_sample": snapshot.sequence.saturating_sub(previous).saturating_sub(1),
                "thread_stack_token": format!("0x{:X}", snapshot.thread_stack_token),
                "entry": {
                    "inventory": format!("0x{:X}", snapshot.inventory),
                    "descriptor_address": format!("0x{:X}", snapshot.descriptor_address),
                    "quantity": snapshot.quantity,
                    "candidate_message_context": format!("0x{:X}", snapshot.candidate_message_context),
                    "candidate_icon_context": format!("0x{:X}", snapshot.candidate_icon_context),
                    "candidate_aux_context": format!("0x{:X}", snapshot.candidate_aux_context),
                    "descriptor_24": descriptor,
                },
                "return": format!("0x{:X}", snapshot.result),
            }));
        }
    }

    fn write(&mut self, value: json::Value) {
        if self.warned || self.records >= MAX_RECORDS {
            return;
        }
        if writeln!(self.file, "{value}")
            .and_then(|_| self.file.flush())
            .is_err()
        {
            self.warned = true;
        } else {
            self.records += 1;
        }
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join("")
}

fn grant_state_rank(state: &str) -> u8 {
    match state {
        "submitted" => 0,
        "pending" => 1,
        "complete" => 2,
        _ => 3,
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_native_calls_and_pending_grants_are_deduplicated() {
        let root = std::env::temp_dir().join(format!(
            "bb-pickup-notification-capture-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut capture =
            PickupNotificationCapture::beside_ledger(&root.join("ledger.json"), 0).unwrap();
        let snapshot = ItemGrantCallSnapshot {
            sequence: 1,
            inventory: 2,
            descriptor_address: 3,
            quantity: 4,
            raw_id: 5,
            internal_pointer: 6,
            normalized_id: 7,
            caller: 8,
        };
        capture.observe_native_call(Some(snapshot.clone()));
        capture.observe_native_call(Some(snapshot));
        let grant = ItemGrant {
            raw_descriptor: 1,
            normalized_item_id: 2,
            item_category: 4,
            quantity: 1,
            expected_before: 0,
            reinforcement_level: None,
            tag: "ap_1".into(),
        };
        capture.grant_state(&grant, "pending");
        capture.grant_state(&grant, "pending");
        capture.grant_state(&grant, "submitted");
        capture.marker("popup");
        drop(capture);
        let text = std::fs::read_to_string(root.join("pickup-notification-capture.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 4, "{text}");
        assert!(text.contains("\"event\":\"operator_marker\""), "{text}");
        std::fs::remove_dir_all(root).unwrap();
    }
}
