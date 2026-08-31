//! A [`Runtime`] backed by [`ProcessMemory`]: the inventory-geometry walk and
//! the request/state cells.
//!
//! A port of `tools/bb_native_delivery/guest.py`. The geometry is transcribed
//! from the Cheat Engine harness's `findItem`: the cached inventory pointer
//! leads to a split pair of fixed-stride record arrays. Every offset comes from
//! the contract, not this file. The walk is live-validated: on 2026-08-27 it
//! identified Ludwig's Holy Blade +1 as normalized id 8,100,100 and selected
//! target level 1; the native grant then delivered Saw Cleaver +1 as 7,000,100.
//!
//! Guest I/O can fail (a torn read during a load, the process exiting). Because
//! the delivery state machine's [`Runtime`] is infallible by design, a failed
//! access degrades to "unavailable" -- `inventory_ready()` false, `find_stack()`
//! `None` -- which stalls delivery at `awaiting_inventory` rather than
//! mis-granting, and the first error is captured for the backend to surface via
//! [`GuestRuntime::take_error`].

use std::cell::RefCell;

use super::contract::{Contract, contract};
use super::delivery::{EMPTY_SLOT, Runtime, SlotRecord, StackView};
use super::descriptor::ItemGrantDescriptor;
use super::mem::ProcessMemory;

