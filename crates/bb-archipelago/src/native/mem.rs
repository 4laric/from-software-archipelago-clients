//! Process memory access behind a trait, plus fail-closed image verification.
//!
//! The trait mirrors the Cheat-Engine services the Python `ProcessMemory`
//! replaced -- `ReadProcessMemory` / `WriteProcessMemory` / `VirtualProtectEx` --
//! so the delivery and install logic is host-testable against [`FakeMemory`]
//! while the real accessor lives behind `#[cfg(windows)]`, exactly as
//! `event_flags.rs` splits its live reads.
//!
//! Writes go through [`write_with_protect_fallback`]: plain
//! `WriteProcessMemory` first, and `VirtualProtectEx(PAGE_EXECUTE_READWRITE)`
//! only as a retry. The unconditional protect the Python path used is wrong on
//! shadPS4's guest heap, where the pages are already writable but their
//! protection cannot be changed.
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
// The write strategy, split out from the Windows syscalls so it is testable
// on any host. See `WinProcessMemory::write`.
// -------------------------------------------------------------------------

/// Which of the two attempts actually landed the bytes. Diagnostics only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritePath {
    /// `WriteProcessMemory` succeeded with the page's existing protection.
    Direct,
    /// The direct write failed and the `VirtualProtectEx` dance was needed.
    ViaProtect,
}

/// The three raw calls [`write_with_protect_fallback`] sequences. Implemented
/// over the real Win32 entry points on Windows and over a recording fake in
/// the tests, the same way `FakeMemory` stands in for `WinProcessMemory`.
pub trait RawWriteSyscalls {
    /// `WriteProcessMemory`, returning the byte count it reported.
    fn write_process_memory(&mut self, address: u64, len: usize) -> Result<usize>;
    /// `VirtualProtectEx(PAGE_EXECUTE_READWRITE)`, remembering the old flags.
    fn protect_rwx(&mut self, address: u64, len: usize) -> Result<()>;
    /// Restore whatever [`Self::protect_rwx`] replaced. Best effort.
    fn restore_protect(&mut self, address: u64, len: usize);
}

/// Write `len` bytes at `address`, trying the plain `WriteProcessMemory`
/// first and only reaching for `VirtualProtectEx` when it fails.
///
/// Fixpack bb-0.1.0.1: the old order was protect-then-write
/// unconditionally, which broke incoming DeathLink on shadPS4 0.18.0. The
/// guest-heap page holding the player HP cell is a mapped/placeholder section
/// whose protection cannot be moved to execute-read-write, so
/// `VirtualProtectEx` returned ERROR_INVALID_PARAMETER and the HP write never
/// ran -- even though the page was plainly writable and the same address read
/// back fine. The protect dance is still required for the eboot code pages the
/// native payload install patches, so it stays as the fallback rather than
/// being deleted.
pub fn write_with_protect_fallback<S: RawWriteSyscalls>(
    syscalls: &mut S,
    address: u64,
    len: usize,
) -> Result<WritePath> {
    let direct = syscalls.write_process_memory(address, len);
    let direct_failure = match direct {
        Ok(written) if written == len => return Ok(WritePath::Direct),
        Ok(written) => {
            format!("WriteProcessMemory({address:#x}) wrote {written} of {len} bytes")
        }
        Err(error) => format!("WriteProcessMemory({address:#x}): {error:#}"),
    };

    if let Err(protect_error) = syscalls.protect_rwx(address, len) {
        bail!(
            "{direct_failure}; VirtualProtectEx({address:#x}, {len}) also failed: {protect_error:#}"
        );
    }
    let retry = syscalls.write_process_memory(address, len);
    syscalls.restore_protect(address, len);
    match retry {
        Ok(written) if written == len => Ok(WritePath::ViaProtect),
        Ok(written) => bail!(
            "{direct_failure}; after VirtualProtectEx, short write at {address:#x}: \
             {written} of {len} bytes"
        ),
        Err(error) => bail!(
            "{direct_failure}; after VirtualProtectEx, WriteProcessMemory({address:#x}): {error:#}"
        ),
    }
}

