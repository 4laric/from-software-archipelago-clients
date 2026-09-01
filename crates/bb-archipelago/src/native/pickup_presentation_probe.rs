//! Observation-only lifecycle probes for Bloodborne's pickup-dialog classes.

use super::install::ThreadController;
use super::mem::ProcessMemory;
use anyhow::{Context, Result, bail};

const STATE_SIZE: usize = 0x20;
const CAVE_CAPACITY: usize = 0x40;
const ZERO_START: u64 = 0x50D_B800;
const ZERO_END: u64 = 0x50D_BA00;

#[derive(Clone, Copy)]
struct Site {
    name: &'static str,
    entry_rva: u64,
    expected: &'static [u8],
    resume_rva: u64,
    cave_rva: u64,
    state_rva: u64,
}

const PROLOGUE: &[u8] = &[0x55, 0x48, 0x89, 0xE5, 0x41, 0x57];
const TAIL_JUMP: &[u8] = &[0xE9, 0xCB, 0x90, 0x92, 0xFF];
const SITES: [Site; 4] = [
    Site {
        name: "FrpgMenuDlgGetItem",
        entry_rva: 0x166_1600,
        expected: PROLOGUE,
        resume_rva: 0x166_1606,
        cave_rva: 0x50D_B800,
        state_rva: 0x50D_B840,
    },
    Site {
        name: "FrpgMenuDlgObjGetItemData",
        entry_rva: 0x166_CCC0,
        expected: PROLOGUE,
        resume_rva: 0x166_CCC6,
        cave_rva: 0x50D_B860,
        state_rva: 0x50D_B8A0,
    },
    Site {
        name: "FrpgMenuDlgItemGet",
        entry_rva: 0x16D_6710,
        expected: PROLOGUE,
        resume_rva: 0x16D_6716,
        cave_rva: 0x50D_B8C0,
        state_rva: 0x50D_B900,
    },
    // This entry is a tail-jump thunk; resume at the original target.
    Site {
        name: "FrpgMenuDlgItemGetPlate",
        entry_rva: 0x16D_8CF0,
        expected: TAIL_JUMP,
        resume_rva: 0x100_1DC0,
        cave_rva: 0x50D_B920,
        state_rva: 0x50D_B960,
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickupPresentationSnapshot {
    pub site: &'static str,
    pub caller_rva: u64,
    pub sequence: u64,
    pub thread_stack_token: u64,
    pub inventory: u64,
    pub descriptor_address: u64,
    pub quantity: u32,
    pub candidate_message_context: u64,
    pub candidate_icon_context: u64,
    pub candidate_aux_context: u64,
    pub result: u64,
    pub descriptor: Option<Vec<u8>>,
}

fn rel32(target: u64, next: u64) -> Result<[u8; 4]> {
    let displacement = i64::try_from(target)? - i64::try_from(next)?;
    Ok(i32::try_from(displacement)
        .context("pickup lifecycle probe displacement exceeds rel32")?
        .to_le_bytes())
}

fn patch_bytes(site: Site) -> Result<Vec<u8>> {
    let mut bytes = vec![0xE9];
    bytes.extend_from_slice(&rel32(site.cave_rva, site.entry_rva + 5)?);
    bytes.resize(site.expected.len(), 0x90);
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
    rip(&mut out, site, &[0x48, 0x89, 0x25], 0x08)?; // rsp
    rip(&mut out, site, &[0x48, 0x89, 0x3D], 0x10)?; // rdi / this
    rip(&mut out, site, &[0x48, 0x89, 0x35], 0x18)?; // rsi
    out.push(0x9C);
    rip(&mut out, site, &[0xF0, 0x48, 0xFF, 0x05], 0x00)?;
    out.push(0x9D);
    if site.expected == PROLOGUE {
        out.extend_from_slice(site.expected);
    }
    let next = site.cave_rva + out.len() as u64 + 5;
    out.push(0xE9);
    out.extend_from_slice(&rel32(site.resume_rva, next)?);
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
            start >= ZERO_START && end <= ZERO_END,
            "pickup lifecycle probe allocation leaves the executable zero census"
        );
    }
    for (index, &(a, b)) in ranges.iter().enumerate() {
        for &(c, d) in &ranges[index + 1..] {
            anyhow::ensure!(
                b <= c || d <= a,
                "pickup lifecycle probe allocations overlap"
            );
        }
    }
    Ok(())
}

