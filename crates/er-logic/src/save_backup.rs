//! Rotating session backups of the ACTIVE game save (client#287).
//!
//! The client participates in state Elden Ring persists, and a crash loop or a bad persisted
//! write leaves a player with no recoverable copy of the save being played. The 2026-08-19
//! report that made this concrete: a save directory holding `ER0000.sl2`, `ER0000.sl2.bak`,
//! `ER0000.sl2.randobak`, `AP_me3.sl2` and `ER0000.mod` side by side, every one the same size,
//! and only the ACTIVE modded namespace current -- the `.bak`/`.randobak` files were not backups
//! of the run being played.
//!
//! This module is the PURE half: which file is the live save, what a backup is named, and which
//! old generations are pruned. The I/O half (walk `%APPDATA%\EldenRing`, copy, log, warn) is
//! `eldenring-archipelago::save_backup`.
//!
//! ## What "the active save" means, per launch stack
//!
//! * **me3 (`ap.me3`)** -- the profile's `savefile = "AP_me3.sl2"` line is authoritative
//!   ([`parse_me3_savefile`]); the world's `build.ps1 -Me3Deploy` writes exactly that.
//! * **a non-me3 loader** (EML `mods\*.dll`, matt's "Add dll mod" button) -- the profile is never
//!   read, so the live namespace is the NEWEST save-shaped file in the save directory
//!   ([`select_active_save`]). That covers vanilla `ER0000.sl2`, Alt Saves' custom extensions
//!   (`ER0000.mod`), and Seamless-style names (`ER0000.co2`) without naming any of them.
//! * **no candidates at all** -- an honest `Unresolved`, which the caller must make LOUD
//!   (a silent no-op would claim protection that does not exist; acceptance #4).

/// Extensions a LIVE Elden Ring save can wear, lowercase, no dot.
///
/// `.sl2` is vanilla and me3; `.mod` is the Alt Saves custom-extension convention from the
/// motivating report; `.co2` is Seamless Co-op's namespace. Backup artifacts (`.bak`,
/// `.randobak`) deliberately do NOT match: they are not namespaces the game writes, and backing
/// up somebody's stale `.bak` as if it were the run is precisely the confusion #287 exists to end.
const SAVE_EXTENSIONS: &[&str] = &["sl2", "mod", "co2"];

/// How many generations of one save namespace are kept. Five launches back is enough to reach
/// before any same-day crash loop, and at ~30 MB apiece the directory stays under ~150 MB per
/// namespace. The retention POLICY is this constant plus "newest wins"; it is documented here
/// because the issue asks for exactly that (acceptance #5 covers the mechanism).
pub const KEEP_GENERATIONS: usize = 5;

/// Is `name` a file the game could be writing as a live save? Case-insensitive.
pub fn is_save_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    let Some((_, ext)) = lower.rsplit_once('.') else {
        return false;
    };
    SAVE_EXTENSIONS.contains(&ext)
}

/// Extract the `savefile = "NAME"` a me3 v1 profile declares, if it does.
///
/// Line-oriented and deliberately NOT a TOML parse: the profile is four keys and a list of
/// packages, and the client crate carries no TOML dependency. A quoted value is unquoted; an
/// absent or unquoted-empty key is `None`. Anything weird (two keys, a comment) resolves to the
/// LAST plain assignment, matching how me3's own parser reads a flat key.
pub fn parse_me3_savefile(profile_text: &str) -> Option<String> {
    let mut found = None;
    for line in profile_text.lines() {
        // Strip a trailing comment before testing the key (`savefile = "x" # comment`).
        let line = line.split('#').next().unwrap_or("").trim();
        let Some(value) = line.strip_prefix("savefile") else {
            continue;
        };
        let Some(value) = value.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'').trim();
        if !value.is_empty() {
            found = Some(value.to_string());
        }
    }
    found
}

