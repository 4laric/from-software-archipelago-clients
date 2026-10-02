//! Cross-DLL serialization for the MinHook create/enable transaction.
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::w;

pub(crate) struct Guard(HANDLE);

impl Guard {
    pub(crate) fn acquire() -> Result<Self, String> {
        Self::acquire_with_timeout(30_000)
    }

    fn acquire_with_timeout(timeout_ms: u32) -> Result<Self, String> {
        let handle =
            unsafe { CreateMutexW(None, false, w!("Local\\ERArchipelagoHudhookInstall.v1")) }
                .map_err(|error| {
                    format!("could not create the shared overlay installer mutex: {error}")
                })?;
        let status = unsafe { WaitForSingleObject(handle, timeout_ms) };
        if status == WAIT_OBJECT_0 || status == WAIT_ABANDONED {
            return Ok(Self(handle));
        }
        let _ = unsafe { CloseHandle(handle) };
        Err(format!(
            "shared overlay installer mutex wait failed ({status:?}); restart the game to retry"
        ))
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = unsafe { ReleaseMutex(self.0) };
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_installer_waits_and_unwinding_releases_ownership() {
        let result = std::panic::catch_unwind(|| {
            let _guard = Guard::acquire().unwrap();
            assert!(
                std::thread::spawn(|| Guard::acquire_with_timeout(1).is_err())
                    .join()
                    .unwrap()
            );
            panic!("test installation failure");
        });
        assert!(result.is_err());
        assert!(
            std::thread::spawn(|| Guard::acquire_with_timeout(100).is_ok())
                .join()
                .unwrap()
        );
    }
}
