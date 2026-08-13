//! Reports whether a RUNTIME icon splice is reachable (er-archipelago#602 and the release-bundle
//! provenance problem). The pure half is [`er_logic::icon_seam`]; read its module doc first.
//!
//! # What this asks, and what it deliberately does not do
//!
//! It **reports**. It does not hook, allocate, or write a byte into a texture. The seam is not
//! understood yet, and the first thing to establish about an unknown seam is what is actually on
//! the other side of it.
//!
//! Three questions, in descending order of how much they change the plan:
//!
//! 1. **Is the game's own Oodle a loadable module?** The atlas is DCX/KRAK. We may not ship an
//!    Oodle, but the game has one. If it is a separate `oo2core_*.dll` we can find and call it, and
//!    decompression stops being a blocker.
//! 2. **Is the splice geometry what the offline probe measured?** Printed so the arithmetic is in
//!    the log next to everything else, rather than living only in a test.
//! 3. **How deep do the mips stay block-aligned?** The answer is 0, and the consequence -- whether
//!    the UI ever samples below mip 0 for a 160px icon -- is the thing to watch for in play.
//!
//! 🛑 THE RESOURCE SEAM IS NOT REACHABLE FROM A SINGLETON, AND THAT IS THE FINDING. `fromsoftware-rs`
//! binds `FD4ResCap` / `FD4ResCapHolder` / `FD4ResRep` / `DLFileDeviceManager`, but the Elden Ring
//! crate exposes 32 singletons and none of them is a file device, resource repository or texture
//! manager. So there is a type for the thing and no entry point to it. Reaching one means a pointer
//! chase or an AOB scan; this probe exists to decide whether that is worth starting.
//!
//! ⭐ THE CHEAPEST TWO EXPERIMENTS ARE NOT IN HERE, because they are not code. Recompress
//! `01_common.tpf.dcx` as non-KRAK DCX and see if the loader still takes it; and build a sheet whose
//! flower is in mip 0 only and see if it renders right at every UI scale. Either answer redirects
//! the whole design, and WitchyBND does both in about twenty minutes.

use std::sync::atomic::{AtomicBool, Ordering};

use er_logic::icon_seam::{Oodle, Sprite, find_oodle};

/// **ON by default**, like `boss_fight` and `trap_feel`. Silenced with `ER_ICON_SEAM_PROBE=0` or
/// `"probes": {"icon_seam": false}`.
///
/// The default-on argument is the one the other two made and it is stronger here: this fires ONCE
/// per session, reads a module list and some integers, touches no game state, and answers a
/// question that is currently blocking a private repo and a hard packaging gate. A probe that
/// silently no-ops because a variable did not make the journey is worse than no probe.
fn enabled() -> bool {
    shared::probes::enabled_by_default("ER_ICON_SEAM_PROBE", "icon_seam")
}

/// 🛑 A default-on probe cannot ride `probes::log_active` -- that line resolves through the
/// default-OFF rule and would report this as off in exactly the case where it is on.
static SAID: AtomicBool = AtomicBool::new(false);

/// Run once. Safe to call from any tick; every call after the first returns immediately.
pub fn run_once() {
    if SAID.swap(true, Ordering::Relaxed) {
        return;
    }
    if !enabled() {
        log::info!("icon-seam probe: SILENCED by ER_ICON_SEAM_PROBE=0 / probes.icon_seam=false");
        return;
    }

    log::info!(
        "icon-seam probe: ON (default). One-shot, read-only: reports whether a runtime AP-icon \
         splice is reachable. Set ER_ICON_SEAM_PROBE=0, or \"probes\": {{\"icon_seam\": false}} in \
         apconfig.json, to silence it"
    );

    // 1. Oodle.
    match shared::utils::loaded_modules() {
        Ok(paths) => {
            let names: Vec<String> = paths
                .iter()
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect();
            match find_oodle(&names) {
                Oodle::Module(name) => log::info!(
                    "icon-seam: Oodle IS a loaded module ({name}) -- the game's own KRAK \
                     decompressor is in this process and callable, so DECOMPRESSION is not the \
                     blocker. Recompression still is; the cheaper question is whether the loader \
                     accepts a non-KRAK DCX at all."
                ),
                // 🛑 "not a module" is not "not present" -- it may be statically linked into the
                // exe, which a module list cannot see. Absence in a log is a prompt, not a proof.
                Oodle::NotAModule => log::info!(
                    "icon-seam: no oo2core_*.dll among {} loaded modules. That does NOT mean the \
                     game has no Oodle -- it may be statically linked into the exe, which this \
                     cannot see. It means we cannot get at it by module handle.",
                    names.len()
                ),
            }
        }
        Err(e) => log::info!("icon-seam: could not enumerate loaded modules ({e})"),
    }

    // 2 and 3. The geometry, and where it stops.
    let s = Sprite::shipped();
    match s.splice() {
        Some(sp) => log::info!(
            "icon-seam: mip0 splice = offset {}, {} rows x {} bytes, stride {} -> {} bytes of OUR \
             art. The atlas we currently ship to deliver it is {} bytes.",
            sp.offset,
            sp.rows,
            sp.row_bytes,
            sp.row_stride,
            sp.payload_bytes(),
            (er_logic::icon_seam::ATLAS_W as usize / 4)
                * (er_logic::icon_seam::ATLAS_H as usize / 4)
                * er_logic::icon_seam::BC_BLOCK_BYTES_DEFAULT,
        ),
        None => log::info!("icon-seam: the shipped rect is NOT block-aligned -- check the layout"),
    }
    log::info!(
        "icon-seam: deepest block-aligned mip = {} (mip1 starts at {},{} and 1066 % 4 = 2). If the \
         UI ever samples below mip 0 for these icons, a mip-0-only splice will look wrong at some \
         scale -- SAY SO if the flower is correct in one menu and a telescope in another.",
        s.deepest_aligned_mip(12),
        s.at_mip(1).x,
        s.at_mip(1).y,
    );
}
