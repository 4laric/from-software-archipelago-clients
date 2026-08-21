//! Player-runnable Seamless Co-op compatibility probe (#488).
//!
//! Diagnostic only: module discovery and event-flag reads. The trace observes writes made through
//! our normal flag helper, but never authors a flag itself.

use std::collections::BTreeMap;
use std::sync::Mutex;

const WATCHED_FLAGS: &[(u32, &str)] = &[
    (300, "capital_world_burn"),
    (302, "capital_pre_burn"),
    (9_116, "capital_selector"),
    (118, "capital_burn_done"),
    (1_252_380_800, "radahn_arena_defeat"),
    (9_130, "radahn_global_defeat"),
    (9_412, "radahn_afterglow"),
    (9_413, "radahn_festival_over"),
];

#[derive(Debug)]
struct Trace {
    baseline: BTreeMap<u32, bool>,
    writes: Vec<(u32, bool)>,
    start_region: Option<i32>,
}

static TRACE: Mutex<Option<Trace>> = Mutex::new(None);

fn snapshot() -> BTreeMap<u32, bool> {
    WATCHED_FLAGS
        .iter()
        .map(|(flag, _)| (*flag, crate::flags::get_event_flag(*flag)))
        .collect()
}

fn flag_name(flag: u32) -> &'static str {
    WATCHED_FLAGS
        .iter()
        .find_map(|(candidate, name)| (*candidate == flag).then_some(*name))
        .unwrap_or("unknown")
}

fn loaded_module_named(name: &str) -> Result<Option<String>, String> {
    let modules = shared::utils::loaded_modules().map_err(|err| err.to_string())?;
    Ok(modules.into_iter().find_map(|path| {
        path.file_name()
            .and_then(|file| file.to_str())
            .filter(|file| file.eq_ignore_ascii_case(name))
            .map(|_| path.display().to_string())
    }))
}

pub fn report(detour_installed: bool) -> Vec<String> {
    let ersc = match loaded_module_named("ersc.dll") {
        Ok(Some(path)) => format!("LOADED ({path})"),
        Ok(None) => "NOT FOUND".to_string(),
        Err(err) => format!("UNKNOWN ({err})"),
    };
    let result = if ersc.starts_with("LOADED") && detour_installed {
        "PASS_TO_CONTINUE"
    } else if !ersc.starts_with("LOADED") {
        "BLOCKED: ersc.dll is not loaded in this process"
    } else {
        "BLOCKED: AddItem hook is not ready; inspect the earlier install error"
    };
    let flags = snapshot()
        .into_iter()
        .map(|(flag, value)| format!("{}({flag})={value}", flag_name(flag)))
        .collect::<Vec<_>>()
        .join(" ");

    vec![
        "=== SEAMLESS PROBE START ===".to_string(),
        format!("client: {}", crate::game::CLIENT_BUILD),
        format!("loader: {}", shared::utils::loader().describe()),
        format!("ersc.dll: {ersc}"),
        format!(
            "AddItem hook: {}",
            if detour_installed {
                "READY"
            } else {
                "NOT READY"
            }
        ),
        "supported shape: HOST ONLY; ONE AP SLOT; guests do not run the AP client".to_string(),
        format!("play_region: {:?}", crate::flags::play_region_id()),
        format!("flags: {flags}"),
        format!("result: {result}"),
        "attach the current archipelago-*.log to er-archipelago#488".to_string(),
        "=== SEAMLESS PROBE END ===".to_string(),
    ]
}

pub fn start() -> Vec<String> {
    let mut guard = TRACE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(Trace {
        baseline: snapshot(),
        writes: Vec::new(),
        start_region: crate::flags::play_region_id(),
    });
    vec![
        "=== SEAMLESS TRACE START ===".to_string(),
        "watching capital and Radahn/festival flags; diagnostic only, no probe writes".to_string(),
        "play the requested test, then run !seamlessprobe stop".to_string(),
    ]
}

pub fn stop() -> Vec<String> {
    let mut guard = TRACE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(trace) = guard.take() else {
        return vec!["seamless trace is not running; use !seamlessprobe start".to_string()];
    };
    let current = snapshot();
    let mut out = vec!["=== SEAMLESS TRACE STOP ===".to_string()];
    out.push(format!(
        "play_region: {:?} -> {:?}",
        trace.start_region,
        crate::flags::play_region_id()
    ));
    let mut changes = 0usize;
    for (flag, before) in trace.baseline {
        let after = current.get(&flag).copied().unwrap_or(false);
        if before == after {
            continue;
        }
        changes += 1;
        let ap_wrote = trace
            .writes
            .iter()
            .any(|(written, value)| *written == flag && *value == after);
        out.push(format!(
            "flag {}({flag}): {before} -> {after} [{}]",
            flag_name(flag),
            if ap_wrote {
                "AP-WROTE"
            } else {
                "OBSERVED-EXTERNAL"
            }
        ));
    }
    if changes == 0 {
        out.push("flag changes: none".to_string());
    }
    if trace.writes.is_empty() {
        out.push("AP flag-write attempts while tracing: none".to_string());
    } else {
        out.push(format!(
            "AP flag-write attempts while tracing: {:?}",
            trace.writes
        ));
    }
    out.push("attach the current archipelago-*.log to er-archipelago#488".to_string());
    out.push("=== SEAMLESS TRACE END ===".to_string());
    out
}

/// Called only after the game's flag holder accepted an ordinary AP client write.
pub fn record_ap_write(flag: u32, value: bool) {
    let mut guard = TRACE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(trace) = guard.as_mut() {
        trace.writes.push((flag, value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watched_flags_are_unique_and_named() {
        let flags: std::collections::BTreeSet<_> =
            WATCHED_FLAGS.iter().map(|(flag, _)| *flag).collect();
        assert_eq!(flags.len(), WATCHED_FLAGS.len());
        assert!(
            WATCHED_FLAGS
                .iter()
                .all(|(flag, _)| flag_name(*flag) != "unknown")
        );
    }
}
