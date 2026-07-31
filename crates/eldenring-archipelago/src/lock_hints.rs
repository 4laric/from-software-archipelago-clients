//! The region-lock hint LEDGER: what the player has already paid to reveal.
//!
//! Pricing and the buy/deny decision are pure and live in `er_logic::lock_hint_economy`. This module
//! owns only the two things that need a live socket: reading the ledger back on connect, and
//! committing a purchase.
//!
//! # Why the ledger lives on the SERVER
//!
//! Earn is already server-authoritative (`checked_locations ∩ progression_surface`). If spend were
//! local, reconnecting would reset it and every hint would be free — so spend has to be server-side
//! too. Archipelago's data storage is exactly that: `Set`/`Get` on an arbitrary key, persisted in
//! the room's `.apsave` (MultiServer.py:673), surviving client restart, save reload and server
//! restart. The client then holds no currency at all, only a derivation over two server facts, and
//! `er_logic::lock_hint_economy::reconnecting_cannot_mint_credit` pins that.
//!
//! This DELIBERATELY diverges from `shop hints`' session-scoped dedupe. That one is a politeness
//! throttle and is intentionally not persisted; this is a currency.
//!
//! # 🛑 Two ordering rules, both load-bearing
//!
//! 1. **Ledger first, hint second.** If we hinted first and crashed, the player would hold a reveal
//!    they never paid for. Charged-but-unhinted is the safe side of that window, and it self-heals:
//!    every connect re-issues `CreateAsHint::New` for ledger entries, which the server ignores when
//!    a hint already stands.
//! 2. **`DataStorageOperation::Appends`, NOT `Add`.** `Add(f64)` is *numeric addition*; appending to
//!    an array is `Appends(Vec<Value>)`. They serialise to the same wire name `"add"`, which makes
//!    the mistake invisible in the protocol docs — it is a Rust type error waiting on the server.

use archipelago_rs as ap;
use ap::{CreateAsHint, DataStorageOperation};
use oneshot::TryRecvError;
use serde_json::Value;
use std::collections::HashMap;

/// Data-storage key holding this slot's paid-for lock hints.
fn ledger_key(slot: u32) -> String {
    format!("er_lockhints_{slot}")
}

/// Server-backed record of which lock locations this slot has bought.
pub struct LockHints {
    key: Option<String>,
    /// Location ids already paid for. Authoritative once `loaded`.
    ledger: Vec<i64>,
    loaded: bool,
    get_rx: Option<oneshot::Receiver<Result<HashMap<String, Value>, ap::Error>>>,
    /// Purchases the UI has requested but we have not committed yet.
    queue: Vec<i64>,
    /// Re-issued the standing hints for existing ledger entries this session?
    reconciled: bool,
    /// A commit is in flight; the UI disables the button so two clicks cannot double-charge.
    committing: bool,
}

impl Default for LockHints {
    fn default() -> Self {
        Self::new()
    }
}

impl LockHints {
    pub fn new() -> Self {
        Self {
            key: None,
            ledger: Vec::new(),
            loaded: false,
            get_rx: None,
            queue: Vec::new(),
            reconciled: false,
            committing: false,
        }
    }

    /// How many hints have been bought. Feeds `lock_hint_economy::balance`.
    pub fn purchases(&self) -> u64 {
        self.ledger.len() as u64
    }

    /// The ledger has been read back from the server. Until then the UI must NOT offer a purchase:
    /// a balance computed against an empty ledger would look like free money.
    pub fn is_ready(&self) -> bool {
        self.loaded && !self.committing
    }

    /// Locations already paid for, so the caller can treat them as hinted even before the server's
    /// hint broadcast comes back.
    pub fn bought(&self) -> &[i64] {
        &self.ledger
    }

    /// Request a purchase. Idempotent per location; committed on the next `pump`.
    pub fn buy(&mut self, location: i64) {
        if self.ledger.contains(&location) || self.queue.contains(&location) {
            return;
        }
        self.queue.push(location);
    }

