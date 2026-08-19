//! Sidecar-file path selection shared by the Windows glue.
//!
//! A release puts data files beside the AP client DLL. Under me3, the loader root can instead be
//! the global me3 installation, so it is only a compatibility fallback and must never outrank the
//! DLL directory.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarSource {
    ClientDll,
    LoaderFallback,
    Missing,
}

pub fn select_sidecar_path(
    name: &str,
    dll_dir: Option<&Path>,
    loader_dir: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> (PathBuf, SidecarSource) {
    let primary = dll_dir.map(|d| d.join(name));
    if let Some(path) = primary.as_ref() {
        if exists(path) {
            return (path.clone(), SidecarSource::ClientDll);
        }
    }

    let fallback = loader_dir.map(|d| d.join(name));
    if let Some(path) = fallback.as_ref() {
        if primary.as_ref() != Some(path) && exists(path) {
            return (path.clone(), SidecarSource::LoaderFallback);
        }
    }

    (
        primary.or(fallback).unwrap_or_else(|| PathBuf::from(name)),
        SidecarSource::Missing,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_dll_directory_outranks_the_me3_host_root() {
        let dll_dir = Path::new(r"C:\release\me3");
        let host_dir = Path::new(r"C:\Program Files\me3");
        let expected = dll_dir.join("check_lots_table.json");
        let host_copy = host_dir.join("check_lots_table.json");

        let (path, source) = select_sidecar_path(
            "check_lots_table.json",
            Some(dll_dir),
            Some(host_dir),
            |p| p == expected || p == host_copy,
        );

        assert_eq!(path, expected);
        assert_eq!(source, SidecarSource::ClientDll);
    }

    #[test]
    fn old_loader_root_remains_a_compatibility_fallback() {
        let dll_dir = Path::new(r"C:\release\me3");
        let host_dir = Path::new(r"C:\Program Files\me3");
        let host_copy = host_dir.join("shoplineup_flags.json");

        let (path, source) = select_sidecar_path(
            "shoplineup_flags.json",
            Some(dll_dir),
            Some(host_dir),
            |p| p == host_copy,
        );

        assert_eq!(path, host_copy);
        assert_eq!(source, SidecarSource::LoaderFallback);
    }

    #[test]
    fn missing_file_reports_the_primary_path_the_install_should_fill() {
        let dll_dir = Path::new(r"C:\release\me3");
        let host_dir = Path::new(r"C:\Program Files\me3");

        let (path, source) = select_sidecar_path(
            "check_lots_table.json",
            Some(dll_dir),
            Some(host_dir),
            |_| false,
        );

        assert_eq!(path, dll_dir.join("check_lots_table.json"));
        assert_eq!(source, SidecarSource::Missing);
    }
}
