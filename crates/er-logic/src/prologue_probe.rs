//! `prologue_probe` — tell "wrong game build" apart from "another mod hooked this function first".
//!
//! ## Why this exists (2026-07-30, dafranky67 via Nexus)
//!
//! A player reported that enabling matt's randomizer "randomizer helper" (auto-equip) permanently
//! broke item RECEIVING while checks kept sending. That is the exact signature of our own
//! fail-closed install guard: `detour::install()` refuses unless the pinned AddItemFunc RVA still
//! holds its pristine 16-byte prologue, `grant_full_id` hard-requires the installed hook, and every
//! grant therefore returns `false` forever. Checks are unaffected because they come from the
//! inventory synthetic scan, not the hook.
//!
//! The guard was written to catch a STALE PINNED RVA on a new game build, and its message says so.
//! But a foreign DLL that detours the same routine first produces a byte mismatch too — same
//! refusal, same message, and the message sends the player looking for a game-version problem that
//! isn't there. One instrument with two explanations is not a diagnosis.
//!
//! The two cases are distinguishable at the bytes. A detour overwrites the HEAD of the function
//! with a jump (`E9 rel32`, or `FF 25` + an absolute slot) and leaves the TAIL of the prologue
//! untouched. A different game build changes the prologue THROUGHOUT — head and tail both.
//!
//! WARNING: this is a PROBABILITY, not a proof, and callers must word it that way. A 14-byte
//! absolute jmp overwrites nearly the whole window, leaving a short tail; a build whose prologue
//! happened to end the same way would look like a hook. The verdict picks the MESSAGE, never the
//! behaviour — the install refuses either way.

/// What the bytes at a pinned function entry appear to say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrologueVerdict {
    /// Byte-for-byte match. Safe to hook.
    Pristine,
    /// Head clobbered, tail survives — most likely ANOTHER MOD detoured this function before us.
    ProbableForeignHook,
    /// Nothing recognisable survives — most likely the pinned RVA is stale for this game build.
    ProbableStaleBuild,
}

/// Opcodes that begin a detour trampoline. `E9` near jmp and `EB` short jmp are the common
/// minhook/retour patches; `FF 25` is the RIP-relative absolute jmp; `68` is the legacy
/// push-imm32/ret thunk.
fn starts_with_jump(actual: &[u8]) -> bool {
    matches!(actual, [0xE9, ..] | [0xEB, ..] | [0x68, ..] | [0xFF, 0x25, ..])
}

/// Count trailing bytes that still match the expected prologue.
fn intact_tail(expected: &[u8], actual: &[u8]) -> usize {
    expected
        .iter()
        .rev()
        .zip(actual.iter().rev())
        .take_while(|(e, a)| e == a)
        .count()
}

/// Classify the bytes found at a pinned entry point against the prologue we pinned it by.
///
/// Ordering matters: an exact match short-circuits, so a routine whose real prologue legitimately
/// BEGINS with a jump can never be misread as hooked.
pub fn classify(expected: &[u8], actual: &[u8]) -> PrologueVerdict {
    if expected == actual {
        return PrologueVerdict::Pristine;
    }
    let tail = intact_tail(expected, actual);
    // A jump at the entry plus ANY surviving tail is the detour shape.
    if starts_with_jump(actual) && tail > 0 {
        return PrologueVerdict::ProbableForeignHook;
    }
    // No recognisable jump, but most of the window survived and only the head moved: still far more
    // consistent with a patch than with a different build's prologue.
    if tail * 3 >= expected.len() * 2 {
        return PrologueVerdict::ProbableForeignHook;
    }
    PrologueVerdict::ProbableStaleBuild
}

/// The operator-facing explanation for a verdict, phrased as the probability it is.
///
/// `what` names the routine in player terms (e.g. `"item pickup"`), not by symbol.
pub fn explain(verdict: PrologueVerdict, what: &str) -> String {
    match verdict {
        PrologueVerdict::Pristine => format!("{what} entry is pristine"),
        PrologueVerdict::ProbableForeignHook => format!(
            "another mod appears to have hooked {what} before us — Archipelago cannot deliver items \
             while that is true. Disable other mods' item-pickup features (for example a \
             randomizer's \"helper\" / auto-equip option) and restart."
        ),
        PrologueVerdict::ProbableStaleBuild => format!(
            "{what} entry matches nothing we pinned — most likely this game build is newer than the \
             one this client pins. A client update is probably needed."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real AddItemFunc prologue the client pins for 2.6.2.0.
    const SIG: &[u8] = &[
        0x40, 0x55, 0x56, 0x57, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57, 0x48, 0x8D, 0xAC,
        0x24,
    ];

    #[test]
    fn pristine_is_pristine() {
        assert_eq!(classify(SIG, SIG), PrologueVerdict::Pristine);
    }

    /// THE MOTIVATING CASE: a 5-byte `E9 rel32` detour laid over the head, tail untouched.
    #[test]
    fn five_byte_near_jmp_reads_as_foreign_hook() {
        let mut actual = SIG.to_vec();
        actual[..5].copy_from_slice(&[0xE9, 0x12, 0x34, 0x56, 0x78]);
        assert_eq!(classify(SIG, &actual), PrologueVerdict::ProbableForeignHook);
    }

    /// A 14-byte `FF 25` absolute jmp leaves only a 2-byte tail. Still a hook — the leading opcode
    /// carries the verdict when the tail alone would not.
    #[test]
    fn fourteen_byte_abs_jmp_reads_as_foreign_hook() {
        let mut actual = SIG.to_vec();
        actual[..14].copy_from_slice(&[
            0xFF, 0x25, 0x00, 0x00, 0x00, 0x00, 0xEF, 0xBE, 0xAD, 0xDE, 0x00, 0x00, 0x00, 0x00,
        ]);
        assert_eq!(intact_tail(SIG, &actual), 2);
        assert_eq!(classify(SIG, &actual), PrologueVerdict::ProbableForeignHook);
    }

    /// A different build: the prologue differs throughout, tail included.
    #[test]
    fn wholly_different_bytes_read_as_stale_build() {
        let actual = [
            0x48, 0x89, 0x5C, 0x24, 0x08, 0x48, 0x89, 0x6C, 0x24, 0x10, 0x48, 0x89, 0x74, 0x24,
            0x18, 0x57,
        ];
        assert_eq!(classify(SIG, &actual), PrologueVerdict::ProbableStaleBuild);
    }

    /// Guard against the obvious mis-read: a routine whose REAL prologue starts with a jump must
    /// not be called hooked when it matches exactly.
    #[test]
    fn exact_match_wins_over_jump_opcode() {
        let jumpy: &[u8] = &[0xE9, 0x01, 0x02, 0x03, 0x04, 0x55, 0x56, 0x57];
        assert_eq!(classify(jumpy, jumpy), PrologueVerdict::Pristine);
    }

    /// Head moved with no jump opcode but the window largely survives — still a patch, not a build.
    #[test]
    fn mostly_intact_tail_without_jump_opcode_reads_as_foreign_hook() {
        let mut actual = SIG.to_vec();
        actual[..4].copy_from_slice(&[0x90, 0x90, 0x90, 0x90]);
        assert_eq!(classify(SIG, &actual), PrologueVerdict::ProbableForeignHook);
    }

    /// The explanations must never assert certainty.
    #[test]
    fn explanations_hedge() {
        let hook = explain(PrologueVerdict::ProbableForeignHook, "item pickup");
        assert!(hook.contains("appears to"), "{hook}");
        let stale = explain(PrologueVerdict::ProbableStaleBuild, "item pickup");
        assert!(stale.contains("most likely"), "{stale}");
    }
}
