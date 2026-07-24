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
    use super::*;

    #[test]
    fn a_reconnect_does_not_toast_grants_the_player_already_had() {
        // THE BUG this guards: flask counting is history-agnostic, so every connect re-observes
        // the full count. Priming must swallow the first observation.
        assert_eq!(new_grants(None, 7), None, "first observation only primes");
        assert_eq!(new_grants(Some(7), 7), None, "steady state is silent");
        assert_eq!(new_grants(Some(7), 9), Some(2), "a real increase announces");
        assert_eq!(new_grants(Some(9), 2), None, "a decrease re-primes silently");
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
