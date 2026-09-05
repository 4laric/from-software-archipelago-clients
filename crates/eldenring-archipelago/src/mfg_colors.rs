//! Optional presentation snapshot. Never loads an engine or reads hidden item placements.
use er_logic::mfg_bridge::{ABI_VERSION, Info};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetModuleHandleExA, GetProcAddress};
use windows::core::s;

const CAP_LOT_STYLES: u32 = 2;
const LEASE_MS: u32 = 3_000;
const REFRESH_MS: u64 = 1_000;

#[repr(C)]
pub struct LotStyle {
    pub lot_table: u32,
    pub lot_row: u32,
    pub style: u32,
}

const _: () = assert!(size_of::<LotStyle>() == 12);

type Query = unsafe extern "C" fn(u32, *mut Info, u32) -> u32;
type SetStyles = unsafe extern "C" fn(u32, *const LotStyle, u32, u32) -> u32;

struct LoadedModule(HMODULE);
impl Drop for LoadedModule {
    fn drop(&mut self) {
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

fn publish(entries: &[LotStyle]) -> Result<(), &'static str> {
    if entries.len() > 8192 {
        return Err("Too many map colors; nothing was sent.");
    }
    let mut handle = HMODULE::default();
    // Retain a loaded module for the duration of both calls. Never LoadLibrary.
    unsafe { GetModuleHandleExA(0, s!("MapForGoblins.dll"), &mut handle) }
        .map_err(|_| "Map engine is not loaded.")?;
    let module = LoadedModule(handle);
    let query = unsafe { GetProcAddress(module.0, s!("MFG_AP_QUERY_V1")) };
    let set = unsafe { GetProcAddress(module.0, s!("MFG_AP_SET_LOT_STYLES_V1")) };
    let (Some(query), Some(set)) = (query, set) else {
        return Err("Update the source-built map engine to enable colored rings.");
    };
    // These exact exports use the shared versioned C contract.
    let query: Query = unsafe { std::mem::transmute(query) };
    let set: SetStyles = unsafe { std::mem::transmute(set) };
    let mut info = Info::default();
    let result = unsafe { query(ABI_VERSION, &mut info, size_of::<Info>() as u32) };
    if result != 0
        || info.abi_version != ABI_VERSION
        || info.struct_size != size_of::<Info>() as u32
        || info.capabilities & CAP_LOT_STYLES == 0
    {
        return Err("Map colors are waiting for the supported map engine.");
    }
    let ptr = if entries.is_empty() {
        std::ptr::null()
    } else {
        entries.as_ptr()
    };
    let lease = if entries.is_empty() { 0 } else { LEASE_MS };
    let result = unsafe { set(ABI_VERSION, ptr, entries.len() as u32, lease) };
    if result != 0 {
        return Err("The map engine could not accept these colors.");
    }
    Ok(())
}

#[derive(Default)]
pub struct Colors {
    enabled: bool,
    next_ms: u64,
    published: bool,
    status: Option<&'static str>,
}

impl Colors {
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
            let _ = publish(&[]);
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

    pub fn send(&mut self, entries: &[LotStyle]) {
        match publish(entries) {
            Ok(()) => {
                self.published = !entries.is_empty();
                self.status =
                    Some("Map colors active. Yellow: hinted; orange: progression-eligible.");
            }
            Err(status) => self.status = Some(status),
        }
    }

    pub fn status(&self) -> &'static str {
        self.status
            .unwrap_or("Map colors will start when connected and in the world.")
    }
}
