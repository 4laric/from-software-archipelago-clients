//! Observation-only probes for the two vanilla pickup-side `ItemGrant` calls.
//!
//! Playtest.32 established that ItemGrant's ordinary client caller is not the
//! presentation seam. These two call edges were the only vanilla callers in
//! that capture. Each detour replays the original call and records its six
//! integer argument registers, stack identity and return value. It never calls
//! a message routine or changes an argument/result.

use anyhow::{Context, Result, bail};

use super::install::ThreadController;
use super::mem::ProcessMemory;

const ITEM_GRANT_RVA: u64 = 0x14D_A0A0;
const STATE_SIZE: usize = 0x48;
const CAVE_CAPACITY: usize = 0xC0;
// Live CUSA03173 01.09 census on playtest.33 found this entire span zeroed:
// 0x50DC03C..0x50DC7A8. Keep every probe allocation inside it. The original
// second cave started at 0x50DBFC0 and crossed 0x50DC000 into executable data.
const OBSERVED_ZERO_START: u64 = 0x50D_C03C;
const OBSERVED_ZERO_END: u64 = 0x50D_C7A8;

#[derive(Clone, Copy)]
struct Site {
    name: &'static str,
    call_rva: u64,
    return_rva: u64,
    cave_rva: u64,
    state_rva: u64,
}

