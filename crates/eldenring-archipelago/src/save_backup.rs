//! Rotating session backups of the ACTIVE Elden Ring save -- the I/O half of client#287
//! (the pure decisions -- which file is live, what a generation is named, what is pruned -- are
//! [`er_logic::save_backup`], host-tested).
//!
//! Runs ONCE per process from the worker preflight in `lib.rs`, before any `Core` exists, which
//! is what "copy before the first AP-owned mutation for the session" means structurally: every
//! AP write to game state is a `Core::update` effect, and the core is built after the preflight.
//!
//! ## The failure this refuses to repeat
//!
//! 2026-08-19 report: a save directory holding `ER0000.sl2`, `ER0000.sl2.bak`,
//! `ER0000.sl2.randobak`, `AP_me3.sl2` and `ER0000.mod` side by side, same sizes, and only the
//! ACTIVE modded namespace current. So the resolver does not guess a name: under me3 it reads the
//! profile's own `savefile = "AP_me3.sl2"` line (what `build.ps1 -Me3Deploy` writes); under any
//! other loader the profile is dead text and the newest save-shaped file wins. No candidates is
//! [`Selection::Unresolved`], and unresolved is LOUD -- a silent no-op would claim a protection
//! that does not exist (acceptance #4).
//!
//! ## Where the backups live
//!
//! `<mod directory>/save-backups/` -- client-owned, beside `log/`, and NOT a sibling of the save
//! in `%APPDATA%\EldenRing`, where an ambiguous extra file risks the game or Steam Cloud treating
//! it as live. Retention: newest [`KEEP_GENERATIONS`] per namespace (er_logic side).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Local;
use er_logic::save_backup::{
    KEEP_GENERATIONS, Selection, backup_name, is_generation_of, is_save_name, parse_me3_savefile,
    prune_generations, select_active_save,
};
use log::{error, info, warn};
use shared::utils::{Loader, loader, mod_directory};

/// One line per launch, for the log AND for the in-game warning decision. Latched: the backup
/// runs once per process, so the status is written once and read by `core.rs` on the first
/// in-world edge (no HUD exists before that -- the icon-override warning's precedent).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackupStatus {
    /// A generation was written. Carries the destination for the toast/log line.
    Written(PathBuf),
    /// me3's profile names a save that does not exist yet -- a first launch of a fresh profile.
    /// Not a failure and NOT player-visible: there is nothing to protect.
    FreshProfile(String),
    /// The active save could not be resolved or copied. The player is told in-game.
    Failed(String),
    /// [`run`] has not executed yet this process.
    Pending,
}

static STATUS: Mutex<BackupStatus> = Mutex::new(BackupStatus::Pending);

/// The verdict for consumers that run after [`run`] (the core's first in-world edge).
pub fn status() -> BackupStatus {
    STATUS.lock().unwrap().clone()
}

fn record(status: BackupStatus) {
    *STATUS.lock().unwrap() = status;
}

/// The directory Elden Ring writes saves into: `%APPDATA%\EldenRing`. `None` when the
/// environment does not say (a stripped launcher environment is not fiction -- the mod_stack
/// probe already treats a missing `SystemRoot` as a real case).
fn save_root() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .filter(|v| !v.is_empty())
        .map(|appdata| Path::new(&appdata).join("EldenRing"))
}

/// Every save-shaped file in `root` and ONE level of its subdirectories (the per-user-id folder
/// vanilla/me3 write into), as `(file_name, mtime_secs)` pairs the pure selector consumes.
/// Directory entries that cannot be stated are skipped, not fatal: a partial census with a log
/// line beats no census.
fn save_candidates(root: &Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                dirs.push(entry.path());
            }
        }
    }
    for dir in dirs {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                info!("save backup: {} not readable ({e})", dir.display());
                continue;
            }
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_save_name(&name) {
                continue;
            }
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            out.push((name, mtime));
        }
    }
    out
}

/// The me3 profile's `savefile` value, or `None` when the file is absent/unreadable/without the
/// key. Only consulted under the me3 loader; everywhere else it is dead text on disk.
fn profile_save(mod_dir: &Path) -> Option<String> {
    let path = mod_dir.join("ap.me3");
    let text = std::fs::read_to_string(&path).ok()?;
    parse_me3_savefile(&text)
}

