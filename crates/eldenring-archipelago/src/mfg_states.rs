//! Optional complete check-state snapshot. Empty active seeds retain a positive lease.
use er_logic::mfg_bridge::{ABI_VERSION, Info};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetModuleHandleExA, GetProcAddress};
use windows::core::s;

const CAP_CHECK_STATES: u32 = 4;
const LEASE_MS: u32 = 3_000;
const REFRESH_MS: u64 = 1_000;

use er_logic::mfg_match::LotCheckState;
const _: () = assert!(size_of::<LotCheckState>() == 12);

type Query = unsafe extern "C" fn(u32, *mut Info, u32) -> u32;
type SetStates = unsafe extern "C" fn(u32, *const LotCheckState, u32, u32) -> u32;

struct LoadedModule(HMODULE);
impl Drop for LoadedModule {
    fn drop(&mut self) {
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

fn publish(entries: &[LotCheckState], active: bool) -> Result<(), &'static str> {
    if entries.len() > 8192 {
        return Err("Too many check states; nothing was sent.");
    }
    let mut handle = HMODULE::default();
    // Retain a loaded module for the duration of both calls. Never LoadLibrary.
    unsafe { GetModuleHandleExA(0, s!("MapForGoblins.dll"), &mut handle) }
        .map_err(|_| "Map engine is not loaded.")?;
    let module = LoadedModule(handle);
    let query = unsafe { GetProcAddress(module.0, s!("MFG_AP_QUERY_V1")) };
    let set = unsafe { GetProcAddress(module.0, s!("MFG_AP_SET_CHECK_STATES_V1")) };
    let (Some(query), Some(set)) = (query, set) else {
        return Err("Update the source-built map engine to enable check filters.");
    };
    // These exact exports use the shared versioned C contract.
    let query: Query = unsafe { std::mem::transmute(query) };
    let set: SetStates = unsafe { std::mem::transmute(set) };
    let mut info = Info::default();
    let result = unsafe { query(ABI_VERSION, &mut info, size_of::<Info>() as u32) };
    if result != 0
        || info.abi_version != ABI_VERSION
        || info.struct_size != size_of::<Info>() as u32
        || info.capabilities & CAP_CHECK_STATES == 0
    {
        return Err("Map check filters are waiting for the supported map engine.");
    }
    let ptr = if entries.is_empty() {
        std::ptr::null()
    } else {
        entries.as_ptr()
    };
    let lease = if active { LEASE_MS } else { 0 };
    let result = unsafe { set(ABI_VERSION, ptr, entries.len() as u32, lease) };
    if result != 0 {
        return Err("The map engine could not accept these check states.");
    }
    Ok(())
}

#[derive(Default)]
pub struct States {
    enabled: bool,
    next_ms: u64,
    published: bool,
    status: Option<&'static str>,
}

impl States {
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.clear();
            self.enabled = enabled;
        }
    }

    /// Immediately withdraw a previous snapshot if possible. A missing/replaced
    /// engine also drops it automatically when its short lease expires.
    pub fn clear(&mut self) {
        if self.published {
            let _ = publish(&[], false);
        }
        self.published = false;
        self.next_ms = 0;
        self.status = None;
    }

    pub fn due(&mut self, now_ms: u64) -> bool {
        if !self.enabled || now_ms < self.next_ms {
            return false;
        }
        self.next_ms = now_ms.saturating_add(REFRESH_MS);
        true
    }

    pub fn send(&mut self, entries: &[LotCheckState]) {
        match publish(entries, true) {
            Ok(()) => {
                self.published = true;
                self.status = Some(
                    "Map check filters active. Local region access; additional quest requirements are not evaluated.",
                );
            }
            Err(status) => self.status = Some(status),
        }
    }

    pub fn status(&self) -> &'static str {
        self.status
            .unwrap_or("Map check filters will start when connected and in the world.")
    }
}
