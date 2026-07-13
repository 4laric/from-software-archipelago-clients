//! scaling_probe.rs — READ-ONLY diagnostic for the runtime enemy-scaling RE (see
//! `SPEC-runtime-enemy-scaling.md`). NO writes; latches after one dump; wire from `update_live`
//! behind in-world.
//!
//! v2 (crash-proof): the v1 probe dereferenced `base+0x20` (= 0x1_0000_0001, inline packed data, NOT
//! a pointer) because the plausibility bound was too loose, and faulted. So v2 reads the `main_player`
//! ChrIns header **inline only** (every read stays inside the mapped ChrIns object — no dereferences
//! of guessed offsets) and merely FLAGS pointer-looking words as candidate sub-modules. From the log
//! we pick the real module that holds the SpEffect list (CE: a sub-module `+0x598` = `SpEffectCount`),
//! and the next probe walks that specific pointer.

use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::WorldChrMan;
use fromsoftware_shared::FromStatic;

static DONE: AtomicBool = AtomicBool::new(false);

/// Real ER user-heap pointers sit ~`0x1xx_xxxx_xxxx` (the player ChrIns was `0x1fcde7ae588`). Reject
/// small packed values like `0x1_0000_0001` (which crashed v1 when dereferenced). Lower bound `16^10`.
fn is_ptr(a: usize) -> bool {
    (0x100_0000_0000..0x7FFF_FFFF_FFFF).contains(&a)
}

/// A code address (exe/dll `.text`) — ER modules are ~`0x7ff7..0x7fff`. A real object's first qword is
/// its vtable (a code ptr); we use that to tell real modules from small/non-module heap pointers.
fn is_code(a: usize) -> bool {
    (0x7FF0_0000_0000..0x8000_0000_0000).contains(&a)
}

/// Read-only probe. Dumps the ChrIns header inline (no derefs) + flags candidate sub-module pointers.
pub fn probe() -> bool {
    if DONE.load(Ordering::Relaxed) {
        return true;
    }
    let Ok(wcm) = (unsafe { WorldChrMan::instance() }) else {
        return false;
    };
    let Some(player) = wcm.main_player.as_ref() else {
        return false;
    };
    let base = player as *const _ as usize;
    if !is_ptr(base) {
        log::info!("[scaling-probe] main_player base {base:#x} not a plausible ptr; skipping");
        DONE.store(true, Ordering::Relaxed);
        return true;
    }
    log::info!("[scaling-probe] === main_player ChrIns base = {base:#x} ===");

    // Phase 1 — inline header sweep (first 0x140 bytes, safe, no deref): collect sub-module pointers.
    let mut modules: Vec<(usize, usize)> = Vec::new();
    for i in 0..0x28usize {
        let off = i * 8;
        // SAFETY: base is a live ChrIns ptr; base+off (off < 0x140) is inside the object. No deref.
        let word = unsafe { ((base + off) as *const usize).read_unaligned() };
        if is_ptr(word) {
            log::info!("[scaling-probe]   +{off:#05x} = {word:#018x}  <- candidate module ptr");
            if off != 0 {
                modules.push((off, word));
            }
        } else {
            log::info!("[scaling-probe]   +{off:#05x} = {word:#018x}");
        }
    }

    // Phase 2 — AGGRESSIVE (crash OK for probing): deref each candidate module and dump the CE count
    // region (`+0x580..+0x5C0`). Per-word logged so a fault leaves a trail and prior modules survive.
    // A small count (0..~40) followed by ids (an enemy would show a 70xx) marks the SpEffect module.
    for &(off, ptr) in &modules {
        // A real game module's first qword is a vtable (code ptr). Skip anything else so we don't
        // fault on small/non-module heap pointers, and log the vtable so we can identify the module.
        let vtable = unsafe { (ptr as *const usize).read_unaligned() };
        if !is_code(vtable) {
            log::info!(
                "[scaling-probe] -- (base+{off:#x})={ptr:#018x}: vtable {vtable:#018x} not code -> skip"
            );
            continue;
        }
        let vt_rva = vtable - 0x7FF700000000; // rough exe-relative for cross-referencing the module type
        log::info!(
            "[scaling-probe] -- module (base+{off:#x})={ptr:#018x} vtable={vtable:#018x} (~rva {vt_rva:#x}): +0x580..+0x5C0 --"
        );
        for j in 0..0x10usize {
            let o = 0x580 + j * 4;
            let v = unsafe { ((ptr + o) as *const u32).read_unaligned() };
            log::info!("[scaling-probe]     +{o:#05x} = {v:#010x}");
        }
    }

    log::info!("[scaling-probe] === end ===");
    DONE.store(true, Ordering::Relaxed);
    true
}
