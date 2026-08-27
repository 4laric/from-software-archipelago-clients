//! `slotLength = 1` for every spell -- the param half of `auto_equip` for sorceries and
//! incantations (#440).
//!
//! ## Why it exists
//!
//! `auto_equip` writes a received spell into the memory slot chosen by
//! [`er_logic::spell_equip::slot_for_spell`], which is a plain modulus over the slots the player
//! has EARNED (`2 + stones`). That modulus is only exact if every spell costs exactly one slot.
//! **24 of the 213 memorisable spells cost more** -- the maximum is 3 (Comet Azur, Placidusax's
//! Ruin, Scarlet Aeonia) -- so without this the planner would need to bin-pack, and a 3-slot spell
//! could not be placed at all until three slots had been earned.
//!
//! ⭐ **Ruling 2026-08-10: normalise the param instead of bin-packing.** `slotLength` is
//! `MAGIC_PARAM_ST +0x21`, one byte sitting immediately before the `requirement_*` bytes
//! [`crate::no_weapon_reqs`] already zeroes. This is a field on a write we already ship, not a new
//! mechanism, and it borrows that module's whole shape: enabled from slot_data at connect, latched
//! once applied, re-armed on the `in_world` edge because a map load streams `Magic` back in.
//!
//! 🛑 **A DELIBERATE GLOBAL write.** It makes every multi-slot spell cheaper for the entire run,
//! not only auto-equipped ones. That is the accepted cost of a pure modulus -- do NOT "fix" it by
//! scoping it to received items, and do not read it as an instance of the sellValue-clamp defect
//! (right mechanism, wrong scope); that one was accidental, this one is ruled.
//!
//! 🛑 It is NOT what makes over-capacity spells castable. The game does not enforce memory-slot
//! capacity at all -- measured on 1.16.2, a spell written to slot 5 of a two-slot character is
//! accepted, castable, and survives a reload. This write makes the PLANNER's arithmetic honest;
//! the clamp itself is ours, in `er_logic`.
//!
//! Gated on `auto_equip` rather than an option of its own: it exists only to make that feature's
//! modulus exact, has no meaning without it, and so needs no new contract key.
//!
//! ## The count is an oracle -- and the oracle is PER BUILD
//!
//! The log line reports what it actually normalised, so a number that disagrees with the vanilla
//! count means the param table is not vanilla -- which on a stacked install (matt's randomizer
//! ships its own `regulation.bin`) is worth knowing and is otherwise invisible to us. THAT is what
//! the check is for, and it stays.
//!
//! 🛑 **The expected number is a property of the executable, not a constant.** On 2.6.2.0 exactly
//! **25** rows of 213 have `slotLength > 1`: the 24 memorisable spells above, plus one unnamed
//! non-memorisable row (`2050001`). Tarnished Edition (2.7.0.0) ships a bigger `Magic` table and a
//! different distribution: **105 of 317**, measured on the first 2.7.0.0 smoke run (clients PR #456
//! triage, log `archipelago-2026-08-27.log`) on an install with no data mod in sight. Left as one
//! hardcoded 25, the oracle accused EVERY Tarnished player's vanilla table of being modded -- a
//! cry-wolf warning is worse than no warning, because the next real one reads as noise.
//!
//! So the count is version-aware, off the SAME detection [`crate::rva_table`] dispatches on, and a
//! build with no measured count falls back to the 2.6.2.0 figure exactly as `rva_table::current`
//! falls back to the verified column. Adding a build means measuring its count on a clean install
//! and adding an arm -- not widening a tolerance.
//!
//! ⚠ The oracle reads on the FIRST apply only. This module is re-armed on the `in_world` edge like
//! every other param writer, and a re-pass over a table the load never reverted finds 0 rows to
//! normalise -- the EXPECTED shape, not a modded table. crash-19968's session (client#351) carried
//! `normalised 0 of 317 ... (param table may be modded)` from exactly such a re-pass, and the line
//! was filed as evidence of a stale param read. `APPLIED_ONCE` splits the cases; the "may be
//! modded" wording now only prints when the first-ever count disagrees with vanilla.

use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{Magic, SoloParamRepository};
use fromsoftware_shared::FromStatic;

use crate::game_version_gate::{Supported, detected};

/// What every spell is normalised to. One slot, so the planner's modulus is exact.
const ONE_SLOT: u8 = 1;

/// Rows expected to need normalising on a vanilla 2.6.2.0 `Magic` table (24 memorisable spells +
/// row `2050001`). Logged, never asserted -- a mismatch is information about the install, not a
/// failure.
const VANILLA_MULTI_SLOT_ROWS_WW262: u32 = 25;

/// The same count measured on vanilla 2.7.0.0 (Tarnished Edition): 105 of 317 `Magic` rows.
/// MEASURED, not derived -- the first 2.7.0.0 smoke run's own probe line, triaged in clients PR
/// #456 on an install carrying no data mod. It is here so that run stops being reported as modded.
const VANILLA_MULTI_SLOT_ROWS_WW270: u32 = 105;

/// The vanilla count for one detected executable. Split out from [`expected_multi_slot_rows`] so
/// the mapping is testable without a game: the defect this fixes was a per-build number frozen as a
/// constant, and a test is what stops the next build freezing it again.
///
/// `None` (detection failed) and the JP build take the 2.6.2.0 figure -- the same principle as
/// `rva_table::current`'s fallback: prefer the VERIFIED column, and note that this arm is not
/// reachable in a normal session because the version gate refuses to initialise at all on an
/// executable we have no table for.
fn expected_for(version: Option<Supported>) -> u32 {
    match version {
        Some(Supported::Ww270) | Some(Supported::Jp2701) => VANILLA_MULTI_SLOT_ROWS_WW270,
        Some(Supported::Ww262) | Some(Supported::Jp2621) | None => VANILLA_MULTI_SLOT_ROWS_WW262,
    }
}

