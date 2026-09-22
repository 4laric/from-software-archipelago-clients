//! Fail-closed handoff from an externally managed BBLauncher activation.
//!
//! The launcher records one exact emulator process and the AP-owned files that
//! process booted.  The native client validates the handoff before patching
//! memory and revalidates the filesystem before every later guest mutation.
//! Once a live guard fails it stays failed for the lifetime of the client: a
//! fresh game boot and a newly verified handoff are required.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const EXTERNAL_ACTIVATION_FORMAT: &str = "bb-external-activation-v1";
const INVALIDATION_FORMAT: &str = "bb-external-invalidation-v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExternalExecutable {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExternalActivationFile {
    /// Effective path below `overlay_root`, always rooted at `dvdroot_ps4`.
    pub path: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExternalActivation {
    pub format: String,
    pub pid: u32,
    /// Windows process creation time as the unsigned 64-bit FILETIME value.
    pub process_creation_time: u64,
    pub executable: ExternalExecutable,
    pub game_path: PathBuf,
    pub overlay_root: PathBuf,
    pub active_root: PathBuf,
    pub package_name: String,
    pub files: Vec<ExternalActivationFile>,
    /// Opaque receipt produced by the activation verifier. The native client
    /// validates its shape; it independently proves every security property.
    pub activation_fingerprint: String,
    /// Durable fail-closed record for this exact PID + creation FILETIME.
    pub invalidation_marker: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct InvalidationMarker {
    format: String,
    pid: u32,
    process_creation_time: u64,
    activation_fingerprint: String,
    reason: String,
}

impl ExternalActivation {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.format == EXTERNAL_ACTIVATION_FORMAT,
            "external_activation format mismatch: expected {EXTERNAL_ACTIVATION_FORMAT:?}, found {:?}",
            self.format
        );
        ensure!(self.pid != 0, "external_activation pid must be non-zero");
        ensure!(
            self.process_creation_time != 0,
            "external_activation process_creation_time must be non-zero"
        );
        require_sha256("external_activation executable", &self.executable.sha256)?;
        require_sha256(
            "external_activation activation_fingerprint",
            &self.activation_fingerprint,
        )?;
        for (label, path) in [
            ("executable.path", &self.executable.path),
            ("game_path", &self.game_path),
            ("overlay_root", &self.overlay_root),
            ("active_root", &self.active_root),
            ("invalidation_marker", &self.invalidation_marker),
        ] {
            ensure!(
                path.is_absolute(),
                "external_activation {label} must be absolute"
            );
        }
        ensure!(
            !self.package_name.is_empty()
                && Path::new(&self.package_name).components().count() == 1
                && matches!(
                    Path::new(&self.package_name).components().next(),
                    Some(Component::Normal(_))
                ),
            "external_activation package_name must be one ordinary path component"
        );
        ensure!(
            self.package_name
                .to_ascii_lowercase()
                .starts_with("archipelago-"),
            "external_activation package_name must begin with Archipelago-"
        );
        ensure!(
            !self.files.is_empty(),
            "external_activation files must not be empty"
        );
        let mut seen = BTreeSet::new();
        for file in &self.files {
            require_sha256("external_activation file", &file.sha256)?;
            let path = safe_relative(&file.path)?;
            ensure!(
                path.components()
                    .next()
                    .is_some_and(|part| part.as_os_str() == "dvdroot_ps4"),
                "external_activation file path must begin with dvdroot_ps4/: {:?}",
                file.path
            );
            let folded = file.path.replace('\\', "/").to_ascii_lowercase();
            ensure!(
                seen.insert(folded),
                "duplicate external_activation file path: {:?}",
                file.path
            );
        }
        Ok(())
    }

    pub fn package_root(&self) -> PathBuf {
        self.active_root.join(&self.package_name)
    }
}

/// Process-lifetime guard for the external activation. A failure is sticky so
/// restoring files under a still-running game can never silently re-arm writes.
#[derive(Debug)]
pub struct ExternalMutationGuard {
    activation: ExternalActivation,
    active_set: BTreeSet<String>,
    latched: Option<String>,
}

