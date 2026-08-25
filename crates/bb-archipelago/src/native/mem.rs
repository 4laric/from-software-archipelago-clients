//! Process memory access behind a trait, plus fail-closed image verification.
//!
//! The trait mirrors the Cheat-Engine services the Python `ProcessMemory`
//! replaced -- `ReadProcessMemory` / `WriteProcessMemory` / `VirtualProtectEx` --
//! so the delivery and install logic is host-testable against [`FakeMemory`]
//! while the real accessor lives behind `#[cfg(windows)]`, exactly as
//! `event_flags.rs` splits its live reads.
//!
//! [`require_validated_image`] is the port of the Python `require_validated_image`:
//! every image assert in the contract must match before anything is written.
//! CUSA00900 and every other serial or app version land here and are refused --
//! a partial match is a different image, not a near-enough one.

use anyhow::{Context, Result, bail};

use super::contract::Contract;

/// Read/write access to a live guest process. Reads and writes take `&self`;
/// the Windows handle is shared and the fake uses interior mutability.
pub trait ProcessMemory {
    fn read(&self, address: u64, len: usize) -> Result<Vec<u8>>;
    fn write(&self, address: u64, data: &[u8]) -> Result<()>;

    fn read_u32(&self, address: u64) -> Result<u32> {
        let bytes = self.read(address, 4)?;
        Ok(u32::from_le_bytes(bytes[..4].try_into().unwrap()))
    }
    fn read_u64(&self, address: u64) -> Result<u64> {
        let bytes = self.read(address, 8)?;
        Ok(u64::from_le_bytes(bytes[..8].try_into().unwrap()))
    }
    fn write_u32(&self, address: u64, value: u32) -> Result<()> {
        self.write(address, &value.to_le_bytes())
    }
    fn write_u64(&self, address: u64, value: u64) -> Result<()> {
        self.write(address, &value.to_le_bytes())
    }
}

/// Parse the last eboot `base_virtual_addr` shadPS4 logged, matching the
/// contract's primary base-resolution strategy and the Python
/// `logged_eboot_base`. A logged base is a *hint*: [`verify_base`] must confirm
/// it against the hook originals before it is trusted.
pub fn logged_eboot_base(log_text: &str) -> Option<u64> {
    let mut pending = false;
    let mut found = None;
    for line in log_text.lines() {
        if line.contains("Loading module eboot.bin") {
            pending = true;
            continue;
        }
        if pending && let Some(idx) = line.find("base_virtual_addr") {
            let rest = &line[idx..];
            if let Some(hex_start) = rest.find("0x") {
                let hex: String = rest[hex_start + 2..]
                    .chars()
                    .take_while(|c| c.is_ascii_hexdigit())
                    .collect();
                if let Ok(value) = u64::from_str_radix(&hex, 16) {
                    found = Some(value);
                    pending = false;
                }
            }
        }
    }
    found
}

