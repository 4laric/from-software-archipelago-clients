//! Thin dynamic ABI adapter. Never loads a DLL or calls SM64 from the AP thread.
use er_logic::mario::{
    ABI_VERSION, Addon, AddonConfig, AddonState, Config, Fludd, FluddState, Session, Stats,
    StatsState,
};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetModuleHandleExA, GetModuleHandleW, GetProcAddress};
use windows::core::{s, w};

use er_logic::mario::BridgeState as State;

/// Input ownership follows the loaded mod, including standalone Mario before
/// AP connects. This only queries the loader; it does not load/retain a DLL or
/// require capability exports from the installed ER-Mario version.
pub fn is_loaded() -> bool {
    unsafe { GetModuleHandleW(w!("er_mario.dll")) }.is_ok()
}

type Version = unsafe extern "C" fn() -> u32;
type Set = unsafe extern "C" fn(u32, u32) -> u32;
type Get = unsafe extern "C" fn(*mut State) -> u32;
type GetStats = unsafe extern "C" fn(*mut StatsState) -> u32;
type GetAddon = unsafe extern "C" fn(*mut AddonState) -> u32;
struct AddonBridge {
    set: Set,
    get: GetAddon,
}
impl AddonBridge {
    fn state(&self) -> Result<AddonState, String> {
        let mut state = AddonState::default();
        if unsafe { (self.get)(&mut state) } != 1 || state.abi_version != ABI_VERSION {
            return Err("Mario progression paused: addon state read-back failed".into());
        }
        Ok(state)
    }
    fn set(&self, expected: AddonConfig) -> Result<(), String> {
        if unsafe { (self.set)(u32::from(expected.enabled), expected.unlocked) } != 1 {
            return Err("Mario progression paused: addon update was rejected".into());
        }
        Ok(())
    }
}
type SetFludd = unsafe extern "C" fn(u32, u32, u32) -> u32;
type GetFludd = unsafe extern "C" fn(*mut FluddState) -> u32;
struct FluddBridge {
    set: SetFludd,
    get: GetFludd,
}
impl FluddBridge {
    fn state(&self) -> Result<FluddState, String> {
        let mut state = FluddState::default();
        if unsafe { (self.get)(&mut state) } != 1 || state.abi_version != ABI_VERSION {
            return Err("Mario progression paused: FLUDD state read-back failed".into());
        }
        Ok(state)
    }
    fn set(&self, expected: Fludd) -> Result<(), String> {
        if unsafe {
            (self.set)(
                u32::from(expected.enabled),
                expected.nozzles,
                expected.tank_level,
            )
        } != 1
        {
            return Err("Mario progression paused: FLUDD update was rejected".into());
        }
        Ok(())
    }
}
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
    fludd: Option<FluddBridge>,
    addons: [Option<AddonBridge>; 2],
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
        let fludd_set = unsafe { GetProcAddress(module.0, s!("er_mario_ap_set_fludd")) };
        let fludd_get = unsafe { GetProcAddress(module.0, s!("er_mario_ap_get_fludd_state")) };
        let fludd = match (fludd_set, fludd_get) {
            (Some(set), Some(get)) => Some(FluddBridge {
                set: unsafe {
                    std::mem::transmute::<unsafe extern "system" fn() -> isize, SetFludd>(set)
                },
                get: unsafe {
                    std::mem::transmute::<unsafe extern "system" fn() -> isize, GetFludd>(get)
                },
            }),
            _ => None,
        };
        let addons = [
            (
                s!("er_mario_ap_set_cappy"),
                s!("er_mario_ap_get_cappy_state"),
            ),
            (
                s!("er_mario_ap_set_sonic"),
                s!("er_mario_ap_get_sonic_state"),
            ),
        ]
        .map(|(set_name, get_name)| {
            match (unsafe { GetProcAddress(module.0, set_name) }, unsafe {
                GetProcAddress(module.0, get_name)
            }) {
                (Some(set), Some(get)) => Some(AddonBridge {
                    set: unsafe {
                        std::mem::transmute::<unsafe extern "system" fn() -> isize, Set>(set)
                    },
                    get: unsafe {
                        std::mem::transmute::<unsafe extern "system" fn() -> isize, GetAddon>(get)
                    },
                }),
                _ => None,
            }
        });
        Ok(Self {
            _module: module,
            set,
            get,
            stats,
            fludd,
            addons,
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
    last_fludd_acknowledgment: Option<Fludd>,
    acknowledged_fludd: Option<Fludd>,
    acknowledged_addons: [Option<AddonConfig>; 2],
    last_addon_acknowledgment: [Option<AddonConfig>; 2],
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
            config.validate_fludd_exports(bridge.fludd.is_some())?;
            for addon in Addon::ALL {
                config.validate_addon_exports(addon, bridge.addons[addon.index()].is_some())?;
            }
        } else if let Ok(bridge) = Bridge::discover() {
            // Vanilla progression never depends on an optional Mario bridge. Queue the normal
            // baseline once at connection, so a previously active stat seed cannot leak its caps.
            if let Some(stats) = bridge.stats
                && let Err(error) = stats.set(Stats::default())
            {
                log::warn!("Could not reset optional Mario stats for vanilla connection: {error}");
            }
            if let Some(fludd) = bridge.fludd
                && let Err(error) = fludd.set(Fludd::default())
            {
                log::warn!(
                    "Could not disable optional Mario FLUDD for vanilla connection: {error}"
                );
            }
            for addon in Addon::ALL {
                if let Some(bridge) = &bridge.addons[addon.index()]
                    && let Err(error) = bridge.set(AddonConfig::default())
                {
                    log::warn!(
                        "Could not disable optional {} for vanilla connection: {error}",
                        addon.feature()
                    );
                }
            }
        }
        if self.session.identity.as_ref() != Some(&identity) || self.session.config != config {
            self.applied = false;
            self.history_len = None;
            self.last_acknowledgment = None;
            self.acknowledged_stats = None;
            self.last_fludd_acknowledgment = None;
            self.acknowledged_fludd = None;
            self.acknowledged_addons = [None; 2];
            self.last_addon_acknowledgment = [None; 2];
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
    pub fn fludd_armed(&self) -> bool {
        self.armed()
            && self
                .session
                .config
                .as_ref()
                .is_some_and(|c| c.fludd_items.is_some())
            && self.acknowledged_fludd == Some(self.session.expected_fludd())
    }
    pub fn addon_armed(&self, addon: Addon) -> bool {
        self.armed()
            && self
                .session
                .config
                .as_ref()
                .is_some_and(|c| c.addon_enabled(addon))
            && self.acknowledged_addons[addon.index()] == Some(self.session.expected_addon(addon))
    }
    pub fn refresh(&mut self) -> Result<(), String> {
        let result = self.refresh_bridge();
        if result.is_err() {
            self.last_acknowledgment = None;
            self.last_fludd_acknowledgment = None;
            self.last_addon_acknowledgment = [None; 2];
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
            if self.acknowledged_fludd.is_some()
                && self.last_fludd_acknowledgment != Some(self.session.expected_fludd())
            {
                let expected = self.session.expected_fludd();
                log::info!(
                    "Mario FLUDD worker acknowledged: identity={:?}, enabled={}, nozzles={:#x}, tank_level={}",
                    self.session.identity,
                    expected.enabled,
                    expected.nozzles,
                    expected.tank_level
                );
                self.last_fludd_acknowledgment = Some(expected);
            }
            for addon in Addon::ALL {
                let expected = self.acknowledged_addons[addon.index()];
                if expected.is_some() && self.last_addon_acknowledgment[addon.index()] != expected {
                    log::info!(
                        "Mario {} worker acknowledged: identity={:?}, configuration={:?}",
                        addon.feature(),
                        self.session.identity,
                        expected
                    );
                    self.last_addon_acknowledgment[addon.index()] = expected;
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
        config.validate_fludd_exports(bridge.fludd.is_some())?;
        for addon in Addon::ALL {
            config.validate_addon_exports(addon, bridge.addons[addon.index()].is_some())?;
        }
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
        let fludd = bridge.fludd.as_ref().map(FluddBridge::state).transpose()?;
        if let Some(fludd) = fludd
            && !fludd.matches(self.session.expected_fludd())
        {
            bridge
                .fludd
                .as_ref()
                .unwrap()
                .set(self.session.expected_fludd())?;
            pending = true;
        }
        let mut addon_acknowledged = true;
        let mut addon_snapshots = [None; 2];
        for addon in Addon::ALL {
            let expected = self.session.expected_addon(addon);
            if let Some(bridge) = &bridge.addons[addon.index()] {
                let state = bridge.state()?;
                if !state.matches(expected) {
                    bridge.set(expected)?;
                    pending = true;
                }
                addon_acknowledged &= state.acknowledged(expected);
                addon_snapshots[addon.index()] = Some(expected);
            } else {
                addon_acknowledged &= !config.addon_enabled(addon);
            }
        }
        if pending {
            return Err(
                "Mario progression paused: waiting for capability/stat/FLUDD/addon worker acknowledgment"
                    .into(),
            );
        }
        if !config.acknowledged(
            &state,
            stats.as_ref(),
            expected,
            self.session.expected_stats(),
        ) || !addon_acknowledged
            || !match fludd {
                Some(state) => state.acknowledged(self.session.expected_fludd()),
                None => config.fludd_items.is_none(),
            }
        {
            return Err(
                "Mario progression paused: capability/stat/FLUDD worker is not ready and enabled"
                    .into(),
            );
        }
        self.applied = true;
        self.acknowledged_stats = stats.map(|_| self.session.expected_stats());
        self.acknowledged_fludd = fludd.map(|_| self.session.expected_fludd());
        self.acknowledged_addons = addon_snapshots;
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
            if let Some(fludd) = bridge.fludd {
                let _ = fludd.set(Fludd::default());
            }
            for bridge in bridge.addons.into_iter().flatten() {
                let _ = bridge.set(AddonConfig::default());
            }
        }
        *self = Self::default();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn addon_probes_read_current_configuration_and_combined_live_ack() {
        let mut runtime = Runtime::default();
        runtime.session.configure(
            "seed:1".into(),
            Some(Config {
                managed: 1023,
                unlock_items: Default::default(),
                requires_regression: false,
                stat_items: None,
                fludd_items: None,
                addon_items: [(400, (Addon::Cappy, 1)), (410, (Addon::Sonic, 1))]
                    .into_iter()
                    .collect(),
            }),
        );
        for addon in Addon::ALL {
            assert!(!runtime.addon_armed(addon));
            runtime.acknowledged_addons[addon.index()] =
                Some(runtime.session.expected_addon(addon));
        }
        runtime.applied = true;
        for addon in Addon::ALL {
            assert!(runtime.addon_armed(addon));
        }
        runtime.receive_history(vec![(0, 400), (1, 410)]);
        for addon in Addon::ALL {
            assert!(!runtime.addon_armed(addon));
            runtime.acknowledged_addons[addon.index()] =
                Some(runtime.session.expected_addon(addon));
            assert!(runtime.addon_armed(addon));
        }
        runtime.applied = false;
        for addon in Addon::ALL {
            assert!(!runtime.addon_armed(addon));
        }
        runtime.session.configure("vanilla:1".into(), None);
        runtime.applied = true;
        for addon in Addon::ALL {
            assert!(!runtime.addon_armed(addon));
        }
    }
    #[test]
    fn fludd_probe_requires_current_exact_worker_ack_and_declared_feature() {
        let mut runtime = Runtime::default();
        runtime.session.configure(
            "seed:1".into(),
            Some(Config {
                managed: 1023,
                unlock_items: Default::default(),
                requires_regression: false,
                stat_items: None,
                fludd_items: Some([300, 301, 302, 303]),
                addon_items: Default::default(),
            }),
        );
        assert!(!runtime.fludd_armed());
        runtime.applied = true;
        assert!(!runtime.fludd_armed());
        runtime.acknowledged_fludd = Some(runtime.session.expected_fludd());
        assert!(runtime.fludd_armed());
        runtime.receive_history(vec![(0, 300)]);
        assert!(!runtime.fludd_armed());
        runtime.acknowledged_fludd = Some(runtime.session.expected_fludd());
        assert!(runtime.fludd_armed());
        runtime.applied = false;
        assert!(!runtime.fludd_armed());
        runtime.session.configure("vanilla:1".into(), None);
        runtime.applied = true;
        assert!(!runtime.fludd_armed());
    }
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
                fludd_items: None,
                addon_items: Default::default(),
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
                fludd_items: None,
                addon_items: Default::default(),
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
                fludd_items: None,
                addon_items: Default::default(),
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