/// Which save the backup should copy, given the launch stack and what is on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    /// me3 declared this namespace in its profile and a file by that name exists: copy it.
    ProfileNamed(String),
    /// me3 declared this namespace but no such file exists yet -- a first launch of a fresh
    /// profile, which has nothing to lose. NOT a failure: log it, do not warn the player.
    FreshProfile(String),
    /// A non-me3 loader: the profile is never read, so the live namespace is the newest
    /// save-shaped file on disk. This is what "the file actually being written" means when no
    /// profile can tell us (Alt Saves `.mod`, Seamless `.co2`, plain `ER0000.sl2`).
    NewestModified(String),
    /// No profile answer and no save-shaped file anywhere we looked. The caller must surface
    /// this prominently -- claiming protection here would be a lie.
    Unresolved,
}

/// Pick the live save. `profile_save` is the me3 `savefile` value and is consulted ONLY when
/// `loader_is_me3` -- under any other loader the profile is dead text on disk, and trusting it
/// would back up `AP_me3.sl2` while the game writes `ER0000.sl2` (the exact misread of the
/// motivating report, where four same-sized namespaces sat side by side).
///
/// `candidates` is `(file_name, mtime_secs)` for every save-shaped file found; the tie-break
/// after mtime is the name, so equal mtimes resolve deterministically rather than by directory
/// order.
pub fn select_active_save(
    loader_is_me3: bool,
    profile_save: Option<&str>,
    candidates: &[(String, u64)],
) -> Selection {
    if loader_is_me3 {
        if let Some(name) = profile_save {
            return if candidates.iter().any(|(n, _)| n == name) {
                Selection::ProfileNamed(name.to_string())
            } else {
                Selection::FreshProfile(name.to_string())
            };
        }
        // An me3 launch with NO savefile line uses me3's own default namespace; me3 v1 defaults
        // to the vanilla `ER0000.sl2`, which the newest-modified fallback below also finds.
    }
    candidates
        .iter()
        .max_by(|(an, am), (bn, bm)| am.cmp(bm).then_with(|| an.cmp(bn)))
        .map(|(name, _)| Selection::NewestModified(name.clone()))
        .unwrap_or(Selection::Unresolved)
}

