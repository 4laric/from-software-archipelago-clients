//! `shop_hints` -- PHASE 2 of shop auto-hints (er-archipelago#455). The player opens a merchant,
//! and every Archipelago check on that shelf is announced to the multiworld as a hint.
//!
//! This is the I/O half. Every rule about WHICH slots qualify lives in `er_logic::shop_hints`,
//! pure and unit-tested on any host; this module only reads the live game and drives the socket.
//!
//! ## The seam, and why we can trust it now
//!
//! Phase 1's log-only probe answered the one question the design could not assume. boblerrr,
//! 2026-08-08: `esd: SHOP talk 600001110 cmd 22 args [Int32(101800), Int32(101897)]` -- command 22
//! fires at a real merchant on this build, and its two arguments are a real `ShopLineupParam` row
//! range (93 of those 98 ids are in our own shop table, and the same pair appears verbatim as
//! `OpenRegularShop(101800, 101897)` in the decompiled talkscript). The detour therefore stops being
//! a probe and becomes the feature's trigger.
//!
//! ## 🛑 Two threads, so the detour does not send anything
//!
//! [`on_shop_open`] runs INSIDE the game's ESD dispatch, on the game thread, with no socket in
//! scope. It reads params, plans, and queues. [`pump`] runs on the tick where a `&mut Client` is
//! free and does the sending -- exactly the take/put-back shape `scout_proof` and `lock_hints`
//! already use. One `create_hints` call per shop open, never per row: the server does a full
//! `ctx.save()` for every hint-creating packet.
//!
//! ## 🛑 Nothing new travels in slot data
//!
//! The `stock flag -> AP location` table is the seed's existing check table, inverted -- the same
//! inversion `shop_sell` and `shop_repoint` already do. No `merchant_shops.tsv`, no contract key, so
//! `CONTRACT_HASH` does not move and an older apworld plays a newer client unchanged.
//!
//! ## Known limit, stated out loud
//!
//! Only the regular buy menu (command 22) triggers. Hewg's Ash-of-War shop, the Twin Maidens'
//! tailoring, weapon upgrading and change-of-purpose all open through DIFFERENT commands whose ids
//! nobody has observed yet -- Hewg produced a 20-command dump in the 08-08 session and no shop line
//! at all. Those merchants do not auto-hint, and the connect banner says so rather than letting a
//! partial feature read as a complete one.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use archipelago_rs as ap;
use eldenring::cs::{ShopLineupParam, SoloParamRepository};
use er_logic::shop_hints::{MAX_RANGE_ROWS, ShopRow, normalize_range, plan_shop_hints};
use fromsoftware_shared::FromStatic;

/// `eventFlag_forStock -> AP location id`, inverted from the seed's check table at connect.
/// `None` until slot_data is parsed -- a shop opened before then hints nothing and says why.
static FLAG_TO_LOC: Mutex<Option<HashMap<u32, i64>>> = Mutex::new(None);

/// Locations already announced this session. `None` until first use (`HashSet::new` is not `const`).
///
/// 🛑 Deliberately NOT persisted across loads or reconnects (DS3 precedent, `core.rs:36-39`): it is
/// a politeness throttle, not a currency. If a hint is lost the player quits out and every hint is
/// re-sent. Contrast `lock_hints`, whose ledger is server-side precisely because it IS a currency.
static HINTED: Mutex<Option<HashSet<i64>>> = Mutex::new(None);

/// One entry per shop open, drained by [`pump`]. Batched, never flattened -- see the module header.
static QUEUE: Mutex<Vec<Vec<i64>>> = Mutex::new(Vec::new());

/// Set when the ESD detour could not be installed. The connect banner MUST render this: a feature
/// that silently does nothing on an unsupported build is a silent regression.
static INACTIVE: AtomicBool = AtomicBool::new(false);

/// Called at slot_data parse with the seed's `{ap_location_id: check_flag}` table.
///
/// Re-arms the session ledger: a reconnect is a new session, and re-announcing a shelf costs one
/// packet the server dedups anyway, while NOT re-announcing after a lost connection loses the hint
/// for good.
pub fn configure(loc_flags: HashMap<i64, u32>) {
    let mut inverted: HashMap<u32, i64> = HashMap::with_capacity(loc_flags.len());
    for (loc, flag) in loc_flags {
        if flag != 0 {
            inverted.insert(flag, loc);
        }
    }
    let n = inverted.len();
    if let Ok(mut g) = FLAG_TO_LOC.lock() {
        *g = Some(inverted);
    }
    if let Ok(mut h) = HINTED.lock() {
        *h = Some(HashSet::new());
    }
    if let Ok(mut q) = QUEUE.lock() {
        q.clear();
    }
    log::info!(
        "shop-hints: configured {n} check flag(s); opening a merchant will hint its unbought, \
         released slots (buy menu only -- Ash of War / tailoring / upgrade / change-of-purpose \
         open through commands whose ids are not known yet)"
    );
}

/// Record that the ESD detour did not install, so the connect banner can say the feature is off.
pub fn mark_inactive() {
    INACTIVE.store(true, Ordering::Relaxed);
}

