//! Read-only instrumentation of Bloodborne's native `ItemGrant` boundary.

use anyhow::{Context, Result, bail};

use super::install::ThreadController;
use super::mem::ProcessMemory;

const ITEM_GRANT_RVA: u64 = 0x14D_A0A0;
const ITEM_GRANT_RETURN_RVA: u64 = ITEM_GRANT_RVA + 8;
const ITEM_GRANT_ORIGINAL: &[u8] = &[0x55, 0x48, 0x89, 0xE5, 0x41, 0x57, 0x41, 0x56];
const PROBE_CAVE_RVA: u64 = 0x50D_BD40;
const PROBE_STATE_RVA: u64 = 0x50D_BE80;
const PROBE_CAVE_CAPACITY: usize = 0x80;
const RING_CAPACITY: usize = 8;
const RING_ENTRY_SIZE: usize = 0x40;
const RING_RVA: u64 = PROBE_STATE_RVA + 0x40;
const PROBE_STATE_SIZE: usize = 0x40 + RING_CAPACITY * RING_ENTRY_SIZE;

const RESERVATION_SEQUENCE: u64 = PROBE_STATE_RVA;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemGrantCallSnapshot {
    pub sequence: u64,
    pub inventory: u64,
    pub descriptor_address: u64,
    pub quantity: u32,
    pub raw_id: u32,
    pub internal_pointer: u64,
    pub normalized_id: u32,
    pub caller: u64,
}

fn disp32(target: u64, next: u64) -> Result<[u8; 4]> {
    let displacement = i64::try_from(target)? - i64::try_from(next)?;
    Ok(i32::try_from(displacement)
        .context("probe displacement exceeds rel32")?
        .to_le_bytes())
}

fn rip(out: &mut Vec<u8>, prefix: &[u8], target: u64) -> Result<()> {
    let next = PROBE_CAVE_RVA + out.len() as u64 + prefix.len() as u64 + 4;
    out.extend_from_slice(prefix);
    out.extend_from_slice(&disp32(target, next)?);
    Ok(())
}

fn cave_bytes() -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.push(0x9C); // preserve flags
    out.push(0x50); // preserve rax
    out.push(0x51); // preserve rcx
    out.extend_from_slice(&[0x41, 0x50]); // preserve r8
    out.extend_from_slice(&[0x48, 0xC7, 0xC0, 1, 0, 0, 0]); // mov rax, 1
    rip(
        &mut out,
        &[0xF0, 0x48, 0x0F, 0xC1, 0x05],
        RESERVATION_SEQUENCE,
    )?; // lock xadd [sequence], rax
    out.extend_from_slice(&[0x48, 0xFF, 0xC0]); // inc rax (published sequence)
    out.extend_from_slice(&[0x48, 0x89, 0xC1]); // mov rcx, rax
    out.extend_from_slice(&[0x83, 0xE1, (RING_CAPACITY - 1) as u8]); // and ecx, mask
    out.extend_from_slice(&[0x48, 0xC1, 0xE1, 6]); // shl rcx, 6
    rip(&mut out, &[0x4C, 0x8D, 0x05], RING_RVA)?; // lea r8, [ring]
    out.extend_from_slice(&[0x49, 0x01, 0xC8]); // add r8, rcx
    out.extend_from_slice(&[0x49, 0xC7, 0x00, 0, 0, 0, 0]); // invalidate slot
    out.extend_from_slice(&[0x49, 0x89, 0x78, 0x08]); // inventory
    out.extend_from_slice(&[0x49, 0x89, 0x70, 0x10]); // descriptor address
    out.extend_from_slice(&[0x41, 0x89, 0x50, 0x18]); // quantity
    out.extend_from_slice(&[0x8B, 0x0E, 0x41, 0x89, 0x48, 0x1C]); // raw id
    out.extend_from_slice(&[0x48, 0x8B, 0x4E, 0x08, 0x49, 0x89, 0x48, 0x20]); // pointer
    out.extend_from_slice(&[0x8B, 0x4E, 0x10, 0x41, 0x89, 0x48, 0x28]); // normalized id
    out.extend_from_slice(&[0x48, 0x8B, 0x4C, 0x24, 0x20, 0x49, 0x89, 0x48, 0x30]); // caller
    out.extend_from_slice(&[0x49, 0x89, 0x00]); // publish sequence last
    out.extend_from_slice(&[0x41, 0x58, 0x59, 0x58, 0x9D]);
    out.extend_from_slice(ITEM_GRANT_ORIGINAL);
    let next = PROBE_CAVE_RVA + out.len() as u64 + 5;
    out.push(0xE9);
    out.extend_from_slice(&disp32(ITEM_GRANT_RETURN_RVA, next)?);
    anyhow::ensure!(
        out.len() <= PROBE_CAVE_CAPACITY,
        "ItemGrant probe cave overflow"
    );
    Ok(out)
}

