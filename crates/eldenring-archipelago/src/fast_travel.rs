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

    // Is travel genuinely allowed right now? (field points at a set flag.)
    let allowed = field > 0 && crate::flags::get_event_flag(field as u32);
    if allowed {
        // Cache this as our trusted "on" flag and reset the block log latch.
        if KNOWN_GOOD_FLAG.swap(field, Ordering::Relaxed) != field {
            log::info!("fast-travel: gate open (field flag {field}) — cached as known-good");
        }
        LAST_LOGGED_BLOCK.store(i32::MIN, Ordering::Relaxed);
        return;
    }

    // Blocked. Prefer the zero-side-effect field overwrite to a known-good on-flag.
    let known_good = KNOWN_GOOD_FLAG.load(Ordering::Relaxed);
    let log_this = LAST_LOGGED_BLOCK.swap(field, Ordering::Relaxed) != field;

    if known_good > 0 {
        if let Ok(fa) = unsafe { FieldArea::instance_mut() } {
            fa.enable_fast_travel_event_flag = known_good;
            if log_this {
                log::info!(
                    "fast-travel: gate was blocked (field {field}); redirected to known-good flag {known_good}"
                );
            }
        }
        return;
    }

    // No known-good flag cached yet this session.
    if field > 0 {
        // Best effort: open the flag the field already names. (Only reached before we've ever seen an
        // allowed state — e.g. booting straight into a dungeon.)
        crate::flags::set_event_flag(field as u32, true);
        if log_this {
            log::info!("fast-travel: gate blocked, no cached flag yet — set field flag {field} on (fallback)");
        }
    } else if log_this {
        log::warn!(
            "fast-travel: gate blocked with field=0 and no known-good flag cached yet — cannot synthesize a flag; use `!warp 11102950` to escape"
        );
    }
}