/// Whether shop auto-hints are dead for this session (the detour never installed).
pub fn is_inactive() -> bool {
    INACTIVE.load(Ordering::Relaxed)
}

/// A merchant opened its buy menu over `ShopLineupParam` rows `[begin, end]`.
///
/// Runs on the game thread inside the ESD dispatch. Reads only; queues only. Never panics past its
/// own frame -- the caller wraps it, and every lock here degrades to silence rather than `unwrap`,
/// because a poisoned mutex must not be able to freeze a conversation.
pub fn on_shop_open(begin: i32, end: i32) {
    let Some((lo, hi)) = normalize_range(begin, end) else {
        log::warn!(
            "shop-hints: refusing shop-open range [{begin}, {end}] -- not a walkable \
             ShopLineupParam span (inclusive, ascending, at most {MAX_RANGE_ROWS} rows). No hints \
             for this open; the arguments are not what we believe command 22 carries."
        );
        return;
    };

    let Ok(flag_guard) = FLAG_TO_LOC.lock() else {
        return;
    };
    let Some(flag_to_loc) = flag_guard.as_ref() else {
        log::info!(
            "shop-hints: shop opened over rows {lo}..={hi} before slot_data was parsed (not \
             connected yet?) -- nothing hinted"
        );
        return;
    };
    if flag_to_loc.is_empty() {
        return; // seed has no check flags at all: no shop checks to hint, and nothing to say.
    }

    // SAFETY: FD4 singleton, read-only, on the game thread. Same sanctioned access `shop_flags`
    // uses; no mutable borrow is taken, so this cannot alias the tick's `instance_mut()` passes.
    let Ok(repo) = (unsafe { SoloParamRepository::instance() }) else {
        log::warn!("shop-hints: param repository not up at shop open -- nothing hinted");
        return;
    };
    // Walk the OPENED RANGE, not the whole table: the range is the shelf, it is bounded by
    // `normalize_range`, and a full-table scan inside the game's dispatch frame is work we would be
    // doing every time a merchant is greeted.
    let mut rows: Vec<ShopRow> = Vec::new();
    for id in lo..=hi {
        if let Some(row) = repo.get::<ShopLineupParam>(id) {
            rows.push(ShopRow {
                id,
                stock_flag: row.event_flag_for_stock(),
                release_flag: row.event_flag_for_release(),
            });
        }
    }

    let Ok(mut hinted_guard) = HINTED.lock() else {
        return;
    };
    let hinted = hinted_guard.get_or_insert_with(HashSet::new);
    let plan = plan_shop_hints(
        (lo, hi),
        &rows,
        flag_to_loc,
        &crate::flags::get_event_flag,
        hinted,
    );
    let t = plan.tally;
    // TALLY, not a bare count. A shelf that hints nothing because everything on it is bought and a
    // shelf that hints nothing because the feature is broken are the same log line without this.
    log::info!(
        "shop-hints: shop open rows {lo}..={hi} ({} live) -> {} hint(s) {:?} | skipped: \
         vanilla {} unreleased {} bought {} unknown-flag {} already-hinted {}",
        rows.len(),
        plan.locations.len(),
        plan.locations,
        t.not_a_check,
        t.not_released,
        t.purchased,
        t.unknown_flag,
        t.already_hinted,
    );
    if plan.locations.is_empty() {
        return;
    }
    // Claim the locations BEFORE the send. Re-opening the shelf a frame later must not queue them
    // twice; `pump` un-claims on a send failure, which is the only path that can lose one.
    hinted.extend(plan.locations.iter().copied());
    if let Ok(mut q) = QUEUE.lock() {
        q.push(plan.locations);
    }
}

/// Send whatever the game thread queued. Call from the tick, where `&mut Client` is free.
pub fn pump(client: &mut ap::Client<serde_json::Value>) {
    let batches: Vec<Vec<i64>> = match QUEUE.lock() {
        Ok(mut q) if !q.is_empty() => std::mem::take(&mut *q),
        _ => return,
    };
    for batch in batches {
        match client.create_hints(batch.iter().copied()) {
            Ok(()) => log::info!(
                "shop-hints: announced {} shop location(s) to the multiworld: {batch:?}",
                batch.len()
            ),
            Err(e) => {
                // Un-claim so the next open retries. Losing a hint silently is the failure mode
                // this feature exists to avoid, and the shelf is re-openable.
                log::warn!(
                    "shop-hints: create_hints failed ({e}); un-claiming {} location(s) so the \
                     next shop open re-sends them",
                    batch.len()
                );
                if let Ok(mut h) = HINTED.lock()
                    && let Some(set) = h.as_mut()
                {
                    for loc in &batch {
                        set.remove(loc);
                    }
                }
            }
        }
    }
}

/// Drop the session ledger (reconnect / seed change). Same reasoning as `configure`.
pub fn reset() {
    if let Ok(mut h) = HINTED.lock() {
        *h = Some(HashSet::new());
    }
    if let Ok(mut q) = QUEUE.lock() {
        q.clear();
    }
}
