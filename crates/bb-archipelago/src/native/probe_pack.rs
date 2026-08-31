//! Bounded, observation-only captures that can ride in ordinary playtests.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::guest::InventoryEntry;
use crate::client_eprintln;

const MAX_RECORDS: usize = 4096;

/// Reviewed from the world's boss-defeat runtime bindings. Keeping the label
/// beside the flag makes captures useful even when a boss is not a location in
/// the current seed contract.
pub const BOSS_FLAGS: &[(u32, &str)] = &[
    (12_411_700, "boss_cleric_beast"),
    (12_411_800, "boss_father_gascoigne"),
    (12_301_800, "boss_blood_starved_beast"),
    (12_301_700, "boss_darkbeast_paarl"),
    (12_401_800, "boss_vicar_amelia"),
    (12_201_800, "boss_witch_of_hemwick"),
    (12_501_800, "boss_martyr_logarius"),
    (12_421_700, "boss_celestial_emissary"),
    (12_421_800, "boss_ebrietas"),
    (12_701_800, "boss_shadows_of_yharnam"),
    (13_201_800, "boss_rom"),
    (12_801_800, "boss_the_one_reborn"),
    (12_601_850, "boss_micolash"),
    (12_601_800, "boss_mergos_wet_nurse"),
    (12_101_800, "boss_gehrman"),
    (12_101_850, "boss_moon_presence"),
    (13_301_800, "boss_amygdala"),
    (13_401_800, "boss_ludwig"),
    (13_501_850, "boss_living_failures"),
    (13_501_800, "boss_lady_maria"),
    (13_601_800, "boss_orphan_of_kos"),
    (13_401_850, "boss_laurence"),
];

struct JsonlCapture {
    file: File,
    records: usize,
    warned: bool,
}

impl JsonlCapture {
    fn new(ledger: &Path, file_name: &str, format: &str, probe: &str) -> std::io::Result<Self> {
        let path = ledger
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(file_name);
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut sink = Self {
            file,
            records: 0,
            warned: false,
        };
        sink.write(json::json!({
            "format": format,
            "event": "session_start",
            "probe": probe,
            "runtime_build": crate::RUNTIME_BUILD,
            "at_unix_ms": now_ms(),
            "mode": "observation_only",
            "max_records": MAX_RECORDS,
        }));
        Ok(sink)
    }

