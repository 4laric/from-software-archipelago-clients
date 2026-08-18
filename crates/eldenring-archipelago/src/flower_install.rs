//! In-client launcher for the locally generated AP flower atlas (issue #269).
//!
//! The client never patches or distributes FromSoft assets. It finds the installer shipped beside
//! the DLL, chooses the mod root the active loader actually reads, and launches the installer in a
//! separate process. The game must be restarted before its already-loaded menu atlas changes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use shared::utils::Loader;

const DATA_MARKERS: &[&str] = &["regulation.bin", "event", "msg", "script", "map", "param"];

fn names(path: &Path) -> Vec<String> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_lowercase())
        .collect()
}

fn is_data_mod_root(path: &Path) -> bool {
    let names = names(path);
    names.iter().any(|name| name.ends_with(".randomizeopt"))
        || DATA_MARKERS
            .iter()
            .any(|marker| names.iter().any(|name| name == marker))
}

/// The destination whose top-level `menu/` is loaded in this configuration.
pub fn destination(loader: Loader, mod_dir: &Path) -> PathBuf {
    if loader == Loader::Me3 {
        return mod_dir.join("ap-package");
    }
    if loader == Loader::DllDirectory {
        return mod_dir
            .ancestors()
            .take(3)
            .find(|path| is_data_mod_root(path))
            .unwrap_or(mod_dir)
            .to_path_buf();
    }
    mod_dir.to_path_buf()
}

fn find_installer(mod_dir: &Path) -> Option<PathBuf> {
    mod_dir
        .ancestors()
        .take(4)
        .map(|root| root.join("install-ap-flower.ps1"))
        .find(|path| path.is_file())
}

pub fn is_installed(loader: Loader, mod_dir: &Path) -> bool {
    let root = destination(loader, mod_dir);
    (root.join(".er-ap-flower.json").is_file()
        || (root.join("menu/hi/01_common.tpf.dcx").is_file()
            && root.join("menu/low/01_common.tpf.dcx").is_file()))
}

/// Launches a visible PowerShell installer and immediately returns a player-facing status line.
///
/// Deliberately asynchronous: downloading/unpacking on imgui's present thread would freeze the
/// game. The terminal stays open so a failure is not swallowed after one frame.
pub fn launch(loader: Loader, mod_dir: &Path) -> Result<String, String> {
    let installer = find_installer(mod_dir).ok_or_else(|| {
        format!(
            "AP flower installer not found beside the client or its parent folders: {}",
            mod_dir.display()
        )
    })?;
    let target = destination(loader, mod_dir);
    Command::new("powershell.exe")
        .args(["-NoExit", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&installer)
        .arg("-Destination")
        .arg(&target)
        .spawn()
        .map_err(|error| format!("Could not open AP flower installer: {error}"))?;
    Ok(format!(
        "AP flower installer opened for {}. Follow its window, then restart Elden Ring.",
        target.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("er-flower-{name}-{nonce}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn me3_targets_its_package() {
        let root = temp("me3");
        assert_eq!(destination(Loader::Me3, &root), root.join("ap-package"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn matt_nested_dll_targets_nearest_data_root() {
        let root = temp("matt");
        let dll = root.join("nested/dll");
        fs::create_dir_all(&dll).unwrap();
        fs::write(root.join("123.randomizeopt"), "").unwrap();
        assert_eq!(destination(Loader::DllDirectory, &dll), root);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn generic_modengine_root_is_detected() {
        let root = temp("modengine");
        let dll = root.join("dll");
        fs::create_dir_all(&dll).unwrap();
        fs::write(root.join("regulation.bin"), []).unwrap();
        assert_eq!(destination(Loader::DllDirectory, &dll), root);
        fs::remove_dir_all(root).unwrap();
    }
}