impl ExternalMutationGuard {
    /// Arm only after the PID and creation FILETIME have been verified. This
    /// may retire an invalidation marker belonging to an older process boot.
    pub(crate) fn arm(activation: ExternalActivation) -> Result<Self> {
        activation.validate()?;
        prepare_invalidation_marker(&activation)?;
        let active_set = match active_package_set(&activation.active_root) {
            Ok(active_set) => active_set,
            Err(error) => return Err(persist_arm_failure(&activation, error)),
        };
        let mut guard = Self {
            activation,
            active_set,
            latched: None,
        };
        guard.verify_before_mutation()?;
        Ok(guard)
    }

    pub fn activation(&self) -> &ExternalActivation {
        &self.activation
    }

    pub fn verify_before_mutation(&mut self) -> Result<()> {
        if let Some(reason) = &self.latched {
            bail!(
                "external activation mutation guard is latched; restart shadPS4 and the AP client: {reason}"
            );
        }
        if let Err(error) = self.verify_inner() {
            let reason = format!("{error:#}");
            self.latched = Some(reason.clone());
            let persistence = persist_invalidation_marker(&self.activation, &reason)
                .map_err(|error| {
                    format!("; durable invalidation marker could not be written: {error:#}")
                })
                .err()
                .unwrap_or_default();
            bail!(
                "external activation changed after boot; all guest mutation is permanently paused for this client process. Restart shadPS4 and the AP client after fixing the activation: {reason}{persistence}"
            );
        }
        Ok(())
    }