    fn write(&mut self, value: json::Value) {
        if self.warned || self.records >= MAX_RECORDS {
            return;
        }
        if let Err(error) = writeln!(self.file, "{value}").and_then(|_| self.file.flush()) {
            self.warned = true;
            client_eprintln!(
                "Research capture could not be written ({error}); gameplay is unaffected and this warning will not repeat."
            );
        } else {
            self.records += 1;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FlagState {
    Unreadable,
    Value(bool),
}

pub struct BossFlagCensus {
    sink: JsonlCapture,
    previous: HashMap<u32, FlagState>,
    save_identity: Option<String>,
}

impl BossFlagCensus {
    pub fn beside_ledger(ledger: &Path) -> std::io::Result<Self> {
        Ok(Self {
            sink: JsonlCapture::new(
                ledger,
                "boss-flag-census.jsonl",
                "bb-boss-flag-census-v1",
                "boss_flag_census",
            )?,
            previous: HashMap::new(),
            save_identity: None,
        })
    }

    pub fn observe(
        &mut self,
        save_identity: Option<&str>,
        gameplay_ready: bool,
        values: &[(u32, &'static str, Option<bool>)],
    ) {
        if self.save_identity.as_deref() != save_identity {
            self.previous.clear();
            self.save_identity = save_identity.map(str::to_owned);
        }
        for &(flag, label, value) in values {
            let state = value.map_or(FlagState::Unreadable, FlagState::Value);
            match self.previous.insert(flag, state) {
                None => self.sink.write(json::json!({
                    "event": "initial_value",
                    "at_unix_ms": now_ms(),
                    "flag": flag,
                    "label": label,
                    "value": value,
                    "save_identity": save_identity,
                    "gameplay_ready": gameplay_ready,
                })),
                Some(FlagState::Value(false)) if state == FlagState::Value(true) => {
                    self.sink.write(json::json!({
                        "event": "flag_flip",
                        "at_unix_ms": now_ms(),
                        "flag": flag,
                        "label": label,
                        "value": true,
                        "save_identity": save_identity,
                        "gameplay_ready": gameplay_ready,
                    }));
                }
                Some(previous) if previous != state && state == FlagState::Unreadable => {
                    self.sink.write(json::json!({
                        "event": "unreadable",
                        "at_unix_ms": now_ms(),
                        "flag": flag,
                        "label": label,
                        "save_identity": save_identity,
                        "gameplay_ready": gameplay_ready,
                    }));
                }
                _ => {}
            }
        }
    }
}

pub struct RuneCapture {
    sink: JsonlCapture,
    previous: Option<HashMap<u32, [u8; 16]>>,
}

impl RuneCapture {
    pub fn beside_ledger(ledger: &Path) -> std::io::Result<Self> {
        let mut sink = JsonlCapture::new(
            ledger,
            "rune-capture.jsonl",
            "bb-rune-capture-v1",
            "rune_capture",
        )?;
        sink.write(json::json!({
            "event": "capture_scope",
            "scope": "new_or_changed_inventory_descriptors",
            "reason": "no reviewed Caryll-rune id allowlist exists yet; broad diffing prevents false negatives",
        }));
        Ok(Self {
            sink,
            previous: None,
        })
    }

    pub fn observe(&mut self, entries: Option<Vec<InventoryEntry>>) {
        let Some(entries) = entries else { return };
        let current = entries
            .iter()
            .map(|entry| (entry.slot, entry.bytes))
            .collect::<HashMap<_, _>>();
        let Some(previous) = self.previous.replace(current) else {
            return;
        };
        for entry in entries {
            if previous.get(&entry.slot) == Some(&entry.bytes) {
                continue;
            }
            self.sink.write(json::json!({
                "event": "inventory_descriptor_change",
                "at_unix_ms": now_ms(),
                "slot": entry.slot,
                "address": format!("0x{:X}", entry.address),
                "raw_descriptor": hex(&entry.bytes),
                "word_0": format!("0x{:08X}", entry.word(0)),
                "word_4": format!("0x{:08X}", entry.word(4)),
                "word_8": format!("0x{:08X}", entry.word(8)),
                "word_12": format!("0x{:08X}", entry.word(12)),
            }));
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
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

    fn temp(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("bb-probe-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn census_distinguishes_initial_true_from_a_live_flip() {
        let root = temp("census");
        let mut census = BossFlagCensus::beside_ledger(&root.join("ledger.json")).unwrap();
        census.observe(Some("save"), true, &[(1, "boss", Some(true))]);
        census.observe(Some("save"), true, &[(1, "boss", Some(true))]);
        census.observe(Some("other-save"), true, &[(1, "boss", Some(false))]);
        census.observe(Some("other-save"), true, &[(1, "boss", Some(true))]);
        drop(census);
        let text = std::fs::read_to_string(root.join("boss-flag-census.jsonl")).unwrap();
        assert_eq!(text.matches("\"event\":\"flag_flip\"").count(), 1, "{text}");
        assert_eq!(
            text.matches("\"event\":\"initial_value\"").count(),
            2,
            "{text}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rune_capture_ignores_baseline_and_records_only_changes() {
        let root = temp("runes");
        let mut capture = RuneCapture::beside_ledger(&root.join("ledger.json")).unwrap();
        let entry = |byte| InventoryEntry {
            slot: 7,
            address: 9,
            bytes: [byte; 16],
        };
        capture.observe(Some(vec![entry(1)]));
        capture.observe(Some(vec![entry(1)]));
        capture.observe(Some(vec![entry(2)]));
        drop(capture);
        let text = std::fs::read_to_string(root.join("rune-capture.jsonl")).unwrap();
        assert_eq!(
            text.matches("inventory_descriptor_change").count(),
            1,
            "{text}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn capture_stops_at_its_bound() {
        let root = temp("bounded");
        let mut sink = JsonlCapture::new(
            &root.join("ledger.json"),
            "bounded.jsonl",
            "test-v1",
            "test",
        )
        .unwrap();
        sink.records = MAX_RECORDS;
        sink.write(json::json!({"event": "must_not_land"}));
        drop(sink);
        let text = std::fs::read_to_string(root.join("bounded.jsonl")).unwrap();
        assert!(!text.contains("must_not_land"), "{text}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn poisoned_writer_fails_silently_and_latches_once() {
        let root = temp("poisoned");
        let path = root.join("poisoned.jsonl");
        std::fs::write(&path, b"read only").unwrap();
        let file = File::open(&path).unwrap();
        let mut sink = JsonlCapture {
            file,
            records: 0,
            warned: false,
        };
        sink.write(json::json!({"event": "first"}));
        assert!(sink.warned);
        sink.write(json::json!({"event": "second"}));
        assert_eq!(sink.records, 0);
        let _ = std::fs::remove_dir_all(root);
    }
}