/// The vanilla count for the executable we are actually running in.
fn expected_multi_slot_rows() -> u32 {
    expected_for(detected())
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static APPLIED: AtomicBool = AtomicBool::new(false);
/// Whether ANY pass has ever applied this session. Distinguishes the first apply from a re-arm
/// re-pass: on a re-pass, `normalised == 0` means the load did NOT revert the table (our write
/// survived) -- the expected shape, not the "param table may be modded" anomaly. crash-19968's
/// session (client#351) carried exactly that false alarm: `normalised 0 of 317 Magic rows
/// (expected 25 ...)` printed by a re-pass, and it read as a stale/moved param table.
static APPLIED_ONCE: AtomicBool = AtomicBool::new(false);

/// Set from slot_data `options.auto_equip` at connect. Shares that option deliberately.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if on {
        log::info!("spell_slot_length: enabled (all spells normalised to one memory slot)");
    }
}

/// Re-arm the once-per-session normalisation on the `in_world` false->true edge. A map load streams
/// `Magic` back in and restores the vanilla `slotLength`s; without this the feature would work
/// until the player's first load and then quietly stop. Clears the LATCH only -- `ENABLED` comes
/// from slot_data at connect and must survive the edge. Safe to re-run: the write is idempotent.
pub fn reset() {
    APPLIED.store(false, Ordering::Relaxed);
}

/// Per-tick until applied. Needs the param repo populated (in-world); retries until rows are
/// visible, then latches for the session.
pub fn tick() {
    if !ENABLED.load(Ordering::Relaxed) || APPLIED.load(Ordering::Relaxed) {
        return;
    }
    // MENU/BOOT GATE: the param repo's holders exist but are not settled during early boot or at
    // the main menu, where the crate's `rows_mut` panics. Same gate every other param writer uses.
    if !crate::flags::in_world() {
        return;
    }
    // SAFETY: FD4 singleton; only mutated on the single-threaded FrameBegin tick.
    let Ok(repo) = (unsafe { SoloParamRepository::instance_mut() }) else {
        return;
    };
    // #351: mid-restream holder -> upstream rows_mut panics. Defer: APPLIED is not latched, so
    // the pass re-runs next tick; the in_world edge re-arm covers the restream case.
    let Some(magic_rows) = crate::param_guard::rows_mut::<Magic>(repo, "spell_slot_length") else {
        return;
    };
    let mut rows = 0u32;
    let mut normalised = 0u32;
    for (_id, row) in magic_rows {
        rows += 1;
        if row.slot_length() != ONE_SLOT {
            row.set_slot_length(ONE_SLOT);
            normalised += 1;
        }
    }
    if rows == 0 {
        return; // param file not populated yet -- retry next tick
    }
    APPLIED.store(true, Ordering::Relaxed);
    let first_apply = !APPLIED_ONCE.swap(true, Ordering::Relaxed);
    let expected = expected_multi_slot_rows();
    if normalised == expected {
        log::info!("spell_slot_length: normalised {normalised} of {rows} Magic rows to one slot");
    } else if first_apply {
        // Not an error. A stacked data mod (e.g. a host randomizer's regulation.bin) legitimately
        // changes this count, and this is the only place we would ever see that. The expectation is
        // per-build (see the module header): saying so keeps the reader from re-deriving whether
        // the number or the build is the surprise.
        log::info!(
            "spell_slot_length: normalised {normalised} of {rows} Magic rows to one slot \
             (expected {expected} on a vanilla table for this build [{}] -- param table may be \
             modded)",
            crate::game_version_gate::measured_clause()
        );
    } else if normalised == 0 {
        // Re-arm re-pass and the table still holds our write: the load did NOT re-stream Magic.
        // Expected, and NOT the modded-table anomaly -- see APPLIED_ONCE.
        log::info!(
            "spell_slot_length: re-arm pass found 0 multi-slot rows of {rows} -- the load did \
             not revert Magic (normalisation intact)"
        );
    } else {
        // Re-arm re-pass that found rows to write: the load reverted the table and we re-applied.
        log::info!(
            "spell_slot_length: re-normalised {normalised} of {rows} Magic rows after the load \
             reverted the table"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect: one hardcoded 25 accused every 2.7.0.0 player of running a modded table, because
    /// vanilla Tarnished Edition normalises 105 of 317 rows. The counts must differ per build.
    #[test]
    fn each_supported_build_carries_its_own_measured_count() {
        assert_eq!(expected_for(Some(Supported::Ww262)), 25);
        assert_eq!(expected_for(Some(Supported::Ww270)), 105);
        assert_ne!(
            expected_for(Some(Supported::Ww262)),
            expected_for(Some(Supported::Ww270))
        );
    }

    /// Undetected / JP fall back to the VERIFIED column, exactly as `rva_table::current` does --
    /// and each JP build falls back to the Worldwide column of ITS OWN generation, not to a
    /// single baked one. The JP counts are UNMEASURED; these numbers are logged, never asserted
    /// against a live install.
    #[test]
    fn an_unmeasured_build_falls_back_to_the_verified_count() {
        assert_eq!(expected_for(None), VANILLA_MULTI_SLOT_ROWS_WW262);
        assert_eq!(
            expected_for(Some(Supported::Jp2621)),
            VANILLA_MULTI_SLOT_ROWS_WW262
        );
        assert_eq!(
            expected_for(Some(Supported::Jp2701)),
            VANILLA_MULTI_SLOT_ROWS_WW270
        );
    }
}
