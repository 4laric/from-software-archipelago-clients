//! Explicit read-only diagnostic. No automatic loading or offset probing.
use er_logic::mfg_bridge::{
    ABI_VERSION, Bridge, Capture, CaptureTick, Hover, Info, RecordedHover, negotiate,
};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetModuleHandleExA, GetProcAddress};
use windows::core::s;

type Query = unsafe extern "C" fn(u32, *mut Info, u32) -> u32;
type CopyHover = unsafe extern "C" fn(*mut Hover, u32) -> u32;

struct LoadedModule(HMODULE);
impl Drop for LoadedModule {
    fn drop(&mut self) {
        // Balance only the reference acquired by GetModuleHandleExA.
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

/// A copied observation from an already-loaded engine; never loads a DLL.
pub fn sample() -> Result<Hover, String> {
    let mut handle = HMODULE::default();
    // Flags=0 retains an already-loaded module without loading an absent DLL.
    if unsafe { GetModuleHandleExA(0, s!("MapForGoblins.dll"), &mut handle) }.is_err() {
        return Err("MapForGoblins.dll is not loaded".to_string());
    }
    let module = LoadedModule(handle);
    let query = unsafe { GetProcAddress(module.0, s!("MFG_AP_QUERY_V1")) };
    let copy = unsafe { GetProcAddress(module.0, s!("MFG_AP_COPY_HOVER_V1")) };
    let (Some(query), Some(copy)) = (query, copy) else {
        return Err(
            "loaded; no complete read-only v1 API (stock builds are unsupported)".to_string(),
        );
    };
    // Exact exports follow docs/include/mfg_ap_readonly_v1.h. A loaded engine
    // must honor that contract: negotiation cannot validate arbitrary code.
    let query: Query = unsafe { std::mem::transmute(query) };
    let copy: CopyHover = unsafe { std::mem::transmute(copy) };
    let mut info = Info::default();
    let result = unsafe { query(ABI_VERSION, &mut info, size_of::<Info>() as u32) };
    if result != 0 {
        return Err(format!("read-only query refused ({result})"));
    }
    if let Err(reason) = negotiate(info) {
        return Err(format!("read-only API refused ({reason:?})"));
    }
    let mut hover = Hover::default();
    let result = unsafe { copy(&mut hover, size_of::<Hover>() as u32) };
    if result != 0 {
        return Err(format!("read-only API ready; hover unavailable ({result})"));
    }
    Ok(hover)
}

/// One-shot console diagnostic retained for compatibility.
pub fn report() -> String {
    let hover = match sample() {
        Ok(hover) => hover,
        Err(reason) => return format!("mfgprobe: {reason}"),
    };
    let mut bridge = Bridge::default();
    bridge.reset(true);
    match bridge.accept(hover) {
        Ok(Some(selected)) => format!(
            "mfgprobe: hover generation={} handle={} original-flag={} lot-table={} lot-row={} (identity only; AP binding not established)",
            selected.generation,
            selected.handle,
            selected.original_flag,
            selected.lot_table,
            selected.lot_row
        ),
        Ok(None) => "mfgprobe: read-only API ready; no hovered marker".to_string(),
        Err(reason) => format!("mfgprobe: hover refused ({reason:?})"),
    }
}

/// UI-owned capture, driven by the always-rendered overlay callback. No worker
/// thread, automatic loading, persistent opt-in, or per-poll logging.
#[derive(Default)]
pub struct HoverCapture {
    capture: Capture,
    last_status: Option<String>,
    summary: Option<String>,
    connected: Option<bool>,
}

impl HoverCapture {
    pub fn reset(&mut self) {
        self.capture.reset();
        self.last_status = None;
        self.summary = None;
    }

    pub fn sync_connection(&mut self, connected: bool) {
        if self.connected.is_some_and(|previous| previous != connected) {
            self.reset();
        }
        self.connected = Some(connected);
    }

    pub fn arm(&mut self, now_ms: u64) {
        self.reset();
        self.capture.arm(now_ms);
    }

    /// Historical copied sample; never exposes a live engine selection.
    pub fn recorded(&self) -> Option<RecordedHover> {
        self.capture.recorded
    }

    pub fn active(&self) -> bool {
        self.capture.active()
    }

    pub fn text(&self) -> &str {
        self.summary.as_deref().unwrap_or(if self.active() {
            "Recording for up to 30 seconds. Close F6, hide the client with F5 if open, then hover a map pin. Reopen F6 to read the result."
        } else {
            "Record a map pin without keeping the client open. Nothing is submitted or changed."
        })
    }

    /// Returns at most one completion line for each explicitly armed window.
    pub fn tick(&mut self, now_ms: u64, input_released: bool) -> Option<String> {
        let summary = match self.capture.tick(now_ms, input_released) {
            CaptureTick::Idle => return None,
            CaptureTick::TimedOut => format!(
                "Map pin recording ended after 30 seconds without a fresh hover. Last observation: {}",
                self.last_status
                    .as_deref()
                    .unwrap_or("none; close F6 and release client input")
            ),
            CaptureTick::Poll => {
                let hover = match sample() {
                    Ok(hover) => hover,
                    Err(reason) => {
                        self.capture.unavailable();
                        self.last_status = Some(reason);
                        return None;
                    }
                };
                match self.capture.accept(now_ms, hover) {
                    Ok(Some(recorded)) => format!(
                        "Recorded map pin at client +{} ms, {} ms after starting (source age {} ms): generation={} handle={} original-flag={} lot-table={} lot-row={}. Historical observation, not a live hover; AP binding not established.",
                        recorded.received_ms,
                        recorded.elapsed_ms,
                        recorded.source_age_ms,
                        recorded.selection.generation,
                        recorded.selection.handle,
                        recorded.selection.original_flag,
                        recorded.selection.lot_table,
                        recorded.selection.lot_row,
                    ),
                    Ok(None) => {
                        self.last_status = Some("no hovered marker".to_string());
                        return None;
                    }
                    Err(reason) => {
                        self.last_status = Some(format!("hover refused ({reason:?})"));
                        return None;
                    }
                }
            }
        };
        self.summary = Some(summary.clone());
        Some(summary)
    }
}
