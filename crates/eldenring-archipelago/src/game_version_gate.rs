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
use std::sync::OnceLock;
use windows::Win32::System::LibraryLoader::GetModuleHandleA;
use windows::Win32::UI::WindowsAndMessaging::MessageBoxW;
use windows::core::{HSTRING, PCSTR};

/// The executables this build's RVA table covers. Mirrors `eldenring::rva::ERGameVersion`, which is
/// private, so it cannot be reused.
///
/// It is also what [`crate::rva_table`] dispatches the client's own eight RVAs on, so the crate's
/// table and ours are decided by one detection rather than two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supported {
    /// Tarnished Edition, Worldwide (exe 2.7.0.0). The crate's 93 RVAs for this build are
    /// upstream's GENERATED 1.17.0 table (vswarte PR #320); the client's own eight are still the
    /// derived candidates. See `crate::rva_table`.
    Ww270,
    /// Tarnished Edition, Japanese (exe 2.7.0.1). Covered by upstream's generated `rva_jp` table;
    /// the client's own eight RVAs were never derived for the JP binary and fall back to the
    /// Worldwide column under the `_SIG` prologue guards, exactly as JP 2.6.2.1 did before.
    Jp2701,
}

impl GameVersion for Supported {
    const NAME: &'static str = "elden ring";

    fn from_lang_version(lang_id: u16, version: &str) -> Option<Self> {
        if lang_id == LANG_ID_EN && version == game_version::REQUIRED_WW {
            Some(Self::Ww270)
        } else if lang_id == LANG_ID_JP && version == game_version::REQUIRED_JP {
            Some(Self::Jp2701)
        } else {
            None
        }
    }
}

/// The detection, done once. Both [`check`] and [`measured_clause`] read it, and so does
/// [`detected`] -- one PE read per process, one verdict, no chance of two callers disagreeing.
fn detect_once() -> &'static Result<Result<Supported, Rejection>, ()> {
    static DETECTED: OnceLock<Result<Result<Supported, Rejection>, ()>> = OnceLock::new();
    DETECTED.get_or_init(|| {
        std::panic::catch_unwind(|| {
            // Safety: the null module name asks for the running executable's own base, which is
            // mapped for the life of the process. `PeView::module` only reads that mapping.
            let base = unsafe { GetModuleHandleA(PCSTR(std::ptr::null())) }
                .map_err(|_| Rejection::Metadata { missing: "module" })?
                .0 as *const u8;
            let module = unsafe { PeView::module(base) };
            Supported::detect(&module).map_err(rejection_of)
        })
        .map_err(|_| ())
    })
}

/// Which supported executable we are running in, or `None` if detection was refused or failed.
///
/// This is the single source of truth for per-version dispatch; [`crate::rva_table`] selects the
/// client's own eight RVAs off it. Callers reach it only AFTER [`check`] has passed, because
/// `DllMain` builds nothing when it does not.
pub fn detected() -> Option<Supported> {
    match detect_once() {
        Ok(Ok(version)) => Some(*version),
        _ => None,
    }
}

/// `Ok(())` if the running executable is one we have RVAs for; `Err(message)` carries the finished,
/// player-facing text.
///
/// Never panics. The trait's `detect()` reaches `unwrap()` on the PE resource directory, which a
/// stripped or packed executable can trip, so the read runs under `catch_unwind` -- a malformed PE
/// is a rejection, not a crash.
pub fn check() -> Result<(), String> {
    let detected = detect_once();

    match detected {
        Ok(Ok(version)) => {
            let _ = version;
            // 🛑 STARTUP HONESTY (#241). The crate's 93 RVAs are now upstream's generated
            // 1.17.0 tables (vswarte PR #320, merged 2026-08-27), but the client's OWN eight
            // (`crate::rva_table::WW270`) are still the offline-derived candidates and have
            // never been executed in a live game. A player reading a log after something goes
            // strange must find that fact stated, not have to infer it from a version number.
            log::warn!(
                "client-private RVAs for Tarnished Edition are DERIVED, UNVERIFIED \
                 (candidate table 2026-08-27; crate RVAs are upstream-generated) -- \
                 Windows acceptance still owed"
            );
            Ok(())
        }
        Ok(Err(rejection)) => Err(game_version::explain(rejection)),
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
    let result = match detect_once() {
        Ok(Ok(Supported::Ww270)) => Ok((game_version::REQUIRED_WW, LANG_ID_EN)),
        Ok(Ok(Supported::Jp2701)) => Ok((game_version::REQUIRED_JP, LANG_ID_JP)),
        Ok(Err(rejection)) => Err(rejection.clone()),
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
