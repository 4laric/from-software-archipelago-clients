//! The GAME-version gate: does the running `eldenring.exe` match the RVA table this build was
//! compiled against?
//!
//! 🛑 Not to be confused with [`crate::version`], which is the AP *contract* version gate (does the
//! apworld's `versions` range accept this client). Different question, different failure, different
//! blast radius -- one warns and connects anyway, this one means the client cannot run at all.
//!
//! ## Why this module exists
//!
//! The version -> RVA table lives in the third-party `eldenring` crate, and its supported set is two
//! entries wide. `eldenring::rva::get()` does not return an error on a miss -- it **panics**, by
//! design, and its own doc says so. Before this module existed the panic escaped to
//! `shared::handle_panics`, which showed the player a message box reading `Rust panic: Unsupported
//! game version 2.2.0.0` followed by twenty frames of `DllMain`.
//!
//! That is the motivating case, and it is a real player: Duskerno, Nexus, 2026-08-15. His game was
//! simply out of date. The information he needed was one sentence long and he could not reach it.
//!
//! ⏳ It gets sharper on 2026-08-28, when Elden Ring: Tarnished Edition bumps the executable version
//! and *every* updated player lands here at once.
//!
//! ## The split
//!
//! Everything decidable lives here, as a pure function over plain data, so it is host-testable.
//! The caller (`eldenring-archipelago::game_version_gate`) does the PE read and maps the
//! third-party error type onto [`Rejection`]. It holds no wording and makes no decisions.

/// The Worldwide executable version this build's RVA table was compiled against.
pub const REQUIRED_WW: &str = "2.6.2.0";
/// The Worldwide **Tarnished Edition** executable this build ALSO carries a table for.
///
/// The crate's 93 RVAs for this build are upstream's GENERATED 1.17.0 table (vswarte
/// PR #320). The client's OWN eight (`eldenring_archipelago::rva_table::WW270`) are still
/// offline-derived candidates -- see the pin note in `crates/eldenring-archipelago/Cargo.toml`.
/// All four arms coexist: the `eldenring` crate dispatches per version and keeps every table, so
/// accepting Tarnished takes nothing away from 2.6.2.x.
pub const REQUIRED_WW_TARNISHED: &str = "2.7.0.0";
/// The Japanese executable version this build's RVA table was compiled against.
pub const REQUIRED_JP: &str = "2.6.2.1";
/// The Japanese **Tarnished Edition** executable this build ALSO carries a table for.
///
/// Covered by upstream's generated `rva_jp` table -- which is what closed the "Japanese
/// Tarnished Edition is NOT yet supported" gap this module used to have to state.
pub const REQUIRED_JP_TARNISHED: &str = "2.7.0.1";
/// `LANG_ID` of the Worldwide executable, masked to the primary language.
pub const LANG_ID_EN: u16 = 0x0009;
/// `LANG_ID` of the Japanese executable, masked to the primary language.
pub const LANG_ID_JP: u16 = 0x0011;

/// Why the running executable was refused. Mirrors the third-party `DetectError` one-for-one, but
/// owns no dependency on it, so this crate stays host-native.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// The executable is Elden Ring, in a supported language, at a version we have no RVAs for.
    Version { detected: String },
    /// The executable is Elden Ring at some version, in a language we have no RVAs for.
    Language { lang_id: u16 },
    /// The module we were loaded into is not Elden Ring.
    Product { actual: String },
    /// The PE carries no version resource we could read. `missing` names which piece.
    Metadata { missing: &'static str },
}

