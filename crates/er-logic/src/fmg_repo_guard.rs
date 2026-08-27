//! fmg_repo_guard.rs -- the fail-closed decision behind `fmg_inject`'s ONE read-not-called address.
//!
//! ## Why this module exists
//!
//! Seven of the client's eight private RVAs (`rva_table` in `eldenring-archipelago`) are CALLED, so
//! a `_SIG` prologue check at the call site proves the address before the jump: a stale address
//! disables that feature instead of executing something else. `fmg_repo` is the exception. It is a
//! STATIC DATA SLOT that is only ever READ (`read_usize(base + fmg_repo)`), so there is no prologue
//! to match, and until this module the entire screen on it was `plausible(p)` -- a range test that
//! any pointer-shaped word passes. It is also the weakest row in the 2.7.0.0 port (2 of 20
//! reference sites voted). A wrong address there does not fault: it walks
//! `repo -> +0x08 -> [0] -> [category*8]`, lands on unrelated heap, and every later step -- the
//! group parse, the string reads, and eventually a POINTER WRITE into `base_array[0][cat]` -- is
//! aimed at a structure we invented.
//!
//! So the address needs an invariant proved from the structure it points AT, checked once at first
//! use, failing CLOSED (feature off for the session, one log line) rather than open.
//!
//! ## The invariant
//!
//! A real `MsgData` block is a `GroupRecord[]` the game BINARY SEARCHES, plus a string-offset
//! table. That gives four properties at once, and [`validate`] demands all of them:
//!
//!   1. **Ordered, disjoint, valid group spans** ([`crate::fmg_groups::is_ordered_disjoint`]).
//!      Binary search requires it, so vanilla always satisfies it; an arbitrary heap window read as
//!      `{u32,u32,u32,u32}` records essentially never does.
//!   2. **An entry count inside a sane band**, consistent with the spans -- the offset table must be
//!      at least as long as the ids the groups claim to cover.
//!   3. **Every probe id resolves.** The probes are KNOWN VANILLA ids that exist on every shipped
//!      GoodsName table; a structure we invented has no reason to cover all of them.
//!   4. **Every probe's text is TEXT.** Decoded UTF-16 with no replacement char (the caller decodes
//!      lossily, so `U+FFFD` is exactly "these bytes were not UTF-16"), no control characters, and
//!      a sane length.
//!
//! 🛑 **Deliberately LANGUAGE-INDEPENDENT.** The tempting fifth clause -- compare the probe text to
//! the English string ("Tarnished's Furled Finger") -- would fail closed on every non-English
//! install, which is a worse defect than the one being fixed. The check is that the ids resolve to
//! well-formed text, never that they resolve to particular words.
//!
//! Measured shape, for scale: on 2.7.0.0 the live GoodsName block parsed as 4302 offset-table
//! entries and all eight probe ids read back correctly through the game's own `SearchStringTable`
//! (smoke log `archipelago-2026-08-27.log` L1004-L1011, clients PR #456). That is the run this
//! guard has to keep passing.

use crate::fmg_groups::{is_ordered_disjoint, Span};

/// Smallest offset-table length a real category block can plausibly have. GoodsName measures 4302;
/// the smallest shipped FMG categories are still in the hundreds. Kept far below any real value so
/// the band rejects garbage, not an unfamiliar build.
pub const MIN_ENTRIES: u32 = 64;

/// Largest offset-table length accepted. Mirrors `fmg_inject::parse`'s own ceiling.
pub const MAX_ENTRIES: u32 = 0x20_0000;

/// Longest probe string accepted, in chars. Item NAMES are short; the cap only has to exclude a
/// runaway walk through unterminated memory.
pub const MAX_PROBE_CHARS: usize = 512;

/// What one probe id resolved to, as decoded by the caller.
///
/// `None` = the id resolved to nothing (no group covers it, or its offset was 0). The text is the
/// caller's LOSSY UTF-16 decode on purpose: a `U+FFFD` in it is the evidence that the bytes were not
/// UTF-16 at all, and a strict decode would have thrown that evidence away as an error with no id
/// attached.
pub type Probe<'a> = (u32, Option<&'a str>);

