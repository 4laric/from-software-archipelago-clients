//! The live [`ThreadController`] for shadPS4, behind `#[cfg(windows)]`.
//!
//! Untested against a live game -- like every other live-attach seam in this
//! crate, it is exercised only by the CI Windows build and must be validated by
//! the owner against a running process before the native path is trusted. The
//! install atomicity *algorithm* that drives it is fully host-tested in
//! `install.rs` against a scripted fake; this module is only the OS glue that
//! enumerates, suspends, samples RIP, and resumes the threads.

#[cfg(windows)]
mod windows_impl {
    use anyhow::{Context, Result, bail};
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Diagnostics::Debug::{CONTEXT, CONTEXT_FLAGS, GetThreadContext};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows::Win32::System::Threading::{
        OpenThread, ResumeThread, SuspendThread, THREAD_GET_CONTEXT, THREAD_SUSPEND_RESUME,
    };

    use super::super::install::ThreadController;

    /// `CONTEXT_CONTROL` for amd64: enough of the register file to read RIP.
    const CONTEXT_CONTROL_AMD64: u32 = 0x0010_0001;

    /// A live thread controller scoped to one process id.
    pub struct WindowsThreadController {
        process_id: u32,
        suspended: Vec<HANDLE>,
    }

    impl WindowsThreadController {
        pub fn new(process_id: u32) -> Self {
            Self {
                process_id,
                suspended: Vec::new(),
            }
        }

        fn resume_and_close(&mut self) {
            for handle in self.suspended.drain(..) {
                unsafe {
                    ResumeThread(handle);
                    let _ = CloseHandle(handle);
                }
            }
        }
    }

    impl Drop for WindowsThreadController {
        fn drop(&mut self) {
            // Never leave a thread suspended if the install is dropped.
            self.resume_and_close();
        }
    }

    impl ThreadController for WindowsThreadController {
        fn suspend_all(&mut self) -> Result<usize> {
            if !self.suspended.is_empty() {
                bail!("suspend_all called while threads are already suspended");
            }
            let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }
                .context("CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD)")?;
            // Zero-initialise rather than rely on a Default impl: THREADENTRY32
            // is a plain FFI record and all-zero is valid.
            let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut result = unsafe { Thread32First(snapshot, &mut entry) };
            while result.is_ok() {
                if entry.th32OwnerProcessID == self.process_id
                    && let Ok(handle) = unsafe {
                        OpenThread(
                            THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT,
                            false,
                            entry.th32ThreadID,
                        )
                    }
                {
                    if unsafe { SuspendThread(handle) } == u32::MAX {
                        let _ = unsafe { CloseHandle(handle) };
                    } else {
                        self.suspended.push(handle);
                    }
                }
                entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
                result = unsafe { Thread32Next(snapshot, &mut entry) };
            }
            let _ = unsafe { CloseHandle(snapshot) };
            if self.suspended.is_empty() {
                bail!(
                    "no shadPS4 threads could be suspended for process {}",
                    self.process_id
                );
            }
            Ok(self.suspended.len())
        }

        fn resume_all(&mut self) -> Result<()> {
            self.resume_and_close();
            Ok(())
        }

        fn instruction_pointers(&mut self) -> Result<Vec<u64>> {
            let mut rips = Vec::with_capacity(self.suspended.len());
            for handle in &self.suspended {
                // CONTEXT is 16-byte aligned and contains a union; zero it
                // rather than depend on a Default impl, then request only the
                // control registers so RIP is populated.
                let mut context: CONTEXT = unsafe { std::mem::zeroed() };
                context.ContextFlags = CONTEXT_FLAGS(CONTEXT_CONTROL_AMD64);
                unsafe { GetThreadContext(*handle, &mut context) }
                    .context("GetThreadContext while sampling RIP")?;
                rips.push(context.Rip);
            }
            Ok(rips)
        }
    }
}

#[cfg(windows)]
pub use windows_impl::WindowsThreadController;

/// Non-Windows placeholder so `NativeBackend` compiles on any host. It refuses
/// to suspend anything; the native path never reaches it because the memory
/// accessor already fails to open off Windows.
#[cfg(not(windows))]
pub struct WindowsThreadController;

#[cfg(not(windows))]
impl WindowsThreadController {
    pub fn new(_process_id: u32) -> Self {
        Self
    }
}

#[cfg(not(windows))]
impl super::install::ThreadController for WindowsThreadController {
    fn suspend_all(&mut self) -> anyhow::Result<usize> {
        anyhow::bail!("native Bloodborne delivery requires Windows")
    }
    fn resume_all(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
    fn instruction_pointers(&mut self) -> anyhow::Result<Vec<u64>> {
        anyhow::bail!("native Bloodborne delivery requires Windows")
    }
}
