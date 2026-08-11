//! `toast` — transient on-screen notices for grants the GAME cannot announce.
//!
//! ## Why this exists (2026-07-24 playtest)
//!
//! A Progressive Flask Upgrade arrives and nothing happens on screen. That is not a delivery
//! failure: `flask.rs` reconciles `max_hp_flask` / `max_fp_flask` up to the rung implied by the
//! received count, so no item ever enters the bag and the game's native item-gain ticker has
//! nothing to fire on. (The earlier build that DID show one raised potency by an in-place flask
//! item-id swap, and that CTD'd on death — so "make the game think an item arrived" is a road we
//! already know the end of.)
//!
//! Every client-APPLIED grant has this shape: the effect is real, the feedback is absent. Absence
//! of feedback is indistinguishable from a broken feature — the exact failure CONTRIBUTING's
//! *Runtime visibility* section is about — so those grants get our own overlay notice instead.
//!
//! The queue lives here, pure and host-tested; the client owns only the drawing.

/// The on-screen line for an incoming DeathLink, from the sending slot's name.
///
/// Until 2026-08-11 an incoming DeathLink killed you with **nothing on screen** -- `core.rs` logged
/// `DeathLink received from '<source>'` and that was the entire record. A player who drops dead for
/// no visible reason reads it as a bug in the mod, not as another world's death arriving, and the
/// one place the truth existed was a file they were not looking at.
///
/// 🛑 ASCII, AND THE SOURCE IS NOT OURS TO TRUST. Toasts draw through the FMG path, where a
/// non-ASCII glyph renders as `?` -- and unlike every other toast in this crate the payload here is
/// an ARBITRARY PLAYER-CHOSEN SLOT NAME. Anything outside printable ASCII becomes `?`, and the name
/// is capped, because a 60-character slot name would push the line off the deck. Both are done here,
/// in the pure half, so they are testable and so no caller can forget.
pub fn deathlink_line(source: &str) -> String {
    const MAX_NAME: usize = 24;
    // 🛑 THE QUESTION IS "DID ANYTHING PRINTABLE SURVIVE", NOT "IS THE RESULT NON-EMPTY".
    // Every unprintable char is substituted with `?`, so a name made entirely of them comes out
    // NON-empty and trims to nothing -- `"\u{200b}\u{200b}"` rendered as `killed by ??`, which
    // tells the player exactly as little as an empty quote and reads as our bug either way. So the
    // substitution is TRACKED as it happens rather than inferred from the result afterwards.
    //
    // ⭐ Tracking it also keeps a LITERAL `???` as a name working: `?` is `ascii_graphic`, so a
    // player really called that is named, while a player whose name merely rendered as `?` is not.
    // Trimming `?` out of the result -- the obvious alternative -- cannot tell those two apart.
    let mut printable = false;
    let mut name = String::new();
    for c in source.chars().take(MAX_NAME) {
        if c.is_ascii_graphic() {
            printable = true;
            name.push(c);
        } else if c == ' ' {
            name.push(c);
        } else {
            name.push('?');
        }
    }
    let trimmed = name.trim();
    // A name that is entirely unprintable still has to say SOMETHING true. "someone" is honest; an
    // empty quote, or a row of question marks, reads as a bug in us.
    //
    // ⚠️ LOSSY FOR A GENUINELY NON-ASCII SLOT NAME -- a CJK or emoji name becomes "someone" rather
    // than being transliterated. That is deliberate under the ASCII-only in-game text rule: between
    // `killed by ??` and `killed by someone`, neither names them and only one reads as intentional.
    // If naming them matters, the answer is a different renderer, not a different fallback.
    let name = if printable && !trimmed.is_empty() {
        trimmed
    } else {
        "someone"
    };
    format!("DeathLink: killed by {name}")
}

/// One notice, with the timestamp it was raised at (caller's monotonic ms).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub born_ms: u64,
}

/// A tiny fixed-capacity, time-expiring notice queue. Oldest is dropped when full: a burst must
/// never push the newest notice off screen, and it must never grow without bound.
#[derive(Debug)]
pub struct Deck {
    items: Vec<Toast>,
    cap: usize,
    ttl_ms: u64,
}

impl Deck {
    pub fn new(cap: usize, ttl_ms: u64) -> Self {
        Deck {
            items: Vec::new(),
            cap: cap.max(1),
            ttl_ms,
        }
    }

    /// Raise a notice. Identical text raised again within its lifetime REFRESHES the existing one
    /// rather than stacking a duplicate (a reconciler that re-applies each tick must not spam).
    pub fn push(&mut self, text: impl Into<String>, now_ms: u64) {
        let text = text.into();
        if let Some(existing) = self.items.iter_mut().find(|t| t.text == text) {
            existing.born_ms = now_ms;
            return;
        }
        self.items.push(Toast {
            text,
            born_ms: now_ms,
        });
        while self.items.len() > self.cap {
            self.items.remove(0);
        }
    }

    /// Drop expired notices. Call once per frame before reading [`Deck::visible`].
    pub fn expire(&mut self, now_ms: u64) {
        let ttl = self.ttl_ms;
        self.items
            .retain(|t| now_ms.saturating_sub(t.born_ms) < ttl);
    }