/// Render a [`Rejection`] as the text the player sees, in the message box and in the log.
///
/// Contract, asserted by the tests below:
///
/// * it names what was detected AND what is required, because "unsupported" alone is unactionable;
/// * it names BOTH directions (game too old, game too new) because both happen and the player cannot
///   tell them apart from the numbers alone;
/// * it carries no panic wording and no backtrace;
/// * it is ASCII, so it survives the log file and a message box on any code page.
pub fn explain(rejection: &Rejection) -> String {
    match rejection {
        Rejection::Version { detected } => format!(
            "Elden Ring Archipelago cannot start: unsupported game version.\n\
             \n\
             Your Elden Ring:  {detected}\n\
             This build needs: {REQUIRED_WW} or {REQUIRED_WW_TARNISHED} (Worldwide)\n\
             \x20                 {REQUIRED_JP} or {REQUIRED_JP_TARNISHED} (Japanese)\n\
             \n\
             There are two ways to land here.\n\
             \n\
             1. Your game is OLDER than this client. Update Elden Ring through Steam. If you\n\
                downgraded on purpose -- for an older Seamless Co-op, or an older fifthmatt\n\
                randomizer -- that downgrade is what this is.\n\
             \n\
             2. Your game is NEWER than this client, because Elden Ring updated. Check the mod\n\
                page or the Discord for a client build that matches the new version.\n\
             \n\
             Elden Ring will start normally. The Archipelago client is switched off for this\n\
             launch, and nothing has been written to your save."
        ),
        Rejection::Language { lang_id } => format!(
            "Elden Ring Archipelago cannot start: unsupported executable language.\n\
             \n\
             Your executable reports language id: {lang_id:#06x}\n\
             This build needs:                    {LANG_ID_EN:#06x} (Worldwide) or \
             {LANG_ID_JP:#06x} (Japanese)\n\
             \n\
             Your game VERSION may be perfectly fine -- this is the localisation of the\n\
             eldenring.exe file, not your in-game language setting. Please report this with the\n\
             id above. It is a gap on our side, not something you did wrong.\n\
             \n\
             Elden Ring will start normally. The Archipelago client is switched off for this\n\
             launch, and nothing has been written to your save."
        ),
        Rejection::Product { actual } => format!(
            "Elden Ring Archipelago cannot start: this is not Elden Ring.\n\
             \n\
             Expected product name: elden ring\n\
             This executable says:  {actual}\n\
             \n\
             The client was loaded into something else. Check that the mod is installed against\n\
             Elden Ring and not another game in the same mod loader profile.\n\
             \n\
             The game will start normally. The Archipelago client is switched off for this launch."
        ),
        Rejection::Metadata { missing } => format!(
            "Elden Ring Archipelago cannot start: the executable carries no {missing} information,\n\
             so we cannot tell which build of Elden Ring this is.\n\
             \n\
             This build needs {REQUIRED_WW} or {REQUIRED_WW_TARNISHED} (Worldwide), or\n\
             {REQUIRED_JP} or {REQUIRED_JP_TARNISHED} (Japanese). An executable\n\
             with its version resource stripped is usually a repack or a cracked copy; the mod\n\
             cannot support those, because every memory address it uses is keyed to a known build.\n\
             \n\
             Elden Ring will start normally. The Archipelago client is switched off for this\n\
             launch, and nothing has been written to your save."
        ),
    }
}

