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
//! ## The count is an oracle
//!
//! On a vanilla `Magic` table exactly **25** rows have `slotLength > 1`: the 24 memorisable spells
//! above, plus one unnamed non-memorisable row (`2050001`). The log line reports what it actually
//! normalised, so any other number means the param table is not vanilla -- which on a stacked
//! install (matt's randomizer ships its own `regulation.bin`) is worth knowing and is otherwise
//! invisible to us.
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

/// What every spell is normalised to. One slot, so the planner's modulus is exact.
const ONE_SLOT: u8 = 1;

/// Rows expected to need normalising on a vanilla table. Logged, never asserted -- a mismatch is
/// information about the install, not a failure.
const VANILLA_MULTI_SLOT_ROWS: u32 = 25;

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
    if normalised == VANILLA_MULTI_SLOT_ROWS {
        log::info!("spell_slot_length: normalised {normalised} of {rows} Magic rows to one slot");
    } else if first_apply {
        // Not an error. A stacked data mod (e.g. a host randomizer's regulation.bin) legitimately
        // changes this count, and this is the only place we would ever see that.
        log::info!(
            "spell_slot_length: normalised {normalised} of {rows} Magic rows to one slot \
             (expected {VANILLA_MULTI_SLOT_ROWS} on a vanilla table -- param table may be modded)"
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