const MAX_SLOTS: u64 = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InventoryEntry {
    pub slot: u32,
    pub address: u64,
    pub bytes: [u8; 16],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedObjectProbe {
    pub entry: InventoryEntry,
    pub address: u64,
    pub bytes: Vec<u8>,
}

impl InventoryEntry {
    pub fn word(&self, offset: usize) -> u32 {
        u32::from_le_bytes(self.bytes[offset..offset + 4].try_into().unwrap())
    }
}

/// Base-game player weapon families in CUSA03173 01.09. These are the same
/// EquipParamWeapon bases the world uses for requirement removal. Exact
/// membership prevents armour/runes that happen to resemble a +N row from
/// influencing the target.
const WEAPON_FAMILIES: &[u32] = &[
    2_000_000, 4_000_000, 5_000_000, 5_100_000, 6_000_000, 6_100_000, 7_000_000, 7_100_000,
    8_000_000, 8_100_000, 9_000_000, 10_000_000, 10_100_000, 11_000_000, 12_000_000, 13_000_000,
    14_000_000, 14_200_000, 15_000_000, 22_000_000,
];

/// Which of the two banks holds `slot`. Pure, so it is testable without memory.
pub fn entry_address(slot: u64, split: u64, primary: u64, secondary: u64, stride: u64) -> u64 {
    if slot < split {
        primary + slot * stride
    } else {
        secondary + (slot - split) * stride
    }
}

struct CellAddresses {
    request: u64,
    quantity: u64,
    result: u64,
    done: u64,
    inventory: u64,
    slot_index: u64,
    item_quantity_pointer: u64,
    manual_trigger: u64,
    descriptor: u64,
    player_status: u64,
}

/// Owns the process-memory accessor so it can be stored inside the delivery
/// engine without a self-referential borrow. `contract` is the process-wide
/// validated singleton.
pub struct GuestRuntime<P: ProcessMemory> {
    memory: P,
    base: u64,
    contract: &'static Contract,
    cells: CellAddresses,
    error: RefCell<Option<String>>,
    generated_probe: Option<InventoryEntry>,
}

impl<P: ProcessMemory> GuestRuntime<P> {
    pub fn new(memory: P, base: u64) -> anyhow::Result<Self> {
        let contract = contract();
        let cell =
            |name: &str| -> anyhow::Result<u64> { Ok(base + contract.state_cell(name)?.rva) };
        let cells = CellAddresses {
            request: cell("request")?,
            quantity: cell("quantity")?,
            result: cell("result")?,
            done: cell("done")?,
            inventory: cell("inventory")?,
            slot_index: cell("slot_index")?,
            item_quantity_pointer: cell("item_quantity_pointer")?,
            manual_trigger: cell("manual_trigger")?,
            descriptor: cell("descriptor")?,
            player_status: cell("player_status")?,
        };
        Ok(Self {
            memory,
            base,
            contract,
            cells,
            error: RefCell::new(None),
            generated_probe: None,
        })
    }

    pub fn base(&self) -> u64 {
        self.base
    }

    pub fn memory(&self) -> &P {
        &self.memory
    }

    /// Kill the currently captured player through the live-validated HP cell.
    /// The HP hook continuously refreshes `player_status`; zero means it has
    /// not run since launch and must never be treated as an address.
    pub fn death_link_kill(&self) -> anyhow::Result<bool> {
        let status = self.memory.read_u64(self.cells.player_status)?;
        if status == 0 {
            return Ok(false);
        }
        if self.memory.read_u32(status + 0xF8)? == 0 {
            // Keep later links queued until the player has actually respawned.
            return Ok(false);
        }
        self.memory.write_u32(status + 0xF8, 0)?;
        Ok(true)
    }

    pub fn clear_player_status(&self) -> anyhow::Result<()> {
        self.memory.write_u64(self.cells.player_status, 0)
    }

    /// Highest reinforcement level among recognized player weapon records.
    /// Read-only and bounded by the already-validated inventory geometry.
    pub fn target_weapon_level(&self) -> Option<u8> {
        let (_inventory, split, last, primary, secondary) = self.geometry()?;
        let g = self.contract.geometry;
        let mut highest = 0u8;
        for slot in 0..=last {
            let entry = entry_address(slot, split, primary, secondary, g.record_stride);
            let id = self.record(self.memory.read_u32(entry + g.record_id))?;
            for &family in WEAPON_FAMILIES {
                let Some(delta) = id.checked_sub(family) else {
                    continue;
                };
                if delta <= 1_000 && delta % 100 == 0 {
                    highest = highest.max((delta / 100) as u8);
                    break;
                }
            }
        }
        Some(highest)
    }

    /// Complete non-empty inventory records for passive natural-award
    /// diagnostics. This is a read-only view: it never stages a request or
    /// writes into the guest. Category 8 deliberately has no filtering rule
    /// yet, so the caller diffs every record and discovers its real shape.
    pub fn inventory_entries(&self) -> Option<Vec<InventoryEntry>> {
        let (_inventory, split, last, primary, secondary) = self.geometry()?;
        let g = self.contract.geometry;
        let mut entries = Vec::new();
        for slot in 0..=last {
            let address = entry_address(slot, split, primary, secondary, g.record_stride);
            let raw = self.record(self.memory.read(address, 16))?;
            let bytes: [u8; 16] = raw.try_into().expect("exact inventory record read");
            if bytes.iter().any(|byte| *byte != 0) {
                entries.push(InventoryEntry {
                    slot: slot as u32,
                    address,
                    bytes,
                });
            }
        }
        Some(entries)
    }

    /// Resolve one naturally-created category instance on the game thread and
    /// return a read-only snapshot of its backing object. Request 3 never calls
    /// ItemGrant and the cave never writes through the resolved pointer.
    pub fn probe_generated_object(
        &mut self,
        candidate: Option<InventoryEntry>,
    ) -> Option<GeneratedObjectProbe> {
        if self.generated_probe.is_none()
            && let Some(entry) = candidate
            && self.record(self.memory.read_u32(self.cells.request)) == Some(0)
        {
            let descriptor = ItemGrantDescriptor::new(entry.word(0), entry.word(4));
            let staged = descriptor.encode(&self.contract.descriptor);
            let _ = self.record(self.memory.write(self.cells.descriptor, &staged));
            let _ = self.record(self.memory.write_u64(self.cells.item_quantity_pointer, 0));
            let _ = self.record(self.memory.write_u32(self.cells.result, EMPTY_SLOT));
            let _ = self.record(self.memory.write_u32(self.cells.done, 0));
            let _ = self.record(self.memory.write_u32(self.cells.request, 3));
            self.generated_probe = Some(entry);
            return None;
        }

        let entry = self.generated_probe.clone()?;
        if self.record(self.memory.read_u32(self.cells.done)) != Some(1) {
            return None;
        }
        let address = self
            .record(self.memory.read_u64(self.cells.item_quantity_pointer))
            .unwrap_or(0);
        self.generated_probe = None;
        if address == 0 {
            return Some(GeneratedObjectProbe {
                entry,
                address,
                bytes: Vec::new(),
            });
        }
        let bytes = self
            .record(self.memory.read(address, 0x80))
            .unwrap_or_default();
        Some(GeneratedObjectProbe {
            entry,
            address,
            bytes,
        })
    }

    /// Take the first captured I/O error, if any, clearing it.
    pub fn take_error(&self) -> Option<String> {
        self.error.borrow_mut().take()
    }

    fn record<T>(&self, result: anyhow::Result<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                let mut slot = self.error.borrow_mut();
                if slot.is_none() {
                    *slot = Some(format!("{error:#}"));
                }
                None
            }
        }
    }

    /// `(inventory, split, last, primary, secondary)` or `None` when geometry is
    /// not hydrated / a read failed.
    fn geometry(&self) -> Option<(u64, u64, u64, u64, u64)> {
        let g = self.contract.geometry;
        let inventory = self.record(self.memory.read_u64(self.cells.inventory))?;
        if inventory == 0 {
            return None;
        }
        let split = self.record(self.memory.read_u32(inventory + g.split))? as u64;
        let last = self.record(self.memory.read_u32(inventory + g.last))? as u64;
        let primary = self.record(self.memory.read_u64(inventory + g.primary_array))?;
        let secondary = self.record(self.memory.read_u64(inventory + g.secondary_array))?;
        if last >= MAX_SLOTS || primary == 0 || secondary == 0 {
            return None;
        }
        Some((inventory, split, last, primary, secondary))
    }
}

