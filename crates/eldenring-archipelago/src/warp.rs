//! Runtime grace-warp primitive — the pure-runtime replacement for the retired bake's
//! `WarpPlayer` reactor (region.rs random-start warp; also exposed as the `!warp` console
//! command). Ported from the community "Fast Travel and Warp" mechanic (Hexinton all-in-one
//! CE table, author Coinsworth), statically re-resolved against the pinned 2.6.2.0
//! `elden_ring_artifacts/eldenring.exe` on 2026-07-02:
//!
//! - `LuaWarp` fn RVA `0x599C10` — AOB `C3 ?? ?? ?? ?? ?? ?? 57 48 83 EC ?? 48 8B FA 44` + 2,
//!   UNIQUE match on 2.6.2.0. Call shape (from the CE script):
//!   `rcx = [CSLuaEventManager + 0x18]`, `rdx = [CSLuaEventManager + 0x08]`,
//!   `r8d = grace_entity_id - 1000` (e.g. Table of Lost Grace 11102950 -> 11101950).
//! - `CSLuaEventManager` static — AOB `48 8B 05 ?? ?? ?? ?? 48 85 C0 74 ?? 41 BE 01 00 00 00
//!   44 89 75` resolves TWO candidates on 2.6.2.0: `0x3D67E48` (the CE scan's first match, in
//!   the same CS-singleton block as the proven inventory ptr slot `0x3D67A50`) and `0x3D5AFE0`.
//!   Both are probed at call time (non-null static whose `+0x08`/`+0x18` members are non-null);
//!   the first live candidate wins and a one-time log line records WHICH armed — the same
//!   dual-candidate confirm pattern as detour.rs's inventory pointer.
//!
//! MUST run on the game thread (FrameBegin tick / update_live), same rule as `grant_full_id`.
//! The warp is asynchronous: the call requests the travel; the load screen follows.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

/// `LuaWarp` entry, 2.6.2.0. Re-resolve via the module AOB above on a game update.
pub(crate) const LUA_WARP_FUNC_RVA: usize = 0x0059_9C10;
/// First 16 bytes at the entry (standard prologue), read from the pinned exe. A mismatch means
/// the RVA is stale for the running build — refuse to call.
const LUA_WARP_FUNC_SIG: &[u8] = &[
    0x48, 0x89, 0x5C, 0x24, 0x10, 0x57, 0x48, 0x83, 0xEC, 0x20, 0x48, 0x8B, 0xFA, 0x44, 0x89, 0x41,
];
/// CSLuaEventManager static-slot candidates (see module docs). Probe order = CE scan order.
const CSLEM_CANDIDATE_RVAS: [usize; 2] = [0x03D6_7E48, 0x03D5_AFE0];

/// The CE dropdown ids are grace ENTITY ids; the warp arg is that id minus 1000.
pub(crate) const GRACE_TO_WARP_ARG_DELTA: u32 = 1000;

/// Windows-x64: rcx, rdx, r8 — matches the CE script's register setup. r8 is set from a 32-bit
/// value (`lea r8d, [eax-3E8]`), so a `u32` third arg is exactly right.
pub(crate) type LuaWarpFn = unsafe extern "C" fn(*mut c_void, *mut c_void, u32) -> u64;

/// One-time confirm log guard: 0 = unprobed, 1 = probed (result may still be per-call).
static PROBE_LOGGED: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn current_module_base() -> Option<usize> {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    let hmodule = unsafe { GetModuleHandleW(None) }.ok()?;
    Some(hmodule.0 as usize)
}