    fn verify_inner(&self) -> Result<()> {
        ensure_plain_directory(
            &self.activation.active_root,
            "external activation active_root",
        )?;
        ensure_plain_directory(
            &self.activation.package_root(),
            "external activation package",
        )?;
        ensure_plain_directory(
            &self.activation.overlay_root,
            "external activation overlay_root",
        )?;
        ensure_plain_directory(&self.activation.game_path, "external activation game_path")?;
        verify_root_contract(&self.activation)?;

        let current_set = active_package_set(&self.activation.active_root)?;
        ensure!(
            current_set == self.active_set,
            "BBLauncher active mod set changed: expected {:?}, found {:?}",
            self.active_set,
            current_set
        );
        let archipelago_packages = current_set
            .iter()
            .filter(|name| name.to_ascii_lowercase().starts_with("archipelago-"))
            .collect::<Vec<_>>();
        ensure!(
            archipelago_packages.len() == 1,
            "exactly one active Archipelago package is required: {archipelago_packages:?}"
        );
        ensure!(
            archipelago_packages[0].eq_ignore_ascii_case(&self.activation.package_name),
            "active Archipelago package {:?} does not match selected package {:?}",
            archipelago_packages[0],
            self.activation.package_name
        );

        let package_root = self.activation.package_root();
        let mut expected = BTreeMap::new();
        for file in &self.activation.files {
            let effective_relative = safe_relative(&file.path)?;
            let package_relative = effective_relative
                .strip_prefix("dvdroot_ps4")
                .expect("validated prefix");
            ensure!(
                package_relative.components().next().is_some(),
                "external_activation file path names dvdroot_ps4 itself"
            );
            expected.insert(package_relative.to_path_buf(), file);
        }

        let package_files = regular_file_set(&package_root, "active Archipelago package")?;
        let expected_files = expected
            .keys()
            .map(|path| folded_relative(path))
            .collect::<BTreeSet<_>>();
        let actual_files = package_files.keys().cloned().collect::<BTreeSet<_>>();
        ensure!(
            actual_files == expected_files,
            "active Archipelago package file set changed: missing={:?}, unexpected={:?}",
            expected_files.difference(&actual_files).collect::<Vec<_>>(),
            actual_files.difference(&expected_files).collect::<Vec<_>>()
        );

        for package in &current_set {
            if package.eq_ignore_ascii_case(&self.activation.package_name) {
                continue;
            }
            let other = self.activation.active_root.join(package);
            ensure_plain_directory(&other, "active BBLauncher package")?;
            for relative in expected.keys() {
                ensure!(
                    !case_insensitive_path_exists(&other, relative)?,
                    "active BBLauncher package {package:?} also supplies AP-owned path {}",
                    relative.display()
                );
            }
        }

        for (package_relative, file) in expected {
            let actual_relative = package_files
                .get(&folded_relative(&package_relative))
                .expect("package file set was verified");
            let target = package_root.join(actual_relative);
            ensure_plain_parent_chain(&package_root, &target)?;
            let target_meta = fs::symlink_metadata(&target)
                .with_context(|| format!("reading AP-owned package file {}", target.display()))?;
            ensure!(
                target_meta.is_file(),
                "AP-owned package path is not a regular file: {}",
                target.display()
            );
            ensure!(
                !is_reparse(&target_meta),
                "AP-owned package file is a reparse point: {}",
                target.display()
            );

            let effective = self
                .activation
                .overlay_root
                .join(safe_relative(&file.path)?);
            reject_case_aliases(&self.activation.overlay_root, &safe_relative(&file.path)?)?;
            ensure_plain_parent_chain(&self.activation.overlay_root, &effective)?;
            let effective_meta = fs::symlink_metadata(&effective).with_context(|| {
                format!("reading effective AP overlay link {}", effective.display())
            })?;
            let actual = sha256_file(&target)?;
            ensure!(
                actual == file.sha256,
                "AP-owned file hash changed at {}: expected {}, found {}",
                target.display(),
                file.sha256,
                actual
            );
            if effective_meta.file_type().is_symlink() {
                let resolved_effective = fs::canonicalize(&effective).with_context(|| {
                    format!(
                        "resolving effective AP overlay link {}",
                        effective.display()
                    )
                })?;
                let resolved_target = fs::canonicalize(&target).with_context(|| {
                    format!("resolving AP-owned package file {}", target.display())
                })?;
                ensure!(
                    same_path(&resolved_effective, &resolved_target),
                    "effective AP overlay link {} resolves to {}, expected {}",
                    effective.display(),
                    resolved_effective.display(),
                    resolved_target.display()
                );
            } else {
                ensure!(
                    effective_meta.is_file() && !is_reparse(&effective_meta),
                    "effective AP overlay path is not a plain file: {}",
                    effective.display()
                );
                let effective_hash = sha256_file(&effective)?;
                ensure!(
                    effective_hash == file.sha256,
                    "effective AP overlay file hash changed at {}: expected {}, found {}",
                    effective.display(),
                    file.sha256,
                    effective_hash
                );
            }
        }
        Ok(())
    }
}

fn safe_relative(raw: &str) -> Result<PathBuf> {
    ensure!(!raw.is_empty(), "external_activation file path is empty");
    let normalized = raw.replace('\\', "/");
    let path = PathBuf::from(normalized);
    ensure!(
        !path.is_absolute(),
        "external_activation file path must be relative: {raw:?}"
    );
    ensure!(
        path.components()
            .all(|part| matches!(part, Component::Normal(_))),
        "external_activation file path contains an unsafe component: {raw:?}"
    );
    Ok(path)
}

fn active_package_set(root: &Path) -> Result<BTreeSet<String>> {
    ensure_plain_directory(root, "external activation active_root")?;
    let mut names = BTreeSet::new();
    let mut folded = BTreeSet::new();
    for entry in
        fs::read_dir(root).with_context(|| format!("listing active mods at {}", root.display()))?
    {
        let entry = entry.with_context(|| format!("listing active mods at {}", root.display()))?;
        let metadata = fs::symlink_metadata(entry.path())?;
        ensure!(
            !is_reparse(&metadata),
            "active mod entry is a reparse point: {}",
            entry.path().display()
        );
        ensure!(
            metadata.is_dir(),
            "active mod entry is not a directory: {}",
            entry.path().display()
        );
        let name = entry.file_name().to_string_lossy().to_string();
        ensure!(
            folded.insert(name.to_ascii_lowercase()),
            "active mod directories differ only by case: {name:?}"
        );
        names.insert(name);
    }
    Ok(names)
}

