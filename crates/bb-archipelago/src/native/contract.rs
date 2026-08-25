//! Typed access to the vendored `bb-native-grant-v5` runtime contract.
//!
//! Every hook-site RVA, native-routine RVA, state-cell offset, descriptor
//! prefix, image-assert byte string and relocatable payload blob the native
//! delivery backend needs is read out of
//! `contract/bb-native-grant-contract.v5.json` at first use, not hand-copied
//! into Rust. That file is a verbatim vendored copy of the world repo's single
//! source of truth (`research/runtime/bb-native-grant-contract.v5.json`); see
//! `contract/README.md`. RESEARCH-BASELINE.md flagged constant-duplication
//! between the Cheat Engine table, the Python prototype and this crate as an
//! "unasserted agreement" hazard -- deriving from one committed artifact is the
//! fix, and [`Contract::assert_agrees_with_crate`] fails a unit test if the
//! vendored copy ever disagrees with the crate's own build/harness/protocol
//! constants.
//!
//! Nothing in this module has run against a live game. Every address it exposes
//! carries the contract's own `provenance` label; the module never upgrades a
//! label. Consumers must keep the whole native path behind the delivery flag
//! and fail closed on any mismatch.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::RUNTIME_BUILD;
use crate::bridge::{BRIDGE_PROTOCOL, HARNESS_VERSION};

/// The vendored contract text, compiled into the binary.
pub const CONTRACT_JSON: &str = include_str!("../../contract/bb-native-grant-contract.v5.json");

#[derive(Debug, Deserialize)]
struct RawContract {
    format: String,
    harness: String,
    build: String,
    bridge_protocol: String,
    target: RawTarget,
    base_resolution: RawBaseResolution,
    hook_sites: Vec<RawHookSite>,
    native_routines: Vec<RawNamedRva>,
    descriptor: RawDescriptor,
    state_cells: RawStateCells,
    inventory_geometry: RawGeometry,
    asserts: Vec<RawAssert>,
    payload: RawPayload,
    policy: RawPolicy,
}

#[derive(Debug, Deserialize)]
struct RawTarget {
    serial: String,
    app_ver: String,
    eboot_sha256: String,
}

#[derive(Debug, Deserialize)]
struct RawBaseResolution {
    consume_signature: String,
}

#[derive(Debug, Deserialize)]
struct RawHookSite {
    name: String,
    rva: u64,
    original_bytes: String,
    return_rva: u64,
}

#[derive(Debug, Deserialize)]
struct RawNamedRva {
    name: String,
    rva: u64,
}

#[derive(Debug, Deserialize)]
struct RawDescriptor {
    size: usize,
    staged_size: usize,
    goods_formula: RawGoodsFormula,
    source_selection: RawSourceSelection,
}

#[derive(Debug, Deserialize)]
struct RawGoodsFormula {
    raw: String,
    normalized: String,
}

#[derive(Debug, Deserialize)]
struct RawSourceSelection {
    test: String,
}

#[derive(Debug, Deserialize)]
struct RawStateCells {
    region_rva: u64,
    region_size: u64,
    cells: Vec<RawCell>,
}

#[derive(Debug, Deserialize)]
struct RawCell {
    name: String,
    rva: u64,
    width: u32,
}

#[derive(Debug, Deserialize)]
struct RawGeometry {
    split: u64,
    last: u64,
    primary_array: u64,
    secondary_array: u64,
    record_stride: u64,
    record_id: u64,
    record_quantity: u64,
}

#[derive(Debug, Deserialize)]
struct RawAssert {
    name: String,
    rva: u64,
    bytes: String,
}

#[derive(Debug, Deserialize)]
struct RawPayload {
    blobs: Vec<RawBlob>,
}

#[derive(Debug, Deserialize)]
struct RawBlob {
    name: String,
    rva: u64,
    size: usize,
    bytes: String,
    #[serde(default)]
    relocations: Vec<RawRelocation>,
}

#[derive(Debug, Deserialize)]
struct RawRelocation {
    offset: usize,
    width: u8,
    target_rva: u64,
}

#[derive(Debug, Deserialize)]
struct RawPolicy {
    absent_blood_vial: String,
    verify_polls: u32,
    hydration_verify_polls: u32,
    min_absent_polls: u32,
}