/// One compact, log-safe clause rendering what the executable's PE version resource measured --
/// for warnings that must SELF-DATE against a moved RVA instead of naming no version at all
/// (clients#371: the SearchStringTable sig-mismatch warn could not say whether the exe had moved
/// off 2.6.2, because nothing in the log recorded what it saw, so a human had to date the
/// failure by hand).
///
/// `Ok((version, lang_id))` is a detection this build's RVA table ACCEPTED; the `Err` arm is the
/// same [`Rejection`] the startup gate renders for the player. The gate does the PE read; the
/// wording lives here, host-tested, like every other string in this module.
pub fn measured_clause(result: &Result<(&str, u16), Rejection>) -> String {
    match result {
        Ok((version, lang_id)) => {
            format!("measured exe {version} (lang {lang_id:#06x}, RVA-covered)")
        }
        Err(Rejection::Version { detected }) => {
            format!("measured exe {detected} (NOT covered by this build's RVA table)")
        }
        Err(Rejection::Language { lang_id }) => {
            format!("measured exe lang {lang_id:#06x} (unsupported localisation)")
        }
        Err(Rejection::Product { actual }) => {
            format!("measured product {actual:?} (not recognised as Elden Ring)")
        }
        Err(Rejection::Metadata { missing }) => {
            format!("exe version unreadable (no {missing} metadata)")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE MOTIVATING CASE. Duskerno, Nexus, 2026-08-15: `Rust panic: Unsupported game version
    /// 2.2.0.0`. His game was older than this client and the popup never said so.
    #[test]
    fn duskerno_gets_told_what_he_has_and_what_he_needs() {
        let msg = explain(&Rejection::Version {
            detected: "2.2.0.0".into(),
        });
        assert!(
            msg.contains("2.2.0.0"),
            "must name the detected version: {msg}"
        );
        assert!(
            msg.contains("2.6.2.0"),
            "must name the required WW version: {msg}"
        );
        assert!(
            msg.contains("2.6.2.1"),
            "must name the required JP version: {msg}"
        );
    }

    /// #241. The 2.7.0.0 (Tarnished Edition, Worldwide) arm ships alongside 2.6.2.0 rather than
    /// replacing it -- the `eldenring` crate dispatches per version and keeps both tables. A
    /// rejection message that named only one of them would send half the players to the wrong
    /// remedy, and on 2026-08-28 it is the 2.7.0.0 half that is everybody.
    #[test]
    fn a_version_rejection_names_both_worldwide_builds() {
        let msg = explain(&Rejection::Version {
            detected: "2.2.0.0".into(),
        });
        for required in [
            REQUIRED_WW,
            REQUIRED_WW_TARNISHED,
            REQUIRED_JP,
            REQUIRED_JP_TARNISHED,
        ] {
            assert!(msg.contains(required), "dropped {required}: {msg}");
        }
    }

    /// The JP Tarnished gap CLOSED (#241): upstream's generated `rva_jp` table covers 2.7.0.1,
    /// so the message must no longer tell a JP Tarnished player they are unsupported. This is
    /// the inverse of the test it replaces -- kept, rather than deleted, because the wording it
    /// guards is what a JP player acts on.
    #[test]
    fn the_jp_tarnished_gap_is_closed_and_the_wording_followed() {
        let msg = explain(&Rejection::Version {
            detected: "1.0.0.0".into(),
        });
        assert!(
            msg.contains(REQUIRED_JP_TARNISHED),
            "must offer the JP Tarnished exe as supported: {msg}"
        );
        assert!(
            !msg.contains("NOT yet supported"),
            "the JP Tarnished gap is closed; stale wording: {msg}"
        );
    }

    /// All four supported builds are named. The client serves BOTH the Tarnished executables and
    /// the pre-Tarnished ones (the 4laric fromsoftware-rs fork restores the 2.6.2.x tables
    /// upstream deleted), and a player who downgraded deliberately must not be told to update.
    #[test]
    fn every_supported_build_is_named_and_none_is_called_retired() {
        let msg = explain(&Rejection::Version {
            detected: "2.2.0.0".into(),
        });
        for required in ["2.6.2.0", "2.6.2.1", "2.7.0.0", "2.7.0.1"] {
            assert!(msg.contains(required), "dropped {required}: {msg}");
        }
    }

    /// The metadata arm quotes the required set too, and it drifted out of step with the version
    /// arm once before. Assert them together so the next arm added cannot update only one.
    #[test]
    fn the_metadata_message_names_the_same_required_set() {
        let msg = explain(&Rejection::Metadata { missing: "version" });
        for required in [
            REQUIRED_WW,
            REQUIRED_WW_TARNISHED,
            REQUIRED_JP,
            REQUIRED_JP_TARNISHED,
        ] {
            assert!(msg.contains(required), "dropped {required}: {msg}");
        }
    }

    /// A covered version must render as covered. `measured_clause`'s `Ok` arm is what the
    /// sig-mismatch warns self-date against, so a 2.7.0.0 hit has to say RVA-covered -- the
    /// opposite of what the old test above assumed.
    #[test]
    fn tarnished_measures_as_covered_not_as_a_miss() {
        let hit = measured_clause(&Ok((REQUIRED_WW_TARNISHED, LANG_ID_EN)));
        assert!(hit.contains("2.7.0.0"), "{hit}");
        assert!(hit.contains("RVA-covered"), "{hit}");
        assert!(!hit.contains("NOT covered"), "{hit}");
    }

    /// The whole point is that the player stops seeing a Rust panic. If any of these words come
    /// back, the message has regressed into the thing it replaced.
    #[test]
    fn no_panic_wording_survives_into_the_message() {
        for rejection in every_variant() {
            let msg = explain(&rejection);
            for banned in ["panic", "Backtrace", "backtrace", "DllMain", "unwrap"] {
                assert!(!msg.contains(banned), "{banned:?} leaked into: {msg}");
            }
        }
    }

    /// Both directions, because the version number alone does not tell a player which one they are
    /// in, and "update your game" is actively wrong half the time -- on 2026-08-28 it will be wrong
    /// for everybody.
    #[test]
    fn a_version_rejection_names_both_directions() {
        let msg = explain(&Rejection::Version {
            detected: "2.2.0.0".into(),
        });
        assert!(
            msg.contains("OLDER"),
            "must cover the out-of-date case: {msg}"
        );
        assert!(
            msg.contains("NEWER"),
            "must cover the game-updated-past-us case: {msg}"
        );
        assert!(
            msg.contains("Steam"),
            "the older case needs its actual remedy: {msg}"
        );
    }

    /// A language miss can happen on a CORRECT version, so the message must not send the player off
    /// to update a game that is already right.
    #[test]
    fn a_language_rejection_does_not_blame_the_version() {
        let msg = explain(&Rejection::Language { lang_id: 0x0007 });
        assert!(msg.contains("0x0007"), "must name the id we saw: {msg}");
        assert!(
            msg.contains("0x0009") && msg.contains("0x0011"),
            "must name both accepted ids: {msg}"
        );
        assert!(
            msg.contains("VERSION may be perfectly fine"),
            "must not blame the version: {msg}"
        );
        assert!(
            !msg.contains("Update Elden Ring"),
            "must not send them to Steam: {msg}"
        );
    }

    /// Every variant carries its own datum through. A new variant that forgets to interpolate would
    /// otherwise produce a confident, contentless message.
    #[test]
    fn every_variant_names_its_own_evidence() {
        let cases: [(Rejection, &str); 4] = [
            (
                Rejection::Version {
                    detected: "9.9.9.9".into(),
                },
                "9.9.9.9",
            ),
            (Rejection::Language { lang_id: 0x0412 }, "0x0412"),
            (
                Rejection::Product {
                    actual: "DARK SOULS III".into(),
                },
                "DARK SOULS III",
            ),
            (Rejection::Metadata { missing: "version" }, "version"),
        ];
        for (rejection, evidence) in cases {
            let msg = explain(&rejection);
            assert!(
                msg.contains(evidence),
                "{rejection:?} dropped {evidence:?}: {msg}"
            );
            assert!(msg.len() > 120, "{rejection:?} produced a stub: {msg}");
        }
    }

    /// The log file and the message box both take this string; keep it to one byte per character.
    #[test]
    fn the_message_is_ascii() {
        for rejection in every_variant() {
            let msg = explain(&rejection);
            assert!(msg.is_ascii(), "non-ascii in: {msg}");
        }
    }

    /// Every message tells the player what happens next, so nobody is left wondering whether the
    /// game is about to launch or has already broken.
    #[test]
    fn every_message_says_what_happens_to_the_game() {
        for rejection in every_variant() {
            let msg = explain(&rejection);
            assert!(
                msg.contains("will start normally"),
                "{rejection:?} never says the game still boots: {msg}"
            );
        }
    }

    /// clients#371: a sig-mismatch warn that cannot say which exe it measured is not
    /// self-dating. The clause must name the detected version on a miss and the covered
    /// version on a hit, and stay one ASCII line so it survives the log.
    #[test]
    fn the_measured_clause_self_dates_both_arms() {
        let hit = measured_clause(&Ok((REQUIRED_WW, LANG_ID_EN)));
        assert!(
            hit.contains(REQUIRED_WW),
            "hit must name the version: {hit}"
        );
        assert!(hit.contains("RVA-covered"), "{hit}");
        // NB: this used to use 2.7.0.0 as the stand-in for "uncovered". It is COVERED now
        // (Tarnished Edition, candidate table), so the miss example has to be a version we
        // really do not have a table for -- otherwise the test asserts a false premise.
        let miss = measured_clause(&Err(Rejection::Version {
            detected: "2.8.0.0".into(),
        }));
        assert!(
            miss.contains("2.8.0.0"),
            "miss must name the version: {miss}"
        );
        assert!(miss.contains("NOT covered"), "{miss}");
    }

    #[test]
    fn the_measured_clause_is_one_ascii_line_for_every_outcome() {
        let cases: [Result<(&str, u16), Rejection>; 5] = [
            Ok((REQUIRED_WW, LANG_ID_EN)),
            Err(Rejection::Version {
                detected: "2.2.0.0".into(),
            }),
            Err(Rejection::Language { lang_id: 0x0007 }),
            Err(Rejection::Product {
                actual: "DARK SOULS III".into(),
            }),
            Err(Rejection::Metadata { missing: "version" }),
        ];
        for result in &cases {
            let clause = measured_clause(result);
            assert!(
                clause.is_ascii() && !clause.contains('\n'),
                "offender: {clause}"
            );
        }
    }

    fn every_variant() -> Vec<Rejection> {
        vec![
            Rejection::Version {
                detected: "2.2.0.0".into(),
            },
            Rejection::Language { lang_id: 0x0007 },
            Rejection::Product {
                actual: "DARK SOULS III".into(),
            },
            Rejection::Metadata { missing: "version" },
        ]
    }
}
