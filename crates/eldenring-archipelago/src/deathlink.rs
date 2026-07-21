//! DeathLink (Milestone B Stage 5).
//!
//! INCOMING: a foreign DeathLink event latches a kill. On a vanilla (pure-runtime) game there is
//! no baked reactor, so the tick does a direct HP-write death; KEEP-RUNES is preserved by
//! snapshotting `rune_count` before the kill, zeroing it so the (late) bloodstain bank drops
//! nothing, and writing the snapshot back on respawn (see `drive_kill`). Flag 76996 is still
//! set best-effort for bake-compat setups. The kill latch clears only on a successful kill,
//! so a kill latched on a menu/load screen retries until in-world.
//!
//! OUTGOING (RE-hole): detecting a LOCAL death needs the player HP / death-state cell, which the
//! standalone never resolved in the `eldenring` crate. `read_local_death` returns false for now, so
//! outgoing is inert; INCOMING is unaffected. Fill `read_local_death` (WorldChrMan.main_player → HP)
//! to enable sending, and add a post-incoming-kill grace window so our own baked kill doesn't echo.

use std::sync::atomic::{AtomicBool, Ordering};

use std::sync::Mutex;

use eldenring::cs::{GameDataMan, WorldChrMan};
use er_logic::deathlink::KeepRunes;
use fromsoftware_shared::FromStatic;

/// Flag the baked `common.emevd` reactor (event 6996) watches: set on an incoming DeathLink -> the
/// reactor does `ForceCharacterDeath(10000, true)` (keep-runes) and clears it.
const DEATHLINK_KILL_FLAG: u32 = 76996;

static ENABLED: AtomicBool = AtomicBool::new(false);
static KILL_PENDING: AtomicBool = AtomicBool::new(false);
static WAS_DEAD: AtomicBool = AtomicBool::new(false);

/// Keep-runes decision state for incoming DeathLink deaths (see `drive_kill`). Single-threaded
/// game-tick access; the `Mutex` just satisfies `static` mutability. Logic lives in er-logic.
static KEEP_RUNES: Mutex<KeepRunes> = Mutex::new(KeepRunes::new());

/// Set from slot_data `options.death_link` at connect.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    log::info!("DeathLink: {}", if on { "ENABLED" } else { "off" });
}
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// A foreign DeathLink arrived (caller already applied the self-source guard): latch a kill.
pub fn latch_incoming_kill() {
    KILL_PENDING.store(true, Ordering::Relaxed);
}

/// Per-tick driver for INCOMING DeathLink. Two legs:
///
/// * KEEP-RUNES RESTORE (ungated, runs first): if a previous kill zeroed the player's runes, write
///   the snapshot back once they are in-world and alive again. Ungated so a `death_link` that is
///   toggled off mid-death still pays back what we withheld.
/// * KILL: if a kill is pending and we're in-world, do the direct HP-write death
///   (`kill_local_player` pre-arms WAS_DEAD, suppressing the outgoing echo), zeroing held runes
///   first so the vanilla death banks an EMPTY bloodstain -- then arm the restore. The dedicated
///   flag is still set best-effort for bake-compat setups. Latch clears only on a successful kill,
///   so a kill latched on a menu/load screen retries.
pub fn drive_kill() {
    // --- KEEP-RUNES RESTORE leg (ungated: we may owe runes even if death_link was just disabled) ---
    if let Ok(mut keep) = KEEP_RUNES.lock()
        && let Some(runes) = keep.poll_restore(crate::flags::in_world(), read_local_hp())
    {
        write_rune_count(runes);
        log::info!("DeathLink: keep-runes restored {runes} runes after respawn");
    }

    // R2 (SWEEP H2): belt-and-braces -- a stale latched kill must never fire once death_link is
    // known-disabled for this slot (the event handler gates too, but the latch can outlive it).
    if !is_enabled() {
        return;
    }
    // PURE-RUNTIME (2026-07-01): no baked common.emevd reactor exists on a vanilla game, so the
    // kill is a direct HP write. KEEP-RUNES (2026-07-20): the reactor's ForceCharacterDeath(_, true)
    // used to hold runes; we replicate it by snapshotting rune_count BEFORE the kill and zeroing it
    // right after, so the (late -- observed "way after YOU DIED") bloodstain bank sees 0 and drops
    // nothing. The restore leg above pays the snapshot back on respawn.
    if KILL_PENDING.load(Ordering::Relaxed) && crate::flags::in_world() {
        let snapshot = read_rune_count(); // BEFORE the kill; None if GameDataMan is down -> vanilla drop
        if kill_local_player() {
            if let Some(runes) = snapshot {
                write_rune_count(0);
                if let Ok(mut keep) = KEEP_RUNES.lock() {
                    keep.arm(Some(runes));
                }
            }
            let _ = crate::flags::try_set_event_flag(DEATHLINK_KILL_FLAG, true);
            KILL_PENDING.store(false, Ordering::Relaxed);
            match snapshot {
                Some(runes) => log::info!(
                    "DeathLink: incoming kill applied (keep-runes: {runes} runes withheld; flag {DEATHLINK_KILL_FLAG} best-effort)"
                ),
                None => log::warn!(
                    "DeathLink: incoming kill applied but GameDataMan was down -- runes NOT kept (vanilla drop); flag {DEATHLINK_KILL_FLAG} best-effort"
                ),
            }
        }
    }
}

