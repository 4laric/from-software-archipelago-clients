//! Fail-closed entry probe for the CUSA03173 01.09 category-8 allocator.
//!
//! This is the spec's option-B capture.  It records the complete entry frame
//! and deliberately does not swap the game's return address; object discovery
//! is correlated with the existing category-8 inventory delta reader.

use anyhow::{Context, Result, bail};
use dynasmrt::{DynasmApi, DynasmLabelApi, dynasm, x64::Assembler};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::install::ThreadController;
use super::mem::ProcessMemory;

pub const ALLOCATOR_RVA: u64 = 0x1A87_590;
const ORIGINAL: &[u8] = &[0x55, 0x48, 0x89, 0xE5, 0x41, 0x57, 0x41, 0x56];
const RETURN_RVA: u64 = ALLOCATOR_RVA + ORIGINAL.len() as u64;
const CAVE_RVA: u64 = 0x50D_BE70;
const CAVE_CAPACITY: usize = 0x190;
const RING_OFFSET: u64 = 0x40;
const RECORD_SIZE: u64 = 0x300;
const RING_CAPACITY: usize = 16;
pub const STATE_SIZE: usize = RING_OFFSET as usize + RING_CAPACITY * RECORD_SIZE as usize;
pub const STATIC_CALLERS: &[u64] = &[0x1A87_1F9, 0x1A87_E60, 0x1A88_A98];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllocEntry {
    pub sequence: u64,
    pub rdtsc: u64,
    pub registers: [u64; 9],
    pub rbp: u64,
    pub rsp: u64,
    pub ret: u64,
    pub this_bytes: [u8; 0x40],
    pub stack_bytes: [u8; 0x200],
}

pub struct AllocCapture {
    file: File,
    previous_sequence: u64,
    base: u64,
    warned: bool,
}

impl AllocCapture {
    pub fn beside_ledger(ledger: &Path, base: u64, prologue: &[u8]) -> std::io::Result<Self> {
        let path = ledger
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("gem-alloc-capture.jsonl");
        let mut this = Self {
            file: OpenOptions::new().create(true).append(true).open(path)?,
            previous_sequence: 0,
            base,
            warned: false,
        };
        this.write(json::json!({
            "event":"probe_armed", "at_unix_ms":now_ms(), "mode":"entry_only_option_b",
            "allocator_rva":format!("0x{ALLOCATOR_RVA:X}"), "allocator_static_callers":STATIC_CALLERS.iter().map(|rva|format!("0x{rva:X}")).collect::<Vec<_>>(),
            "prologue":hex(prologue), "ring_entries":RING_CAPACITY, "record_size":RECORD_SIZE,
        }));
        Ok(this)
    }

    pub fn observe(&mut self, rows: Vec<AllocEntry>) {
        for row in rows {
            if row.sequence <= self.previous_sequence {
                continue;
            }
            self.previous_sequence = row.sequence;
            let caller_rva = row.ret.checked_sub(self.base);
            let names = ["rax", "rdi", "rsi", "rdx", "rcx", "r8", "r9", "r10", "r11"];
            self.write(json::json!({
                "event":"allocator_entry", "at_unix_ms":now_ms(), "sequence":row.sequence,
                "rdtsc":row.rdtsc, "caller":format!("0x{:X}",row.ret), "caller_rva":caller_rva.map(|v|format!("0x{v:X}")),
                "rbp":format!("0x{:X}",row.rbp), "rsp":format!("0x{:X}",row.rsp),
                "registers":names.into_iter().zip(row.registers).map(|(name,value)|(name,format!("0x{value:X}"))).collect::<std::collections::BTreeMap<_,_>>(),
                "this_bytes":hex(&row.this_bytes), "stack_bytes":hex(&row.stack_bytes),
            }));
        }
    }

