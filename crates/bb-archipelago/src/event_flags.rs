//! Bloodborne event-flag access through shadPS4 process memory.
//!
//! All executable offsets here are runtime/version data for Bloodborne 01.09.
//! They intentionally do not live in AP world data.

use anyhow::{Context, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttachmentInfo {
    pub process_id: u32,
    pub eboot_base: u64,
}

/// The event-flag manager pointer is still null (clients#420).
///
/// This is a **startup-ordering** state, not a bad build: the manager is a
/// guest global the game only populates further into boot -- plausibly only
/// once a character save is loaded. Every live validation so far attached to an
/// already-in-gameplay process, which is why it was never seen before.
///
/// It is a distinct error type so callers can tell it apart from a real refusal
/// (signature mismatch, unknown build) and wait instead of exiting. In
/// particular `main::native_attach_failure` must never wrap this class in the
/// unrecognised-build guidance: this is a gameplay initialization state, not
/// evidence that the image is unsupported (clients#416).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EventFlagManagerNotInitialized;

impl std::fmt::Display for EventFlagManagerNotInitialized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // ASCII only: this reaches the console notice surface.
        f.write_str(
            "Bloodborne event-flag manager is not initialized yet (the game has not finished loading a character)",
        )
    }
}

impl std::error::Error for EventFlagManagerNotInitialized {}

/// True when `error` is the clients#420 startup-ordering state anywhere in its
/// cause chain.
pub fn is_manager_not_initialized(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<EventFlagManagerNotInitialized>()
            .is_some()
    })
}

/// CUSA03173 01.09: pointer slot for the event-flag manager.
const MANAGER_ROOT_RVA: u64 = 0x0553_B100;
/// CUSA03173 01.09: one of the inlined MSB-first flag writes.
const SETTER_WRITE_RVA: u64 = 0x017D_6EFA;
const SETTER_WRITE_SIGNATURE: [u8; 3] = [0x88, 0x0C, 0x02];
const GROUP_DIVISOR_OFFSET: u64 = 0x1C;
const PACKED_STRIDE_OFFSET: u64 = 0x20;
const PACKED_BASE_OFFSET: u64 = 0x28;
const GROUP_TREE_OFFSET: u64 = 0x38;
const NODE_NIL_OFFSET: u64 = 0x19;
const NODE_KEY_OFFSET: u64 = 0x20;
const NODE_STORAGE_TYPE_OFFSET: u64 = 0x28;
const NODE_STORAGE_OFFSET: u64 = 0x30;

