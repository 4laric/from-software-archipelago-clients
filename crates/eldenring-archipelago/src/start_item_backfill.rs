//! Inventory-verified start-item backfill: grant any `startItems` entry that isn't actually in the
//! bag. A last-resort backstop for whatever the primary start-item paths dropped -- verifies against
//! the BAG, not any bookkeeping flag. Live case: no healing flask on a Roundtable-hub start; the
//! flask WAS in `startItems`, but the RECONCILER (which owns start-item goods, `apply=...,goods,...`)
//! converged without placing it, and the old boolean-gated drain had already stood down. Logic +
//! flask-family handling live in `er_logic::start_backfill`.
//!
//! Runs ONCE per client launch (in-memory latch), in-world, AFTER an in-world SETTLE (so the
//! reconciler/drain have taken their pass first -> on a healthy save it finds nothing missing and
//! never double-grants), snapshotting the inventory fresh each tick. Inventory verification is the
//! anti-double-grant guarantee: anything a primary path placed reads as present and is skipped.
//!
//! # DURABLE-ONLY INVARIANT (2026-08-01)
//!
//! An absent start item is re-granted on the next launch. That is correct precisely because **every
//! shipped `startItems` entry is DURABLE**: flasks (the family ranges cover the empty/charged pairs,
//! so a drained flask still reads present), pot/perfume/hefty vessels (permanent reusable
//! containers whose count only rises), the lantern, whetblades. Possession is therefore a valid
//! "already delivered" signal for every one of them, which is what lets this be the start-item
//! DEDUP rather than a backstop -- and why no per-character key is needed for start items at all.
//!
//! 🛑 It would NOT be valid for a stackable consumable the player used up: the bag cannot tell
//! "never granted" from "granted and consumed" (count the RECEIVED stream, never the bag). The
//! world side asserts the durable-only invariant so a future consumable start item fails a test
//! instead of silently becoming a per-launch refill.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{GameDataMan, ItemCategory};
use er_logic::start_backfill::ScanVerdict;
use fromsoftware_shared::{FromStatic, NonEmptyIteratorExt};

static START_ITEMS: Mutex<Vec<i32>> = Mutex::new(Vec::new());
static DONE: AtomicBool = AtomicBool::new(false);
/// Cross-tick convergence bookkeeping (#248). Owns the per-fid attempt counters, the previous
/// snapshot for the two-tick agreement, and the BAG-CONFIRMED delivered set. All decisions live in
/// the pure `er_logic::start_backfill`; this module only supplies snapshots and outcomes.
static STATE: Mutex<Option<er_logic::start_backfill::BackfillState>> = Mutex::new(None);

/// Set from slot_data `startItems` at connect (the same list `startgrants` parses).
pub fn set_start_items(items: Vec<i32>) {
    if let Ok(mut g) = START_ITEMS.lock() {
        *g = items;
    }
    DONE.store(false, Ordering::Relaxed);
    if let Ok(mut st) = STATE.lock() {
        *st = Some(er_logic::start_backfill::BackfillState::new());
    }
}

/// FullID for a held item id: `(category<<28) | row`, matching the `startItems` / `grant_full_id`
/// encoding (`er_codec` category nibbles).
fn full_id_of(cat: ItemCategory, row: u32) -> u32 {
    let nibble: u32 = match cat {
        ItemCategory::Weapon => 0x0000_0000,
        ItemCategory::Protector => 0x1000_0000,
        ItemCategory::Accessory => 0x2000_0000,
        ItemCategory::Goods => 0x4000_0000,
        ItemCategory::Gem => 0x8000_0000,
    };
    nibble | (row & 0x0FFF_FFFF)
}

