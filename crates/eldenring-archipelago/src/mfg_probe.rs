//! Observation-only probe for a stable MapForGoblins marker-control seam.
//!
//! MapForGoblins 2.1.3 contains internal visibility and colour controls, but does not export them.
//! We deliberately detect only named exports: writing version-specific DLL offsets would turn an
//! optional UI integration into a crash hazard every time MapForGoblins updates.

use er_logic::mfg_probe::{Capability, classify};
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::core::s;

const VERSION_EXPORT: windows::core::PCSTR = s!("MFG_AP_API_VERSION");
const SET_STATE_EXPORT: windows::core::PCSTR = s!("MFG_AP_SET_MARKER_STATE_V1");
const CLEAR_EXPORT: windows::core::PCSTR = s!("MFG_AP_CLEAR_MARKER_STATES_V1");

fn has_export(module: windows::Win32::Foundation::HMODULE, name: windows::core::PCSTR) -> bool {
    // Safety: `module` came from the loader and we only test whether the named address exists. No
    // foreign function is called, so an accidental ABI mismatch cannot corrupt the process.
    unsafe { GetProcAddress(module, name).is_some() }
}

pub fn report() -> String {
    // GetModuleHandle does not load or initialize the optional DLL; it only observes loader state.
    let Ok(module) = (unsafe { GetModuleHandleA(s!("MapForGoblins.dll")) }) else {
        return "mfgprobe: MapForGoblins.dll is not loaded".to_string();
    };

    let version = has_export(module, VERSION_EXPORT);
    let set_state = has_export(module, SET_STATE_EXPORT);
    let clear = has_export(module, CLEAR_EXPORT);
    let exports = format!("version={version}, set-state-v1={set_state}, clear-v1={clear}");

    match classify(true, version, set_state, clear) {
        Capability::ObserveOnly => format!(
            "mfgprobe: loaded; {exports}; observe-only (2.1.3 has internal icon controls but no stable external API)"
        ),
        Capability::IncompleteApi => format!(
            "mfgprobe: loaded; {exports}; REFUSED partial API (all v1 exports are required)"
        ),
        Capability::ControllableV1 => format!(
            "mfgprobe: loaded; {exports}; v1 control surface detected (probe does not mutate icons)"
        ),
        Capability::NotLoaded => unreachable!("module handle was resolved"),
    }
}