/// One image byte-assert: at `rva`, memory must equal `bytes` before anything
/// is written. Fail closed on any mismatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageAssert {
    pub name: String,
    pub rva: u64,
    pub bytes: Vec<u8>,
}

/// One absolute quadword inside a payload blob that install time relocates by
/// adding the eboot base to `target_rva`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Relocation {
    pub offset: usize,
    pub width: u8,
    pub target_rva: u64,
}

/// A contiguous run of bytes destined for one eboot RVA, plus the relocations
/// that turn its base-0 absolutes into live addresses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayloadBlob {
    pub name: String,
    pub rva: u64,
    pub bytes: Vec<u8>,
    pub relocations: Vec<Relocation>,
}

impl PayloadBlob {
    /// The bytes to write at `eboot_base + self.rva`, with every relocation
    /// resolved. Fails closed on an unsupported relocation width so a wrong
    /// fixup can never reach a player's process.
    pub fn relocated(&self, eboot_base: u64) -> Result<Vec<u8>> {
        let mut out = self.bytes.clone();
        for reloc in &self.relocations {
            if reloc.width != 8 {
                bail!(
                    "blob {} has unsupported relocation width {}",
                    self.name,
                    reloc.width
                );
            }
            let end = reloc.offset + 8;
            anyhow::ensure!(
                end <= out.len(),
                "blob {} relocation at {} overruns its {} bytes",
                self.name,
                reloc.offset,
                out.len()
            );
            let value = eboot_base
                .checked_add(reloc.target_rva)
                .context("relocation target overflows the address space")?;
            out[reloc.offset..end].copy_from_slice(&value.to_le_bytes());
        }
        Ok(out)
    }
}

/// The descriptor formula and source-selection rule, read from the contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DescriptorFormula {
    pub size: usize,
    pub staged_size: usize,
    pub goods_raw_prefix: u32,
    pub goods_normalized_prefix: u32,
    /// The high-nibble value the cave compares `raw & 0xF000_0000` against to
    /// take the persistent (equipment) descriptor branch.
    pub persistent_source_marker: u32,
}