/// How many writes took each path since the process started. The crate has no
/// logging framework, so diagnostics read these counters instead of a
/// debug-level line; nothing is printed at info level.
pub mod write_path_counters {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::WritePath;

    static DIRECT: AtomicU64 = AtomicU64::new(0);
    static VIA_PROTECT: AtomicU64 = AtomicU64::new(0);

    pub(super) fn record(path: WritePath) {
        match path {
            WritePath::Direct => &DIRECT,
            WritePath::ViaProtect => &VIA_PROTECT,
        }
        .fetch_add(1, Ordering::Relaxed);
    }

    /// `(direct, via_protect)` write counts.
    pub fn counts() -> (u64, u64) {
        (
            DIRECT.load(Ordering::Relaxed),
            VIA_PROTECT.load(Ordering::Relaxed),
        )
    }

    /// A one-line summary for diagnostics and failure contexts.
    pub fn summary() -> String {
        let (direct, via_protect) = counts();
        format!("writes: {direct} direct, {via_protect} via VirtualProtectEx")
    }
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
        MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS, PAGE_READWRITE,
        VirtualAllocEx, VirtualProtectEx,
    };
    use windows::Win32::System::ProcessStatus::{EnumProcesses, GetModuleBaseNameW};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ,
        PROCESS_VM_WRITE,
    };

    use super::{
        ProcessMemory, RawWriteSyscalls, write_path_counters, write_with_protect_fallback,
    };

    /// The Win32 side of [`RawWriteSyscalls`], holding the source bytes and the
    /// protection flags a fallback has to put back.
    struct WinWriteSyscalls<'a> {
        handle: HANDLE,
        data: &'a [u8],
        old: PAGE_PROTECTION_FLAGS,
    }

    impl RawWriteSyscalls for WinWriteSyscalls<'_> {
        fn write_process_memory(&mut self, address: u64, len: usize) -> Result<usize> {
            debug_assert_eq!(len, self.data.len());
            let mut written = 0usize;
            unsafe {
                WriteProcessMemory(
                    self.handle,
                    address as *const c_void,
                    self.data.as_ptr().cast(),
                    len,
                    Some(&mut written),
                )
            }?;
            Ok(written)
        }

        fn protect_rwx(&mut self, address: u64, len: usize) -> Result<()> {
            unsafe {
                VirtualProtectEx(
                    self.handle,
                    address as *const c_void,
                    len,
                    PAGE_EXECUTE_READWRITE,
                    &mut self.old,
                )
            }?;
            Ok(())
        }

        fn restore_protect(&mut self, address: u64, len: usize) {
            let mut restore = PAGE_PROTECTION_FLAGS(0);
            let _ = unsafe {
                VirtualProtectEx(
                    self.handle,
                    address as *const c_void,
                    len,
                    self.old,
                    &mut restore,
                )
            };
        }
    }

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

        /// Allocate private read/write storage in the target process. The
        /// allocation is released automatically when shadPS4 exits.
        pub fn allocate(&self, len: usize) -> Result<u64> {
            let address = unsafe {
                VirtualAllocEx(
                    self.handle,
                    None,
                    len,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_READWRITE,
                )
            };
            ensure!(!address.is_null(), "VirtualAllocEx({len}) returned null");
            Ok(address as u64)
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
            // Write first; only fall back to the RWX dance if that fails. See
            // `write_with_protect_fallback` for why the order was inverted.
            let mut syscalls = WinWriteSyscalls {
                handle: self.handle,
                data,
                old: PAGE_PROTECTION_FLAGS(0),
            };
            let path = write_with_protect_fallback(&mut syscalls, address, data.len())?;
            write_path_counters::record(path);
            Ok(())
        }
    }
}

