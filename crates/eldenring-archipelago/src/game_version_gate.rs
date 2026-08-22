//! Refuse to initialise on an executable this build has no RVAs for, and say so in words.
//!
//! # The seam
//!
//! This module is I/O only. It reads the running module's PE version resource, asks the SAME
//! question the `eldenring` crate asks, and maps the third-party error onto
//! [`er_logic::game_version::Rejection`]. Every word the player reads is decided in `er-logic`,
//! where it is host-tested; nothing here chooses wording, and nothing here decides policy.
//!
//! # Why we re-implement the detect instead of calling `rva::get()`
//!
//! `eldenring::rva::get()` **panics** on an unsupported executable -- its own doc says so, and
//! `esd_probe` has been wrapping it in `catch_unwind` for exactly that reason. A panic is not a
//! control-flow tool: it costs us the error VALUE, so all we could recover is a formatted string to
//! re-parse. `fromsoftware_shared::game_version::GameVersion` is a public trait whose `detect()`
//! returns `Result<Self, DetectError>`, so implementing it over the same two arms asks the same
//! question and hands back the typed reason.
//!
//! 🛑 Keep the two arms below in lockstep with the `eldenring` crate's `rva.rs`. If they drift, this
//! gate passes an executable the RVA table will then panic on -- which is worse than no gate,
//! because the player is told it is fine first.

use er_logic::game_version::{self, Rejection};
use fromsoftware_shared::game_version::{DetectError, GameVersion, LANG_ID_EN, LANG_ID_JP};
use pelite::pe64::PeView;
use windows::Win32::System::LibraryLoader::GetModuleHandleA;
use windows::Win32::UI::WindowsAndMessaging::MessageBoxW;
use windows::core::{HSTRING, PCSTR};

/// The executables this build's RVA table covers. Mirrors `eldenring::rva::ERGameVersion`, which is
/// private, so it cannot be reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Supported {
    Ww262,
    Jp2621,
}

impl GameVersion for Supported {
    const NAME: &'static str = "elden ring";

    fn from_lang_version(lang_id: u16, version: &str) -> Option<Self> {
        if lang_id == LANG_ID_EN && version == game_version::REQUIRED_WW {
            Some(Self::Ww262)
        } else if lang_id == LANG_ID_JP && version == game_version::REQUIRED_JP {
            Some(Self::Jp2621)
        } else {
            None
        }
    }
}

/// `Ok(())` if the running executable is one we have RVAs for; `Err(message)` carries the finished,
/// player-facing text.
///
/// Never panics. The trait's `detect()` reaches `unwrap()` on the PE resource directory, which a
/// stripped or packed executable can trip, so the read runs under `catch_unwind` -- a malformed PE
/// is a rejection, not a crash.
pub fn check() -> Result<(), String> {
    let detected = std::panic::catch_unwind(|| {
        // Safety: the null module name asks for the running executable's own base, which is mapped
        // for the life of the process. `PeView::module` only reads that mapping.
        let base = unsafe { GetModuleHandleA(PCSTR(std::ptr::null())) }
            .map_err(|_| Rejection::Metadata { missing: "module" })?
            .0 as *const u8;
        let module = unsafe { PeView::module(base) };
        Supported::detect(&module).map_err(rejection_of)
    });

    match detected {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(rejection)) => Err(game_version::explain(&rejection)),
        Err(_) => Err(game_version::explain(&Rejection::Metadata {
            missing: "readable version resource",
        })),
    }
}

/// Translate the third-party reason into ours. Deliberately exhaustive: if upstream adds a variant,
/// this fails to compile, which is the outcome we want -- a wildcard arm would silently route a new
/// and unknown failure into a message that does not describe it.
fn rejection_of(error: DetectError) -> Rejection {
    match error {
        DetectError::UnsupportedVersion(detected) => Rejection::Version { detected },
        DetectError::UnsupportedLanguage(lang_id) => Rejection::Language { lang_id },
        DetectError::WrongProduct { actual, .. } => Rejection::Product { actual },
        DetectError::MissingVersionMetadata => Rejection::Metadata { missing: "version" },
        DetectError::MissingLanguageMetadata => Rejection::Metadata {
            missing: "language",
        },
        DetectError::MissingProductName => Rejection::Metadata {
            missing: "product name",
        },
    }
}

/// What the running executable's PE version resource says, rendered for log lines. Same read as
/// [`check`], same `catch_unwind` armour, but NO refusal: the answer is data, not a verdict.
/// clients#371: a sig-mismatch warn that cannot say which exe it measured forces a human to date
/// the failure by hand, so every SearchStringTable reject carries this clause.
pub fn measured_clause() -> String {
    let detected = std::panic::catch_unwind(|| {
        // Safety: identical to `check` -- the running executable's own base, mapped for the life
        // of the process; `PeView::module` only reads that mapping.
        let base = unsafe { GetModuleHandleA(PCSTR(std::ptr::null())) }
            .map_err(|_| Rejection::Metadata { missing: "module" })?
            .0 as *const u8;
        let module = unsafe { PeView::module(base) };
        Supported::detect(&module).map_err(rejection_of)
    });
    let result = match detected {
        Ok(Ok(Supported::Ww262)) => Ok((game_version::REQUIRED_WW, LANG_ID_EN)),
        Ok(Ok(Supported::Jp2621)) => Ok((game_version::REQUIRED_JP, LANG_ID_JP)),
        Ok(Err(rejection)) => Err(rejection),
        Err(_) => Err(Rejection::Metadata {
            missing: "readable version resource",
        }),
    };
    game_version::measured_clause(&result)
}

/// Put the message in front of the player. `shared::handle_panics` owns the only other message box
/// in the client and its helper is private, so this is a deliberate second one rather than a
/// duplicate: this text is NOT a panic and must not be dressed as one.
pub fn show(message: &str) {
    // Safety: a modal message box on the loader thread, the same thing the panic hook already does.
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(message),
            &HSTRING::from("Elden Ring Archipelago"),
            Default::default(),
        );
    }
}