fn persist_arm_failure(activation: &ExternalActivation, error: anyhow::Error) -> anyhow::Error {
    let reason = format!("{error:#}");
    let persistence = persist_invalidation_marker(activation, &reason)
        .map_err(|marker_error| {
            format!("; durable invalidation marker could not be written: {marker_error:#}")
        })
        .err()
        .unwrap_or_default();
    anyhow::anyhow!(
        "external activation was invalid at native arm; restart shadPS4 after fixing the activation: {reason}{persistence}"
    )
}

fn verify_root_contract(activation: &ExternalActivation) -> Result<()> {
    ensure!(
        activation
            .game_path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("CUSA03173")),
        "external activation game_path must name CUSA03173: {}",
        activation.game_path.display()
    );
    let expected_overlay = activation.game_path.with_file_name("CUSA03173-mods");
    ensure!(
        same_path(&activation.overlay_root, &expected_overlay),
        "external activation overlay_root must be the CUSA03173-mods sibling of game_path: expected {}, found {}",
        expected_overlay.display(),
        activation.overlay_root.display()
    );
    ensure!(
        activation.active_root.file_name().is_some_and(|name| name
            .to_string_lossy()
            .eq_ignore_ascii_case("Mods-Active (DO NOT DELETE)")),
        "external activation active_root must name Mods-Active (DO NOT DELETE): {}",
        activation.active_root.display()
    );
    Ok(())
}

fn folded_relative(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn regular_file_set(root: &Path, label: &str) -> Result<BTreeMap<String, PathBuf>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![(root.to_path_buf(), PathBuf::new())];
    while let Some((directory, prefix)) = pending.pop() {
        let mut local_names = BTreeSet::new();
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("listing {label} {}", directory.display()))?
        {
            let entry =
                entry.with_context(|| format!("listing {label} {}", directory.display()))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("reading {label} entry {}", path.display()))?;
            ensure!(
                !is_reparse(&metadata),
                "{label} contains a reparse point: {}",
                path.display()
            );
            let name = entry.file_name();
            let folded_name = name.to_string_lossy().to_ascii_lowercase();
            ensure!(
                local_names.insert(folded_name),
                "{label} contains entries that differ only by case: {}",
                directory.display()
            );
            let relative = prefix.join(&name);
            if metadata.is_dir() {
                pending.push((path, relative));
            } else {
                ensure!(
                    metadata.is_file(),
                    "{label} contains a non-file entry: {}",
                    path.display()
                );
                let folded = folded_relative(&relative);
                ensure!(
                    found.insert(folded, relative).is_none(),
                    "{label} contains paths that differ only by case"
                );
            }
        }
    }
    Ok(found)
}

