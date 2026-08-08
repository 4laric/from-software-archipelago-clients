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
use std::path::{Path, PathBuf};

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

/// Suffixes that NAME a specific third-party randomizer rather than merely proving one is present.
///
/// `*.randomizeopt` is thefifthmatt's ER Randomizer options file, which his output folder always
/// carries. Confirmed from boblerrr's 2026-08-07 log, which listed `72131.randomizeopt` beside our
/// DLL -- the first real matt output directory anyone here had seen, and the reason this list is
/// evidence rather than a guess.
///
/// 🛑 A hit is a STRONG hint, not a proof, and a miss proves nothing at all: a user can rename or
/// prune anything. It exists to short-circuit triage ("this is the matt stack") not to gate logic.
const NAMED_RANDOMIZER_SUFFIXES: &[(&str, &str)] =
    &[(".randomizeopt", "thefifthmatt ER Randomizer")];

/// How many ancestor directories to inspect above the mod directory.
///
/// 🛑🛑 TWO IS NOT ARBITRARY, IT IS THE OBSERVED SHAPE. boblerrr's resolved mod directory was
/// `...\ER-Archipelago-v0.3.7\me3\randomizer\dll` -- our DLL sits in a NESTED folder inside the
/// host randomizer's tree, so a scan of that folder alone reports "no third-party data files" while
/// matt's `regulation.bin` and `script/` sit one or two levels up, entirely unseen. The old probe
/// was not wrong (its own wording scoped the negative to "HERE"), it was just looking at the wrong
/// floor of the building.
const ANCESTOR_DEPTH: usize = 2;

/// One entry in the mod directory. Split out from the filesystem so [`classify`] is testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

impl Entry {
    pub fn dir(name: &str) -> Self {
        Self {
            name: name.to_string(),
            is_dir: true,
        }
    }

    pub fn file(name: &str) -> Self {
        Self {
            name: name.to_string(),
            is_dir: false,
        }
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

    /// Names of third-party randomizers fingerprinted in this directory, if any.
    ///
    /// Scans every bucket, not just `other`: the point is to name the stack for a triager, and a
    /// future rename could move a fingerprint file between buckets without changing what it means.
    pub fn named_randomizers(&self) -> Vec<&'static str> {
        let mut found = Vec::new();
        let all = self
            .foreign
            .iter()
            .chain(&self.ambiguous)
            .chain(&self.ours)
            .chain(&self.other);
        for name in all {
            let lower = name.to_lowercase();
            for (suffix, label) in NAMED_RANDOMIZER_SUFFIXES {
                if lower.ends_with(suffix) && !found.contains(label) {
                    found.push(*label);
                }
            }
        }
        found
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
        entries.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir,
        });
    }
    Ok(classify(&entries))
}