/// Read the local player's held rune count (`GameDataMan -> main_player_game_data -> rune_count`),
/// or None before the player game data is up. Same singleton idiom as `inventory`/`upgrades`.
fn read_rune_count() -> Option<u32> {
    let gdm = unsafe { GameDataMan::instance() }.ok()?;
    Some(gdm.main_player_game_data.as_ref().rune_count)
}

/// Write the local player's held rune count. No-op before the player game data is up.
fn write_rune_count(value: u32) {
    if let Ok(gdm) = unsafe { GameDataMan::instance_mut() } {
        gdm.main_player_game_data.as_mut().rune_count = value;
    }
}

/// Rising-edge local-death detector for OUTGOING DeathLink — true exactly once per death.
/// Inert until `read_local_death` is filled (see module doc).
pub fn poll_local_death() -> bool {
    let dead = read_local_death();
    let was = WAS_DEAD.swap(dead, Ordering::Relaxed);
    dead && !was
}

/// Typed access to the local player's current HP (fromsoftware-rs `eldenring` crate):
/// `WorldChrMan.main_player -> PlayerIns.chr_ins.modules.data.hp` -- all public fields, same shape
/// DS3 uses (`super_chr_ins.modules.data.hp`). REPLACES the pinned raw-offset chain
/// (main_player +0x10 -> [+0x190] -> [0] -> +0x138), which never resolved live: the 2026-07-01
/// playtest log shows ARMED absent across every session on the exact pinned exe version, which
/// silently disabled outgoing deathlink, incoming kills, AND the region kick (all three shared it).
pub(crate) fn read_local_hp() -> Option<i32> {
    let wcm = unsafe { WorldChrMan::instance() }.ok()?;
    let player = wcm.main_player.as_ref()?;
    Some(player.chr_ins.modules.data.hp)
}

static HP_ARMED_LOGGED: AtomicBool = AtomicBool::new(false);

fn read_local_death() -> bool {
    let Some(hp) = read_local_hp() else {
        return false; // not in-world (no main player)
    };
    if !HP_ARMED_LOGGED.swap(true, Ordering::Relaxed) {
        log::info!(
            "DeathLink: HP read via typed CSChrDataModule -- outgoing death detection ARMED"
        );
    }
    hp <= 0
}

/// LIVE since 2026-07-01 (pure-runtime): drive_kill + region::tick_kick call this directly (the
/// baked `common.emevd` reactors on flags 76970/76996 are gone on a vanilla game). Writes current
/// HP -> 0 through the typed module. NOTE: no keep-runes here (the reactor's
/// `ForceCharacterDeath(10000, true)` provided that) -- pair with a "Should Receive Runes" write or
/// a souls snapshot/restore when that lands.
pub fn kill_local_player() -> bool {
    let Ok(wcm) = (unsafe { WorldChrMan::instance_mut() }) else {
        return false;
    };
    let Some(player) = wcm.main_player.as_mut() else {
        return false;
    };
    let data = &mut player.chr_ins.modules.data;
    if data.hp <= 0 {
        return false; // already dead/dying -> don't re-kill
    }
    data.hp = 0;
    WAS_DEAD.store(true, Ordering::Relaxed); // suppress our own kill echoing out as a local death
    true
}