    /// Drop everything on seed change / disconnect. The ledger is re-read from the server, so this
    /// loses nothing — and NOT clearing it would carry one seed's purchases into another.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Call once per serve-loop iteration with a live client (mirrors `ScoutProof::pump`).
    pub fn pump(&mut self, client: &mut ap::Client<Value>) {
        // 1) Key, once we know our own slot.
        if self.key.is_none() {
            self.key = Some(ledger_key(client.this_player().slot()));
        }
        let key = self.key.clone().expect("key set above");

        // 2) Read the ledger back exactly once.
        if !self.loaded {
            if self.get_rx.is_none() {
                self.get_rx = Some(client.get([key.clone()]));
                return; // the reply cannot be here yet
            }
            let Some(rx) = self.get_rx.as_mut() else {
                return;
            };
            match rx.try_recv() {
                Ok(Ok(map)) => {
                    self.get_rx = None;
                    self.ledger = map
                        .get(&key)
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_i64()).collect())
                        .unwrap_or_default();
                    self.loaded = true;
                    log::info!(
                        "lock hints: ledger loaded from {key} -- {} hint(s) already bought",
                        self.ledger.len()
                    );
                }
                Ok(Err(e)) => {
                    // FAIL CLOSED. An unreadable ledger must not read as "nothing bought", or the
                    // player gets their purchases back for free every time the server hiccups.
                    self.get_rx = None;
                    log::warn!(
                        "lock hints: could not read {key} ({e}) -- hint purchases stay disabled \
                         this session rather than re-granting spent credit"
                    );
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.get_rx = None;
                    log::warn!("lock hints: ledger request dropped; purchases stay disabled");
                }
            }
            return;
        }

        // 3) Re-assert the standing hints for everything already paid for. Idempotent: the server
        //    ignores a hint that already exists, and `New` skips already-hinted locations outright.
        //    This is what heals a charged-but-unhinted crash window.
        if !self.reconciled {
            self.reconciled = true;
            if !self.ledger.is_empty() {
                log::info!(
                    "lock hints: re-asserting {} standing hint(s) from the ledger",
                    self.ledger.len()
                );
                let _ = client.scout_locations(self.ledger.clone(), CreateAsHint::New);
            }
        }

        // 4) Commit queued purchases. 🛑 LEDGER FIRST, then the hint.
        if self.queue.is_empty() {
            self.committing = false;
            return;
        }
        let pending: Vec<i64> = std::mem::take(&mut self.queue);
        for loc in pending {
            self.committing = true;
            match client.change(
                key.clone(),
                Value::Array(Vec::new()),
                [DataStorageOperation::Appends(vec![Value::from(loc)])],
                false,
            ) {
                Ok(()) => {
                    self.ledger.push(loc);
                    let _ = client.scout_locations(vec![loc], CreateAsHint::New);
                    log::info!("lock hints: bought a hint for location {loc}");
                }
                Err(e) => {
                    // Not charged, so not hinted. Re-queue nothing: the player can click again.
                    log::warn!("lock hints: purchase of {loc} failed ({e}); nothing was charged");
                }
            }
        }
        self.committing = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ledger_key_is_per_slot() {
        // Two slots in one room must not share a purse.
        assert_eq!(ledger_key(1), "er_lockhints_1");
        assert_ne!(ledger_key(1), ledger_key(2));
    }

    #[test]
    fn buying_is_idempotent_per_location() {
        let mut lh = LockHints::new();
        lh.buy(42);
        lh.buy(42);
        assert_eq!(lh.queue, vec![42], "a double click must not double-charge");
        lh.ledger.push(7);
        lh.buy(7);
        assert_eq!(lh.queue, vec![42], "an already-bought location is never re-queued");
    }

    #[test]
    fn nothing_is_offered_until_the_ledger_has_been_read() {
        // 🛑 The failure mode that would hand out free hints: an unread ledger looks like zero
        // purchases, so the balance looks full. is_ready() gates the whole UI on the read landing.
        let mut lh = LockHints::new();
        assert!(!lh.is_ready());
        assert_eq!(lh.purchases(), 0);
        lh.loaded = true;
        assert!(lh.is_ready());
        lh.committing = true;
        assert!(!lh.is_ready(), "a commit in flight must close the button");
    }

    #[test]
    fn reset_forgets_the_previous_seed() {
        let mut lh = LockHints::new();
        lh.loaded = true;
        lh.ledger.push(99);
        lh.reset();
        assert!(!lh.is_ready());
        assert_eq!(lh.purchases(), 0);
        assert!(lh.bought().is_empty());
    }
}