/// The backup file name for one generation: the save's own name plus a timestamp, so generations
/// sort lexicographically and no generation ever overwrites another ("never overwrite the only
/// prior backup" is structural here, not a check). `taken` is the names already in the backup
/// directory; two launches inside one second get a `-2` / `-3` suffix instead of a collision.
pub fn backup_name(
    save_name: &str,
    timestamp: &str,
    taken: &std::collections::HashSet<String>,
) -> String {
    let base = format!("{save_name}.{timestamp}.bak");
    if !taken.contains(&base) {
        return base;
    }
    for n in 2.. {
        let candidate = format!("{base}-{n}");
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

/// Is `backup` a generation of the save namespace `save_name`? Matches both the plain
/// `{name}.{timestamp}.bak` and the same-second `{name}.{timestamp}.bak-{n}` collision suffix
/// [`backup_name`] mints. A backup of a DIFFERENT namespace (`ER0000.sl2` vs `AP_me3.sl2`) is not,
/// so retention prunes families independently.
pub fn is_generation_of(backup: &str, save_name: &str) -> bool {
    let Some(rest) = backup.strip_prefix(save_name) else {
        return false;
    };
    rest.ends_with(".bak") || rest.contains(".bak-")
}

/// The generations to DELETE under the retention policy: everything past the newest
/// [`KEEP_GENERATIONS`] of one namespace. `generations` is `(backup_file_name, mtime_secs)`;
/// ordering is by mtime with the name as tie-break (same rule as [`select_active_save`], so a
/// filesystem that hands back equal mtimes still prunes deterministically).
pub fn prune_generations(generations: &[(String, u64)], keep: usize) -> Vec<String> {
    let mut sorted: Vec<&(String, u64)> = generations.iter().collect();
    sorted.sort_by(|(an, am), (bn, bm)| bm.cmp(am).then_with(|| bn.cmp(an)));
    sorted
        .into_iter()
        .skip(keep)
        .map(|(name, _)| name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The motivating case (#287, 2026-08-19): five same-sized namespaces, only the modded one
    /// current. An me3 launch must name `AP_me3.sl2` from the PROFILE, not guess from mtimes --
    /// the whole point of the report is that mtime alone cannot tell these apart reliably.
    #[test]
    fn me3_launch_backs_up_the_profile_named_save() {
        let candidates = vec![
            ("ER0000.sl2".to_string(), 100u64),
            ("ER0000.sl2.bak".to_string(), 999), // not save-shaped; caller filtered it
            ("AP_me3.sl2".to_string(), 50),
            ("ER0000.mod".to_string(), 10),
        ];
        let candidates: Vec<(String, u64)> = candidates
            .into_iter()
            .filter(|(n, _)| is_save_name(n))
            .collect();
        assert_eq!(
            select_active_save(true, Some("AP_me3.sl2"), &candidates),
            Selection::ProfileNamed("AP_me3.sl2".to_string())
        );
    }

    /// build.ps1 writes exactly this profile; the parser must read its savefile line.
    #[test]
    fn the_shipped_ap_me3_profile_parses() {
        let profile =
            "profileVersion = \"v1\"\nsavefile = \"AP_me3.sl2\"\ndisable_arxan = true\n\n\
                       [[supports]]\ngame = \"eldenring\"\n\n[[packages]]\npath = 'ap-package'\n";
        assert_eq!(parse_me3_savefile(profile).as_deref(), Some("AP_me3.sl2"));
    }

    #[test]
    fn profile_without_a_savefile_line_is_none() {
        assert_eq!(parse_me3_savefile("profileVersion = \"v1\"\n"), None);
        assert_eq!(parse_me3_savefile(""), None);
        assert_eq!(parse_me3_savefile("savefile = \"\"\n"), None);
    }

    #[test]
    fn a_trailing_comment_does_not_corrupt_the_value() {
        assert_eq!(
            parse_me3_savefile("savefile = \"ER0000.sl2\" # custom\n").as_deref(),
            Some("ER0000.sl2")
        );
    }

    /// Acceptance #1's other half: a FIRST me3 launch has no `AP_me3.sl2` yet. That is a normal
    /// fresh profile, not a protection failure -- nothing exists to protect.
    #[test]
    fn me3_fresh_profile_is_not_an_error() {
        assert_eq!(
            select_active_save(true, Some("AP_me3.sl2"), &[]),
            Selection::FreshProfile("AP_me3.sl2".to_string())
        );
    }

    /// Acceptance #2: a custom-extension launch (no me3, Alt Saves `.mod`) backs up the file
    /// actually being written -- the NEWEST save-shaped one -- not an unrelated `.sl2`.
    #[test]
    fn non_me3_backs_up_the_newest_namespace_whatever_its_extension() {
        let candidates = vec![
            ("ER0000.sl2".to_string(), 100u64),
            ("ER0000.mod".to_string(), 200),
        ];
        assert_eq!(
            select_active_save(false, Some("AP_me3.sl2"), &candidates),
            Selection::NewestModified("ER0000.mod".to_string()),
            "the profile line is DEAD TEXT under a non-me3 loader -- trusting it here is the \
             exact misread of the motivating report"
        );
    }

    /// 🛑 An me3 profile line must not leak into a non-me3 decision even when the named file
    /// exists and is newer: matt's loader never read ap.me3, so the game is writing something
    /// else.
    #[test]
    fn the_profile_is_ignored_when_the_loader_never_read_it() {
        let candidates = vec![
            ("ER0000.sl2".to_string(), 100u64),
            ("AP_me3.sl2".to_string(), 300),
        ];
        assert_eq!(
            select_active_save(false, Some("AP_me3.sl2"), &candidates),
            Selection::NewestModified("AP_me3.sl2".to_string()),
            "here newest WINS because it is also the truth; the next assertion shows the rule, \
             not the coincidence"
        );
        let candidates = vec![
            ("ER0000.sl2".to_string(), 300u64),
            ("AP_me3.sl2".to_string(), 100),
        ];
        assert_eq!(
            select_active_save(false, Some("AP_me3.sl2"), &candidates),
            Selection::NewestModified("ER0000.sl2".to_string())
        );
    }

    /// Acceptance #4: nothing save-shaped anywhere is an honest Unresolved, never a guess.
    #[test]
    fn no_candidates_is_unresolved_not_a_guess() {
        assert_eq!(select_active_save(false, None, &[]), Selection::Unresolved);
        // me3 without a savefile line and no files either: still unresolved.
        assert_eq!(select_active_save(true, None, &[]), Selection::Unresolved);
    }

    #[test]
    fn backup_artifacts_are_not_live_saves() {
        assert!(is_save_name("ER0000.sl2"));
        assert!(is_save_name("ER0000.mod"));
        assert!(is_save_name("ER0000.co2"));
        assert!(is_save_name("AP_me3.SL2"));
        assert!(!is_save_name("ER0000.sl2.bak"));
        assert!(!is_save_name("ER0000.sl2.randobak"));
        assert!(!is_save_name("graphicsconfig.ini"));
        assert!(!is_save_name("ER0000"));
    }

    /// Equal mtimes resolve by NAME, so two filesystems that disagree about granularity still
    /// pick the same file.
    #[test]
    fn equal_mtimes_pick_deterministically() {
        let candidates = vec![
            ("ER0000.sl2".to_string(), 100u64),
            ("AP_me3.sl2".to_string(), 100),
        ];
        let first = select_active_save(false, None, &candidates);
        let second = select_active_save(
            false,
            None,
            &candidates.iter().rev().cloned().collect::<Vec<_>>(),
        );
        assert_eq!(first, second);
    }

    /// "Never overwrite the only prior backup" is structural: the timestamp is in the name, and
    /// a same-second second launch suffixes instead of colliding.
    #[test]
    fn same_second_backups_never_collide() {
        let mut taken = HashSet::new();
        let first = backup_name("AP_me3.sl2", "2026-08-21_14-30-00", &taken);
        assert_eq!(first, "AP_me3.sl2.2026-08-21_14-30-00.bak");
        taken.insert(first.clone());
        let second = backup_name("AP_me3.sl2", "2026-08-21_14-30-00", &taken);
        assert_eq!(second, "AP_me3.sl2.2026-08-21_14-30-00.bak-2");
        taken.insert(second.clone());
        let third = backup_name("AP_me3.sl2", "2026-08-21_14-30-00", &taken);
        assert_eq!(third, "AP_me3.sl2.2026-08-21_14-30-00.bak-3");
        assert_ne!(first, second);
        assert_ne!(second, third);
    }

    /// Generations of one namespace are recognised BOTH ways `backup_name` mints them, and a
    /// sibling namespace never matches -- retention must not prune `ER0000.sl2`'s history because
    /// `AP_me3.sl2` launched.
    #[test]
    fn generation_membership_is_per_namespace() {
        assert!(is_generation_of(
            "AP_me3.sl2.2026-08-21_14-30-00.bak",
            "AP_me3.sl2"
        ));
        assert!(is_generation_of(
            "AP_me3.sl2.2026-08-21_14-30-00.bak-2",
            "AP_me3.sl2"
        ));
        assert!(!is_generation_of(
            "ER0000.sl2.2026-08-21_14-30-00.bak",
            "AP_me3.sl2"
        ));
        assert!(
            !is_generation_of("AP_me3.sl2", "AP_me3.sl2"),
            "the live file is not its own backup"
        );
        assert!(!is_generation_of("AP_me3.sl2.randobak", "AP_me3.sl2"));
    }

    /// Acceptance #3 + #5: repeated launches retain the newest KEEP_GENERATIONS, oldest pruned
    /// first, by mtime and not by directory order.
    #[test]
    fn retention_keeps_the_newest_generations() {
        let generations: Vec<(String, u64)> = (1..=7)
            .map(|n| (format!("AP_me3.sl2.2026-08-{n:02}_12-00-00.bak"), n as u64))
            .collect();
        let pruned = prune_generations(&generations, KEEP_GENERATIONS);
        assert_eq!(pruned.len(), 7 - KEEP_GENERATIONS);
        assert!(pruned.contains(&"AP_me3.sl2.2026-08-01_12-00-00.bak".to_string()));
        assert!(pruned.contains(&"AP_me3.sl2.2026-08-02_12-00-00.bak".to_string()));
        assert!(!pruned.contains(&"AP_me3.sl2.2026-08-07_12-00-00.bak".to_string()));
        // Input order must not matter.
        let mut shuffled = generations.clone();
        shuffled.reverse();
        let mut p = prune_generations(&shuffled, KEEP_GENERATIONS);
        let mut q = pruned.clone();
        p.sort();
        q.sort();
        assert_eq!(p, q);
    }
}
