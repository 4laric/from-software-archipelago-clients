//! Region Sync (er-archipelago#1005) -- the game-side half.
//!
//! Session state for the `RegionSync` link: the parsed toggle, the inbound queue, and the set of
//! regions this session opened BECAUSE of the link. The wire shape and the two anti-loop decisions
//! are host-tested in `er_logic::region_sync`; nothing here decides anything.
//!
//! 🛑 APPLYING IS NOT RECEIVING. `apply_pending` calls the SAME `region::open_on_received_name`
//! that a locally received Lock calls, which sets the region's open flag, its reveal flags and its
//! graces -- and nothing else. The AP item is not granted, the receive watermark does not move, no
//! check is sent, and logic/goal state is untouched. This is the console's
//! `!setflag <region open flag> 1`, automatic and shared.
//!
//! The inbound queue exists because flag writes are silently DISCARDED at menu/load (the same
//! hazard `region::tick_reconcile_received_locks` was written for): an open handed to us during a
//! load screen would otherwise be lost for the session. Entries stay queued until the open flag
//! actually reads back set, so a not-ready apply self-heals on a later tick.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use er_logic::region_sync::Inbound;

static ENABLED: AtomicBool = AtomicBool::new(false);
static PENDING: Mutex<Vec<Inbound>> = Mutex::new(Vec::new());
/// Regions opened here by the link. Feeds `er_logic::region_sync::outbound` so an applied open is
/// never rebroadcast -- see that function for why a round-robin is the failure being prevented.
static APPLIED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Queue one vetted inbound open. Dropped silently when the link is off for this slot, so a stray
/// packet from a mixed session can never open a door the player did not ask to have opened.
pub fn enqueue(inbound: Inbound) {
    if !is_enabled() {
        return;
    }
    if let Ok(mut q) = PENDING.lock()
        && !q.iter().any(|i| i.region == inbound.region)
    {
        q.push(inbound);
    }
}

/// Regions the link has opened here this session.
pub fn applied_snapshot() -> HashSet<String> {
    match APPLIED.lock() {
        Ok(guard) => guard.as_ref().cloned().unwrap_or_default(),
        Err(_) => HashSet::new(),
    }
}

fn note_applied(region: &str) {
    if let Ok(mut g) = APPLIED.lock() {
        g.get_or_insert_with(HashSet::new)
            .insert(region.to_string());
    }
}

/// A new seed (or a reconnect to a different one) invalidates both sets: the region NAMES are the
/// seed's, and an anti-echo entry from a previous seed would silence a genuine open.
pub fn reset() {
    if let Ok(mut q) = PENDING.lock() {
        q.clear();
    }
    if let Ok(mut g) = APPLIED.lock() {
        *g = None;
    }
}

/// Per-tick (settled / in-world): apply queued inbound opens. Returns the console/toast lines for
/// the ones that actually landed this tick.
///
/// An entry is dropped only once its open flag READS BACK set -- the read-back, not the write, is
/// what proves the game accepted it. Anything still unset stays queued for the next tick.
pub fn apply_pending(cfg: &crate::region::RegionConfig) -> Vec<String> {
    let mut lines = Vec::new();
    let Ok(mut queue) = PENDING.lock() else {
        return lines;
    };
    if queue.is_empty() {
        return lines;
    }
    let mut still: Vec<Inbound> = Vec::new();
    for inbound in queue.drain(..) {
        // `region_open_flags` is keyed by LOCK ITEM name; the wire carries the region name, which
        // is that key minus the suffix (the same string the local "Region unlocked: X" line uses).
        let key = format!("{} Lock", inbound.region);
        let Some(&flag) = cfg.region_open_flags.get(&key) else {
            log::warn!(
                "RegionSync: '{}' from {:?} names no region in this seed -- ignored",
                inbound.region,
                inbound.source
            );
            continue;
        };
        if crate::flags::get_event_flag(flag) {
            // Already open here (our own Lock arrived first, or a previous tick landed it). Not a
            // link-caused open, so it is deliberately NOT noted as applied: the anti-echo must not
            // suppress a broadcast of an open this slot made on its own.
            continue;
        }
        crate::region::open_on_received_name(cfg, &key);
        if crate::flags::get_event_flag(flag) {
            note_applied(&inbound.region);
            lines.push(er_logic::region_sync::sync_open_line(
                &inbound.region,
                &inbound.source,
            ));
        } else {
            still.push(inbound);
        }
    }
    *queue = still;
    lines
}