/// Both hook originals must be present at the candidate base for it to be a
/// plausible eboot base.
pub fn verify_base(memory: &impl ProcessMemory, base: u64, contract: &Contract) -> Result<bool> {
    for name in ["consume_return", "idle_heartbeat"] {
        let site = contract.hook_site(name)?;
        let actual = memory.read(base + site.rva, site.original_bytes.len())?;
        if actual != site.original_bytes {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Find the single offset in `haystack` matching `pattern` (with `None`
/// wildcards). The contract's AOB fallback requires *exactly one* candidate;
/// zero or many is a refusal, never a guess.
pub fn scan_unique(haystack: &[u8], pattern: &[Option<u8>]) -> Result<usize> {
    if pattern.is_empty() {
        bail!("cannot scan for an empty pattern");
    }
    let width = pattern.len();
    let mut matches = Vec::new();
    if haystack.len() >= width {
        for offset in 0..=haystack.len() - width {
            if pattern
                .iter()
                .zip(&haystack[offset..offset + width])
                .all(|(expected, actual)| expected.is_none_or(|e| e == *actual))
            {
                matches.push(offset);
                if matches.len() > 1 {
                    break;
                }
            }
        }
    }
    match matches.as_slice() {
        [only] => Ok(*only),
        [] => bail!("AOB signature not found"),
        _ => bail!("AOB signature is not unique; refusing to guess a base"),
    }
}

/// Fail closed unless every image assert in the contract matches at `base`.
///
/// This is the port of the Python `require_validated_image`. A single mismatched
/// byte is enough to refuse: the recorded contract is CUSA03173 01.09 only.
pub fn require_validated_image(
    memory: &impl ProcessMemory,
    base: u64,
    contract: &Contract,
) -> Result<()> {
    let mut failures = Vec::new();
    for assert in &contract.asserts {
        let actual = memory
            .read(base + assert.rva, assert.bytes.len())
            .with_context(|| format!("reading assert {} at +{:#x}", assert.name, assert.rva))?;
        if actual != assert.bytes {
            failures.push(format!(
                "{}@+{:#x} expected [{}] got [{}]",
                assert.name,
                assert.rva,
                hex(&assert.bytes),
                hex(&actual)
            ));
        }
    }
    if !failures.is_empty() {
        bail!(
            "refusing to patch: this is not the validated CUSA03173 01.09 image. {}",
            failures.join("; ")
        );
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

// -------------------------------------------------------------------------
// A host-side fake, available on every platform so the logic tests run
// anywhere.
// -------------------------------------------------------------------------

#[cfg(any(test, not(windows)))]
mod fake {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use anyhow::{Result, bail};

    use super::ProcessMemory;

    /// A sparse byte-addressable memory for tests. Unwritten reads return an
    /// error, matching a short/failed `ReadProcessMemory`.
    #[derive(Default)]
    pub struct FakeMemory {
        bytes: RefCell<BTreeMap<u64, u8>>,
    }

    impl FakeMemory {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn store(&self, address: u64, data: &[u8]) {
            let mut map = self.bytes.borrow_mut();
            for (i, byte) in data.iter().enumerate() {
                map.insert(address + i as u64, *byte);
            }
        }
    }

    impl ProcessMemory for FakeMemory {
        fn read(&self, address: u64, len: usize) -> Result<Vec<u8>> {
            let map = self.bytes.borrow();
            let mut out = Vec::with_capacity(len);
            for i in 0..len as u64 {
                match map.get(&(address + i)) {
                    Some(byte) => out.push(*byte),
                    None => bail!("fake read of unmapped address {:#x}", address + i),
                }
            }
            Ok(out)
        }

        fn write(&self, address: u64, data: &[u8]) -> Result<()> {
            self.store(address, data);
            Ok(())
        }
    }
}

#[cfg(any(test, not(windows)))]
pub use fake::FakeMemory;

// -------------------------------------------------------------------------
// The real Windows accessor. Only compiled on Windows; on other hosts the
// logic above is exercised through `FakeMemory`.
// -------------------------------------------------------------------------

#[cfg(windows)]
mod windows_impl {
    use std::ffi::c_void;

    use anyhow::{Context, Result, bail, ensure};
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
    use windows::Win32::System::Memory::{
        PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS, VirtualProtectEx,
    };
    use windows::Win32::System::ProcessStatus::{EnumProcesses, GetModuleBaseNameW};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ,
        PROCESS_VM_WRITE,
    };

    use super::ProcessMemory;

    /// A writable handle to shadPS4.exe.
    pub struct WinProcessMemory {
        process_id: u32,
        handle: HANDLE,
    }

    impl Drop for WinProcessMemory {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }

    impl WinProcessMemory {
        pub fn process_id(&self) -> u32 {
            self.process_id
        }

        /// Open the single running shadPS4.exe with read+write+operation access.
        pub fn open_shad() -> Result<Self> {
            let mut ids = vec![0u32; 4096];
            let mut needed = 0u32;
            unsafe {
                EnumProcesses(
                    ids.as_mut_ptr(),
                    (ids.len() * std::mem::size_of::<u32>()) as u32,
                    &mut needed,
                )
            }
            .context("enumerating Windows processes")?;
            ids.truncate(needed as usize / std::mem::size_of::<u32>());

            let mut matches = Vec::new();
            for pid in ids.into_iter().filter(|pid| *pid != 0) {
                let Ok(handle) = (unsafe {
                    OpenProcess(
                        PROCESS_QUERY_INFORMATION
                            | PROCESS_VM_READ
                            | PROCESS_VM_WRITE
                            | PROCESS_VM_OPERATION,
                        false,
                        pid,
                    )
                }) else {
                    continue;
                };
                if process_name(handle).is_some_and(|n| n.eq_ignore_ascii_case("shadPS4.exe")) {
                    matches.push(Self {
                        process_id: pid,
                        handle,
                    });
                } else {
                    let _ = unsafe { CloseHandle(handle) };
                }
            }
            match matches.len() {
                1 => Ok(matches.pop().expect("one match")),
                0 => bail!(
                    "shadPS4.exe is not running or cannot be opened for writing; run the client as administrator if shadPS4 is elevated"
                ),
                count => bail!("found {count} shadPS4.exe processes; close the unused instances"),
            }
        }

        pub fn raw_handle(&self) -> HANDLE {
            self.handle
        }
    }

    fn process_name(handle: HANDLE) -> Option<String> {
        let mut buffer = [0u16; 260];
        let length = unsafe { GetModuleBaseNameW(handle, None, &mut buffer) } as usize;
        (length > 0).then(|| String::from_utf16_lossy(&buffer[..length]))
    }

    impl ProcessMemory for WinProcessMemory {
        fn read(&self, address: u64, len: usize) -> Result<Vec<u8>> {
            let mut buffer = vec![0u8; len];
            let mut read = 0usize;
            unsafe {
                ReadProcessMemory(
                    self.handle,
                    address as *const c_void,
                    buffer.as_mut_ptr().cast(),
                    len,
                    Some(&mut read),
                )
            }
            .with_context(|| format!("ReadProcessMemory({address:#x}, {len})"))?;
            ensure!(
                read == len,
                "short read at {address:#x}: {read} of {len} bytes"
            );
            Ok(buffer)
        }

        fn write(&self, address: u64, data: &[u8]) -> Result<()> {
            // Match the Python path: temporarily make the page RWX, write, then
            // restore the previous protection.
            let mut old = PAGE_PROTECTION_FLAGS(0);
            unsafe {
                VirtualProtectEx(
                    self.handle,
                    address as *const c_void,
                    data.len(),
                    PAGE_EXECUTE_READWRITE,
                    &mut old,
                )
            }
            .with_context(|| format!("VirtualProtectEx({address:#x}, {})", data.len()))?;
            let mut written = 0usize;
            let write_result = unsafe {
                WriteProcessMemory(
                    self.handle,
                    address as *const c_void,
                    data.as_ptr().cast(),
                    data.len(),
                    Some(&mut written),
                )
            };
            let mut restore = PAGE_PROTECTION_FLAGS(0);
            let _ = unsafe {
                VirtualProtectEx(
                    self.handle,
                    address as *const c_void,
                    data.len(),
                    old,
                    &mut restore,
                )
            };
            write_result.with_context(|| format!("WriteProcessMemory({address:#x})"))?;
            ensure!(
                written == data.len(),
                "short write at {address:#x}: {written} of {} bytes",
                data.len()
            );
            Ok(())
        }
    }
}

#[cfg(windows)]
pub use windows_impl::WinProcessMemory;

#[cfg(test)]
mod tests {
    use super::super::contract::contract;
    use super::*;

    /// Store the whole validated image (every assert's bytes at base+rva) into a
    /// fake, then verify.
    fn install_validated(memory: &FakeMemory, base: u64) {
        let c = contract();
        for assert in &c.asserts {
            memory.store(base + assert.rva, &assert.bytes);
        }
        for name in ["consume_return", "idle_heartbeat"] {
            let site = c.hook_site(name).unwrap();
            memory.store(base + site.rva, &site.original_bytes);
        }
    }

    #[test]
    fn require_validated_image_accepts_the_exact_image() {
        let base = 0x4000_0000;
        let memory = FakeMemory::new();
        install_validated(&memory, base);
        require_validated_image(&memory, base, contract()).unwrap();
    }

    #[test]
    fn require_validated_image_fails_closed_on_a_one_byte_diff() {
        let base = 0x4000_0000;
        let memory = FakeMemory::new();
        install_validated(&memory, base);
        // Flip a single byte inside the consume-hook assert.
        let site_rva = contract()
            .asserts
            .iter()
            .find(|a| a.name == "consume_hook")
            .unwrap()
            .rva;
        memory.store(base + site_rva + 1, &[0xFF]);
        let error = require_validated_image(&memory, base, contract()).unwrap_err();
        assert!(format!("{error:#}").contains("not the validated CUSA03173 01.09 image"));
        assert!(format!("{error:#}").contains("consume_hook"));
    }

    #[test]
    fn verify_base_confirms_the_hook_originals() {
        let base = 0x1_2340_0000;
        let memory = FakeMemory::new();
        install_validated(&memory, base);
        assert!(verify_base(&memory, base, contract()).unwrap());
    }

    #[test]
    fn logged_eboot_base_reads_the_last_base_virtual_addr() {
        let log = "\
Loading module eboot.bin
  base_virtual_addr ..: 0x5700000
other line
Loading module eboot.bin
  base_virtual_addr ..: 0x5660000
";
        assert_eq!(logged_eboot_base(log), Some(0x5660000));
    }

    #[test]
    fn scan_unique_requires_exactly_one_candidate() {
        let pattern = [Some(0xAA), None, Some(0xCC)];
        assert_eq!(
            scan_unique(&[0x00, 0xAA, 0xBB, 0xCC, 0x11], &pattern).unwrap(),
            1
        );
        // Two candidates: refuse.
        let two = [0xAA, 0xBB, 0xCC, 0xAA, 0x00, 0xCC];
        assert!(scan_unique(&two, &pattern).is_err());
        // None: refuse.
        assert!(scan_unique(&[0, 0, 0], &pattern).is_err());
    }

    #[test]
    fn fake_read_of_unmapped_memory_fails() {
        let memory = FakeMemory::new();
        assert!(memory.read(0x1000, 4).is_err());
        memory.write_u32(0x1000, 0xDEAD_BEEF).unwrap();
        assert_eq!(memory.read_u32(0x1000).unwrap(), 0xDEAD_BEEF);
    }
}

// -------------------------------------------------------------------------
// The concrete accessor the native backend attaches. On Windows it is the
// real `WinProcessMemory`; on other hosts it is a stub that fails to open,
// so `NativeBackend` compiles everywhere but only functions on Windows --
// exactly how `event_flags::LiveEventFlags` is split.
// -------------------------------------------------------------------------

#[cfg(windows)]
pub type NativeMemory = WinProcessMemory;

#[cfg(not(windows))]
mod native_stub {
    use anyhow::{Result, bail};

    use super::ProcessMemory;

    pub struct StubMemory;

    impl StubMemory {
        pub fn open_shad() -> Result<Self> {
            bail!("native Bloodborne delivery requires Windows")
        }
    }

    impl ProcessMemory for StubMemory {
        fn read(&self, _address: u64, _len: usize) -> Result<Vec<u8>> {
            bail!("native Bloodborne delivery requires Windows")
        }
        fn write(&self, _address: u64, _data: &[u8]) -> Result<()> {
            bail!("native Bloodborne delivery requires Windows")
        }
    }
}

#[cfg(not(windows))]
pub use native_stub::StubMemory as NativeMemory;
