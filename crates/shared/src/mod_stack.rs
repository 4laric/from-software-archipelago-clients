//! Mod-stack provenance: names the DATA mods sitting in our mod directory.
//!
//! MOTIVATION (2026-08-06, boblerrr). Two of his boss health bars carried composite names our
//! pipeline cannot produce -- "Tibia Deathbird", "Royal Knight Rykard" -- and we spent a session
//! arguing about whether a third-party mod was in play. The evidence we reached for was his crash
//! report: it showed only our own DLL paths and "no third-party DLL in either backtrace".
//!
//! 🛑 **That evidence was vacuous, and this module exists because of it.** `crash_handler`'s
//! `modules:` section is built by resolving each backtrace FRAME address through
//! `GetModuleHandleExW(FROM_ADDRESS)` -- it can only ever list modules that own a frame. And
//! thefifthmatt's randomizer, the mod actually in his stack, ships **no DLL at all**: its output is
//! DATA -- `regulation.bin`, `event`, `msg`, `script`, `map`. A module list, however it is built,
//! is structurally blind to it. We were quantifying over an empty set and reading the empty result
//! as an all-clear.
//!
//! The instrument that CAN see a data mod is the mod DIRECTORY, which we already resolve at
//! startup to place `apconfig.json` and `log/`. This module stats it once and writes what it finds
//! into the log, so every user report from here on describes its own stack instead of requiring a
//! round-trip to the player.
//!
//! ## Why a name list is a sound test here
//!
//! Precisely because this client ships almost nothing on disk: its me3 payload is the native DLL
//! plus `apconfig.json` / `check_lots_table.json` / `log/`, and the cosmetic icon override, which
//! lives one level down in `ap-package/`. It writes **no** `regulation.bin`, `event`, `msg`,
//! `script`, `map` or `param`. So any of those names at the TOP level of the mod directory belongs
//! to somebody else. That is a statement about our own deploy ([`build.ps1`] me3 deploy step), not
//! a guess about anyone's mod.
//!
//! ## What this deliberately does NOT claim
//!
//! * It does not name matt. `regulation.bin` next to us means "a data mod is co-loaded", full
//!   stop -- ModEngine2 profiles, convergence-style overhauls and matt's randomizer all look
//!   identical from here. Naming him needs his spoiler-log/options artifacts, which we do not have
//!   a sample of.
//! * It does not prove ENEMY randomisation specifically. That needs a runtime comparison of a live
//!   boss against the datamine baseline; see the follow-up issue.
//! * It says nothing about whether the co-loaded mod is a PROBLEM. Stacking with matt's enemy rando
//!   is a supported configuration. This is provenance, not a warning.
//!
//! A miss is as useful as a hit: an empty `foreign` list on a crash report positively rules the
//! stack out, which is the thing we could not do in August.

use std::fs;
use std::path::Path;

use log::{info, warn};

/// Top-level names a FromSoft DATA mod brings and this client never ships.
///
/// Kept lowercase; comparison lowercases the observed name. Sourced from our own deploy: if you
/// add a file to the me3 deploy step in `build.ps1`, check it against this list.
const FOREIGN_MARKERS: &[&str] = &[
    "regulation.bin",
    "event",
    "msg",
    "script",
    "map",
    "param",
    "action",
    "chr",
    "obj",
    "parts",
    "sfx",
    "sound",
    "font",
];

/// Top-level names our own deploy writes. Everything here is expected and uninteresting.
const OURS: &[&str] = &[
    "eldenring_archipelago.dll",
    "eldenring_ap.dll",
    "apconfig.json",
    "check_lots_table.json",
    "shoplineup_flags.json",
    "ap-package",
    "log",
];

/// Names we cannot adjudicate from the name alone.
///
/// `menu/` is the live example: our icon override ships one, but under `ap-package/menu`, so a
/// TOP-level `menu/` is either a foreign mod or somebody's flattened install. Reporting it as
/// foreign would manufacture a false positive on our own users; hiding it would lose a real
/// signal. So it gets its own bucket and the reader decides.
const AMBIGUOUS: &[&str] = &["menu"];

/// One entry in the mod directory. Split out from the filesystem so [`classify`] is testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

impl Entry {
    pub fn dir(name: &str) -> Self {
        Self { name: name.to_string(), is_dir: true }
    }

    pub fn file(name: &str) -> Self {
        Self { name: name.to_string(), is_dir: false }
    }
}

/// What we found beside us.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Names that can only have come from another mod.
    pub foreign: Vec<String>,
    /// Names we cannot adjudicate (see [`AMBIGUOUS`]).
    pub ambiguous: Vec<String>,
    /// Names our own deploy writes.
    pub ours: Vec<String>,
    /// Everything else -- saves, configs, the loader's own files, user junk.
    pub other: Vec<String>,
}

impl Report {
    /// True when at least one name can only have come from another mod.
    ///
    /// 🛑 FALSE does NOT mean "no other mod is loaded" -- a mod that ships only a DLL, or one
    /// installed somewhere other than our mod directory, is invisible here. It means "no data mod
    /// in OUR directory", which is a narrower and honest claim.
    pub fn data_mod_present(&self) -> bool {
        !self.foreign.is_empty()
    }
}

