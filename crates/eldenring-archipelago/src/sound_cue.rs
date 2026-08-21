//! The multiworld-pickup audio cue (client#336) — the PLATFORM half only. WHEN a cue fires (new
//! checks only, one per sweep, cooldown-silenced bursts never replaying stale) is decided in
//! `er_logic::collect_cue`, host-tested. This module is the one OS call that produces a sound.
//!
//! WHY A WINDOWS SYSTEM SOUND, not an Elden Ring sound event: the issue prefers an in-game SE so
//! the cue rides the game's own volume settings, but the pinned `fromsoftware-rs` (rev 8c67a84)
//! exposes no audio manager — `cs/sfx.rs`/`world_sfx_man.rs` are VISUAL effects, and the only
//! sound-shaped fields anywhere in the crate are SE-id slots on param rows. Reaching the game's
//! WWise playback means a native function RVA we do not have pinned and must not guess (the same
//! rule that gates every other RVA in this crate). A system alias needs no shipped asset and no
//! unverified address. The trade is real and stated: the cue plays through the Windows mixer at
//! system volume, IGNORING the game's audio sliders. When a sound-play RVA is verified, this
//! module is the single seam to re-point — nothing else changes.
//!
//! `SystemAsterisk` because it is the short, non-alarming info chime; the choice is one constant.

use windows::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_NODEFAULT};
use windows::core::w;

/// Fire-and-forget: SND_ASYNC returns immediately (safe on the game thread), SND_ALIAS reads the
/// system sound (no asset shipped), SND_NODEFAULT keeps a missing alias SILENT rather than
/// substituting the default beep (a sound we did not choose is worse than none).
pub fn play() {
    // SAFETY: no module handle and no buffer — an alias lookup is a pure read of the registry's
    // sound scheme, and ASYNC means nothing here is borrowed by the playback.
    unsafe {
        // A FALSE return means the alias didn't resolve (no sound scheme entry); with
        // SND_NODEFAULT that is simply silence, which the module doc argues is the safe failure.
        let _ = PlaySoundW(
            w!("SystemAsterisk"),
            None,
            SND_ALIAS | SND_ASYNC | SND_NODEFAULT,
        );
    }
}