/// `state_cells.request` = 1: the cave inserts a new stack via `ItemGrant`.
pub const REQUEST_NATIVE_INSERT: u32 = 1;
/// `state_cells.request` = 2: the cave applies `quantity` as a DELTA to the
/// stack named by `slot_index` / `item_quantity_pointer`, via
/// `native_routines.quantity_delta`. Running it on the game thread is what
/// clients#433 needed: the external write it replaced is refused
/// intermittently by shadPS4's protection tracking of the inventory pages.
pub const REQUEST_EXISTING_STACK_DELTA: u32 = 2;

impl<P: ProcessMemory> Runtime for GuestRuntime<P> {
    fn target_weapon_level(&mut self) -> Option<u8> {
        GuestRuntime::target_weapon_level(self)
    }

    fn inventory_ready(&mut self) -> bool {
        self.record(self.memory.read_u64(self.cells.inventory))
            .is_some_and(|inventory| inventory != 0)
    }

    fn find_stack(&mut self, normalized_id: u32) -> Option<StackView> {
        let (_inventory, split, last, primary, secondary) = self.geometry()?;
        let g = self.contract.geometry;
        for slot in 0..=last {
            let entry = entry_address(slot, split, primary, secondary, g.record_stride);
            let id = self.record(self.memory.read_u32(entry + g.record_id))?;
            if id == normalized_id {
                let quantity = self.record(self.memory.read_u32(entry + g.record_quantity))?;
                return Some(StackView {
                    quantity,
                    exists: true,
                    slot: Some(slot as u32),
                    quantity_address: Some(entry + g.record_quantity),
                });
            }
        }
        Some(StackView {
            quantity: 0,
            exists: false,
            slot: None,
            quantity_address: None,
        })
    }