/// `dir` plus up to [`ANCESTOR_DEPTH`] of its parents, nearest first, each with its own [`Report`].
///
/// Returned as a list rather than merged because WHERE a marker sits is the whole finding: a
/// `regulation.bin` beside our DLL and one two levels up mean different things about the install,
/// and flattening them would throw that away.
pub fn probe_with_ancestors(dir: &Path) -> Vec<(PathBuf, std::io::Result<Report>)> {
    let mut out = vec![(dir.to_path_buf(), probe(dir))];
    let mut cursor = dir;
    for _ in 0..ANCESTOR_DEPTH {
        match cursor.parent() {
            // A root has itself as an ancestor in some shapes; stop rather than loop or re-probe.
            Some(parent) if parent != cursor && !parent.as_os_str().is_empty() => {
                out.push((parent.to_path_buf(), probe(parent)));
                cursor = parent;
            }
            _ => break,
        }
    }
    out
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

    // SCAN UPWARD TOO. Our DLL is routinely nested inside the host randomizer's own tree (see
    // ANCESTOR_DEPTH), so probing only our own folder answers a narrower question than the one
    // being asked.
    let levels = probe_with_ancestors(dir);
    let mut any_data_mod = false;
    let mut named: Vec<&'static str> = Vec::new();

    for (idx, (path, result)) in levels.iter().enumerate() {
        let label = match idx {
            0 => "mod directory".to_string(),
            n => format!("{n} level(s) up"),
        };
        let report = match result {
            Ok(report) => report,
            Err(err) => {
                // An unreadable ANCESTOR is ordinary (permissions, a drive root) and must not read
                // as a fault; an unreadable mod directory already warned above.
                info!(
                    "mod stack: {label} ({}) not readable ({err})",
                    path.display()
                );
                continue;
            }
        };
        for label in report.named_randomizers() {
            if !named.contains(&label) {
                named.push(label);
            }
        }
        if report.data_mod_present() {
            any_data_mod = true;
            info!(
                "mod stack: THIRD-PARTY DATA MOD at {label} ({}) -- {}.",
                path.display(),
                report.foreign.join(", ")
            );
        }
    }

    if any_data_mod {
        info!(
            "mod stack: a co-loaded data mod is SUPPORTED (matt's enemy rando is the expected \
             case), but treat this session as NON-VANILLA: enemy/arena bindings, params, ESD talk \
             scripts and FMG strings may not be the game's own. Do not use this session as an \
             oracle for vanilla ids."
        );
    } else {
        info!(
            "mod stack: no third-party data files in our mod directory or its {ANCESTOR_DEPTH} \
             parents. NOTE a DLL-only mod, or a data mod installed further away, is still \
             invisible here."
        );
    }
    if !named.is_empty() {
        info!(
            "mod stack: fingerprinted randomizer(s): {}",
            named.join(", ")
        );
    }

    let report = match &levels[0].1 {
        Ok(report) => report.clone(),
        Err(_) => return,
    };
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

    /// THE MOTIVATING CASE (CONTRIBUTING rule 11), and the reason this change exists: boblerrr's
    /// 2026-08-07 log said "no third-party data files in our mod directory" while he was demonstrably
    /// running matt's enemy rando. His resolved mod directory was
    /// `...\\ER-Archipelago-v0.3.7\\me3\\randomizer\\dll` -- our DLL nested inside the host
    /// randomizer's tree, with the data one or two levels up and entirely out of scope.
    ///
    /// The old probe was not lying (its wording scoped the negative to "HERE"); it was standing on
    /// the wrong floor. This asserts the shape that misled us cannot report a clean stack again.
    #[test]
    fn a_data_mod_one_level_above_us_is_not_missed() {
        // what the `dll/` folder itself holds: nothing foreign
        let ours = classify(&[
            Entry::dir("ap-package"),
            Entry::file("apconfig.json"),
            Entry::file("eldenring_archipelago.dll"),
            Entry::file("72131.randomizeopt"),
        ]);
        assert!(
            !ours.data_mod_present(),
            "the nested folder really does look clean -- that is the trap"
        );

        // ...and the parent, which is where matt actually installs
        let parent = classify(&[
            Entry::file("regulation.bin"),
            Entry::dir("script"),
            Entry::dir("msg"),
        ]);
        assert!(parent.data_mod_present());
        assert_eq!(parent.foreign, vec!["msg/", "regulation.bin", "script/"]);
    }

    /// `script/` is the ESD talk-script tree, so its presence is what decides whether a session may
    /// be used as an oracle for vanilla shop/talk ids. Pinned because the shop auto-hint probe
    /// depends on this distinction.
    #[test]
    fn the_esd_script_tree_counts_as_a_data_mod() {
        let report = classify(&[Entry::dir("script")]);
        assert!(report.data_mod_present());
        assert_eq!(report.foreign, vec!["script/"]);
    }

    /// The fingerprint that names matt rather than merely proving "some data mod". Taken verbatim
    /// from boblerrr's directory listing -- the first real matt output folder we have seen.
    #[test]
    fn a_randomizeopt_file_names_the_randomizer() {
        let report = classify(&[Entry::file("72131.randomizeopt")]);
        assert_eq!(
            report.named_randomizers(),
            vec!["thefifthmatt ER Randomizer"]
        );
    }

    #[test]
    fn an_unfingerprinted_directory_names_nobody() {
        let report = classify(&[Entry::file("apconfig.json"), Entry::dir("overlay")]);
        assert!(report.named_randomizers().is_empty());
    }

    /// Walking up must terminate and must not re-probe a root as its own parent.
    #[test]
    fn ancestor_probing_terminates_at_the_root() {
        let levels = probe_with_ancestors(Path::new("/"));
        assert!(
            levels.len() <= ANCESTOR_DEPTH + 1,
            "got {} levels",
            levels.len()
        );
        let mut seen = std::collections::HashSet::new();
        for (path, _) in &levels {
            assert!(seen.insert(path.clone()), "probed {path:?} twice");
        }
    }

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
        assert_eq!(
            report.foreign,
            vec!["event/", "map/", "msg/", "regulation.bin", "script/"]
        );
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