/// Why the structure behind the repo pointer was not believed. Every arm names the clause, so the
/// single log line the client prints says which invariant failed, not just "bad".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    /// No group records at all.
    NoGroups,
    /// Group spans are not ascending-and-disjoint, so the game's binary search could not work on
    /// this block -- therefore the game did not build it.
    GroupsUnordered,
    /// Offset-table length outside [`MIN_ENTRIES`]..=[`MAX_ENTRIES`].
    EntryCountOutOfBand(u32),
    /// The groups claim to cover more ids than the offset table has slots.
    CoverageExceedsTable { covered: u32, entries: u32 },
    /// A known-vanilla probe id resolved to nothing.
    ProbeMissing(u32),
    /// A probe resolved to an empty string.
    ProbeEmpty(u32),
    /// A probe resolved to more than [`MAX_PROBE_CHARS`] chars.
    ProbeTooLong(u32),
    /// A probe's bytes did not decode as UTF-16 text (replacement or control chars present).
    ProbeNotText(u32),
}

impl Reject {
    /// One ASCII clause for the client's log line.
    pub fn reason(&self) -> String {
        match *self {
            Reject::NoGroups => "no group records".to_string(),
            Reject::GroupsUnordered => {
                "group records are not ascending/disjoint (the game binary-searches them)"
                    .to_string()
            }
            Reject::EntryCountOutOfBand(n) => {
                format!("offset table length {n} outside {MIN_ENTRIES}..={MAX_ENTRIES}")
            }
            Reject::CoverageExceedsTable { covered, entries } => {
                format!("groups cover {covered} id(s) but the offset table holds {entries}")
            }
            Reject::ProbeMissing(id) => format!("known vanilla id {id} resolves to nothing"),
            Reject::ProbeEmpty(id) => format!("known vanilla id {id} resolves to an empty string"),
            Reject::ProbeTooLong(id) => {
                format!("id {id} resolves to more than {MAX_PROBE_CHARS} chars (unterminated?)")
            }
            Reject::ProbeNotText(id) => {
                format!("id {id} does not decode as UTF-16 text (replacement/control chars)")
            }
        }
    }
}

/// The verdict on the structure behind `fmg_repo`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every clause held: the address points at something the game built.
    Trusted,
    /// Fail CLOSED. The caller disables the FMG feature for the session.
    Rejected(Reject),
}

impl Verdict {
    pub fn is_trusted(&self) -> bool {
        matches!(self, Verdict::Trusted)
    }
}

/// Is this decoded string believable as an FMG entry?
///
/// `U+FFFD` means the lossy decode hit bytes that are not UTF-16. Control characters other than the
/// line breaks FMG captions legitimately carry mean the same thing in a different disguise.
fn is_texty(s: &str) -> bool {
    s.chars()
        .all(|c| c != '\u{FFFD}' && (!c.is_control() || c == '\n' || c == '\r' || c == '\t'))
}