    fn read_slot_record(&mut self, slot: u32) -> SlotRecord {
        if slot == EMPTY_SLOT {
            return SlotRecord::default();
        }
        let Some((_inventory, split, last, primary, secondary)) = self.geometry() else {
            return SlotRecord::default();
        };
        let g = self.contract.geometry;
        let slot = slot as u64;
        if slot > last {
            return SlotRecord::default();
        }
        let entry = entry_address(slot, split, primary, secondary, g.record_stride);
        let normalized_id = self.record(self.memory.read_u32(entry + g.record_id));
        let quantity = self.record(self.memory.read_u32(entry + g.record_quantity));
        SlotRecord {
            normalized_id,
            quantity,
            address: Some(entry),
        }
    }

    fn write_quantity(&mut self, address: u64, value: u32) -> bool {
        if self.record(self.memory.write_u32(address, value)).is_none() {
            return false;
        }
        self.record(self.memory.read_u32(address))
            .is_some_and(|read_back| read_back == value)
    }

    fn request_pending(&mut self) -> bool {
        // A read failure is treated as "pending" so the machine waits (busy)
        // rather than racing a second native request.
        self.record(self.memory.read_u32(self.cells.request))
            .is_none_or(|request| request != 0)
    }

    fn queue_native(
        &mut self,
        descriptor: &ItemGrantDescriptor,
        quantity: u32,
        slot: Option<u32>,
        quantity_address: Option<u64>,
        manual_trigger: bool,
    ) {
        let staged = descriptor.encode(&self.contract.descriptor);
        // Order mirrors guest.py: everything but `request` first, then `request`
        // last as the cave's arm signal.
        let _ = self.record(self.memory.write(self.cells.descriptor, &staged));
        let _ = self.record(
            self.memory
                .write_u32(self.cells.slot_index, slot.unwrap_or(EMPTY_SLOT)),
        );
        let _ = self.record(self.memory.write_u64(
            self.cells.item_quantity_pointer,
            quantity_address.unwrap_or(0),
        ));
        let _ = self.record(
            self.memory
                .write_u32(self.cells.manual_trigger, u32::from(manual_trigger)),
        );
        let _ = self.record(self.memory.write_u32(self.cells.quantity, quantity));
        let _ = self.record(self.memory.write_u32(self.cells.result, EMPTY_SLOT));
        let _ = self.record(self.memory.write_u32(self.cells.done, 0));
        // Contract v5 `state_cells.request`: 1 = native insert, 2 =
        // existing-stack delta. The delta branch is the one that takes
        // `slot_index` and `item_quantity_pointer` and reads `quantity` as a
        // DELTA (`native_routines.quantity_delta`, edx = delta); an insert
        // has neither argument. So the lane IS "both arguments present".
        let request = if slot.is_some() && quantity_address.is_some() {
            REQUEST_EXISTING_STACK_DELTA
        } else {
            REQUEST_NATIVE_INSERT
        };
        let _ = self.record(self.memory.write_u32(self.cells.request, request));
    }

    fn native_done(&mut self) -> bool {
        self.record(self.memory.read_u32(self.cells.done))
            .is_some_and(|done| done == 1)
    }

    fn native_result(&mut self) -> u32 {
        self.record(self.memory.read_u32(self.cells.result))
            .unwrap_or(EMPTY_SLOT)
    }

    fn clear_request(&mut self) {
        let _ = self.record(self.memory.write_u32(self.cells.request, 0));
    }
}

#[cfg(test)]
mod tests {
    use super::super::contract::contract;
    use super::super::mem::FakeMemory;
    use super::*;

    #[test]
    fn entry_address_splits_across_the_two_banks() {
        assert_eq!(entry_address(0, 2, 0x100, 0x200, 0x10), 0x100);
        assert_eq!(entry_address(1, 2, 0x100, 0x200, 0x10), 0x110);
        assert_eq!(entry_address(2, 2, 0x100, 0x200, 0x10), 0x200);
        assert_eq!(entry_address(3, 2, 0x100, 0x200, 0x10), 0x210);
    }

