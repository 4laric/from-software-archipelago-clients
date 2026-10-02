//! Thin dynamic ABI adapter. Never loads a DLL or calls SM64 from the AP thread.
use er_logic::mario::{ABI_VERSION, Config, Session};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetModuleHandleExA, GetProcAddress};
use windows::core::s;

use er_logic::mario::BridgeState as State;
type Version = unsafe extern "C" fn() -> u32;
type Set = unsafe extern "C" fn(u32, u32) -> u32;
type Get = unsafe extern "C" fn(*mut State) -> u32;
struct Module(HMODULE);
impl Drop for Module {
    fn drop(&mut self) {
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

struct Bridge {
    _module: Module,
    set: Set,
    get: Get,
}
impl Bridge {
    fn discover() -> Result<Self, String> {
        let mut handle = HMODULE::default();
        // Retain only an already loaded module while inspecting/calling its exports.
        unsafe { GetModuleHandleExA(0, s!("er_mario.dll"), &mut handle) }.map_err(|_| {
            "Mario seed refused: load the AP-compatible er_mario.dll through me3".to_string()
        })?;
        let module = Module(handle);
        let version = unsafe { GetProcAddress(module.0, s!("er_mario_ap_abi_version")) };
        let set = unsafe { GetProcAddress(module.0, s!("er_mario_ap_set_capabilities")) };
        let get = unsafe { GetProcAddress(module.0, s!("er_mario_ap_get_state")) };
        let (Some(version), Some(set), Some(get)) = (version, set, get) else {
            return Err(
                "Mario seed refused: update er_mario.dll to the capability ABI v1 build".into(),
            );
        };
        let version: Version = unsafe { std::mem::transmute(version) };
        if unsafe { version() } != ABI_VERSION {
            return Err(
                "Mario seed refused: er_mario.dll capability ABI differs from this AP client"
                    .into(),
            );
        }
        let set: Set = unsafe { std::mem::transmute(set) };
        let get: Get = unsafe { std::mem::transmute(get) };
        Ok(Self {
            _module: module,
            set,
            get,
        })
    }
    fn state(&self) -> Result<State, String> {
        let mut state = State::default();
        if unsafe { (self.get)(&mut state) } != 1 || state.abi_version != ABI_VERSION {
            return Err("Mario progression paused: capability state read-back failed".into());
        }
        Ok(state)
    }
}

#[derive(Default)]
pub struct Runtime {
    pub session: Session,
    applied: bool,
    history_len: Option<usize>,
    warning: Option<String>,
    warning_shown_at_ms: u64,
}
impl Runtime {
    /// Validate exports before any slot configuration or progression is applied.
    pub fn configure(&mut self, identity: String, config: Option<Config>) -> Result<(), String> {
        if config.is_some() {
            Bridge::discover()?.state()?;
        }
        if self.session.identity.as_ref() != Some(&identity) || self.session.config != config {
            self.applied = false;
            self.history_len = None;
        }
        self.session.configure(identity, config);
        Ok(())
    }
    pub fn history_due(&self, len: usize) -> bool {
        self.session.config.is_some() && self.history_len != Some(len)
    }
    pub fn receive_history(&mut self, received: Vec<(i64, i64)>) {
        self.history_len = Some(received.len());
        self.session.receive_history(received);
    }
    pub fn armed(&self) -> bool {
        self.session.config.is_some() && self.applied
    }
    pub fn refresh(&mut self) -> Result<(), String> {
        let Some(config) = &self.session.config else {
            return Ok(());
        };
        self.applied = false;
        let bridge = Bridge::discover()?;
        let state = bridge.state()?;
        let expected = self.session.unlocked;
        // Setter queues only on changed state. Waiting for worker acknowledgment is intentional:
        // seeing exports alone does not prove the actual action gate is enforcing the seed.
        if state.managed != config.managed || state.unlocked != expected || state.flags & 4 == 0 {
            if unsafe { (bridge.set)(config.managed, expected) } != 1 {
                return Err("Mario progression paused: capability update was rejected".into());
            }
            return Err(
                "Mario progression paused: waiting for capability worker acknowledgment".into(),
            );
        }
        if !state.acknowledged(config.managed, expected) {
            return Err("Mario progression paused: Mario is not ready and enabled".into());
        }
        self.applied = true;
        Ok(())
    }
    pub fn warning_due(&mut self, warning: &str, now_ms: u64) -> bool {
        if self.warning.as_deref() == Some(warning)
            && now_ms.saturating_sub(self.warning_shown_at_ms) < 10_000
        {
            return false;
        }
        self.warning = Some(warning.to_string());
        self.warning_shown_at_ms = now_ms;
        true
    }
    pub fn clear_warning(&mut self) {
        self.warning = None;
        self.warning_shown_at_ms = 0;
    }
    pub fn reset(&mut self) {
        // Remove a previous seed's managed locks, if its bridge remains loaded. No vanilla seed
        // depends on Mario, so an absent bridge must not prevent a normal connection.
        if self.session.config.is_some()
            && let Ok(bridge) = Bridge::discover()
        {
            let _ = unsafe { (bridge.set)(0, 0) };
        }
        *self = Self::default();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unchanged_failure_is_renewed_without_frame_spam_and_success_clears_it() {
        let mut runtime = Runtime::default();
        assert!(runtime.warning_due("not ready", 500));
        assert!(!runtime.warning_due("not ready", 501));
        assert!(!runtime.warning_due("not ready", 10_499));
        assert!(runtime.warning_due("not ready", 10_500));
        assert!(!runtime.warning_due("not ready", 10_501));
        assert!(runtime.warning_due("bridge missing", 10_502));
        assert!(!runtime.warning_due("bridge missing", 10_503));
        runtime.clear_warning();
        assert!(runtime.warning_due("bridge missing", 10_504));
    }
    #[test]
    fn history_snapshot_is_idle_off_and_rebuilt_only_when_needed() {
        let mut runtime = Runtime::default();
        assert!(!runtime.history_due(0));
        assert!(!runtime.history_due(5000));
        runtime.session.configure(
            "seed:1".into(),
            Some(Config {
                managed: 1023,
                unlock_items: [(1, 129)].into_iter().collect(),
            }),
        );
        assert!(runtime.history_due(1));
        runtime.receive_history(vec![(0, 1)]);
        assert!(!runtime.history_due(1));
        assert!(runtime.history_due(0));
        runtime.receive_history(vec![]);
        assert_eq!(runtime.session.unlocked, 1);
        assert!(runtime.history_due(2));
        runtime.receive_history(vec![(0, 1), (1, 1)]);
        assert_eq!(runtime.session.unlocked, 129);
        assert!(!runtime.history_due(2));
        runtime.reset();
        assert!(!runtime.history_due(2));
        assert_eq!(runtime.session.unlocked, 0);
    }
}