/// THE GATE. Pure: the caller does the raw reads, this decides.
///
/// `spans` are the parsed group id ranges, `entries` the offset-table length, `probes` the lookup
/// results for a set of ids that exist on every vanilla table.
pub fn validate(spans: &[Span], entries: u32, probes: &[Probe<'_>]) -> Verdict {
    if spans.is_empty() {
        return Verdict::Rejected(Reject::NoGroups);
    }
    if !is_ordered_disjoint(spans) {
        return Verdict::Rejected(Reject::GroupsUnordered);
    }
    if !(MIN_ENTRIES..=MAX_ENTRIES).contains(&entries) {
        return Verdict::Rejected(Reject::EntryCountOutOfBand(entries));
    }
    let covered: u32 = spans
        .iter()
        .map(|s| s.last_id.saturating_sub(s.first_id).saturating_add(1))
        .fold(0u32, |a, b| a.saturating_add(b));
    if covered > entries {
        return Verdict::Rejected(Reject::CoverageExceedsTable { covered, entries });
    }
    for &(id, text) in probes {
        match text {
            None => return Verdict::Rejected(Reject::ProbeMissing(id)),
            Some("") => return Verdict::Rejected(Reject::ProbeEmpty(id)),
            Some(t) if t.chars().count() > MAX_PROBE_CHARS => {
                return Verdict::Rejected(Reject::ProbeTooLong(id));
            }
            Some(t) if !is_texty(t) => return Verdict::Rejected(Reject::ProbeNotText(id)),
            Some(_) => {}
        }
    }
    Verdict::Trusted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vanilla_spans() -> Vec<Span> {
        vec![
            Span::new(100, 199),
            Span::new(8000, 8100),
            Span::new(10100, 10400),
        ]
    }
    fn good_probes<'a>() -> Vec<Probe<'a>> {
        vec![
            (100, Some("Tarnished's Furled Finger")),
            (8000, Some("Golden Rune [1]")),
            (10100, Some("Smithing Stone [1]")),
        ]
    }

    #[test]
    fn a_vanilla_shaped_block_is_trusted() {
        assert_eq!(
            validate(&vanilla_spans(), 4302, &good_probes()),
            Verdict::Trusted
        );
    }

    /// The point of the whole module: the OLD screen was a range test on the pointer, which garbage
    /// passes. Records read out of unrelated heap are overwhelmingly unordered/overlapping.
    #[test]
    fn heap_garbage_read_as_records_is_rejected() {
        let garbage = vec![
            Span::new(0x8E1F_2210, 0x0002_0044),
            Span::new(12, 900_000),
            Span::new(7, 8),
        ];
        assert_eq!(
            validate(&garbage, 4302, &good_probes()),
            Verdict::Rejected(Reject::GroupsUnordered)
        );
    }

    #[test]
    fn empty_group_array_is_rejected() {
        assert_eq!(
            validate(&[], 4302, &good_probes()),
            Verdict::Rejected(Reject::NoGroups)
        );
    }

    #[test]
    fn entry_count_band_is_enforced_both_ends() {
        assert_eq!(
            validate(&vanilla_spans(), 3, &good_probes()),
            Verdict::Rejected(Reject::EntryCountOutOfBand(3))
        );
        assert_eq!(
            validate(&vanilla_spans(), MAX_ENTRIES + 1, &good_probes()),
            Verdict::Rejected(Reject::EntryCountOutOfBand(MAX_ENTRIES + 1))
        );
    }

    #[test]
    fn groups_may_not_claim_more_ids_than_the_table_holds() {
        // One span covering 100_000 ids against a 4302-slot table: the block cannot be what it says.
        let spans = vec![Span::new(100, 100_099)];
        assert!(matches!(
            validate(&spans, 4302, &good_probes()),
            Verdict::Rejected(Reject::CoverageExceedsTable { .. })
        ));
    }

    #[test]
    fn a_missing_known_id_fails_closed() {
        let probes = vec![(100, Some("Tarnished's Furled Finger")), (10100, None)];
        assert_eq!(
            validate(&vanilla_spans(), 4302, &probes),
            Verdict::Rejected(Reject::ProbeMissing(10100))
        );
    }

    #[test]
    fn an_empty_string_fails_closed() {
        let probes = vec![(100, Some(""))];
        assert_eq!(
            validate(&vanilla_spans(), 4302, &probes),
            Verdict::Rejected(Reject::ProbeEmpty(100))
        );
    }

    /// A lossy UTF-16 decode of non-text bytes carries `U+FFFD`. That is the tell, and it is the
    /// reason the caller decodes lossily instead of erroring.
    #[test]
    fn non_utf16_bytes_show_up_as_replacement_chars_and_fail_closed() {
        let probes = vec![(100, Some("A\u{FFFD}\u{FFFD}x"))];
        assert_eq!(
            validate(&vanilla_spans(), 4302, &probes),
            Verdict::Rejected(Reject::ProbeNotText(100))
        );
    }

    #[test]
    fn control_characters_fail_closed_but_caption_line_breaks_do_not() {
        assert_eq!(
            validate(&vanilla_spans(), 4302, &[(100, Some("na\u{0007}me"))]),
            Verdict::Rejected(Reject::ProbeNotText(100))
        );
        assert_eq!(
            validate(&vanilla_spans(), 4302, &[(100, Some("line one\nline two"))]),
            Verdict::Trusted
        );
    }

    #[test]
    fn an_unterminated_walk_fails_closed() {
        let long: String = "x".repeat(MAX_PROBE_CHARS + 1);
        assert_eq!(
            validate(&vanilla_spans(), 4302, &[(100, Some(&long))]),
            Verdict::Rejected(Reject::ProbeTooLong(100))
        );
    }

    /// 🛑 The clause that must NOT exist: a localised install reads different words for the same id
    /// and is still a healthy install. Non-English, non-ASCII text is TRUSTED.
    #[test]
    fn a_localised_install_is_trusted() {
        let probes = vec![
            (100, Some("Doigt recroqueville du Sans-eclat")),
            (8000, Some("\u{7802}\u{6ED1}\u{77F3}")),
        ];
        assert_eq!(validate(&vanilla_spans(), 4302, &probes), Verdict::Trusted);
    }

    /// Every rejection has to be printable in the client's ASCII-only log.
    #[test]
    fn every_reason_is_ascii() {
        for r in [
            Reject::NoGroups,
            Reject::GroupsUnordered,
            Reject::EntryCountOutOfBand(3),
            Reject::CoverageExceedsTable {
                covered: 9,
                entries: 4,
            },
            Reject::ProbeMissing(100),
            Reject::ProbeEmpty(100),
            Reject::ProbeTooLong(100),
            Reject::ProbeNotText(100),
        ] {
            assert!(r.reason().is_ascii(), "{r:?} reason is not ASCII");
        }
    }
}
