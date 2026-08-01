//! THE teardown guard: is it safe to touch the player's `chr_ins` / `special_effect` lists?
//!
//! ## The rule, and why it is one function
//!
//! At the **death-cam transition** -- the window between the player's HP reaching 0 and the respawn
//! at a grace, while YOU DIED is on screen -- the engine tears down `chr_ins` and its
//! `special_effect` list. Iterating or mutating either one during that teardown is a native CTD.
//! It is not theoretical: `no_equip_load` unconditionally walked the player's list every frame to
//! compute `has`, and crashed there (`archipelago20260719 Copy 2.log`). Every list-touching module
//! then grew its own copy of `hp <= 0`.
//!
//! `hp <= 0` is the right signal because it is observable IMMEDIATELY -- frames before any
//! in-world edge. The respawn's in-world false->true edge only fires AFTER teardown has run, so
//! anything keyed on that edge is already too late.
//!
//! ## 🛑 What this is NOT, and must never be merged with
//!
//! `deathlink.rs` also tests `hp <= 0`, TWICE, and neither one is this rule:
//!
//! * `local_death_edge` -- "is the player dead", the OUTGOING DeathLink observation. A game-state
//!   fact we broadcast.
//! * `kill_local_player` -- "already dead, don't re-kill", an idempotence guard.
//!
//! Same expression, three different meanings. Folding them together would mean a future change
//! here -- say, widening the guard to `hp <= 0 || in_load_screen` because teardown turns out to
//! start earlier -- silently changing when we broadcast a death and when we refuse to kill. That is
//! the "one bit, two jobs" failure this repo has already paid for three times (#239, #240, the
//! SignPuddle/shop-flag collision). They stay separate ON PURPOSE.
//!
//! (`scaling.rs`'s comment claimed it was generalising the guard "no_fall_damage / no_equip_load /
//! deathlink have each carried". The first two, yes. DeathLink's was never this guard.)

/// True when the player's `chr_ins` / `special_effect` lists must not be walked or mutated.
///
/// Worst case of a false positive is the documented degrade: the calling feature skips a tick or
/// two while the player is dead, and re-runs on respawn (`hp > 0`). Every caller is idempotent and
/// re-applies, so a skipped tick costs nothing visible in play.
pub fn lists_unsafe_to_touch(player_hp: i32) -> bool {
    player_hp <= 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Direct calls with synthetic input: no seed corpus reaches a death frame, so this guard is
    /// untested unless it is called on purpose (`guard-absent-from-corpus-needs-a-direct-call`).
    #[test]
    fn zero_and_below_are_unsafe_and_positive_hp_is_safe() {
        assert!(
            lists_unsafe_to_touch(0),
            "hp 0 IS the death-cam edge, not a safe boundary"
        );
        assert!(
            lists_unsafe_to_touch(-1),
            "overkill damage drives hp negative before teardown"
        );
        assert!(lists_unsafe_to_touch(i32::MIN));
        assert!(!lists_unsafe_to_touch(1));
        assert!(!lists_unsafe_to_touch(i32::MAX));
    }

    /// 🛑 THE KEEPER. Five modules had grown a private `hp <= 0` before anyone counted them, and my
    /// own first pass at unifying them said "four sites" in a comment while the real number was
    /// five. A miscount in a comment is folklore with syntax highlighting, so this counts instead.
    ///
    /// Scans the client crate for raw `hp <= 0` in CODE (comments excluded) and allows it only in
    /// `deathlink.rs`, whose two uses are different rules by design (module docs above). Skips
    /// cleanly if the sibling crate is not beside us, so `cargo test -p er-logic` alone never fails
    /// for the wrong reason.
    #[test]
    fn no_module_grows_a_private_copy_of_the_death_guard() {
        use std::path::Path;
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../eldenring-archipelago/src")
            .canonicalize();
        let Ok(src) = src else {
            return; // sibling crate absent -- nothing to police
        };
        // Files whose raw `hp <= 0` is a DIFFERENT rule, named so the exemption is auditable.
        const EXEMPT: &[&str] = &["deathlink.rs"];
        let mut scanned = 0usize;
        let mut offenders: Vec<String> = vec![];
        for entry in std::fs::read_dir(&src).expect("client src is readable") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let text = std::fs::read_to_string(&path).expect("read");
            scanned += 1;
            if EXEMPT.contains(&name.as_str()) {
                continue;
            }
            for (i, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                if code.contains("hp <= 0") {
                    offenders.push(format!("{name}:{}", i + 1));
                }
            }
        }
        // Rule 2: an empty sweep is a failure, not a clean run.
        assert!(
            scanned > 20,
            "only {scanned} client modules scanned -- the sweep is blind"
        );
        assert!(
            offenders.is_empty(),
            "private death-guard copies found at {offenders:?}. Call \
             er_logic::death_guard::lists_unsafe_to_touch instead, or add the file to EXEMPT with \
             a written reason if it is a DIFFERENT rule."
        );
    }
}