    fn laid_out_inventory() -> (FakeMemory, u64, u32) {
        let c = contract();
        let base = 0x4000_0000;
        let g = c.geometry;
        let memory = FakeMemory::new();
        let inventory = 0x9000_0000u64;
        let primary = 0x9100_0000u64;
        let secondary = 0x9200_0000u64;
        memory.store(
            base + c.state_cell("inventory").unwrap().rva,
            &inventory.to_le_bytes(),
        );
        memory.store(inventory + g.split, &2u32.to_le_bytes());
        memory.store(inventory + g.last, &3u32.to_le_bytes());
        memory.store(inventory + g.primary_array, &primary.to_le_bytes());
        memory.store(inventory + g.secondary_array, &secondary.to_le_bytes());
        let normalized = c.descriptor.goods_normalized_prefix | 0x384;
        let entry = entry_address(1, 2, primary, secondary, g.record_stride);
        memory.store(entry + g.record_id, &normalized.to_le_bytes());
        memory.store(entry + g.record_quantity, &12u32.to_le_bytes());
        for slot in [0u64, 2, 3] {
            let e = entry_address(slot, 2, primary, secondary, g.record_stride);
            memory.store(e + g.record_id, &0xDEADu32.to_le_bytes());
            memory.store(e + g.record_quantity, &1u32.to_le_bytes());
        }
        (memory, base, normalized)
    }

    #[test]
    fn find_stack_walks_the_geometry() {
        let (memory, base, normalized) = laid_out_inventory();
        let g = contract().geometry;
        let mut guest = GuestRuntime::new(memory, base).unwrap();
        assert!(guest.inventory_ready());
        let stack = guest.find_stack(normalized).unwrap();
        assert!(stack.exists);
        assert_eq!(stack.quantity, 12);
        assert_eq!(stack.slot, Some(1));
        let expected =
            entry_address(1, 2, 0x9100_0000, 0x9200_0000, g.record_stride) + g.record_quantity;
        assert_eq!(stack.quantity_address, Some(expected));
        assert!(guest.take_error().is_none());
    }

    #[test]
    fn target_level_reads_only_recognized_weapon_families() {
        let (memory, base, _normalized) = laid_out_inventory();
        let c = contract();
        let g = c.geometry;
        let inventory = memory
            .read_u64(base + c.state_cell("inventory").unwrap().rva)
            .unwrap();
        let primary = memory.read_u64(inventory + g.primary_array).unwrap();
        let secondary = memory.read_u64(inventory + g.secondary_array).unwrap();
        let ludwig = entry_address(0, 2, primary, secondary, g.record_stride);
        memory.store(ludwig + g.record_id, &8_100_100u32.to_le_bytes());
        let lookalike = entry_address(2, 2, primary, secondary, g.record_stride);
        memory.store(lookalike + g.record_id, &1_000_900u32.to_le_bytes());
        let guest = GuestRuntime::new(memory, base).unwrap();
        assert_eq!(guest.target_weapon_level(), Some(1));
    }

    #[test]
    fn missing_stack_returns_a_non_existent_view_not_none() {
        let (memory, base, _normalized) = laid_out_inventory();
        let mut guest = GuestRuntime::new(memory, base).unwrap();
        let stack = guest.find_stack(0x4000_9999).unwrap();
        assert!(!stack.exists);
    }

    #[test]
    fn unhydrated_inventory_reports_not_ready() {
        let c = contract();
        let base = 0x4000_0000;
        let memory = FakeMemory::new();
        memory.store(
            base + c.state_cell("inventory").unwrap().rva,
            &0u64.to_le_bytes(),
        );
        let mut guest = GuestRuntime::new(memory, base).unwrap();
        assert!(!guest.inventory_ready());
        assert!(guest.find_stack(0x4000_0384).is_none());
        assert!(guest.take_error().is_none());
    }

    #[test]
    fn a_failed_read_is_captured_and_degrades_to_unavailable() {
        let base = 0x4000_0000;
        let memory = FakeMemory::new();
        let mut guest = GuestRuntime::new(memory, base).unwrap();
        assert!(!guest.inventory_ready());
        assert!(guest.take_error().is_some());
    }

