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
const PROBE_STATE_SIZE: usize = 0x38;

const SEQUENCE: u64 = PROBE_STATE_RVA;
const INVENTORY: u64 = PROBE_STATE_RVA + 0x08;
const DESCRIPTOR_ADDRESS: u64 = PROBE_STATE_RVA + 0x10;
const QUANTITY: u64 = PROBE_STATE_RVA + 0x18;
const RAW_ID: u64 = PROBE_STATE_RVA + 0x1C;
const INTERNAL_POINTER: u64 = PROBE_STATE_RVA + 0x20;
const NORMALIZED_ID: u64 = PROBE_STATE_RVA + 0x28;
const CALLER: u64 = PROBE_STATE_RVA + 0x30;

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
    rip(&mut out, &[0x48, 0x89, 0x3D], INVENTORY)?;
    rip(&mut out, &[0x48, 0x89, 0x35], DESCRIPTOR_ADDRESS)?;
    rip(&mut out, &[0x89, 0x15], QUANTITY)?;
    out.push(0x50); // preserve rax
    out.extend_from_slice(&[0x8B, 0x06]);
    rip(&mut out, &[0x89, 0x05], RAW_ID)?;
    out.extend_from_slice(&[0x48, 0x8B, 0x46, 0x08]);
    rip(&mut out, &[0x48, 0x89, 0x05], INTERNAL_POINTER)?;
    out.extend_from_slice(&[0x8B, 0x46, 0x10]);
    rip(&mut out, &[0x89, 0x05], NORMALIZED_ID)?;
    out.extend_from_slice(&[0x48, 0x8B, 0x44, 0x24, 0x08]);
    rip(&mut out, &[0x48, 0x89, 0x05], CALLER)?;
    out.push(0x58);
    out.push(0x9C); // preserve flags while publishing
    rip(&mut out, &[0xF0, 0x48, 0xFF, 0x05], SEQUENCE)?;
    out.push(0x9D);
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

pub fn snapshot(memory: &impl ProcessMemory, base: u64) -> Option<ItemGrantCallSnapshot> {
    let address = base + PROBE_STATE_RVA;
    let first = memory.read_u64(address).ok()?;
    if first == 0 {
        return None;
    }
    let bytes = memory.read(address, PROBE_STATE_SIZE).ok()?;
    if first != memory.read_u64(address).ok()? {
        return None;
    }
    let u32_at = |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let u64_at = |offset: usize| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
    Some(ItemGrantCallSnapshot {
        sequence: first,
        inventory: u64_at(0x08),
        descriptor_address: u64_at(0x10),
        quantity: u32_at(0x18),
        raw_id: u32_at(0x1C),
        internal_pointer: u64_at(0x20),
        normalized_id: u32_at(0x28),
        caller: u64_at(0x30),
    })
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
