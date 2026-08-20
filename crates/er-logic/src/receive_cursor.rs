//! Character identity and cursor-ahead recovery for the positive AP ReceivedItems frontier.
//!
//! The legacy client save keyed its cursor only by `(room seed, AP slot)`. Two Elden Ring
//! characters playing that slot therefore shared one frontier: a fresh character could begin at
//! 172 and silently skip the entire replay. This module applies the same `(ER save slot,
//! monotonically stamped play time)` identity rule as the reconciler's consumable ledger.

use crate::reconcile::{seed_trust, CharLedger};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorEntry {
    pub index: i64,
    pub play_time_ms: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    /// Identity evidence is not ready. Do not process ReceivedItems from a guessed frontier.
    Wait,
    /// This character owns no prior positive frontier.
    Fresh,
    /// Resume this character's own persisted frontier.
    Resume(i64),
    /// One-time adoption of the pre-character-keyed legacy frontier.
    Migrate(i64),
}

/// Bind one live character to its positive receive frontier.
///
/// `marker_fresh` is the save-embedded marker verdict when available. It wins over a save-slot
/// collision: a delete-and-recreate can reuse slot 6 and begin with a play time close enough to a
/// very young predecessor that play time alone is ambiguous. A legacy cursor is adopted only for
/// a character positively identified as returning; ambiguity waits rather than inheriting it.
pub fn bind(
    entry: Option<CursorEntry>,
    legacy_index: i64,
    live_play_time_ms: u32,
    marker_fresh: Option<bool>,
) -> Binding {
    if marker_fresh == Some(true) {
        return Binding::Fresh;
    }
    if let Some(entry) = entry {
        let (fresh, persisted) = seed_trust(
            Some(CharLedger {
                watermark: entry.index,
                play_time_ms: entry.play_time_ms,
            }),
            live_play_time_ms,
        );
        return if fresh {
            Binding::Fresh
        } else {
            Binding::Resume(persisted.unwrap_or(0).max(0))
        };
    }
    match marker_fresh {
        Some(false) if legacy_index > 0 => Binding::Migrate(legacy_index),
        None if legacy_index > 0 => Binding::Wait,
        Some(_) | None => Binding::Fresh,
    }
}

/// A cursor ahead of the server stream must not stay ahead forever, but clamping against a
/// partially-arrived stream would replay items already granted. Require the shorter stream length
/// to remain unchanged for a bounded interval before accepting it as the re-hosted truth.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AheadGuard {
    candidate: Option<(usize, u64)>,
}

impl AheadGuard {
    pub const SETTLE_MS: u64 = 5_000;

    pub fn reset(&mut self) {
        self.candidate = None;
    }

    /// Returns the safe new cursor exactly once when a shorter stream has settled.
    pub fn observe(&mut self, cursor: usize, stream_len: usize, now_ms: u64) -> Option<usize> {
        if cursor <= stream_len {
            self.reset();
            return None;
        }
        match self.candidate {
            Some((candidate, since)) if candidate == stream_len => {
                if now_ms.saturating_sub(since) >= Self::SETTLE_MS {
                    self.reset();
                    Some(stream_len)
                } else {
                    None
                }
            }
            _ => {
                self.candidate = Some((stream_len, now_ms));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(index: i64, play_time_ms: u32) -> CursorEntry {
        CursorEntry {
            index,
            play_time_ms,
        }
    }

    #[test]
    fn fresh_character_never_inherits_an_old_characters_positive_cursor() {
        assert_eq!(
            bind(Some(entry(172, 90_000)), 172, 1_000, Some(true)),
            Binding::Fresh
        );
        assert_eq!(bind(None, 172, 1_000, Some(true)), Binding::Fresh);
    }

    #[test]
    fn same_character_resumes_and_delete_recreate_resets_by_play_time() {
        assert_eq!(
            bind(Some(entry(172, 90_000)), 0, 100_000, Some(false)),
            Binding::Resume(172)
        );
        assert_eq!(
            bind(Some(entry(172, 90_000)), 0, 1_000, Some(false)),
            Binding::Fresh
        );
    }

    #[test]
    fn legacy_cursor_requires_positive_returning_character_evidence() {
        assert_eq!(bind(None, 172, 90_000, None), Binding::Wait);
        assert_eq!(bind(None, 172, 90_000, Some(true)), Binding::Fresh);
        assert_eq!(bind(None, 172, 90_000, Some(false)), Binding::Migrate(172));
    }

    #[test]
    fn shorter_stream_must_settle_before_cursor_is_repaired() {
        let mut guard = AheadGuard::default();
        assert_eq!(guard.observe(172, 100, 1_000), None);
        assert_eq!(
            guard.observe(172, 120, 5_999),
            None,
            "growth restarts the clock"
        );
        assert_eq!(guard.observe(172, 120, 10_998), None);
        assert_eq!(guard.observe(172, 120, 10_999), Some(120));
        assert_eq!(
            guard.observe(120, 121, 11_000),
            None,
            "the next item is no longer skipped"
        );
    }
}