pub fn install(
    memory: &impl ProcessMemory,
    base: u64,
    threads: &mut impl ThreadController,
) -> Result<()> {
    validate_layout()?;
    for site in SITES {
        let found = memory.read(base + site.entry_rva, site.expected.len())?;
        if found != site.expected {
            bail!(
                "{} exact entry mismatch: expected {:02X?}, found {:02X?}",
                site.name,
                site.expected,
                found
            );
        }
        if memory
            .read(base + site.cave_rva, CAVE_CAPACITY)?
            .iter()
            .any(|b| *b != 0)
        {
            bail!("{} cave is occupied", site.name);
        }
        if memory
            .read(base + site.state_rva, STATE_SIZE)?
            .iter()
            .any(|b| *b != 0)
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
        .context("suspending for pickup lifecycle probe")?;
    let result = (|| {
        let rips = threads.instruction_pointers()?;
        for site in SITES {
            if rips.iter().any(|rip| {
                *rip >= base + site.entry_rva
                    && *rip < base + site.entry_rva + site.expected.len() as u64
            }) {
                bail!("a guest thread occupied the {} patch window", site.name);
            }
        }
        let mut patched: Vec<Site> = Vec::new();
        for site in SITES {
            if let Err(error) = memory.write(base + site.entry_rva, &patch_bytes(site)?) {
                for previous in patched {
                    let _ = memory.write(base + previous.entry_rva, previous.expected);
                }
                return Err(error);
            }
            patched.push(site);
        }
        Ok(())
    })();
    let resumed = threads
        .resume_all()
        .context("resuming after pickup lifecycle probe");
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
    let sequence = memory.read_u64(address).ok()?;
    if sequence == 0 {
        return None;
    }
    let bytes = memory.read(address, STATE_SIZE).ok()?;
    if sequence != memory.read_u64(address).ok()? {
        return None;
    }
    let u64_at = |offset: usize| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
    Some(PickupPresentationSnapshot {
        site: site.name,
        caller_rva: site.entry_rva,
        sequence,
        thread_stack_token: u64_at(8),
        inventory: u64_at(16),
        descriptor_address: u64_at(24),
        quantity: 0,
        candidate_message_context: 0,
        candidate_icon_context: 0,
        candidate_aux_context: 0,
        result: 0,
        descriptor: None,
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
        let m = FakeMemory::new();
        for s in SITES {
            m.store(base + s.entry_rva, s.expected);
            m.store(base + s.cave_rva, &[0; CAVE_CAPACITY]);
            m.store(base + s.state_rva, &[0; STATE_SIZE]);
        }
        m
    }
    #[test]
    fn layout_and_caves_are_bounded() {
        validate_layout().unwrap();
        for s in SITES {
            assert!(cave_bytes(s).unwrap().len() <= CAVE_CAPACITY);
        }
    }
    #[test]
    fn installs_all_sites() {
        let base = 0x4000_0000;
        let m = image(base);
        let mut t = FakeThreads::default();
        install(&m, base, &mut t).unwrap();
        for s in SITES {
            assert_eq!(
                m.read(base + s.entry_rva, s.expected.len()).unwrap(),
                patch_bytes(s).unwrap()
            );
        }
    }
    #[test]
    fn mismatch_refuses_before_patching() {
        let base = 0x4000_0000;
        let m = image(base);
        m.store(base + SITES[3].entry_rva, &[0x90; 5]);
        assert!(install(&m, base, &mut FakeThreads::default()).is_err());
        assert_eq!(
            m.read(base + SITES[0].entry_rva, SITES[0].expected.len())
                .unwrap(),
            SITES[0].expected
        );
    }
}