/// Resolve the live CSLuaEventManager instance by probing both candidate static slots.
/// Returns `(rcx, rdx)` for the call, i.e. `([inst+0x18], [inst+0x08])`.
fn resolve_lua_event_manager(base: usize) -> Option<(*mut c_void, *mut c_void)> {
    for (i, rva) in CSLEM_CANDIDATE_RVAS.iter().enumerate() {
        // SAFETY: pinned data RVA inside the loaded eldenring.exe image; reads pointer-sized
        // words and dereferences only after non-null checks.
        unsafe {
            let slot = (base + rva) as *const usize;
            let inst = slot.read();
            if inst < 0x10000 {
                continue;
            }
            let rdx = ((inst + 0x08) as *const usize).read();
            let rcx = ((inst + 0x18) as *const usize).read();
            if rdx < 0x10000 || rcx < 0x10000 {
                continue;
            }
            if PROBE_LOGGED.swap(1, Ordering::Relaxed) == 0 {
                log::info!(
                    "warp: CSLuaEventManager CONFIRM candidate {} (*(base+{rva:#x}) = {inst:#x})",
                    i + 1
                );
            }
            return Some((rcx as *mut c_void, rdx as *mut c_void));
        }
    }
    None
}

pub(crate) fn warp_fn(base: usize) -> Option<LuaWarpFn> {
    let addr = base + LUA_WARP_FUNC_RVA;
    // Once warp_hook's LuaWarp detour is installed, retour has PATCHED the prologue (a jmp to
    // the detour), so the raw byte check below would false-negative and break every client
    // warp. The hook only installs after verifying this same signature, so the address is
    // known-good; calling the patched entry still warps (detour -> trampoline -> original).
    if crate::warp_hook::installed() {
        // SAFETY: signature was verified by warp_hook::install before it patched the entry.
        return Some(unsafe { std::mem::transmute::<usize, LuaWarpFn>(addr) });
    }
    // SAFETY: reads LUA_WARP_FUNC_SIG.len() bytes inside the loaded image.
    let actual = unsafe { std::slice::from_raw_parts(addr as *const u8, LUA_WARP_FUNC_SIG.len()) };
    if actual != LUA_WARP_FUNC_SIG {
        return None;
    }
    // SAFETY: signature just verified at the pinned RVA.
    Some(unsafe { std::mem::transmute::<usize, LuaWarpFn>(addr) })
}

/// Request a grace warp to `grace_entity_id` (e.g. 11102950 = Table of Lost Grace / Roundtable
/// Hold). Game-thread only. Returns Err with a loggable reason instead of silently no-oping
/// (CONTRIBUTING "runtime visibility": every degrade says so).
pub fn warp_to_grace(grace_entity_id: u32) -> Result<(), &'static str> {
    if !crate::flags::in_world() {
        return Err("not in world (menu/load) -- warp needs a placed player");
    }
    let base = current_module_base().ok_or("no module base for eldenring.exe")?;
    let f = warp_fn(base)
        .ok_or("LuaWarp signature mismatch -- pinned 2.6.2.0 RVA stale for this build")?;
    let (rcx, rdx) = resolve_lua_event_manager(base)
        .ok_or("CSLuaEventManager not resolvable (both candidates dead)")?;
    let arg = grace_entity_id
        .checked_sub(GRACE_TO_WARP_ARG_DELTA)
        .ok_or("grace id underflow (expected a full grace entity id like 11102950)")?;
    // SAFETY: game thread (caller contract), args resolved + verified above.
    unsafe {
        f(rcx, rdx, arg);
    }
    log::info!("warp: requested grace warp to {grace_entity_id} (arg {arg})");
    // Capital-version intercept: handled by the LuaWarp hook (warp_hook.rs), which fires INSIDE
    // the `f(...)` call above (the patched entry) for EVERY warp -- menu fast-travel AND every
    // client-initiated warp (kick, random start, `!warp`). Confirmed in-game 2026-07-15: two menu
    // fast-travels logged `LuaWarp hook: warp arg ...` with no `warp: requested` companion, proving
    // menu travel routes through LuaWarp. So no explicit intercept is needed here -- it would only
    // double-fire (idempotent). If the hook fails to install (signature mismatch), `warp_fn` above
    // also refuses, so this line would never be reached anyway; there is nothing to fall back to.
    Ok(())
}
