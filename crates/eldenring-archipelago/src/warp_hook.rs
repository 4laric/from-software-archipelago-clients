//! LuaWarp probe hook — capture the pending fast-travel TARGET at the moment of warp.
//!
//! THE SEAM (SPEC-capital-reconciler.md): `region::capital_pending_warp_target()` has no crate
//! API for the engine's queued MENU fast-travel destination, so a map fast-travel only gets its
//! 9116 correction one tick AFTER the load (the per-tick latch), not before. This hook fills the
//! seam push-style instead: detour the game's own `LuaWarp` entry — the function
//! `warp::warp_to_grace` already calls, RVA + prologue signature pinned there — and hand EVERY
//! warp's target to `region::capital_warp_intercept` before the load resolves.
//!
//! PROBE FIRST (unverified assumption, 2026-07-14): we believe the game's own map fast-travel
//! routes through this same `LuaWarp` (it is the "Fast Travel and Warp" mechanic the CE table
//! ports), but that is exactly what this hook exists to CONFIRM. Every call logs a
//! `LuaWarp hook:` line. A menu fast-travel that produces that line — with NO adjacent
//! `warp: requested grace warp` line (that one only comes from `warp_to_grace`) — proves menu
//! warps route through LuaWarp and the seam is filled. A menu fast-travel that produces NO
//! `LuaWarp hook:` line means menu warps take another path, and we pivot back to a poll-style
//! seam (see the NEEDS CRATE API note on `capital_pending_warp_target`).
//!
//! Install: from `core::update_live` (game thread, same timing as the AddItemFunc detour in
//! detour.rs, whose structure this module copies). Degrade, don't crash: a prologue-signature
//! mismatch (stale 2.6.2.0 RVA for the running build) refuses the hook with one log line and
//! the reconciler keeps its latch-after-load behaviour.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use retour::GenericDetour;

use crate::warp::{self, GRACE_TO_WARP_ARG_DELTA, LUA_WARP_FUNC_RVA, LuaWarpFn};

static HOOK: OnceLock<GenericDetour<LuaWarpFn>> = OnceLock::new();
/// One-shot attempt latch: `install` runs its body exactly once per session. Every failure mode
/// (no module base, signature mismatch, retour error) is permanent for the running build, so
/// unlike the AddItemFunc detour there is nothing to gain by retrying — and a per-tick retry
/// would spam the refusal log line.
static ATTEMPTED: AtomicBool = AtomicBool::new(false);

/// Whether the LuaWarp entry has been patched. `warp::warp_fn` consults this to skip its raw
/// prologue-byte check, which would false-negative on retour's patched entry and break every
/// client-initiated warp (kick, random start, `!warp`).
pub(crate) fn installed() -> bool {
    HOOK.get().is_some()
}

/// Install the LuaWarp detour. Call from `core::update_live` (game thread — the same thread
/// that calls LuaWarp, so no warp can race the enable/set window below). Self-guarded one-shot;
/// degrades with a log line instead of returning an error (the reconciler's per-tick latch is
/// the fallback), so the caller needs no install latch.
pub fn install() {
    if ATTEMPTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let Some(base) = warp::current_module_base() else {
        log::warn!("LuaWarp hook NOT installed: no module base for eldenring.exe");
        return;
    };
    // Same guard warp_to_grace uses: refuse on a prologue mismatch (pinned 2.6.2.0 RVA stale
    // for this build). warp_fn returns the verified, transmuted entry on match. Its
    // installed()-bypass cannot fire here: HOOK is only set further down this function.
    let Some(target) = warp::warp_fn(base) else {
        log::warn!(
            "LuaWarp hook NOT installed: signature mismatch @ {:#x} (pinned 2.6.2.0 RVA stale \
             for this build) — menu warps fall back to the per-tick capital latch",
            base + LUA_WARP_FUNC_RVA
        );
        return;
    };
    // SAFETY: target address signature-verified above; lua_warp_detour matches LuaWarpFn's
    // calling convention exactly.
    let hook = match unsafe { GenericDetour::<LuaWarpFn>::new(target, lua_warp_detour) } {
        Ok(h) => h,
        Err(e) => {
            log::warn!("LuaWarp hook NOT installed: retour error: {e}");
            return;
        }
    };
    // SAFETY: patching a verified, executable entry inside the loaded image.
    if let Err(e) = unsafe { hook.enable() } {
        log::warn!("LuaWarp hook NOT installed: enable failed: {e}");
        return;
    }
    let _ = HOOK.set(hook);
    log::info!(
        "LuaWarp probe hook installed @ {:#x} — every warp (menu or client) will log its target",
        base + LUA_WARP_FUNC_RVA
    );
}

/// The detour body. Runs INSIDE the game's warp call, on the game thread. Must never panic
/// across the FFI boundary: the id arithmetic is wrapping and the intercept is caught.
unsafe extern "C" fn lua_warp_detour(rcx: *mut c_void, rdx: *mut c_void, warp_arg: u32) -> u64 {
    // Reconstruct the grace ENTITY id (the space capital_warp_intercept speaks): the CE call
    // shape passes `entity_id - 1000` in r8d (see warp.rs module docs).
    let entity = warp_arg.wrapping_add(GRACE_TO_WARP_ARG_DELTA);
    // THE PROBE SIGNAL — one line per warp, menu- or client-initiated (see module docs for how
    // the adjacent warp_to_grace line distinguishes the two).
    log::info!("LuaWarp hook: warp arg {warp_arg} -> grace entity {entity} (menu or client)");
    // Original first (same order warp_to_grace uses: request the warp, then intercept). The
    // None arm is unreachable in practice — install() runs on the game thread, the same thread
    // that calls LuaWarp, so no call can land between enable() and HOOK.set() — but if it ever
    // fired we must NOT silently swallow a warp, so it logs.
    let ret = match HOOK.get() {
        // SAFETY: trampoline to the original, same args, same convention.
        Some(h) => unsafe { h.call(rcx, rdx, warp_arg) },
        None => {
            log::error!("LuaWarp hook: trampoline missing (enable/set race?) — warp swallowed");
            0
        }
    };
    // Capital intercept AFTER the original returns; the warp is asynchronous, so this still
    // lands before the load screen. Client-initiated warps ALSO intercept in warp_to_grace —
    // the double call is harmless (reconcile_write only writes on mismatch; note there).
    // catch_unwind: a poisoned CAPITAL mutex would panic in .lock().unwrap(); inside the game's
    // own call frame that must degrade to a logged miss, not an unwind across FFI.
    if std::panic::catch_unwind(|| crate::region::capital_warp_intercept(entity)).is_err() {
        log::error!("LuaWarp hook: capital_warp_intercept panicked; suppressed (warp unaffected)");
    }
    ret
}