fn parse_latest_eboot_base(log: &str) -> Result<u64> {
    const MARKER: &str = "Loading module eboot.bin to 0x";
    log.lines()
        .rev()
        .find_map(|line| {
            let (_, suffix) = line.split_once(MARKER)?;
            let hex = suffix
                .split(|character: char| !character.is_ascii_hexdigit())
                .next()?;
            u64::from_str_radix(hex, 16).ok()
        })
        .context("shad log does not contain an eboot load address")
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::mem::{MaybeUninit, size_of};
    use std::path::Path;

    use anyhow::{Context, Result, bail, ensure};
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
    use windows::Win32::System::ProcessStatus::{EnumProcesses, GetModuleBaseNameW};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ,
        PROCESS_VM_WRITE,
    };

    use super::{
        AttachmentInfo, EventFlagManagerNotInitialized, GROUP_DIVISOR_OFFSET, GROUP_TREE_OFFSET,
        MANAGER_ROOT_RVA, NODE_KEY_OFFSET, NODE_NIL_OFFSET, NODE_STORAGE_OFFSET,
        NODE_STORAGE_TYPE_OFFSET, PACKED_BASE_OFFSET, PACKED_STRIDE_OFFSET, SETTER_WRITE_RVA,
        SETTER_WRITE_SIGNATURE, parse_latest_eboot_base,
    };

    struct ProcessHandle(HANDLE);

    impl Drop for ProcessHandle {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    impl ProcessHandle {
        fn read<T: Copy>(&self, address: u64) -> Result<T> {
            let mut value = MaybeUninit::<T>::uninit();
            let mut bytes_read = 0usize;
            unsafe {
                ReadProcessMemory(
                    self.0,
                    address as *const c_void,
                    value.as_mut_ptr().cast(),
                    size_of::<T>(),
                    Some(&mut bytes_read),
                )
            }
            .with_context(|| format!("reading shadPS4 memory at 0x{address:X}"))?;
            ensure!(
                bytes_read == size_of::<T>(),
                "short shadPS4 memory read at 0x{address:X}: {bytes_read} of {} bytes",
                size_of::<T>()
            );
            Ok(unsafe { value.assume_init() })
        }

        fn read_bytes(&self, address: u64, output: &mut [u8]) -> Result<()> {
            let mut bytes_read = 0usize;
            unsafe {
                ReadProcessMemory(
                    self.0,
                    address as *const c_void,
                    output.as_mut_ptr().cast(),
                    output.len(),
                    Some(&mut bytes_read),
                )
            }
            .with_context(|| format!("reading shadPS4 memory at 0x{address:X}"))?;
            ensure!(
                bytes_read == output.len(),
                "short shadPS4 memory read at 0x{address:X}: {bytes_read} of {} bytes",
                output.len()
            );
            Ok(())
        }

        fn write_byte(&self, address: u64, value: u8) -> Result<()> {
            let mut bytes_written = 0usize;
            unsafe {
                WriteProcessMemory(
                    self.0,
                    address as *const c_void,
                    (&value as *const u8).cast(),
                    1,
                    Some(&mut bytes_written),
                )
            }
            .with_context(|| format!("writing shadPS4 memory at 0x{address:X}"))?;
            ensure!(
                bytes_written == 1,
                "short shadPS4 memory write at 0x{address:X}"
            );
            Ok(())
        }
    }

    fn process_name(handle: HANDLE) -> Option<String> {
        let mut buffer = [0u16; 260];
        let length = unsafe { GetModuleBaseNameW(handle, None, &mut buffer) } as usize;
        (length > 0).then(|| String::from_utf16_lossy(&buffer[..length]))
    }

    fn open_shad() -> Result<(u32, ProcessHandle)> {
        let mut process_ids = vec![0u32; 4096];
        let mut bytes_needed = 0u32;
        unsafe {
            EnumProcesses(
                process_ids.as_mut_ptr(),
                (process_ids.len() * size_of::<u32>()) as u32,
                &mut bytes_needed,
            )
        }
        .context("enumerating Windows processes")?;
        process_ids.truncate(bytes_needed as usize / size_of::<u32>());

        let mut matches = Vec::new();
        for process_id in process_ids
            .into_iter()
            .filter(|process_id| *process_id != 0)
        {
            let Ok(handle) = (unsafe {
                OpenProcess(
                    PROCESS_QUERY_INFORMATION
                        | PROCESS_VM_READ
                        | PROCESS_VM_WRITE
                        | PROCESS_VM_OPERATION,
                    false,
                    process_id,
                )
            }) else {
                continue;
            };
            if process_name(handle).is_some_and(|name| name.eq_ignore_ascii_case("shadPS4.exe")) {
                matches.push((process_id, ProcessHandle(handle)));
            } else {
                let _ = unsafe { CloseHandle(handle) };
            }
        }

        match matches.len() {
            1 => Ok(matches.pop().expect("one match")),
            0 => bail!(
                "shadPS4.exe is not running or cannot be opened; run the client as administrator if shadPS4 is elevated"
            ),
            count => bail!("found {count} shadPS4.exe processes; close the unused instances"),
        }
    }

    pub struct LiveEventFlags {
        process_id: u32,
        process: ProcessHandle,
        eboot_base: u64,
        shad_log: std::path::PathBuf,
    }

    impl LiveEventFlags {
        pub fn attach(shad_log: &Path) -> Result<Self> {
            // clients#369: name the setting and distinguish a missing log from
            // an unreadable one; the OS error stays the source of the chain.
            let log = std::fs::read_to_string(shad_log).map_err(|error| {
                let action = if error.kind() == std::io::ErrorKind::NotFound {
                    format!("shad_log does not exist: {}", shad_log.display())
                } else {
                    format!("shad_log cannot be read: {}", shad_log.display())
                };
                anyhow::Error::new(error).context(action)
            })?;
            let eboot_base = parse_latest_eboot_base(&log)?;
            Self::attach_at_base(shad_log, eboot_base)
        }

        /// Attach at a base the caller has *already* confirmed against live
        /// memory.
        ///
        /// clients#418: reading the appended shad log once yields the previous
        /// run's base when the game is still loading. `NativeBackend::attach`
        /// resolves that race properly (bounded wait, freshness floor,
        /// `verify_base`); this entry point lets it hand the answer over instead
        /// of re-reading the log and re-running the same race. The signature and
        /// manager checks below still gate the attach, so a wrong base is
        /// refused here as before.
        pub fn attach_at_base(shad_log: &Path, eboot_base: u64) -> Result<Self> {
            let (process_id, process) = open_shad()?;
            let mut signature = [0u8; SETTER_WRITE_SIGNATURE.len()];
            process.read_bytes(eboot_base + SETTER_WRITE_RVA, &mut signature)?;
            ensure!(
                signature == SETTER_WRITE_SIGNATURE,
                "Bloodborne 01.09 event-flag signature mismatch at eboot+0x{SETTER_WRITE_RVA:X}: expected {:02X?}, found {:02X?}",
                SETTER_WRITE_SIGNATURE,
                signature
            );
            let manager: u64 = process.read(eboot_base + MANAGER_ROOT_RVA)?;
            if manager == 0 {
                // clients#420: startup ordering, not a bad build. A distinct
                // error type so the caller can wait instead of exiting.
                return Err(anyhow::Error::new(EventFlagManagerNotInitialized));
            }
            Ok(Self {
                process_id,
                process,
                eboot_base,
                shad_log: shad_log.to_owned(),
            })
        }

        pub fn info(&self) -> AttachmentInfo {
            AttachmentInfo {
                process_id: self.process_id,
                eboot_base: self.eboot_base,
            }
        }

        fn address_and_mask(&self, event_flag: u32) -> Result<(u64, u8)> {
            let manager: u64 = self.process.read(self.eboot_base + MANAGER_ROOT_RVA)?;
            ensure!(
                manager != 0,
                "Bloodborne event-flag manager became unavailable"
            );

            let divisor: u32 = self.process.read(manager + GROUP_DIVISOR_OFFSET)?;
            ensure!(divisor != 0, "Bloodborne event-flag group divisor is zero");
            let group = event_flag / divisor;
            let suffix = event_flag % divisor;
            let packed_stride: u32 = self.process.read(manager + PACKED_STRIDE_OFFSET)?;
            let packed_base: u64 = self.process.read(manager + PACKED_BASE_OFFSET)?;
            let sentinel: u64 = self.process.read(manager + GROUP_TREE_OFFSET)?;
            ensure!(
                sentinel != 0,
                "Bloodborne event-flag group tree is unavailable"
            );

            let mut candidate = sentinel;
            let mut link = sentinel + 8;
            for _ in 0..512 {
                let node: u64 = self.process.read(link)?;
                ensure!(node != 0, "null node in Bloodborne event-flag group tree");
                let is_nil: u8 = self.process.read(node + NODE_NIL_OFFSET)?;
                if is_nil != 0 {
                    break;
                }
                let key: u32 = self.process.read(node + NODE_KEY_OFFSET)?;
                if key < group {
                    link = node + 0x10;
                } else {
                    candidate = node;
                    link = node;
                }
            }

            ensure!(candidate != sentinel, "event-flag group {group} is absent");
            let candidate_key: u32 = self.process.read(candidate + NODE_KEY_OFFSET)?;
            ensure!(candidate_key == group, "event-flag group {group} is absent");
            let storage_type: u32 = self.process.read(candidate + NODE_STORAGE_TYPE_OFFSET)?;
            let bank_base = match storage_type {
                2 => self.process.read(candidate + NODE_STORAGE_OFFSET)?,
                1 => {
                    let bank_index: u32 = self.process.read(candidate + NODE_STORAGE_OFFSET)?;
                    packed_base + u64::from(bank_index) * u64::from(packed_stride)
                }
                other => bail!("unsupported event-flag group storage type {other}"),
            };
            ensure!(bank_base != 0, "event-flag group {group} has no storage");

            let mask = 1u8 << (7 - (suffix % 8));
            Ok((bank_base + u64::from(suffix / 8), mask))
        }

        pub fn read(&self, event_flag: u32) -> Result<bool> {
            let (address, mask) = self.address_and_mask(event_flag)?;
            let value: u8 = self.process.read(address)?;
            Ok(value & mask != 0)
        }

        /// Set a save-resident event flag and verify the resulting byte.
        /// The operation is idempotent, which makes received-item replay safe.
        pub fn write(&self, event_flag: u32, enabled: bool) -> Result<()> {
            let (address, mask) = self.address_and_mask(event_flag)?;
            let before: u8 = self.process.read(address)?;
            let after = if enabled {
                before | mask
            } else {
                before & !mask
            };
            if after != before {
                self.process.write_byte(address, after)?;
            }
            ensure!(
                self.read(event_flag)? == enabled,
                "event flag {event_flag} write did not stick"
            );
            Ok(())
        }

        /// Validate the minimum live event-flag-manager structure needed for
        /// gameplay reads without interpreting any particular world flag.
        ///
        /// This is deliberately a readiness probe, not save identification.
        /// The unsafe MVP mode uses several consecutive successful probes only
        /// after the player has explicitly attested that the correct save is
        /// loaded.
        pub fn probe_manager(&self) -> Result<()> {
            let manager: u64 = self.process.read(self.eboot_base + MANAGER_ROOT_RVA)?;
            ensure!(
                manager != 0,
                "Bloodborne event-flag manager became unavailable"
            );
            let divisor: u32 = self.process.read(manager + GROUP_DIVISOR_OFFSET)?;
            ensure!(divisor != 0, "Bloodborne event-flag group divisor is zero");
            let packed_stride: u32 = self.process.read(manager + PACKED_STRIDE_OFFSET)?;
            ensure!(
                packed_stride != 0,
                "Bloodborne event-flag packed stride is zero"
            );
            let sentinel: u64 = self.process.read(manager + GROUP_TREE_OFFSET)?;
            ensure!(
                sentinel != 0,
                "Bloodborne event-flag group tree is unavailable"
            );
            let _: u8 = self.process.read(sentinel + NODE_NIL_OFFSET)?;
            Ok(())
        }

        pub fn probe_manager_resilient(&mut self) -> Result<()> {
            match self.probe_manager() {
                Ok(()) => Ok(()),
                Err(read_error) => {
                    let replacement = Self::attach(&self.shad_log).with_context(|| {
                        format!(
                            "reattaching to shadPS4 after gameplay probe failed: {read_error:#}"
                        )
                    })?;
                    *self = replacement;
                    self.probe_manager()
                        .context("probing event-flag manager after shadPS4 reattachment")
                }
            }
        }

        pub fn read_resilient(&mut self, event_flag: u32) -> Result<bool> {
            match self.read(event_flag) {
                Ok(value) => Ok(value),
                Err(read_error) => {
                    let replacement = Self::attach(&self.shad_log).with_context(|| {
                        format!(
                            "reattaching to shadPS4 after event-flag read failed: {read_error:#}"
                        )
                    })?;
                    *self = replacement;
                    self.read(event_flag)
                        .context("reading event flag after shadPS4 reattachment")
                }
            }
        }

        pub fn write_resilient(&mut self, event_flag: u32, enabled: bool) -> Result<()> {
            match self.write(event_flag, enabled) {
                Ok(()) => Ok(()),
                Err(write_error) => {
                    let replacement = Self::attach(&self.shad_log).with_context(|| {
                        format!(
                            "reattaching to shadPS4 after event-flag write failed: {write_error:#}"
                        )
                    })?;
                    *self = replacement;
                    self.write(event_flag, enabled)
                        .context("writing event flag after shadPS4 reattachment")
                }
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::path::Path;

    use anyhow::{Result, bail};

    use super::AttachmentInfo;

    pub struct LiveEventFlags;

    impl LiveEventFlags {
        pub fn attach(_shad_log: &Path) -> Result<Self> {
            bail!("live Bloodborne event-flag reads require Windows")
        }

        pub fn attach_at_base(_shad_log: &Path, _eboot_base: u64) -> Result<Self> {
            bail!("live Bloodborne event-flag reads require Windows")
        }

        pub fn read(&self, _event_flag: u32) -> Result<bool> {
            bail!("live Bloodborne event-flag reads require Windows")
        }

        pub fn read_resilient(&mut self, _event_flag: u32) -> Result<bool> {
            bail!("live Bloodborne event-flag reads require Windows")
        }

        pub fn write_resilient(&mut self, _event_flag: u32, _enabled: bool) -> Result<()> {
            bail!("live Bloodborne event-flag writes require Windows")
        }

        pub fn probe_manager(&self) -> Result<()> {
            bail!("live Bloodborne event-flag reads require Windows")
        }

        pub fn probe_manager_resilient(&mut self) -> Result<()> {
            bail!("live Bloodborne event-flag reads require Windows")
        }

        pub fn info(&self) -> AttachmentInfo {
            AttachmentInfo {
                process_id: 0,
                eboot_base: 0,
            }
        }
    }
}

pub use platform::LiveEventFlags;

#[cfg(test)]
mod tests {
    use super::parse_latest_eboot_base;

    #[test]
    fn parses_latest_eboot_mapping() {
        let log =
            "Loading module eboot.bin to 0x5700000\nother\nLoading module eboot.bin to 0x5660000\n";
        assert_eq!(parse_latest_eboot_base(log).unwrap(), 0x5660000);
    }
}