    #[test]
    fn death_link_kill_requires_a_captured_pointer_then_writes_only_current_hp() {
        let c = contract();
        let base = 0x4000_0000;
        let status = 0x9000_0000u64;
        let memory = FakeMemory::new();
        memory.store(
            base + c.state_cell("player_status").unwrap().rva,
            &0u64.to_le_bytes(),
        );
        let guest = GuestRuntime::new(memory, base).unwrap();
        assert!(!guest.death_link_kill().unwrap());

        guest.memory().store(
            base + c.state_cell("player_status").unwrap().rva,
            &status.to_le_bytes(),
        );
        guest.memory().store(status + 0xF8, &777u32.to_le_bytes());
        assert!(guest.death_link_kill().unwrap());
        assert_eq!(guest.memory().read_u32(status + 0xF8).unwrap(), 0);
        assert!(
            !guest.death_link_kill().unwrap(),
            "a second link waits for respawn"
        );
    }

    /// clients#433: the `request` value IS the lane. Both arguments present
    /// (slot + record pointer) is the contract's existing-stack delta branch
    /// (`request = 2`, `quantity` read as a delta); neither is the insert
    /// (`request = 1`).
    #[test]
    fn the_request_value_names_the_lane() {
        let c = contract();
        let base = 0x4000_0000;
        let request_rva = base + c.state_cell("request").unwrap().rva;
        let quantity_rva = base + c.state_cell("quantity").unwrap().rva;
        let slot_rva = base + c.state_cell("slot_index").unwrap().rva;
        let pointer_rva = base + c.state_cell("item_quantity_pointer").unwrap().rva;
        let descriptor = ItemGrantDescriptor::new(0xB000_0384, 0x4000_0384);

        let mut guest = GuestRuntime::new(FakeMemory::new(), base).unwrap();
        guest.queue_native(&descriptor, 2, Some(3), Some(0x1000), false);
        assert_eq!(
            guest.memory().read_u32(request_rva).unwrap(),
            REQUEST_EXISTING_STACK_DELTA
        );
        // The delta, not the resulting total, and both cave arguments.
        assert_eq!(guest.memory().read_u32(quantity_rva).unwrap(), 2);
        assert_eq!(guest.memory().read_u32(slot_rva).unwrap(), 3);
        assert_eq!(guest.memory().read_u64(pointer_rva).unwrap(), 0x1000);

        let mut guest = GuestRuntime::new(FakeMemory::new(), base).unwrap();
        guest.queue_native(&descriptor, 2, None, None, false);
        assert_eq!(
            guest.memory().read_u32(request_rva).unwrap(),
            REQUEST_NATIVE_INSERT
        );
        assert_eq!(guest.memory().read_u32(slot_rva).unwrap(), EMPTY_SLOT);
        assert_eq!(guest.memory().read_u64(pointer_rva).unwrap(), 0);
    }

    #[test]
    fn queue_native_writes_request_last_and_stages_the_descriptor() {
        let c = contract();
        let base = 0x4000_0000;
        let memory = FakeMemory::new();
        let mut guest = GuestRuntime::new(memory, base).unwrap();
        let descriptor = ItemGrantDescriptor::new(0xB000_0384, 0x4000_0384);
        guest.queue_native(&descriptor, 3, Some(5), Some(0xABCD), false);
        assert!(guest.request_pending());
        let staged = guest
            .memory()
            .read(base + c.state_cell("descriptor").unwrap().rva, 32)
            .unwrap();
        assert_eq!(&staged[0x00..0x04], &0xB000_0384u32.to_le_bytes());
        assert_eq!(&staged[0x10..0x14], &0x4000_0384u32.to_le_bytes());
        assert_eq!(guest.native_result(), EMPTY_SLOT);
        assert!(!guest.native_done());
    }
}