fn prepare_invalidation_marker(activation: &ExternalActivation) -> Result<()> {
    let marker = &activation.invalidation_marker;
    let parent = marker
        .parent()
        .context("external_activation invalidation_marker has no parent")?;
    ensure_plain_directory(parent, "external activation marker directory")?;
    match fs::symlink_metadata(marker) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file() && !is_reparse(&metadata),
                "external activation invalidation marker is not a plain file: {}",
                marker.display()
            );
            let bytes = fs::read(marker).with_context(|| {
                format!(
                    "reading external activation invalidation marker {}",
                    marker.display()
                )
            })?;
            let prior: InvalidationMarker = json::from_slice(&bytes).with_context(|| {
                format!(
                    "parsing external activation invalidation marker {}",
                    marker.display()
                )
            })?;
            ensure!(
                prior.format == INVALIDATION_FORMAT,
                "external activation invalidation marker has unknown format {:?}",
                prior.format
            );
            ensure!(
                prior.pid != activation.pid
                    || prior.process_creation_time != activation.process_creation_time,
                "this shadPS4 process identity was invalidated after activation drift; restart shadPS4 before reconnecting the AP client (pid {}, creation FILETIME {})",
                activation.pid,
                activation.process_creation_time
            );
            fs::remove_file(marker).with_context(|| {
                format!(
                    "removing invalidation marker from the previous shadPS4 boot {}",
                    marker.display()
                )
            })?;
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "inspecting external activation invalidation marker {}",
                    marker.display()
                )
            });
        }
    }
    // A failed marker write after drift must not become a silent restart path.
    // Prove the exact directory is writable before the first guest mutation.
    let probe = marker_temp_path(marker, "probe");
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&probe)
            .with_context(|| format!("creating invalidation marker probe {}", probe.display()))?;
        file.write_all(b"probe")?;
        file.sync_all()?;
        Ok(())
    })();
    let cleanup = fs::remove_file(&probe);
    result?;
    cleanup.with_context(|| format!("removing invalidation marker probe {}", probe.display()))?;
    Ok(())
}

fn persist_invalidation_marker(activation: &ExternalActivation, reason: &str) -> Result<()> {
    let value = InvalidationMarker {
        format: INVALIDATION_FORMAT.to_owned(),
        pid: activation.pid,
        process_creation_time: activation.process_creation_time,
        activation_fingerprint: activation.activation_fingerprint.clone(),
        reason: reason.to_owned(),
    };
    let bytes = json::to_vec_pretty(&value).context("serializing external invalidation marker")?;
    let marker = &activation.invalidation_marker;
    let temporary = marker_temp_path(marker, "new");
    let write_result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .with_context(|| {
                format!(
                    "creating external invalidation marker temporary {}",
                    temporary.display()
                )
            })?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, marker).with_context(|| {
            format!(
                "publishing external invalidation marker {}",
                marker.display()
            )
        })?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn marker_temp_path(marker: &Path, purpose: &str) -> PathBuf {
    let name = marker.file_name().map_or_else(
        || "external-invalidation".into(),
        |name| name.to_os_string(),
    );
    let mut temporary = name;
    temporary.push(format!(".{purpose}.{}.tmp", std::process::id()));
    marker.with_file_name(temporary)
}

fn case_insensitive_path_exists(root: &Path, relative: &Path) -> Result<bool> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(expected) = component else {
            bail!(
                "unsafe case-insensitive path component in {}",
                relative.display()
            );
        };
        let mut matches = Vec::new();
        for entry in fs::read_dir(&current)
            .with_context(|| format!("listing {} for path collision", current.display()))?
        {
            let entry = entry
                .with_context(|| format!("listing {} for path collision", current.display()))?;
            if entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(&expected.to_string_lossy())
            {
                matches.push(entry);
            }
        }
        let mut matches = matches.into_iter();
        let Some(found) = matches.next() else {
            return Ok(false);
        };
        ensure!(
            matches.next().is_none(),
            "directory {} contains multiple case-insensitive aliases for {:?}",
            current.display(),
            expected
        );
        current = found.path();
    }
    Ok(true)
}

fn reject_case_aliases(root: &Path, relative: &Path) -> Result<()> {
    let _ = case_insensitive_path_exists(root, relative)?;
    Ok(())
}

fn ensure_plain_parent_chain(root: &Path, target: &Path) -> Result<()> {
    let parent = target
        .parent()
        .context("AP-owned package file has no parent")?;
    let relative = parent
        .strip_prefix(root)
        .context("AP-owned package file escaped package root")?;
    let mut current = root.to_path_buf();
    ensure_plain_directory(&current, "external activation package")?;
    for part in relative.components() {
        current.push(part);
        ensure_plain_directory(&current, "AP-owned package directory")?;
    }
    Ok(())
}