const SITES: [Site; 2] = [
    Site {
        name: "vanilla_pickup_17D93FE",
        call_rva: 0x17D_93F9,
        return_rva: 0x17D_93FE,
        cave_rva: 0x50D_C100,
        state_rva: 0x50D_C280,
    },
    Site {
        name: "vanilla_pickup_14DA9FF",
        call_rva: 0x14D_A9FA,
        return_rva: 0x14D_A9FF,
        cave_rva: 0x50D_C1C0,
        state_rva: 0x50D_C300,
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickupPresentationSnapshot {
    pub site: &'static str,
    pub caller_rva: u64,
    pub sequence: u64,
    /// Guest RSP at the original call edge. This is an opaque per-thread
    /// correlation token, not an OS thread id.
    pub thread_stack_token: u64,
    pub inventory: u64,
    pub descriptor_address: u64,
    pub quantity: u32,
    /// Ambient fourth through sixth integer registers. They are intentionally
    /// preserved as candidates rather than claimed to be message/icon fields.
    pub candidate_message_context: u64,
    pub candidate_icon_context: u64,
    pub candidate_aux_context: u64,
    pub result: u64,
    pub descriptor: Option<Vec<u8>>,
}

fn rel32(target: u64, next: u64) -> Result<[u8; 4]> {
    let displacement = i64::try_from(target)? - i64::try_from(next)?;
    Ok(i32::try_from(displacement)
        .context("pickup presentation probe displacement exceeds rel32")?
        .to_le_bytes())
}

fn direct_call_bytes(site: Site) -> Result<[u8; 5]> {
    let mut bytes = [0u8; 5];
    bytes[0] = 0xE8;
    bytes[1..].copy_from_slice(&rel32(ITEM_GRANT_RVA, site.return_rva)?);
    Ok(bytes)
}

fn detour_bytes(site: Site) -> Result<[u8; 5]> {
    let mut bytes = [0u8; 5];
    bytes[0] = 0xE9;
    bytes[1..].copy_from_slice(&rel32(site.cave_rva, site.return_rva)?);
    Ok(bytes)
}

fn rip(out: &mut Vec<u8>, site: Site, prefix: &[u8], state_offset: u64) -> Result<()> {
    let next = site.cave_rva + out.len() as u64 + prefix.len() as u64 + 4;
    out.extend_from_slice(prefix);
    out.extend_from_slice(&rel32(site.state_rva + state_offset, next)?);
    Ok(())
}

fn cave_bytes(site: Site) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    rip(&mut out, site, &[0x48, 0x89, 0x25], 0x08)?; // rsp token
    rip(&mut out, site, &[0x48, 0x89, 0x3D], 0x10)?; // rdi
    rip(&mut out, site, &[0x48, 0x89, 0x35], 0x18)?; // rsi
    rip(&mut out, site, &[0x89, 0x15], 0x20)?; // edx
    rip(&mut out, site, &[0x48, 0x89, 0x0D], 0x28)?; // rcx
    rip(&mut out, site, &[0x4C, 0x89, 0x05], 0x30)?; // r8
    rip(&mut out, site, &[0x4C, 0x89, 0x0D], 0x38)?; // r9
    let next = site.cave_rva + out.len() as u64 + 5;
    out.push(0xE8); // replay the exact original call
    out.extend_from_slice(&rel32(ITEM_GRANT_RVA, next)?);
    rip(&mut out, site, &[0x48, 0x89, 0x05], 0x40)?; // rax result
    out.push(0x9C); // preserve ItemGrant's return flags while publishing
    rip(&mut out, site, &[0xF0, 0x48, 0xFF, 0x05], 0x00)?;
    out.push(0x9D);
    let next = site.cave_rva + out.len() as u64 + 5;
    out.push(0xE9);
    out.extend_from_slice(&rel32(site.return_rva, next)?);
    anyhow::ensure!(out.len() <= CAVE_CAPACITY, "{} cave overflow", site.name);
    Ok(out)
}

fn validate_layout() -> Result<()> {
    let mut ranges = Vec::new();
    for site in SITES {
        ranges.push((site.cave_rva, site.cave_rva + CAVE_CAPACITY as u64));
        ranges.push((site.state_rva, site.state_rva + STATE_SIZE as u64));
    }
    for &(start, end) in &ranges {
        anyhow::ensure!(
            start >= OBSERVED_ZERO_START && end <= OBSERVED_ZERO_END,
            "pickup presentation probe allocation leaves the live zero census"
        );
    }
    for (index, &(left_start, left_end)) in ranges.iter().enumerate() {
        for &(right_start, right_end) in &ranges[index + 1..] {
            anyhow::ensure!(
                left_end <= right_start || right_end <= left_start,
                "pickup presentation probe allocations overlap"
            );
        }
    }
    Ok(())
}

/// Install both optional call-edge probes. Every byte/cave/state preflight is
/// completed before any write; a mismatch leaves both sites untouched.
pub fn install(
    memory: &impl ProcessMemory,
    base: u64,
    threads: &mut impl ThreadController,
) -> Result<()> {
    validate_layout()?;
    for site in SITES {
        let found = memory.read(base + site.call_rva, 5)?;
        let expected = direct_call_bytes(site)?;
        if found != expected {
            bail!(
                "{} exact call mismatch: expected {:02X?}, found {:02X?}",
                site.name,
                expected,
                found
            );
        }
        if memory
            .read(base + site.cave_rva, CAVE_CAPACITY)?
            .iter()
            .any(|byte| *byte != 0)
        {
            bail!("{} cave is occupied", site.name);
        }
        if memory
            .read(base + site.state_rva, STATE_SIZE)?
            .iter()
            .any(|byte| *byte != 0)
        {
            bail!("{} state is occupied", site.name);
        }
    }

    for site in SITES {
        memory.write(base + site.state_rva, &[0; STATE_SIZE])?;
        memory.write(base + site.cave_rva, &cave_bytes(site)?)?;
    }

    threads
        .suspend_all()
        .context("suspending for pickup presentation probe")?;
    let result = (|| {
        let rips = threads.instruction_pointers()?;
        for site in SITES {
            if rips
                .iter()
                .any(|rip| *rip >= base + site.call_rva && *rip < base + site.return_rva)
            {
                bail!("a guest thread occupied the {} patch window", site.name);
            }
        }
        let mut patched: Vec<Site> = Vec::new();
        for site in SITES {
            if let Err(error) = memory.write(base + site.call_rva, &detour_bytes(site)?) {
                for previous in patched {
                    let _ = memory.write(base + previous.call_rva, &direct_call_bytes(previous)?);
                }
                return Err(error);
            }
            patched.push(site);
        }
        Ok(())
    })();
    let resumed = threads
        .resume_all()
        .context("resuming after pickup presentation probe");
    result.and(resumed)
}

pub fn snapshots(memory: &impl ProcessMemory, base: u64) -> Vec<PickupPresentationSnapshot> {
    SITES
        .iter()
        .filter_map(|site| snapshot(memory, base, *site))
        .collect()
}

fn snapshot(
    memory: &impl ProcessMemory,
    base: u64,
    site: Site,
) -> Option<PickupPresentationSnapshot> {
    let address = base + site.state_rva;
    let first = memory.read_u64(address).ok()?;
    if first == 0 {
        return None;
    }
    let bytes = memory.read(address, STATE_SIZE).ok()?;
    if first != memory.read_u64(address).ok()? {
        return None;
    }
    let u32_at = |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let u64_at = |offset: usize| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
    let descriptor_address = u64_at(0x18);
    let descriptor = (descriptor_address != 0)
        .then(|| memory.read(descriptor_address, 24).ok())
        .flatten();
    Some(PickupPresentationSnapshot {
        site: site.name,
        caller_rva: site.return_rva,
        sequence: first,
        thread_stack_token: u64_at(0x08),
        inventory: u64_at(0x10),
        descriptor_address,
        quantity: u32_at(0x20),
        candidate_message_context: u64_at(0x28),
        candidate_icon_context: u64_at(0x30),
        candidate_aux_context: u64_at(0x38),
        result: u64_at(0x40),
        descriptor,
    })
}

#[cfg(test)]
mod tests {
    use super::super::mem::FakeMemory;
    use super::*;

    #[derive(Default)]
    struct FakeThreads {
        suspended: bool,
    }

    impl ThreadController for FakeThreads {
        fn suspend_all(&mut self) -> Result<usize> {
            self.suspended = true;
            Ok(1)
        }

        fn resume_all(&mut self) -> Result<()> {
            self.suspended = false;
            Ok(())
        }

        fn instruction_pointers(&mut self) -> Result<Vec<u64>> {
            assert!(self.suspended);
            Ok(Vec::new())
        }
    }

    fn image(base: u64) -> FakeMemory {
        let memory = FakeMemory::new();
        for site in SITES {
            memory.store(base + site.call_rva, &direct_call_bytes(site).unwrap());
            memory.store(base + site.cave_rva, &[0; CAVE_CAPACITY]);
            memory.store(base + site.state_rva, &[0; STATE_SIZE]);
        }
        memory
    }

    #[test]
    fn observed_return_addresses_encode_direct_calls_to_item_grant() {
        assert_eq!(
            direct_call_bytes(SITES[0]).unwrap(),
            [0xE8, 0xA2, 0x0C, 0xD0, 0xFF]
        );
        assert_eq!(
            direct_call_bytes(SITES[1]).unwrap(),
            [0xE8, 0xA1, 0xF6, 0xFF, 0xFF]
        );
    }

    #[test]
    fn caves_are_bounded_and_call_item_grant_before_returning() {
        for site in SITES {
            let cave = cave_bytes(site).unwrap();
            assert!(cave.len() <= CAVE_CAPACITY);
            assert!(cave.contains(&0xE8));
            assert_eq!(detour_bytes(site).unwrap()[0], 0xE9);
        }
    }

    #[test]
    fn cave_and_state_claims_are_disjoint_and_inside_the_live_zero_census() {
        validate_layout().unwrap();
    }

    #[test]
    fn exact_preflight_installs_both_edges_in_one_suspend_cycle() {
        let base = 0x4000_0000;
        let memory = image(base);
        let mut threads = FakeThreads::default();
        install(&memory, base, &mut threads).unwrap();
        assert!(!threads.suspended);
        for site in SITES {
            assert_eq!(
                memory.read(base + site.call_rva, 5).unwrap(),
                detour_bytes(site).unwrap()
            );
        }
    }

    #[test]
    fn one_bad_call_byte_refuses_before_any_patch() {
        let base = 0x4000_0000;
        let memory = image(base);
        memory.store(base + SITES[1].call_rva, &[0x90; 5]);
        let first_original = direct_call_bytes(SITES[0]).unwrap();
        let error = install(&memory, base, &mut FakeThreads::default()).unwrap_err();
        assert!(format!("{error:#}").contains("exact call mismatch"));
        assert_eq!(
            memory.read(base + SITES[0].call_rva, 5).unwrap(),
            first_original
        );
    }
}