/// The session backup. Once per process, from the worker preflight (see the module doc).
/// Never panics, never blocks startup on a retry: one attempt, fully logged, and a latched
/// [`BackupStatus`] the player sees if it failed.
pub fn run() {
    let mod_dir = match mod_directory() {
        Ok(dir) => dir.to_path_buf(),
        Err(e) => {
            // No mod directory means no client-owned place to put a backup AND no log beside the
            // DLL -- but start_logger fell back to the CWD, so this line does reach a file.
            error!(
                "save backup: mod directory unresolved ({e}); the active save is NOT backed up this session"
            );
            record(BackupStatus::Failed(format!(
                "mod directory unresolved: {e}"
            )));
            return;
        }
    };

    let Some(root) = save_root() else {
        error!(
            "save backup: %APPDATA% is not set; cannot locate the Elden Ring save directory, the active save is NOT backed up this session"
        );
        record(BackupStatus::Failed("%APPDATA% not set".to_string()));
        return;
    };
    let candidates = save_candidates(&root);
    let profile = if loader() == Loader::Me3 {
        profile_save(&mod_dir)
    } else {
        None
    };
    let selection = select_active_save(loader() == Loader::Me3, profile.as_deref(), &candidates);

    let save_name = match &selection {
        Selection::Unresolved => {
            error!(
                "save backup: no save-shaped file under {} and no me3 profile answer -- the \
                 active save is NOT backed up this session. If a save exists elsewhere, tell us \
                 where; do not assume a copy exists.",
                root.display()
            );
            record(BackupStatus::Failed("no save file found".to_string()));
            return;
        }
        Selection::FreshProfile(name) => {
            info!(
                "save backup: me3 profile save '{name}' does not exist yet (fresh profile, first \
                 launch) -- nothing to back up, and that is normal"
            );
            record(BackupStatus::FreshProfile(name.clone()));
            return;
        }
        Selection::ProfileNamed(name) => {
            info!("save backup: active save = '{name}' (me3 profile `savefile` line)");
            name.clone()
        }
        Selection::NewestModified(name) => {
            info!(
                "save backup: active save = '{name}' (newest save-shaped file under {}; non-me3 \
                 loader, no profile to consult)",
                root.display()
            );
            name.clone()
        }
    };

    // The name selects the NAMESPACE; the file sits in `root` or one per-user subdir. Find it.
    let source = std::iter::once(root.clone())
        .chain(
            std::fs::read_dir(&root)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path()),
        )
        .map(|dir| dir.join(&save_name))
        .find(|p| p.is_file());
    let Some(source) = source else {
        // The census saw it a moment ago and now it is gone -- a save-manager race, not a logic
        // error. Failed, not silent.
        error!(
            "save backup: '{save_name}' was enumerated but no readable file matches it now; NOT backed up"
        );
        record(BackupStatus::Failed(format!(
            "{save_name} vanished between scan and copy"
        )));
        return;
    };

    let size = source.metadata().map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        error!(
            "save backup: {} is EMPTY (0 bytes) -- refusing to back up a corrupt-looking save; \
             NOT backed up this session",
            source.display()
        );
        record(BackupStatus::Failed(format!("{save_name} is 0 bytes")));
        return;
    }

    let backup_dir = mod_dir.join("save-backups");
    if let Err(e) = std::fs::create_dir_all(&backup_dir) {
        error!(
            "save backup: cannot create {} ({e}); the active save is NOT backed up this session",
            backup_dir.display()
        );
        record(BackupStatus::Failed(format!(
            "backup dir creation failed: {e}"
        )));
        return;
    }

    let taken: HashSet<String> = std::fs::read_dir(&backup_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let dest = backup_dir.join(backup_name(&save_name, &timestamp, &taken));

    match std::fs::copy(&source, &dest) {
        Ok(bytes) => {
            info!(
                "save backup: {} -> {} ({} bytes; {} generation(s) kept, newest first)",
                source.display(),
                dest.display(),
                bytes,
                KEEP_GENERATIONS
            );
            record(BackupStatus::Written(dest));
        }
        Err(e) => {
            error!(
                "save backup: copy {} -> {} FAILED ({e}); the active save is NOT backed up this \
                 session. Check the save is not locked by another tool.",
                source.display(),
                dest.display()
            );
            record(BackupStatus::Failed(format!("copy failed: {e}")));
            return;
        }
    }

    // Retention, after the copy landed (never prune a namespace into having FEWER than one
    // verified new generation). Scoped per namespace: `AP_me3.sl2.*.bak` and `ER0000.sl2.*.bak`
    // are independent families.
    let prefix_match = |name: &str| is_generation_of(name, &save_name);
    let generations: Vec<(String, u64)> = std::fs::read_dir(&backup_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !prefix_match(&name) {
                return None;
            }
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            Some((name, mtime))
        })
        .collect();
    for old in prune_generations(&generations, KEEP_GENERATIONS) {
        match std::fs::remove_file(backup_dir.join(&old)) {
            Ok(()) => info!("save backup: pruned old generation {old}"),
            Err(e) => warn!("save backup: could not prune {old} ({e}); retention is best-effort"),
        }
    }
}

/// The one-shot in-game warning, pushed by `core.rs` on the first in-world edge (there is no HUD
/// before that). Returns the line to toast, once.
pub fn failure_toast() -> Option<&'static str> {
    match status() {
        BackupStatus::Failed(_) => {
            Some("AP: save backup FAILED this session -- no recovery copy exists. Read the log.")
        }
        _ => None,
    }
}
