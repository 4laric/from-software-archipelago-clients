//! fast_travel.rs — anti-stuck override for the game's "no fast travel here" gate.
//!
//! Elden Ring decides whether the map's Site-of-Grace warp is usable by reading a single field on the
//! `FieldArea` singleton: `enable_fast_travel_event_flag` (i32 @ +0xA0). The engine treats it as an
//! event-flag id — "can travel" == `EventFlagMan.get(field)`. In the overworld the engine points it at
//! a persistent, already-set flag; inside legacy dungeons / catacombs / caves it repoints it at a flag
//! that stays OFF (or, in some areas, sets it to 0 = "unconditionally blocked"), which is how you end
//! up stranded with "Unable to travel." (Confirmed via the fromsoftware-rs `FieldArea` binding — the
//! field carries the RTTI comment "Flag to check if fast travel should be enabled." — and the Grand
//! Archives CE table's FieldArea base @ the `48 8B 3D ?? ?? ?? ?? 49 8B D8 …` static.)
//!
//! Override strategy — SELF-CALIBRATING FIELD OVERWRITE (zero quest-state mutation):
//!   * While travel is genuinely allowed (`field > 0` AND that flag is set), we cache the field value
//!     as the "known-good on-flag" — a flag the *game itself* is currently using and that is really on.
//!   * While travel is blocked, we OVERWRITE the field with that cached known-good on-flag, so the
//!     game's own check returns true. We never call SetEventFlag on a live gate (which could be a
//!     shared/quest flag) and we never invent a flag id — we only ever redirect the field to a value
//!     the engine already trusted this session. This also handles the `field == 0` block that setting
//!     a flag cannot.
//!   * Fallbacks when nothing is cached yet (e.g. a save that boots straight into a dungeon): if
//!     `field > 0` we set that flag on (best-effort); if `field == 0` we can't safely synthesize a
//!     flag, so we log once and leave it — `!warp 11102950` (Roundtable) is the guaranteed escape,
//!     since `LuaWarp` bypasses this gate entirely (see warp.rs).
//!
//! MUST run on the game thread (FrameBegin / update_live, in-world) — `instance_mut()` mutates a live
//! singleton. Kill switch: set `ER_NO_FASTTRAVEL_OVERRIDE=0` to disable (default on).

use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use eldenring::cs::FieldArea;
use fromsoftware_shared::FromStatic;

/// The last field value we saw that resolved to "travel allowed" — a flag id the game trusted and
/// that was genuinely set. `0` = none observed yet this session. This is the value we redirect the
/// field to whenever travel would otherwise be blocked.
static KNOWN_GOOD_FLAG: AtomicI32 = AtomicI32::new(0);
/// One-shot-per-distinct-value log throttle so we narrate each new blocking field id exactly once
/// (runtime visibility) without spamming every tick.
static LAST_LOGGED_BLOCK: AtomicI32 = AtomicI32::new(i32::MIN);
/// Light throttle — the gate only matters when the player opens the map, so a few writes a second is
/// ample and keeps this off the hot path.
static TICK: AtomicU32 = AtomicU32::new(0);
const THROTTLE: u32 = 5;

/// Kill switch. Default ON (this is a safety feature). `ER_NO_FASTTRAVEL_OVERRIDE=0` disables it.
fn enabled() -> bool {
    !matches!(
        std::env::var("ER_NO_FASTTRAVEL_OVERRIDE").ok().as_deref(),
        Some("0") | Some("false") | Some("off")
    )
}

/// Per-tick (call from `update_live`, in-world, game thread). Keeps the FieldArea fast-travel gate
/// open so the player can always warp out. No-op unless in-world and enabled.
pub fn tick() {
    if !enabled() {
        return;
    }
    if TICK.fetch_add(1, Ordering::Relaxed) % THROTTLE != 0 {
        return;
    }
    if !crate::flags::in_world() {
        return;
    }

    // Read the current gate field.
    let field = match unsafe { FieldArea::instance() } {
        Ok(fa) => fa.enable_fast_travel_event_flag,
        Err(_) => return, // FieldArea not placed yet (load screen) — retry next tick.
    };

    // THE DECISION is er_logic::fast_travel::gate_action (host-tested; see fast_travel_replay.rs).
    //
    // 2026-07-11, Gael Tunnel: the old code, with nothing cached, did
    //     crate::flags::set_event_flag(field as u32, true)
    // -- it SET whatever flag the field named. But `enable_fast_travel_event_flag` is the flag VANILLA
    // uses to gate warping out of an area, and in a boss dungeon THAT IS THE BOSS'S DEFEAT FLAG (you
    // cannot warp out until the boss is dead). So walking into Gael Tunnel set 32070800, the Magma
    // Wyrm's defeat flag: the game ran `if (EventFlag(32070800)) DisableCharacter(32070800)` and the
    // boss never spawned -- and then our own sweep watcher saw the flag flip and paid out that boss's
    // 6-check sweep. The client killed the boss to unblock its own travel, then rewarded itself.
    //
    // NEVER SET A FLAG WHOSE MEANING THE GAME OWNS. There is no "set" branch any more: allow, redirect
    // the field at a flag we have OBSERVED to be on (zero side effects), or wait -- vanilla blocks here
    // too, and waiting is correct.
    let field_on = field > 0 && crate::flags::get_event_flag(field as u32);
    let known_good = KNOWN_GOOD_FLAG.load(Ordering::Relaxed);
    let log_this = LAST_LOGGED_BLOCK.swap(field, Ordering::Relaxed) != field;

    match er_logic::fast_travel::gate_action(field, field_on, known_good as u32) {
        er_logic::fast_travel::GateAction::AllowAndCache(f) => {
            if KNOWN_GOOD_FLAG.swap(f as i32, Ordering::Relaxed) != f as i32 {
                log::info!("fast-travel: gate open (field flag {f}) — cached as known-good");
            }
            LAST_LOGGED_BLOCK.store(i32::MIN, Ordering::Relaxed);
        }
        er_logic::fast_travel::GateAction::RedirectField(f) => {
            if let Ok(fa) = unsafe { FieldArea::instance_mut() } {
                fa.enable_fast_travel_event_flag = f as i32;
                if log_this {
                    log::info!(
                        "fast-travel: gate was blocked (field {field}); redirected to known-good flag {f}"
                    );
                }
            }
        }
        er_logic::fast_travel::GateAction::Wait => {
            if log_this {
                log::info!(
                    "fast-travel: gate blocked (field {field}) and no known-good flag observed yet -- WAITING. \
                     We will not set the field's flag: in a boss dungeon that flag is the boss's defeat \
                     flag. Travel opens as soon as you reach anywhere it is legitimately allowed."
                );
            }
        }
    }
}

/// Seed the known-good flag from the start graces the client itself sets at spawn -- they are really
/// on, and pointing the gate field at one is inert. This removes the only case the old destructive
/// fallback existed for (booting straight into a dungeon with nothing cached).
pub fn prime_known_good(start_graces: &[u32]) {
    if KNOWN_GOOD_FLAG.load(Ordering::Relaxed) > 0 {
        return;
    }
    if let Some(f) = er_logic::fast_travel::prime_known_good(start_graces) {
        KNOWN_GOOD_FLAG.store(f as i32, Ordering::Relaxed);
        log::info!("fast-travel: primed known-good flag {f} from startGraces (no flag is ever SET)");
    }
}