/// Fully parsed, validated view of the runtime contract.
#[derive(Debug)]
pub struct Contract {
    pub format: String,
    pub harness: String,
    pub build: String,
    pub bridge_protocol: String,
    pub serial: String,
    pub app_ver: String,
    pub eboot_sha256: String,
    pub consume_signature: Vec<Option<u8>>,
    pub hook_sites: BTreeMap<String, HookSite>,
    pub native_routines: BTreeMap<String, u64>,
    pub descriptor: DescriptorFormula,
    pub state_region_rva: u64,
    pub state_region_size: u64,
    pub state_cells: BTreeMap<String, StateCell>,
    pub geometry: InventoryGeometry,
    pub asserts: Vec<ImageAssert>,
    pub blobs: Vec<PayloadBlob>,
    pub policy: Policy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookSite {
    pub rva: u64,
    pub original_bytes: Vec<u8>,
    pub return_rva: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateCell {
    pub rva: u64,
    pub width: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InventoryGeometry {
    pub split: u64,
    pub last: u64,
    pub primary_array: u64,
    pub secondary_array: u64,
    pub record_stride: u64,
    pub record_id: u64,
    pub record_quantity: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Policy {
    /// `true` only when the contract permits inserting an absent Blood Vial.
    /// The v5 contract refuses it; a native insert of a missing Vial must be
    /// treated as a terminal failure.
    pub absent_blood_vial_allowed: bool,
    pub verify_polls: u32,
    pub hydration_verify_polls: u32,
    pub min_absent_polls: u32,
}

impl Contract {
    /// Parse and validate the vendored contract. Fails closed on any missing or
    /// malformed field so the native path can never arm against a half-read
    /// contract.
    pub fn parse(text: &str) -> Result<Self> {
        let raw: RawContract =
            json::from_str(text).context("parsing the vendored native-grant contract")?;

        let mut hook_sites = BTreeMap::new();
        for site in &raw.hook_sites {
            let original_bytes = parse_hex_exact(&site.original_bytes)
                .with_context(|| format!("hook site {} original_bytes", site.name))?;
            hook_sites.insert(
                site.name.clone(),
                HookSite {
                    rva: site.rva,
                    original_bytes,
                    return_rva: site.return_rva,
                },
            );
        }

        let mut native_routines = BTreeMap::new();
        for routine in &raw.native_routines {
            native_routines.insert(routine.name.clone(), routine.rva);
        }

        let descriptor = DescriptorFormula {
            size: raw.descriptor.size,
            staged_size: raw.descriptor.staged_size,
            goods_raw_prefix: parse_prefix(&raw.descriptor.goods_formula.raw)
                .context("descriptor goods_formula.raw")?,
            goods_normalized_prefix: parse_prefix(&raw.descriptor.goods_formula.normalized)
                .context("descriptor goods_formula.normalized")?,
            persistent_source_marker: parse_source_marker(&raw.descriptor.source_selection.test)
                .context("descriptor source_selection.test")?,
        };

        let mut state_cells = BTreeMap::new();
        for cell in &raw.state_cells.cells {
            state_cells.insert(
                cell.name.clone(),
                StateCell {
                    rva: cell.rva,
                    width: cell.width,
                },
            );
        }

        let asserts = raw
            .asserts
            .iter()
            .map(|a| {
                Ok(ImageAssert {
                    name: a.name.clone(),
                    rva: a.rva,
                    bytes: parse_hex_exact(&a.bytes)
                        .with_context(|| format!("assert {} bytes", a.name))?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let mut blobs = Vec::with_capacity(raw.payload.blobs.len());
        for blob in &raw.payload.blobs {
            let bytes = parse_hex_exact(&blob.bytes)
                .with_context(|| format!("payload blob {} bytes", blob.name))?;
            anyhow::ensure!(
                bytes.len() == blob.size,
                "payload blob {} declares size {} but carries {} bytes",
                blob.name,
                blob.size,
                bytes.len()
            );
            let relocations = blob
                .relocations
                .iter()
                .map(|r| Relocation {
                    offset: r.offset,
                    width: r.width,
                    target_rva: r.target_rva,
                })
                .collect();
            blobs.push(PayloadBlob {
                name: blob.name.clone(),
                rva: blob.rva,
                bytes,
                relocations,
            });
        }

        let policy = Policy {
            absent_blood_vial_allowed: match raw.policy.absent_blood_vial.as_str() {
                "refused" => false,
                "allowed" => true,
                other => bail!("unknown policy.absent_blood_vial value {other:?}"),
            },
            verify_polls: raw.policy.verify_polls,
            hydration_verify_polls: raw.policy.hydration_verify_polls,
            min_absent_polls: raw.policy.min_absent_polls,
        };

        Ok(Self {
            format: raw.format,
            harness: raw.harness,
            build: raw.build,
            bridge_protocol: raw.bridge_protocol,
            serial: raw.target.serial,
            app_ver: raw.target.app_ver,
            eboot_sha256: raw.target.eboot_sha256,
            consume_signature: parse_pattern(&raw.base_resolution.consume_signature)
                .context("base_resolution.consume_signature")?,
            hook_sites,
            native_routines,
            descriptor,
            state_region_rva: raw.state_cells.region_rva,
            state_region_size: raw.state_cells.region_size,
            state_cells,
            geometry: InventoryGeometry {
                split: raw.inventory_geometry.split,
                last: raw.inventory_geometry.last,
                primary_array: raw.inventory_geometry.primary_array,
                secondary_array: raw.inventory_geometry.secondary_array,
                record_stride: raw.inventory_geometry.record_stride,
                record_id: raw.inventory_geometry.record_id,
                record_quantity: raw.inventory_geometry.record_quantity,
            },
            asserts,
            blobs,
            policy,
        })
    }

    /// Tripwire: the vendored contract must still describe the same
    /// build/harness/protocol the crate compiles against. A mismatch means the
    /// vendored copy drifted from the code that consumes it.
    pub fn assert_agrees_with_crate(&self) -> Result<()> {
        anyhow::ensure!(
            self.build == RUNTIME_BUILD,
            "vendored contract build {:?} != crate RUNTIME_BUILD {RUNTIME_BUILD:?}",
            self.build
        );
        anyhow::ensure!(
            self.harness == HARNESS_VERSION,
            "vendored contract harness {:?} != crate HARNESS_VERSION {HARNESS_VERSION:?}",
            self.harness
        );
        anyhow::ensure!(
            self.bridge_protocol == BRIDGE_PROTOCOL,
            "vendored contract protocol {:?} != crate BRIDGE_PROTOCOL {BRIDGE_PROTOCOL:?}",
            self.bridge_protocol
        );
        Ok(())
    }

    pub fn hook_site(&self, name: &str) -> Result<&HookSite> {
        self.hook_sites
            .get(name)
            .with_context(|| format!("contract has no hook site {name:?}"))
    }

    pub fn native_routine(&self, name: &str) -> Result<u64> {
        self.native_routines
            .get(name)
            .copied()
            .with_context(|| format!("contract has no native routine {name:?}"))
    }

    pub fn state_cell(&self, name: &str) -> Result<StateCell> {
        self.state_cells
            .get(name)
            .copied()
            .with_context(|| format!("contract has no state cell {name:?}"))
    }
}

/// The process-wide parsed contract, validated once.
pub fn contract() -> &'static Contract {
    static CONTRACT: OnceLock<Contract> = OnceLock::new();
    CONTRACT.get_or_init(|| {
        let parsed =
            Contract::parse(CONTRACT_JSON).expect("the vendored native-grant contract must parse");
        parsed
            .assert_agrees_with_crate()
            .expect("the vendored native-grant contract must agree with the crate constants");
        parsed
    })
}

/// Parse a space-or-run hex byte string with no wildcards (asserts, blobs,
/// hook originals). Fails on any non-hex or odd-length token.
fn parse_hex_exact(text: &str) -> Result<Vec<u8>> {
    let pattern = parse_pattern(text)?;
    pattern
        .into_iter()
        .map(|value| value.context("exact byte string may not contain wildcards"))
        .collect()
}

/// Parse a Cheat-Engine-style byte string that may contain `??`/`*` wildcards
/// and may be space-separated or a continuous hex run.
fn parse_pattern(text: &str) -> Result<Vec<Option<u8>>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        bail!("empty byte pattern");
    }
    // Space-separated (asserts, signatures) vs continuous run (payload blobs).
    if trimmed.contains(char::is_whitespace) || trimmed.contains(',') {
        let mut out = Vec::new();
        for token in trimmed.split([' ', '\t', '\n', '\r', ',']) {
            if token.is_empty() {
                continue;
            }
            if matches!(token, "??" | "**" | "?" | "*") {
                out.push(None);
                continue;
            }
            anyhow::ensure!(
                token.len() == 2,
                "expected a two-hex-digit byte, got {token:?}"
            );
            out.push(Some(
                u8::from_str_radix(token, 16)
                    .with_context(|| format!("{token:?} is not a hex byte"))?,
            ));
        }
        anyhow::ensure!(!out.is_empty(), "byte pattern parsed to nothing");
        Ok(out)
    } else {
        anyhow::ensure!(
            trimmed.len().is_multiple_of(2),
            "continuous hex run has an odd number of digits"
        );
        (0..trimmed.len())
            .step_by(2)
            .map(|i| {
                let byte = &trimmed[i..i + 2];
                Ok(Some(
                    u8::from_str_radix(byte, 16)
                        .with_context(|| format!("{byte:?} is not a hex byte"))?,
                ))
            })
            .collect()
    }
}

/// Parse a `"0xB0000000 | goods_id"` style formula into its constant prefix.
fn parse_prefix(text: &str) -> Result<u32> {
    let head = text.split('|').next().unwrap_or("").trim();
    parse_u32_literal(head)
}

/// Parse `"raw_id & 0xF0000000 == 0x80000000"` into the compared marker
/// (`0x80000000`).
fn parse_source_marker(text: &str) -> Result<u32> {
    let tail = text
        .rsplit("==")
        .next()
        .context("source_selection test has no comparison")?
        .trim();
    parse_u32_literal(tail)
}

fn parse_u32_literal(text: &str) -> Result<u32> {
    let text = text.trim();
    let value = if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).with_context(|| format!("{text:?} is not hex"))?
    } else {
        text.parse::<u64>()
            .with_context(|| format!("{text:?} is not a number"))?
    };
    u32::try_from(value).with_context(|| format!("{text:?} does not fit in u32"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendored_contract_parses_and_agrees_with_the_crate() {
        let contract = Contract::parse(CONTRACT_JSON).unwrap();
        contract.assert_agrees_with_crate().unwrap();
        assert_eq!(contract.format, "bb-native-grant-contract-v5");
        assert_eq!(contract.serial, "CUSA03173");
        assert_eq!(contract.app_ver, "01.09");
    }

    #[test]
    fn hook_sites_and_routines_match_the_published_rvas() {
        let c = contract();
        // Cross-checked against the contract's own numbers (task-supplied):
        // consume 0x14D9575, heartbeat 0x1BFE882; ItemGrant 0x14DA0A0,
        // quantity-delta 0x14D94A0, find-slot 0x14DA2C0.
        assert_eq!(c.hook_site("consume_return").unwrap().rva, 0x14D9575);
        assert_eq!(c.hook_site("consume_return").unwrap().return_rva, 0x14D957C);
        assert_eq!(c.hook_site("idle_heartbeat").unwrap().rva, 0x1BFE882);
        assert_eq!(c.native_routine("ItemGrant").unwrap(), 0x14DA0A0);
        assert_eq!(c.native_routine("quantity_delta").unwrap(), 0x14D94A0);
        assert_eq!(
            c.native_routine("find_slot_by_descriptor").unwrap(),
            0x14DA2C0
        );
    }

    #[test]
    fn descriptor_formula_is_read_from_the_contract() {
        let d = contract().descriptor;
        assert_eq!(d.size, 24);
        assert_eq!(d.staged_size, 32);
        assert_eq!(d.goods_raw_prefix, 0xB000_0000);
        assert_eq!(d.goods_normalized_prefix, 0x4000_0000);
        assert_eq!(d.persistent_source_marker, 0x8000_0000);
    }

    #[test]
    fn state_cells_and_geometry_are_read_from_the_contract() {
        let c = contract();
        assert_eq!(c.state_region_rva, 0x50DBE00);
        assert_eq!(c.state_region_size, 0x70);
        assert_eq!(c.state_cell("request").unwrap().rva, 0x50DBE00);
        assert_eq!(c.state_cell("descriptor").unwrap().rva, 0x50DBE00 + 0x60);
        assert_eq!(c.state_cell("descriptor").unwrap().width, 32);
        assert_eq!(c.geometry.split, 0x24);
        assert_eq!(c.geometry.record_stride, 0x10);
        assert_eq!(c.geometry.record_quantity, 0x08);
    }

    #[test]
    fn asserts_and_blobs_decode_to_bytes() {
        let c = contract();
        let consume = c.asserts.iter().find(|a| a.name == "consume_hook").unwrap();
        assert_eq!(
            consume.bytes,
            vec![0x44, 0x89, 0xE0, 0x48, 0x83, 0xC4, 0x28]
        );
        let detour = c.blobs.iter().find(|b| b.name == "consume_detour").unwrap();
        assert_eq!(detour.bytes.len(), 7);
        assert_eq!(detour.bytes[0], 0xE9); // jmp rel32
        // The two caves carry the only relocations, both 8-byte quadwords.
        let cave = c.blobs.iter().find(|b| b.name == "consume_cave").unwrap();
        assert_eq!(cave.relocations.len(), 2);
        assert!(cave.relocations.iter().all(|r| r.width == 8));
    }

    #[test]
    fn policy_refuses_the_absent_blood_vial() {
        let p = contract().policy;
        assert!(!p.absent_blood_vial_allowed);
        assert_eq!(p.verify_polls, 20);
        assert_eq!(p.hydration_verify_polls, 240);
        assert_eq!(p.min_absent_polls, 40);
    }

    #[test]
    fn consume_signature_parses_as_a_wildcard_free_pattern() {
        let sig = &contract().consume_signature;
        assert_eq!(sig.len(), 16);
        assert_eq!(sig[0], Some(0x44));
        assert!(sig.iter().all(|b| b.is_some()));
    }
}