#[cfg(windows)]
pub use windows_impl::WinProcessMemory;

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

    // ---------------------------------------------------------------------
    // The write strategy. `RecordingSyscalls` stands in for the three Win32
    // calls so the ordering is asserted on every host, not just Windows.
    // ---------------------------------------------------------------------

    #[derive(Debug, PartialEq, Eq)]
    enum Call {
        Write,
        Protect,
        Restore,
    }

    struct RecordingSyscalls {
        /// One entry per `write_process_memory` call, in order.
        writes: Vec<Result<usize, String>>,
        protect: Result<(), String>,
        calls: Vec<Call>,
    }

    impl RecordingSyscalls {
        fn new(writes: Vec<Result<usize, String>>, protect: Result<(), String>) -> Self {
            Self {
                writes,
                protect,
                calls: Vec::new(),
            }
        }
    }

    impl RawWriteSyscalls for RecordingSyscalls {
        fn write_process_memory(&mut self, _address: u64, _len: usize) -> Result<usize> {
            self.calls.push(Call::Write);
            match self.writes.remove(0) {
                Ok(written) => Ok(written),
                Err(message) => bail!("{message}"),
            }
        }

        fn protect_rwx(&mut self, _address: u64, _len: usize) -> Result<()> {
            self.calls.push(Call::Protect);
            match &self.protect {
                Ok(()) => Ok(()),
                Err(message) => bail!("{message}"),
            }
        }

        fn restore_protect(&mut self, _address: u64, _len: usize) {
            self.calls.push(Call::Restore);
        }
    }

    #[test]
    fn a_direct_write_never_touches_virtualprotectex() {
        // The shadPS4 guest heap case: the page is writable as it stands, and
        // asking to make it RWX is what used to fail.
        let mut syscalls = RecordingSyscalls::new(vec![Ok(4)], Err("must not be called".into()));
        let path = write_with_protect_fallback(&mut syscalls, 0x224e_dd2a8, 4).unwrap();
        assert_eq!(path, WritePath::Direct);
        assert_eq!(syscalls.calls, vec![Call::Write]);
    }

    #[test]
    fn a_refused_direct_write_falls_back_to_the_protect_dance() {
        // The eboot code-page case the payload install depends on.
        let mut syscalls =
            RecordingSyscalls::new(vec![Err("ERROR_NOACCESS".into()), Ok(8)], Ok(()));
        let path = write_with_protect_fallback(&mut syscalls, 0x5660_0000, 8).unwrap();
        assert_eq!(path, WritePath::ViaProtect);
        assert_eq!(
            syscalls.calls,
            vec![Call::Write, Call::Protect, Call::Write, Call::Restore]
        );
    }

    #[test]
    fn a_partial_direct_write_also_falls_back() {
        let mut syscalls = RecordingSyscalls::new(vec![Ok(2), Ok(4)], Ok(()));
        let path = write_with_protect_fallback(&mut syscalls, 0x1000, 4).unwrap();
        assert_eq!(path, WritePath::ViaProtect);
        assert_eq!(
            syscalls.calls,
            vec![Call::Write, Call::Protect, Call::Write, Call::Restore]
        );
    }

    #[test]
    fn both_failing_reports_the_write_and_the_protect_error_together() {
        let mut syscalls = RecordingSyscalls::new(
            vec![Err("ERROR_NOACCESS".into())],
            Err("The parameter is incorrect. (0x80070057)".into()),
        );
        let error = write_with_protect_fallback(&mut syscalls, 0x224e_dd2a8, 4).unwrap_err();
        let text = format!("{error:#}");
        assert!(text.contains("ERROR_NOACCESS"), "{text}");
        assert!(text.contains("0x80070057"), "{text}");
        assert!(text.contains("VirtualProtectEx(0x224edd2a8, 4)"), "{text}");
        // No retry write, and nothing to restore.
        assert_eq!(syscalls.calls, vec![Call::Write, Call::Protect]);
    }

    #[test]
    fn a_short_write_after_the_fallback_is_still_an_error() {
        let mut syscalls = RecordingSyscalls::new(vec![Ok(0), Ok(2)], Ok(()));
        let error = write_with_protect_fallback(&mut syscalls, 0x1000, 4).unwrap_err();
        assert!(format!("{error:#}").contains("short write at 0x1000: 2 of 4 bytes"));
        // The protection is put back even on the failing path.
        assert_eq!(syscalls.calls.last(), Some(&Call::Restore));
    }
}