fn detour_bytes() -> Result<Vec<u8>> {
    let mut out = vec![0xE9];
    out.extend_from_slice(&disp32(PROBE_CAVE_RVA, ITEM_GRANT_RVA + 5)?);
    out.extend_from_slice(&[0x90, 0x90, 0x90]);
    Ok(out)
}

/// Install the optional probe. All preflight checks complete before ItemGrant
/// is touched; a mismatch leaves the diagnostic inactive.
pub fn install(
    memory: &impl ProcessMemory,
    base: u64,
    threads: &mut impl ThreadController,
) -> Result<()> {
    let hook = base + ITEM_GRANT_RVA;
    if memory.read(hook, ITEM_GRANT_ORIGINAL.len())? != ITEM_GRANT_ORIGINAL {
        bail!("ItemGrant probe prologue mismatch");
    }
    if memory
        .read(base + PROBE_CAVE_RVA, PROBE_CAVE_CAPACITY)?
        .iter()
        .any(|b| *b != 0)
    {
        bail!("ItemGrant probe cave is occupied");
    }
    if memory
        .read(base + PROBE_STATE_RVA, PROBE_STATE_SIZE)?
        .iter()
        .any(|b| *b != 0)
    {
        bail!("ItemGrant probe state is occupied");
    }
    memory.write(base + PROBE_STATE_RVA, &[0; PROBE_STATE_SIZE])?;
    memory.write(base + PROBE_CAVE_RVA, &cave_bytes()?)?;

    threads
        .suspend_all()
        .context("suspending for ItemGrant probe")?;
    let result = (|| {
        let rips = threads.instruction_pointers()?;
        if rips.iter().any(|rip| *rip >= hook && *rip < hook + 8) {
            bail!("a guest thread occupied the ItemGrant probe window");
        }
        memory.write(hook, &detour_bytes()?)?;
        Ok(())
    })();
    let resumed = threads
        .resume_all()
        .context("resuming after ItemGrant probe");
    result.and(resumed)
}

pub fn snapshots(memory: &impl ProcessMemory, base: u64) -> Vec<ItemGrantCallSnapshot> {
    let mut snapshots = Vec::new();
    for slot in 0..RING_CAPACITY {
        let address = base + RING_RVA + (slot * RING_ENTRY_SIZE) as u64;
        let first = match memory.read_u64(address) {
            Ok(sequence) if sequence != 0 => sequence,
            _ => continue,
        };
        let Ok(bytes) = memory.read(address, RING_ENTRY_SIZE) else {
            continue;
        };
        let u32_at =
            |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let u64_at =
            |offset: usize| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        if memory.read_u64(address).ok() != Some(first) || u64_at(0) != first {
            continue;
        }
        snapshots.push(ItemGrantCallSnapshot {
            sequence: first,
            inventory: u64_at(0x08),
            descriptor_address: u64_at(0x10),
            quantity: u32_at(0x18),
            raw_id: u32_at(0x1C),
            internal_pointer: u64_at(0x20),
            normalized_id: u32_at(0x28),
            caller: u64_at(0x30),
        });
    }
    snapshots.sort_unstable_by_key(|snapshot| snapshot.sequence);
    snapshots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cave_is_bounded_and_replays_the_exact_prologue() {
        let cave = cave_bytes().unwrap();
        assert!(cave.len() <= PROBE_CAVE_CAPACITY);
        assert!(cave.windows(8).any(|window| window == ITEM_GRANT_ORIGINAL));
        assert_eq!(detour_bytes().unwrap().len(), 8);
    }
}