/// Per-tick until done. `settled` = the world has loaded + the primary start-item paths have had
/// time to run (in-world settle, same signal `apply_start_flags`/the drain use). Once settled and
/// in-world with the inventory populated, grant any `startItems` NOT in the bag.
///
/// GATE FIX (2026-07-18): originally gated on the persisted `start_items_granted` boolean, on the
/// theory that a stale-TRUE boolean made the old drain skip. WRONG for the live case: start-item
/// GOODS (the flask) are now owned by the RECONCILER (`apply=flags,goods,ledger`), the old drain
/// stands down, so `start_items_granted` never latches TRUE -> the backfill never ran. Now gated on
/// an in-world SETTLE instead, so it runs as a true backstop AFTER the reconciler converges,
/// independent of whichever primary path (drain or reconciler) dropped the item. Inventory
/// verification is what prevents a double-grant: anything the reconciler did place reads as present
/// and is skipped; only genuinely-absent startItems are granted.
pub fn tick(settled: bool) {
    if DONE.load(Ordering::Relaxed) || !settled || !crate::flags::in_world() {
        return;
    }
    let items = match START_ITEMS.lock() {
        Ok(g) if !g.is_empty() => g.clone(),
        _ => return, // no startItems (or lock poisoned) -- nothing to backfill
    };

    // SAFETY: FD4 singleton; read on the single-threaded FrameBegin tick. Same path as inventory.rs.
    let Ok(gdm) = (unsafe { GameDataMan::instance() }) else {
        return;
    };
    let pgd = gdm.main_player_game_data.as_ref();

    // Snapshot the held inventory as FullIDs.
    //
    // MULTIPLAYER KEY-LIST SCAN (fix 2026-07-19; Andrew's co-op reconnect re-granted his base flask).
    // Do NOT use `items_data.items()` here: it follows `key_items_accessor`, which in an ONLINE
    // session switches to `multiplay_key_items` (pots + wondrous physick tears only). The Flask of
    // Crimson Tears and other key-item startItems live in the always-single-player `key_items` list,
    // so `items()` reads them as ABSENT in co-op -> the backfill re-grants a flask the player already
    // holds. Scan all three backing lists (normal + always-SP key + multiplay key) so a held item is
    // always seen. (Sold/discarded items still read absent and are re-granted -- acceptable per the
    // backstop's per-launch design.)
    let inv = &pgd.equipment.equip_inventory_data.items_data;
    let mut present: HashSet<u32> = HashSet::new();
    // Per-list counts, logged with every scan: when a scan reads implausibly small (the 17-id
    // incident) this says WHICH list came up short, instead of leaving it to be guessed.
    let counts = (
        inv.normal_entries().len(),
        inv.key_entries().len(),
        inv.multiplay_key_entries().len(),
    );
    for entry in inv
        .normal_entries()
        .iter()
        .chain(inv.key_entries().iter())
        .chain(inv.multiplay_key_entries().iter())
        .non_empty()
    {
        // `entry.item_id` is a valid `ItemId` here (not `OptionalItemId`), so category()/param_id()
        // return the values directly -- same access inventory.rs::scan_synthetics uses.
        present.insert(full_id_of(
            entry.item_id.category(),
            entry.item_id.param_id(),
        ));
    }
    if present.is_empty() {
        return; // inventory holder not populated yet -- retry next tick (don't latch)
    }

    // CONVERGENCE LOOP (#248). The decision is pure; this block is glue.
    //
    // The old code scanned once, granted, counted `grant_full_id() == true` as delivered, and
    // latched DONE unconditionally. All three were wrong: the scan could run against a filling bag
    // (the 17-id scan), a capped-to-zero pot returns true, and hard failures were never retried.
    let Ok(mut guard) = STATE.lock() else { return };
    let st = guard.get_or_insert_with(er_logic::start_backfill::BackfillState::new);

    // Fold this snapshot into the delivered set FIRST: an item we asked for last tick and can now
    // SEE is delivered. This -- not the grant call's return value -- is what may be reported.
    st.confirm(&present);

    match st.observe(&present, &items) {
        ScanVerdict::Unsettled => {
            // Bag empty or still changing between ticks. Do NOT latch, do NOT read absences off it.
        }
        ScanVerdict::Converged => {
            // #308 -- CONVERGED IS NOT UNCONDITIONALLY GOOD NEWS.
            //
            // The scan is PRESENCE-based on purpose: counting quantity would re-grant a stack the
            // player has merely used (`present_stack_is_not_topped_up`). But that means one
            // delivered copy makes N requested copies look satisfied, and a grant the pot cap ate
            // is invisible to it. Alaric's 2026-08-03 log, timestamps unedited:
            //
            //   16:54:29 9/40 startItems absent -> attempting ["0x401ea99c" x9]
            //   16:54:29 grant 0x401ea99c -> Placed
            //   16:54:29 CONVERGED -- all 40 startItems present in bag. granted 1 this session
            //   16:54:30 [WARN] pot-cap: grant of 1 CAPPED to 0 (held 10, cap 10)
            //
            // Converged in the same second, after one grant of nine. `GrantOutcome::Capped` is the
            // ONLY evidence the rest never landed, so say so here rather than reporting a clean
            // success the bag cannot contradict.
            let shortfall = st.capped_shortfall();
            if shortfall.is_empty() {
                log::info!(
                    "start-item backfill: CONVERGED -- all {} startItems present in bag ({} inventory id(s) scanned: {} normal, {} key, {} multiplay). granted {} this session",
                    items.len(),
                    present.len(),
                    counts.0,
                    counts.1,
                    counts.2,
                    st.confirmed_count()
                );
            } else {
                log::warn!(
                    "start-item backfill: converged with a SHORTFALL -- every startItem is PRESENT, \
                     but {} id(s) hit a delivery cap and the extra copies were never added: {:02x?}. \
                     The server counts them delivered. This is a SEED issue, not a client one: the \
                     start-item list asks for more of a capped good than the game will hold (#308).",
                    shortfall.len(),
                    shortfall
                );
            }
            DONE.store(true, Ordering::Relaxed);
        }
        ScanVerdict::Grant(missing) => {
            log::info!(
                "start-item backfill: {}/{} startItems absent ({} inventory id(s): {} normal, {} key, {} multiplay) -> attempting {:?}",
                missing.len(),
                items.len(),
                present.len(),
                counts.0,
                counts.1,
                counts.2,
                missing
                    .iter()
                    .map(|&f| format!("{:#010x}", f as u32))
                    .collect::<Vec<_>>()
            );
            for &fid in &missing {
                let outcome = crate::detour::grant_full_id_outcome(fid, 1);
                st.record(fid, outcome);
                log::info!(
                    "start-item backfill: grant {:#010x} -> {outcome:?}",
                    fid as u32
                );
            }
            // No latch. The NEXT tick's snapshot decides whether any of that actually landed.
        }
        ScanVerdict::Exhausted(failed) => {
            // FAIL LOUD. These were attempted MAX_ATTEMPTS times and no snapshot ever showed them.
            // The old code's silence here is exactly what produced "granted 22/32".
            log::warn!(
                "start-item backfill: FAILED to deliver {} startItem(s) after {} attempts each -- {:?} \
                 are NOT in the bag and will not be retried this session. If these are pots, the \
                 delivery cap swallowed them; check the yaml against POT_DELIVERY_CAPS",
                failed.len(),
                er_logic::start_backfill::MAX_ATTEMPTS,
                failed
                    .iter()
                    .map(|&f| format!("{:#010x}", f as u32))
                    .collect::<Vec<_>>()
            );
            DONE.store(true, Ordering::Relaxed);
        }
    }
}
