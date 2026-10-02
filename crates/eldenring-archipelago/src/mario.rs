//! Thin dynamic ABI adapter. Never loads a DLL or calls SM64 from the AP thread.
use er_logic::mario::{ABI_VERSION, Config, Session, Stats, StatsState};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetModuleHandleExA, GetProcAddress};
use windows::core::s;

use er_logic::mario::BridgeState as State;
type Version = unsafe extern "C" fn() -> u32;
type Set = unsafe extern "C" fn(u32, u32) -> u32;
type Get = unsafe extern "C" fn(*mut State) -> u32;
type GetStats = unsafe extern "C" fn(*mut StatsState) -> u32;
struct StatsBridge {
    set: Set,
    get: GetStats,
}
impl StatsBridge {
    fn state(&self) -> Result<StatsState, String> {
        let mut state = StatsState::default();
        if unsafe { (self.get)(&mut state) } != 1 || state.abi_version != ABI_VERSION {
            return Err("Mario progression paused: stat state read-back failed".into());
        }
        Ok(state)
    }
    fn set(&self, expected: Stats) -> Result<(), String> {
        if unsafe { (self.set)(expected.max_wedges, expected.power_basis_points) } != 1 {
            return Err("Mario progression paused: stat update was rejected".into());
        }
        Ok(())
    }
}
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
    stats: Option<StatsBridge>,
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
        let stats_set = unsafe { GetProcAddress(module.0, s!("er_mario_ap_set_stats")) };
        let stats_get = unsafe { GetProcAddress(module.0, s!("er_mario_ap_get_stats_state")) };
        let stats = match (stats_set, stats_get) {
            (Some(set), Some(get)) => Some(StatsBridge {
                set: unsafe {
                    std::mem::transmute::<unsafe extern "system" fn() -> isize, Set>(set)
                },
                get: unsafe {
                    std::mem::transmute::<unsafe extern "system" fn() -> isize, GetStats>(get)
                },
            }),
            _ => None,
        };
        Ok(Self {
            _module: module,
            set,
            get,
            stats,
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
    last_acknowledgment: Option<(u32, u32, Option<Stats>)>,
    acknowledged_stats: Option<Stats>,
    warning: Option<String>,
    warning_shown_at_ms: u64,
}
impl Runtime {
    /// Validate exports before any slot configuration or progression is applied.
    pub fn configure(&mut self, identity: String, config: Option<Config>) -> Result<(), String> {
        if let Some(config) = &config {
            let bridge = Bridge::discover()?;
            config.validate_bridge_features(&bridge.state()?)?;
            config.validate_stats_exports(bridge.stats.is_some())?;
        } else if let Ok(bridge) = Bridge::discover()
            && let Some(stats) = bridge.stats
        {
            // Vanilla progression never depends on an optional Mario bridge. Queue the normal
            // baseline once at connection, so a previously active stat seed cannot leak its caps.
            if let Err(error) = stats.set(Stats::default()) {
                log::warn!("Could not reset optional Mario stats for vanilla connection: {error}");
            }
        }
        if self.session.identity.as_ref() != Some(&identity) || self.session.config != config {
            self.applied = false;
            self.history_len = None;
            self.last_acknowledgment = None;
            self.acknowledged_stats = None;
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
    pub fn regression_armed(&self) -> bool {
        self.armed()
            && self
                .session
                .config
                .as_ref()
                .is_some_and(|c| c.requires_regression)
    }
    pub fn stats_armed(&self) -> bool {
        self.armed()
            && self
                .session
                .config
                .as_ref()
                .is_some_and(|c| c.stat_items.is_some())
            && self.acknowledged_stats == Some(self.session.expected_stats())
    }
    pub fn refresh(&mut self) -> Result<(), String> {
        let result = self.refresh_bridge();
        if result.is_err() {
            self.last_acknowledgment = None;
        } else if let Some(config) = &self.session.config {
            let masks = (
                config.managed,
                self.session.unlocked,
                self.acknowledged_stats,
            );
            if self.acknowledgment_due(masks) {
                log::info!(
                    "Mario capability worker acknowledged: identity={:?}, managed={:#010x}, unlocked={:#010x}",
                    self.session.identity,
                    masks.0,
                    masks.1
                );
                if let Some(stats) = masks.2 {
                    log::info!(
                        "Mario stat worker acknowledged: identity={:?}, max_wedges={}, power_basis_points={}",
                        self.session.identity,
                        stats.max_wedges,
                        stats.power_basis_points
                    );
                }
            }
        }
        result
    }
    fn acknowledgment_due(&mut self, masks: (u32, u32, Option<Stats>)) -> bool {
        if self.last_acknowledgment == Some(masks) {
            return false;
        }
        self.last_acknowledgment = Some(masks);
        true
    }
    fn refresh_bridge(&mut self) -> Result<(), String> {
        let Some(config) = &self.session.config else {
            return Ok(());
        };
        self.applied = false;
        let bridge = Bridge::discover()?;
        let state = bridge.state()?;
        config.validate_bridge_features(&state)?;
        config.validate_stats_exports(bridge.stats.is_some())?;
        let expected = self.session.unlocked;
        // Setter queues only on changed state. Waiting for worker acknowledgment is intentional:
        // seeing exports alone does not prove the actual action gate is enforcing the seed.
        let mut pending = false;
        if state.managed != config.managed || state.unlocked != expected || state.flags & 4 == 0 {
            if unsafe { (bridge.set)(config.managed, expected) } != 1 {
                return Err("Mario progression paused: capability update was rejected".into());
            }
            pending = true;
        }
        let stats = bridge.stats.as_ref().map(StatsBridge::state).transpose()?;
        if let Some(stats) = stats
            && !stats.matches(self.session.expected_stats())
        {
            bridge
                .stats
                .as_ref()
                .unwrap()
                .set(self.session.expected_stats())?;
            pending = true;
        }
        if pending {
            return Err(
                "Mario progression paused: waiting for capability/stat worker acknowledgment"
                    .into(),
            );
        }
        if !config.acknowledged(
            &state,
            stats.as_ref(),
            expected,
            self.session.expected_stats(),
        ) {
            return Err(
                "Mario progression paused: capability/stat worker is not ready and enabled".into(),
            );
        }
        self.applied = true;
        self.acknowledged_stats = stats.map(|_| self.session.expected_stats());
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
            if let Some(stats) = bridge.stats {
                let _ = stats.set(Stats::default());
            }
        }
        *self = Self::default();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn regression_probe_requires_declared_feature_and_live_acknowledgment() {
        let mut runtime = Runtime::default();
        runtime.session.configure(
            "seed:1".into(),
            Some(Config {
                managed: 1023,
                unlock_items: [(1, 129)].into_iter().collect(),
                requires_regression: true,
                stat_items: None,
            }),
        );
        assert!(!runtime.regression_armed());
        runtime.applied = true;
        assert!(runtime.regression_armed());
        runtime.session.config.as_mut().unwrap().requires_regression = false;
        assert!(!runtime.regression_armed());
        runtime.session.configure("vanilla:1".into(), None);
        assert!(!runtime.regression_armed());
    }
    #[test]
    fn stats_probe_requires_declared_feature_and_the_current_live_snapshot() {
        let mut runtime = Runtime::default();
        runtime.session.configure(
            "seed:1".into(),
            Some(Config {
                managed: 1023,
                unlock_items: Default::default(),
                requires_regression: false,
                stat_items: Some(er_logic::mario::StatItems {
                    health: 200,
                    power: 201,
                }),
            }),
        );
        assert!(!runtime.stats_armed());
        runtime.applied = true;
        assert!(!runtime.stats_armed());
        runtime.acknowledged_stats = Some(runtime.session.expected_stats());
        assert!(runtime.stats_armed());
        runtime.receive_history(vec![(0, 200)]);
        assert!(!runtime.stats_armed());
        runtime.acknowledged_stats = Some(runtime.session.expected_stats());
        assert!(runtime.stats_armed());
        runtime.applied = false;
        assert!(!runtime.stats_armed());
        runtime.session.configure("vanilla:1".into(), None);
        runtime.applied = true;
        assert!(!runtime.stats_armed());
    }
    #[test]
    fn acknowledgment_logs_first_arm_changed_masks_and_recovery_only() {
        let mut runtime = Runtime::default();
        assert!(runtime.acknowledgment_due((1023, 0, None)));
        assert!(!runtime.acknowledgment_due((1023, 0, None)));
        assert!(runtime.acknowledgment_due((1023, 1, None)));
        assert!(!runtime.acknowledgment_due((1023, 1, None)));
        runtime.last_acknowledgment = None;
        assert!(runtime.acknowledgment_due((1023, 1, None)));
        assert!(!runtime.acknowledgment_due((1023, 1, None)));
        runtime.reset();
        assert!(runtime.acknowledgment_due((1023, 1, None)));
        let stats = Stats {
            max_wedges: 4,
            power_basis_points: 7500,
        };
        assert!(runtime.acknowledgment_due((1023, 1, Some(stats))));
        assert!(!runtime.acknowledgment_due((1023, 1, Some(stats))));
        assert!(runtime.acknowledgment_due((
            1023,
            1,
            Some(Stats {
                max_wedges: 5,
                ..stats
            })
        )));
    }
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
                requires_regression: false,
                stat_items: None,
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