    pub fn visible(&self) -> &[Toast] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Fade factor in `0.0..=1.0` for a notice: solid until the last quarter of its life, then out.
    pub fn alpha(&self, t: &Toast, now_ms: u64) -> f32 {
        let age = now_ms.saturating_sub(t.born_ms);
        if age >= self.ttl_ms {
            return 0.0;
        }
        let fade_from = self.ttl_ms - self.ttl_ms / 4;
        if age <= fade_from {
            return 1.0;
        }
        let left = (self.ttl_ms - age) as f32;
        let span = (self.ttl_ms - fade_from).max(1) as f32;
        (left / span).clamp(0.0, 1.0)
    }
}

/// How many NEW client-applied grants to announce, given the previous observed count and the
/// current one.
///
/// `prev == None` means "not primed yet": the first observation after a connect establishes the
/// baseline and announces NOTHING. That matters because the flask reconciler is history-agnostic —
/// on every connect the count jumps 0 -> N for grants the player received hours ago, and toasting
/// those would be a lie on every reconnect. Only a genuine increase after priming is announced;
/// a decrease (seed change, re-snapshot) re-primes silently.
pub fn new_grants(prev: Option<usize>, now: usize) -> Option<usize> {
    match prev {
        None => None,
        Some(p) if now > p => Some(now - p),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn deathlink_line_is_always_ascii_and_bounded() {
        // The payload is an arbitrary player-chosen slot name, so the interesting inputs are the
        // hostile ones. Each must still produce a drawable line.
        for src in [
            "bobler",
            "Alaric",
            "Kalé",   // the accent this repo has been bitten by before
            "плеер",  // wholly non-latin
            "🐺🐺🐺", // emoji
            "   ",    // whitespace only
            "",       // empty
            "a-very-long-slot-name-that-would-run-off-the-toast-deck-entirely",
        ] {
            let line = deathlink_line(src);
            assert!(line.is_ascii(), "non-ASCII line for {src:?}: {line}");
            assert!(line.len() <= 45, "line too long for {src:?}: {line}");
            assert!(line.starts_with("DeathLink: killed by "), "{line}");
            // WITNESS: a line that degenerated to the prefix alone would pass the three assertions
            // above and tell the player nothing.
            assert!(
                line.len() > "DeathLink: killed by ".len(),
                "no name at all for {src:?}"
            );
        }
    }

    #[test]
    fn deathlink_line_keeps_an_ordinary_name_verbatim() {
        // The sanitiser must not be so eager that it mangles the common case.
        assert_eq!(deathlink_line("bobler"), "DeathLink: killed by bobler");
    }

    #[test]
    fn deathlink_line_never_renders_an_empty_quote() {
        // An all-unprintable name used to be the shape that produced `killed by ` with nothing
        // after it, which reads as our bug rather than as their name.
        for src in ["", "   ", "\u{200b}\u{200b}"] {
            assert_eq!(
                deathlink_line(src),
                "DeathLink: killed by someone",
                "src={src:?}"
            );
        }
    }

    use super::*;

    #[test]
    fn a_reconnect_does_not_toast_grants_the_player_already_had() {
        // THE BUG this guards: flask counting is history-agnostic, so every connect re-observes
        // the full count. Priming must swallow the first observation.
        assert_eq!(new_grants(None, 7), None, "first observation only primes");
        assert_eq!(new_grants(Some(7), 7), None, "steady state is silent");
        assert_eq!(new_grants(Some(7), 9), Some(2), "a real increase announces");
        assert_eq!(
            new_grants(Some(9), 2),
            None,
            "a decrease re-primes silently"
        );
    }

    #[test]
    fn a_reconciler_re_raising_the_same_notice_refreshes_it() {
        let mut d = Deck::new(4, 1000);
        d.push("Flask upgraded", 0);
        d.push("Flask upgraded", 400);
        assert_eq!(d.visible().len(), 1, "no duplicate stacking");
        d.expire(1200);
        assert_eq!(d.visible().len(), 1, "refreshed, so still alive at 1200ms");
        d.expire(1500);
        assert!(d.is_empty(), "and gone once the REFRESHED lifetime elapses");
    }

    #[test]
    fn a_burst_keeps_the_newest_not_the_oldest() {
        let mut d = Deck::new(2, 1000);
        d.push("one", 0);
        d.push("two", 10);
        d.push("three", 20);
        let texts: Vec<&str> = d.visible().iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["two", "three"], "oldest dropped, newest kept");
    }

    #[test]
    fn notices_expire_and_fade() {
        let mut d = Deck::new(4, 1000);
        d.push("x", 0);
        let t = d.visible()[0].clone();
        assert_eq!(d.alpha(&t, 0), 1.0);
        assert_eq!(d.alpha(&t, 750), 1.0, "solid until the last quarter");
        assert!(d.alpha(&t, 900) < 1.0 && d.alpha(&t, 900) > 0.0, "fading");
        assert_eq!(d.alpha(&t, 1000), 0.0);
        d.expire(1000);
        assert!(d.is_empty());
    }
}