fn ensure_plain_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("reading {label} {}", path.display()))?;
    ensure!(
        metadata.is_dir(),
        "{label} is not a directory: {}",
        path.display()
    );
    ensure!(
        !is_reparse(&metadata),
        "{label} is a reparse point: {}",
        path.display()
    );
    Ok(())
}

#[cfg(windows)]
fn is_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("opening {} for SHA-256", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("hashing {}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn require_sha256(label: &str, value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "{label} SHA-256 must contain exactly 64 lowercase hexadecimal characters"
    );
    Ok(())
}

fn same_path(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture() -> (PathBuf, ExternalActivation) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("bb-external-activation-{unique}"));
        let active = root.join("BBLauncher/Mods-Active (DO NOT DELETE)");
        let overlay = root.join("CUSA03173-mods");
        let package = active.join("Archipelago-test");
        fs::create_dir_all(package.join("param/gameparam")).unwrap();
        fs::create_dir_all(overlay.join("dvdroot_ps4/param/gameparam")).unwrap();
        fs::create_dir_all(root.join("CUSA03173")).unwrap();
        let target = package.join("param/gameparam/gameparam.parambnd.dcx");
        fs::write(&target, b"owned").unwrap();
        fs::copy(
            &target,
            overlay.join("dvdroot_ps4/param/gameparam/gameparam.parambnd.dcx"),
        )
        .unwrap();
        let activation = ExternalActivation {
            format: EXTERNAL_ACTIVATION_FORMAT.to_owned(),
            pid: 42,
            process_creation_time: 123,
            executable: ExternalExecutable {
                path: root.join("shadPS4.exe"),
                sha256: "1".repeat(64),
            },
            game_path: root.join("CUSA03173"),
            overlay_root: overlay,
            active_root: active,
            package_name: "Archipelago-test".to_owned(),
            files: vec![ExternalActivationFile {
                path: "dvdroot_ps4/param/gameparam/gameparam.parambnd.dcx".to_owned(),
                sha256: sha256_file(&target).unwrap(),
            }],
            activation_fingerprint: "2".repeat(64),
            invalidation_marker: root.join("session/external-invalidation.json"),
        };
        fs::create_dir_all(activation.invalidation_marker.parent().unwrap()).unwrap();
        (root, activation)
    }

    #[test]
    fn a_second_active_package_cannot_supply_an_ap_owned_path() {
        let (root, activation) = fixture();
        let collision = activation
            .active_root
            .join("OtherMod/param/gameparam/gameparam.parambnd.dcx");
        fs::create_dir_all(collision.parent().unwrap()).unwrap();
        fs::write(&collision, b"foreign").unwrap();
        let error = ExternalMutationGuard::arm(activation).unwrap_err();
        assert!(format!("{error:#}").contains("also supplies AP-owned path"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_set_failure_stays_latched_after_restoration() {
        let (root, activation) = fixture();
        let mut guard = ExternalMutationGuard::arm(activation.clone()).unwrap();
        let added = activation.active_root.join("AddedAfterBoot");
        fs::create_dir(&added).unwrap();
        let first = guard.verify_before_mutation().unwrap_err();
        assert!(format!("{first:#}").contains("permanently paused"));
        fs::remove_dir(&added).unwrap();
        let second = guard.verify_before_mutation().unwrap_err();
        assert!(format!("{second:#}").contains("is latched"));
        let restart = ExternalMutationGuard::arm(activation).unwrap_err();
        assert!(format!("{restart:#}").contains("was invalidated"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn regular_effective_copy_is_verified_and_drift_durably_blocks_restart() {
        let (root, activation) = fixture();
        let effective = activation
            .overlay_root
            .join("dvdroot_ps4/param/gameparam/gameparam.parambnd.dcx");
        let mut guard = ExternalMutationGuard::arm(activation.clone()).unwrap();
        guard.verify_before_mutation().unwrap();
        fs::write(&effective, b"changed after boot").unwrap();
        let drift = guard.verify_before_mutation().unwrap_err();
        assert!(format!("{drift:#}").contains("effective AP overlay file hash changed"));
        fs::write(&effective, b"owned").unwrap();
        let restart = ExternalMutationGuard::arm(activation).unwrap_err();
        assert!(format!("{restart:#}").contains("was invalidated"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_second_archipelago_package_is_rejected_without_a_file_collision() {
        let (root, activation) = fixture();
        fs::create_dir(activation.active_root.join("Archipelago-other")).unwrap();
        let error = ExternalMutationGuard::arm(activation).unwrap_err();
        assert!(format!("{error:#}").contains("exactly one active Archipelago package"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_unexpected_file_in_the_selected_package_invalidates_the_boot() {
        let (root, activation) = fixture();
        fs::write(
            activation.package_root().join("unexpected.bin"),
            b"not in the receipt",
        )
        .unwrap();
        let error = ExternalMutationGuard::arm(activation.clone()).unwrap_err();
        assert!(format!("{error:#}").contains("package file set changed"));
        let restart = ExternalMutationGuard::arm(activation).unwrap_err();
        assert!(format!("{restart:#}").contains("was invalidated"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_non_directory_active_root_entry_invalidates_the_boot() {
        let (root, activation) = fixture();
        fs::write(activation.active_root.join("unexpected.txt"), b"invalid").unwrap();
        let error = ExternalMutationGuard::arm(activation.clone()).unwrap_err();
        assert!(format!("{error:#}").contains("active mod entry is not a directory"));
        let restart = ExternalMutationGuard::arm(activation).unwrap_err();
        assert!(format!("{restart:#}").contains("was invalidated"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn root_paths_must_match_the_bblauncher_game_contract() {
        let (root, mut activation) = fixture();
        activation.overlay_root = root.join("somewhere-else");
        fs::create_dir(&activation.overlay_root).unwrap();
        let error = ExternalMutationGuard::arm(activation).unwrap_err();
        assert!(format!("{error:#}").contains("CUSA03173-mods sibling"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalidation_is_scoped_to_pid_and_creation_time() {
        let (root, activation) = fixture();
        let effective = activation
            .overlay_root
            .join("dvdroot_ps4/param/gameparam/gameparam.parambnd.dcx");
        let mut guard = ExternalMutationGuard::arm(activation.clone()).unwrap();
        fs::write(&effective, b"drift").unwrap();
        guard.verify_before_mutation().unwrap_err();
        fs::write(&effective, b"owned").unwrap();
        let mut next_boot = activation;
        next_boot.process_creation_time += 1;
        ExternalMutationGuard::arm(next_boot).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn marker_publication_failure_is_latched_and_restart_fails_closed() {
        let (root, activation) = fixture();
        let effective = activation
            .overlay_root
            .join("dvdroot_ps4/param/gameparam/gameparam.parambnd.dcx");
        let mut guard = ExternalMutationGuard::arm(activation.clone()).unwrap();
        fs::create_dir(&activation.invalidation_marker).unwrap();
        fs::write(&effective, b"drift").unwrap();
        let failure = guard.verify_before_mutation().unwrap_err();
        assert!(format!("{failure:#}").contains("marker could not be written"));
        let latched = guard.verify_before_mutation().unwrap_err();
        assert!(format!("{latched:#}").contains("is latched"));
        let restart = ExternalMutationGuard::arm(activation).unwrap_err();
        assert!(format!("{restart:#}").contains("not a plain file"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collision_checks_are_case_insensitive_even_on_case_sensitive_directories() {
        let (root, activation) = fixture();
        let collision = activation
            .active_root
            .join("OtherMod/PARAM/GameParam/GAMEPARAM.PARAMBND.DCX");
        fs::create_dir_all(collision.parent().unwrap()).unwrap();
        fs::write(collision, b"foreign").unwrap();
        let error = ExternalMutationGuard::arm(activation).unwrap_err();
        assert!(format!("{error:#}").contains("also supplies AP-owned path"));
        fs::remove_dir_all(root).unwrap();
    }
}
