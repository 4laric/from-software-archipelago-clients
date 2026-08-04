//! `runes` -- the client's ONLY door to the player's held rune count (world issue #259).
//!
//! The accessors used to be private to `deathlink.rs`. They are not DeathLink-specific: they are
//! `GameDataMan -> main_player_game_data -> rune_count`, the same typed singleton idiom
//! `inventory` / `upgrades` use. Lifting them here is a visibility change, not new game-memory
//! work -- and it buys the thing the 2026-08-01 Alt-F4 report could not be given: a rune count
//! that appears in the log at every edge, and a write that cannot happen silently.
//!
//! **Every rune write in this crate goes through [`write`].** That is the invariant worth keeping:
//! a second private `rune_count = x` anywhere else re-opens exactly the hole this module closes,
//! because the log would keep looking complete while a write went unrecorded.
//!
//! Wording, the read-back rule and the tests live in `er_logic::rune_log` (pure, host-tested).
//! This file is the I/O half: singleton borrow, log call, nothing else.
//!
//! NOT on the `reset()` edge, and not a candidate for it: `rune_count` is live player state, not
//! a param row or an FMG block, so a map load does not stream a file over our write -- it either
//! kept it or the save did. There is no latch here to re-arm (see the world repo's
//! `test_gf_client_resets_are_called`, which excludes `GameDataMan::instance_mut()` for this
//! reason).

use eldenring::cs::GameDataMan;
use er_logic::rune_log::{self, Sample};
use fromsoftware_shared::FromStatic;

/// Read the local player's held rune count, or `None` before the player game data is up
/// (main menu, mid-load). Same typed singleton idiom as `inventory` / `upgrades`.
pub fn read() -> Option<u32> {
    let gdm = unsafe { GameDataMan::instance() }.ok()?;
    Some(gdm.main_player_game_data.as_ref().rune_count)
}

/// Write the local player's held rune count, logging `before -> after` with `cause` named.
///
/// Returns whether the write was OBSERVED to land -- the value is read back and compared, so a
/// caller can never treat a refused or reverted write as done. `cause` is free text and belongs
/// in the log, so keep it short and in the reader's vocabulary ("keep-runes restore").
///
/// No-op (and says so, at WARN) before the player game data is up.
pub fn write(value: u32, cause: &str) -> bool {
    let before = read();
    let wrote = if let Ok(gdm) = unsafe { GameDataMan::instance_mut() } {
        gdm.main_player_game_data.as_mut().rune_count = value;
        true
    } else {
        false
    };
    // Read BACK, not the value we asked for: "we wrote it" and "the game kept it" are different
    // claims, and only the second one is evidence.
    let after = if wrote { read() } else { None };
    let report = rune_log::report_write(cause, before, value, after);
    if report.landed {
        log::info!("{}", report.line);
    } else {
        log::warn!("{}", report.line);
    }
    report.landed
}

/// Re-assert a value the client already asked for, writing (and logging) only if the game does
/// not already hold it.
///
/// This is the DeathLink keep-runes restore window: the same value is offered on
/// `RESTORE_REASSERT_TICKS` consecutive alive ticks to beat a late engine zero. Five unconditional
/// writes would be five identical log lines for one restore -- per-tick noise, and it would bury
/// the one line that matters. The decision is `er_logic::rune_log::needs_write`, host-tested, so
/// this is wiring only.
///
/// Returns true if the game now holds `value` (either it already did, or the write landed).
pub fn write_if_changed(value: u32, cause: &str) -> bool {
    if !rune_log::needs_write(read(), value) {
        return true;
    }
    write(value, cause)
}

/// Log one rune-count reading at an edge (connect, or the in-world false->true edge).
///
/// Called from exactly two sites in `core.rs`. Deliberately NOT called per tick: rune count moves
/// constantly in normal play, and a per-tick line is noise that gets filtered out and takes the
/// useful lines with it. An unreadable sample is logged at WARN rather than skipped -- a gap must
/// never be ambiguous between "we did not look" and "we could not tell".
pub fn log_sample(sample: Sample) {
    let runes = read();
    let line = rune_log::describe_sample(sample, runes);
    if runes.is_some() {
        log::info!("{line}");
    } else {
        log::warn!("{line}");
    }
}