    fn write(&mut self, value: json::Value) {
        if self.warned {
            return;
        }
        if writeln!(self.file, "{value}")
            .and_then(|_| self.file.flush())
            .is_err()
        {
            self.warned = true;
        }
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn cave_bytes(state: u64, cave: u64, resume: u64) -> Result<Vec<u8>> {
    let mut ops = Assembler::new()?;
    let skip_this = ops.new_dynamic_label();
    dynasm!(ops
        ; .arch x64
        ; pushfq
        ; push rax
        ; push rcx
        ; push rdx
        ; push rsi
        ; push rdi
        ; push r8
        ; push r9
        ; push r10
        ; push r11
        ; mov r11, QWORD state as i64
        ; mov rax, 1
        ; lock xadd QWORD [r11], rax
        ; inc rax
        ; mov r10, rax
        ; mov rcx, rax
        ; and ecx, 15
        ; imul rcx, rcx, RECORD_SIZE as i32
        ; lea r8, [r11 + rcx + RING_OFFSET as i32]
        ; mov QWORD [r8], 0
        ; mov BYTE [r8 + 8], 1
        ; rdtsc
        ; shl rdx, 32
        ; or rax, rdx
        ; mov QWORD [r8 + 0x10], rax
        // Saved stack: r11,r10,r9,r8,rdi,rsi,rdx,rcx,rax,flags.
        ; mov rax, QWORD [rsp + 0x40]
        ; mov QWORD [r8 + 0x18], rax
        ; mov rax, QWORD [rsp + 0x20]
        ; mov QWORD [r8 + 0x20], rax
        ; mov rax, QWORD [rsp + 0x28]
        ; mov QWORD [r8 + 0x28], rax
        ; mov rax, QWORD [rsp + 0x30]
        ; mov QWORD [r8 + 0x30], rax
        ; mov rax, QWORD [rsp + 0x38]
        ; mov QWORD [r8 + 0x38], rax
        ; mov rax, QWORD [rsp + 0x18]
        ; mov QWORD [r8 + 0x40], rax
        ; mov rax, QWORD [rsp + 0x10]
        ; mov QWORD [r8 + 0x48], rax
        ; mov rax, QWORD [rsp + 0x08]
        ; mov QWORD [r8 + 0x50], rax
        ; mov rax, QWORD [rsp]
        ; mov QWORD [r8 + 0x58], rax
        ; mov QWORD [r8 + 0x60], rbp
        ; lea rax, [rsp + 0x50]
        ; mov QWORD [r8 + 0x68], rax
        ; mov rax, QWORD [rax]
        ; mov QWORD [r8 + 0x70], rax
        // Copy the bounded entry stack.
        ; lea rsi, [rsp + 0x50]
        ; lea rdi, [r8 + 0xC0]
        ; mov rcx, 0x200
        ; rep movsb
        // Copy allocator-owner bytes only for PS4 guest pointers.
        ; mov rsi, QWORD [rsp + 0x20]
        ; mov rax, QWORD 0x1_0000_0000u64 as i64
        ; cmp rsi, rax
        ; jb =>skip_this
        ; mov rax, QWORD 0x10_0000_0000u64 as i64
        ; cmp rsi, rax
        ; jae =>skip_this
        ; lea rdi, [r8 + 0x80]
        ; mov rcx, 0x40
        ; rep movsb
        ; =>skip_this
        ; mov QWORD [r8], r10
        ; pop r11
        ; pop r10
        ; pop r9
        ; pop r8
        ; pop rdi
        ; pop rsi
        ; pop rdx
        ; pop rcx
        ; pop rax
        ; popfq
        ; push rbp
        ; mov rbp, rsp
        ; push r15
        ; push r14
        ; mov r11, QWORD resume as i64
        ; jmp r11
    );
    let bytes = ops
        .finalize()
        .map_err(|_| anyhow::anyhow!("finalizing allocator probe cave"))?;
    let out = bytes.to_vec();
    anyhow::ensure!(
        out.len() <= CAVE_CAPACITY,
        "allocator probe cave overflow: {}",
        out.len()
    );
    let _ = cave;
    Ok(out)
}

fn detour(cave: u64, hook: u64) -> Result<Vec<u8>> {
    let displacement = i64::try_from(cave)? - i64::try_from(hook + 5)?;
    let mut out = vec![0xE9];
    out.extend_from_slice(&i32::try_from(displacement)?.to_le_bytes());
    out.extend_from_slice(&[0x90; 3]);
    Ok(out)
}

pub fn install(
    memory: &impl ProcessMemory,
    base: u64,
    state: u64,
    threads: &mut impl ThreadController,
) -> Result<Vec<u8>> {
    let hook = base + ALLOCATOR_RVA;
    for &caller_rva in STATIC_CALLERS {
        let call = memory.read(base + caller_rva, 5)?;
        if call[0] != 0xE8 {
            bail!("allocator_static_callers mismatch at +0x{caller_rva:X}");
        }
        let displacement = i32::from_le_bytes(call[1..5].try_into().unwrap()) as i64;
        let target = (base + caller_rva + 5) as i64 + displacement;
        if target != hook as i64 {
            bail!("allocator_static_callers target mismatch at +0x{caller_rva:X}");
        }
    }
    let prologue = memory.read(hook, 32)?;
    if &prologue[..ORIGINAL.len()] != ORIGINAL {
        bail!("prologue_not_relocatable: {}", hex(&prologue));
    }
    let cave = base + CAVE_RVA;
    if memory
        .read(cave, CAVE_CAPACITY)?
        .iter()
        .any(|byte| *byte != 0)
    {
        bail!("gem allocator probe cave is occupied");
    }
    memory.write(state, &vec![0; STATE_SIZE])?;
    let code = cave_bytes(state, cave, base + RETURN_RVA)?;
    memory.write(cave, &code)?;
    threads
        .suspend_all()
        .context("suspending for gem allocator probe")?;
    let result = (|| {
        if threads
            .instruction_pointers()?
            .iter()
            .any(|rip| *rip >= hook && *rip < hook + ORIGINAL.len() as u64)
        {
            bail!("a guest thread occupied the gem allocator probe window");
        }
        memory.write(hook, &detour(cave, hook)?)
    })();
    let resumed = threads
        .resume_all()
        .context("resuming after gem allocator probe");
    result.and(resumed)?;
    Ok(prologue)
}

pub fn snapshots(memory: &impl ProcessMemory, state: u64) -> Vec<AllocEntry> {
    let mut out = Vec::new();
    for slot in 0..RING_CAPACITY {
        let address = state + RING_OFFSET + slot as u64 * RECORD_SIZE;
        let Ok(first) = memory.read_u64(address) else {
            continue;
        };
        if first == 0 {
            continue;
        }
        let Ok(bytes) = memory.read(address, RECORD_SIZE as usize) else {
            continue;
        };
        if memory.read_u64(address).ok() != Some(first) {
            continue;
        }
        let u64_at =
            |offset: usize| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        let mut registers = [0; 9];
        for (index, value) in registers.iter_mut().enumerate() {
            *value = u64_at(0x18 + index * 8);
        }
        out.push(AllocEntry {
            sequence: first,
            rdtsc: u64_at(0x10),
            registers,
            rbp: u64_at(0x60),
            rsp: u64_at(0x68),
            ret: u64_at(0x70),
            this_bytes: bytes[0x80..0xC0].try_into().unwrap(),
            stack_bytes: bytes[0xC0..0x2C0].try_into().unwrap(),
        });
    }
    out.sort_unstable_by_key(|row| row.sequence);
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cave_fits_fresh_region() {
        assert!(
            cave_bytes(0x6000_0000, 0xA000_0000, 0x7000_0000)
                .unwrap()
                .len()
                <= CAVE_CAPACITY
        );
    }
}