/// Sorts directory entries into [`Report`] buckets. Pure -- no I/O, so it is unit-testable.
pub fn classify(entries: &[Entry]) -> Report {
    let mut report = Report::default();
    for entry in entries {
        let lower = entry.name.to_lowercase();
        // Directories are rendered with a trailing slash: `event/` vs `regulation.bin` is the
        // difference between "a mod brought a whole asset tree" and "a mod brought one file", and
        // the reader of a bug report should not have to know which markers are which.
        let shown = if entry.is_dir {
            format!("{}/", entry.name)
        } else {
            entry.name.clone()
        };
        // `.tables.json` is a build stamp we emit beside the DLL under a variable name.
        let is_ours = OURS.contains(&lower.as_str()) || lower.ends_with(".tables.json");
        if is_ours {
            report.ours.push(shown);
        } else if FOREIGN_MARKERS.contains(&lower.as_str()) {
            report.foreign.push(shown);
        } else if AMBIGUOUS.contains(&lower.as_str()) {
            report.ambiguous.push(shown);
        } else {
            report.other.push(shown);
        }
    }
    report.foreign.sort();
    report.ambiguous.sort();
    report.ours.sort();
    report.other.sort();
    report
}

/// Reads `dir` and classifies its top-level entries.
pub fn probe(dir: &Path) -> std::io::Result<Report> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        // A `file_type` failure on one entry must not lose the whole probe; assume "file", which
        // only affects the logged shape, never the bucket.
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        entries.push(Entry { name: entry.file_name().to_string_lossy().into_owned(), is_dir });
    }
    Ok(classify(&entries))
}

/// Writes the provenance block into the log. Call once, immediately after the logger exists.
///
/// Every line here is INFO: a co-loaded data mod is a supported configuration, not a fault. The
/// point is that the log answers the question without anyone having to ask the player.
pub fn log_provenance() {
    info!("mod stack: loader = {}", crate::utils::loader().describe());

    let dir = match crate::utils::mod_directory() {
        Ok(dir) => dir,
        Err(err) => {
            warn!("mod stack: mod directory unresolved ({err}); cannot probe for co-loaded mods");
            return;
        }
    };
    info!("mod stack: mod directory = {}", dir.display());

    let report = match probe(dir) {
        Ok(report) => report,
        Err(err) => {
            warn!("mod stack: could not read mod directory ({err}); no co-load information");
            return;
        }
    };

    if report.data_mod_present() {
        info!(
            "mod stack: THIRD-PARTY DATA MOD present -- {} beside us. Stacking is supported \
             (matt's enemy rando is the expected case), but treat this session as NON-VANILLA: \
             enemy/arena bindings, params and FMG strings may not be the game's own.",
            report.foreign.join(", ")
        );
    } else {
        info!(
            "mod stack: no third-party data files in our mod directory. NOTE this rules out a \
             data mod HERE only -- a DLL-only mod, or one installed elsewhere, is invisible here."
        );
    }
    if !report.ambiguous.is_empty() {
        info!(
            "mod stack: ambiguous (ours ships these under ap-package/, so a top-level copy is \
             foreign or a flattened install): {}",
            report.ambiguous.join(", ")
        );
    }
    info!("mod stack: ours = [{}]", report.ours.join(", "));
    info!("mod stack: other = [{}]", report.other.join(", "));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE MOTIVATING CASE (CONTRIBUTING rule 11): boblerrr's directory, as we now know it to be --
    /// matt's randomizer output hosting our DLL. The probe must call this a data mod.
    #[test]
    fn bobler_stack_is_named_as_a_data_mod() {
        let report = classify(&[
            Entry::file("eldenring_archipelago.dll"),
            Entry::file("apconfig.json"),
            Entry::dir("log"),
            Entry::file("regulation.bin"),
            Entry::dir("event"),
            Entry::dir("msg"),
            Entry::dir("script"),
            Entry::dir("map"),
        ]);
        assert!(report.data_mod_present());
        assert_eq!(report.foreign, vec!["event/", "map/", "msg/", "regulation.bin", "script/"]);
    }

    /// The NEGATIVE CONTROL, and the one that decides whether this is worth shipping: our own
    /// deploy, alone, must be silent. A probe that cries mod on a clean install teaches everyone to
    /// ignore the line.
    #[test]
    fn our_own_deploy_alone_is_clean() {
        let report = classify(&[
            Entry::file("eldenring_archipelago.dll"),
            Entry::file("eldenring_archipelago.dll.tables.json"),
            Entry::file("apconfig.json"),
            Entry::file("check_lots_table.json"),
            Entry::file("shoplineup_flags.json"),
            Entry::dir("ap-package"),
            Entry::dir("log"),
        ]);
        assert!(!report.data_mod_present());
        assert!(report.foreign.is_empty());
        assert!(report.other.is_empty(), "unexpected: {:?}", report.other);
    }

    /// Our icon override lives at `ap-package/menu`, so a TOP-level `menu/` is not ours -- but it
    /// is not proof of a data mod either. It must land in neither extreme bucket.
    #[test]
    fn top_level_menu_is_ambiguous_not_foreign() {
        let report = classify(&[Entry::dir("menu"), Entry::dir("ap-package")]);
        assert!(!report.data_mod_present());
        assert_eq!(report.ambiguous, vec!["menu/"]);
    }

    /// Windows paths are case-insensitive and mod tools are inconsistent about it.
    #[test]
    fn marker_match_is_case_insensitive() {
        let report = classify(&[Entry::file("Regulation.BIN"), Entry::dir("Event")]);
        assert_eq!(report.foreign, vec!["Event/", "Regulation.BIN"]);
    }

    /// An unrelated file is neither ours nor a mod; it must not inflate either signal.
    #[test]
    fn unknown_names_go_to_other() {
        let report = classify(&[Entry::file("me3.toml"), Entry::file("AP_me3.sl2")]);
        assert!(!report.data_mod_present());
        assert_eq!(report.other, vec!["AP_me3.sl2", "me3.toml"]);
    }
}
