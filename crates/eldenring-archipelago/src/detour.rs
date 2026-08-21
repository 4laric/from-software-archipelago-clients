//! `AddItemFunc` detour — ECHO model (`own_world:true`). Supersedes the stage-2 local-grant detour.
//!
//! Self-found synthetic pickup → report the check + SUPPRESS the world pickup, and let the server
//! ECHO the item back as a received item (the `update_live` received-item path grants it, running
//! progressive / region-open / notify by name). The detour does NOT grant locally — that's what kept
//! self-found progressive/region/notify from working under the old `own_world:false` local-grant.
//! `grant_full_id` stays (used by the received-item path). RVA + signature pinned to 2.6.2.0.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use eldenring::cs::GameDataMan;
use fromsoftware_shared::FromStatic;
use retour::GenericDetour;

use er_codec::{decode_synthetic, is_synthetic_goods, row_id_of};

use crate::params;

type AddItemFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, u64) -> u64;

static HOOK: OnceLock<GenericDetour<AddItemFn>> = OnceLock::new();
/// Live inventory pointer captured on every pickup; reused to grant items that never pass a pickup.
static LAST_INVENTORY: AtomicUsize = AtomicUsize::new(0);
/// World epoch the pointer in `LAST_INVENTORY` was captured in. See [`WORLD_EPOCH`].
static LAST_INVENTORY_EPOCH: AtomicU64 = AtomicU64::new(0);
/// Bumped on every in-world false->true edge (load / save-load / warp arrival / respawn), i.e.
/// every point at which the game may have freed and rebuilt the inventory object. A pointer
/// captured in an older epoch is DEAD -- handing it to the game's AddItemFunc is a use-after-free,
/// and it is what crashed the 2026-07-24 playtest twice (er_logic::inv_ptr, replay-tested).
static WORLD_EPOCH: AtomicU64 = AtomicU64::new(0);
/// Raw `AddItemFunc` return values, keyed by the good the grant was FOR.
///
/// `grant_item` used to drop the game's `u64` on the floor, so `grant_full_id_outcome` reported
/// `Placed` for every call it dispatched -- including the ones the game refused and dropped at the
/// player's feet. The reconciler then re-granted the good until `MAX_GRANT_ATTEMPTS` parked it, and
/// re-armed after every world edge, forever (bobler 2026-08-04, goods 0x4000230c). This records
/// what the call actually returned so the stall log can NAME it.
///
/// It deliberately does not interpret the value -- see `er_logic::add_item_probe` for why, and for
/// the keyed-by-good rule that stops a stall quoting some other grant's return.
static ADD_ITEM_PROBE: Mutex<Option<er_logic::add_item_probe::AddItemProbe>> = Mutex::new(None);
/// World epoch the probe's records belong to; a bump clears them, so a post-load stall never
/// quotes a pre-load return.
static ADD_ITEM_PROBE_EPOCH: AtomicU64 = AtomicU64::new(0);
/// Rust panics caught at the AddItem detour boundary during this client process. Keeping the count
/// in the diagnostic turns a repeated fail-open into a visible escalating fault instead of an
/// indistinguishable stream of one-off warnings.
static ADD_ITEM_PANIC_COUNT: AtomicU64 = AtomicU64::new(0);

/// Collection state used by the AddItem boundary must remain usable after a contained panic.
/// `Mutex::lock().unwrap()` would make one panic poison the mutex and every later pickup panic in
/// turn, silently reducing the detour to its vanilla pass-through fallback for the rest of the
/// session. The protected values remain structurally valid, so recover their guards.
fn recover_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Remember a DISPATCHED call's raw return. Never called for a grant that did not reach the game.
fn record_add_item_return(full_id: i32, ret: u64) {
    let epoch = WORLD_EPOCH.load(Ordering::Relaxed);
    let Ok(mut guard) = ADD_ITEM_PROBE.lock() else {
        return;
    };
    let probe = guard.get_or_insert_with(er_logic::add_item_probe::AddItemProbe::new);
    if ADD_ITEM_PROBE_EPOCH.swap(epoch, Ordering::Relaxed) != epoch {
        probe.clear();
    }
    probe.record(full_id, ret);
}

/// What `AddItemFunc` last returned for `full_id`, rendered for the stall log. `NEVER DISPATCHED`
/// is a distinct and meaningful answer: it means the grant never reached the game at all, which
/// indicts the inventory pointer / hook / pot cap rather than a refusal.
pub fn add_item_return_for(full_id: i32) -> String {
    match ADD_ITEM_PROBE.lock() {
        Ok(guard) => match guard.as_ref() {
            Some(p) => er_logic::add_item_probe::describe(p, full_id),
            None => "add_item_ret=NEVER DISPATCHED".to_string(),
        },
        Err(_) => "add_item_ret=UNAVAILABLE (probe lock poisoned)".to_string(),
    }
}
/// `now_ms()` timestamp of the most recent warp REQUEST (LuaWarp detour), or
/// [`er_logic::inv_ptr::NEVER_WARPED`] if none yet. From the request until
/// `inv_ptr::PRIME_HOLDOFF_MS` later, the static-slot primer sits out: the slot still points at
/// the ORIGIN map's inventory object while the engine frees it, and a prime in that window would
/// recapture the dying object in the current epoch -- reopening the exact use-after-free the
/// warp-request epoch bump just closed (er_logic::inv_ptr::may_prime, replay-tested).
static LAST_WARP_REQUEST_MS: AtomicU64 = AtomicU64::new(er_logic::inv_ptr::NEVER_WARPED);

/// Process-relative monotonic clock for the warp-request holdoff (same shape as scaling.rs's).
fn now_ms() -> u64 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u64
}
/// checkItemFlags from slot_data: full AddItemFunc-space item id -> acquisition flags of the check
/// locations that vanilla-hold it. LIVE vanilla-suppressor since 2026-07-01. The re-pickup
/// discriminator is now the COLLECTED set (KNOWN_COLLECTED_FLAGS), not the live game flag —
/// see that static for why the old live-flag heuristic leaked.
static CHECK_ITEM_FLAGS: Mutex<Option<HashMap<u32, Vec<u32>>>> = Mutex::new(None);

/// Acquisition flags of check locations the client has ALREADY reported as collected (the server
/// checked-set, bridged loc->flag through `locationFlags`; rebuilt each flag-poll tick by
/// `core::update_live`). This REPLACES the old "is the live game flag set at AddItem time?"
/// re-pickup test, which leaked: for ~13% of lots (the probe's "25 true"), and systematically for
/// the 224 shared-flag multi-item lots (605 locations — armor sets, NPC-corpse bundles, boss
/// remembrance drops), the game sets the acquisition flag AT or BEFORE the bag-add, so the live
/// flag already reads set at AddItem and the vanilla item passed through as a bogus "re-pickup"
/// (e.g. Traveler's Clothes 0x100f90c4 / flag 15007980, 2026-07-03 playtest).
///
/// Collected-set logic is race-safe in the correct direction: a location enters this set only on a
/// flag-poll tick STRICTLY AFTER its check was sent, so a first-time pickup (flag not yet in the
/// set) always SUPPRESSES; a genuine re-pickup of a farmable/respawning source (flag collected on a
/// prior, separate event) PASSES. `None` until the first poll → suppress-by-default (never leaks).
static KNOWN_COLLECTED_FLAGS: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

/// #759 watchdog: every vanilla-suppressed pickup, watched until its flags collect/fire (the
/// pickup was the check -- silently dropped) or a grace period passes with neither (the
/// suppression ate something that never became a check: the Leave-drop signature, or a lot-less
/// ware farmed early -- #321). Decision logic is `er_logic::vanilla_suppress::split_unresolved`,
/// host-tested; this static is only the buffer between the detour and the flag-poll tick.
static SUPPRESSED_WATCH: Mutex<Vec<er_logic::vanilla_suppress::SuppressedPickup>> =
    Mutex::new(Vec::new());
/// How long a suppressed pickup may sit unresolved before the watchdog names it. A genuine check
/// pickup's flags collect within a poll tick or two; 20s is far outside that and far inside a
/// play session, so an announcement is never about a working check (the er_logic doc states the
/// direction of error: shared flags can MISS an eaten drop, never accuse a working one).
const SUPPRESS_WATCH_GRACE_MS: u64 = 20_000;

/// #321 -- is the FLAG-SET DISARM legal for THIS seed's `checkItemFlags`?
///
/// Set at connect from the live table, never from the apworld version: a seed rolled before the
/// world-side lot-coverage drop still maps some flags to two ids, and enabling the disarm there
/// would reopen the shared-flag leak (Traveler's Clothes, 2026-07-03). Such a seed simply keeps the
/// collected-set-only policy it was rolled for.
static FLAG_DISARM_LEGAL: AtomicBool = AtomicBool::new(false);

pub fn configure_check_item_flags(map: HashMap<u32, Vec<u32>>) {
    // Armed-or-inert (house rule): one line at configure time says which state the suppressor
    // is in, so a missing/empty checkItemFlags in slot_data is visible instead of silent.
    if map.is_empty() {
        log::info!("vanilla suppressor INERT: checkItemFlags empty/absent in slot_data");
    } else {
        log::info!("vanilla suppressor ARMED for {} check item ids", map.len());
    }
    // PRECONDITION CHECK, on the live table. Same rule the world asserts as a regen gate
    // (test_gf_check_item_flags_lot_covered::test_no_emitted_flag_is_mapped_by_two_ids); checking
    // it here too means a stale or hand-rolled slot_data cannot turn the disarm on by accident.
    let legal = er_logic::vanilla_suppress::flags_are_unshared(
        map.values().map(|v| v.as_slice()).collect::<Vec<_>>(),
    );
    FLAG_DISARM_LEGAL.store(legal, Ordering::Relaxed);
    // Armed-or-inert again: the disarm is the difference between "a lot-less check's ware is eaten
    // until you collect it" and "until its award fires", so which one is live must be in the log.
    if legal {
        log::info!(
            "vanilla-suppress: flag-set DISARM enabled -- no flag is mapped by two ids, so an id \
             releases as soon as its own check's acquisition flag fires (#321)"
        );
    } else {
        log::info!(
            "vanilla-suppress: flag-set disarm OFF -- this seed maps at least one flag to two item \
             ids, so releasing on a live flag could free a neighbour whose check never fired. \
             Collected-set only (the pre-#321 policy this seed was rolled for)."
        );
    }
    *recover_lock(&CHECK_ITEM_FLAGS) = Some(map);
}

/// Replace the collected-flag set. Called by the flag-poll each tick with the acquisition flags of
/// every location currently in the server checked-set (loc->flag via `locationFlags`).
pub fn set_known_collected_flags(flags: HashSet<u32>) {
    // #759 watchdog, on the same tick that delivers the fresh collected-set. An entry whose
    // flags collected (or live-fired) resolves silently; one that outlives the grace with
    // neither was a pickup the suppressor ate that never became a check -- most likely a weapon
    // the player put down with Leave and picked back up (the matt's-rando habit), or a lot-less
    // check ware farmed early (#321). WARN once per event, with the console rescue inline: the
    // item is gone from the world, so `!give` is the only path back.
    {
        let watch = std::mem::take(&mut *recover_lock(&SUPPRESSED_WATCH));
        if !watch.is_empty() {
            let (keep, overdue) = er_logic::vanilla_suppress::split_unresolved(
                watch,
                &flags,
                &|f| crate::flags::get_event_flag(f),
                now_ms(),
                SUPPRESS_WATCH_GRACE_MS,
            );
            for e in &overdue {
                log::warn!(
                    "vanilla-suppress: pickup {:#x} was suppressed {}s ago and its check never \
                     collected or fired -- the suppressor likely ate an item the player placed \
                     on the ground (Leave) or a farmed copy of a lot-less check ware (#759/#321). \
                     Rescue: `!give {:#x} 1` in the client console.",
                    e.raw_id,
                    (now_ms().saturating_sub(e.at_ms)) / 1000,
                    e.raw_id,
                );
            }
            *recover_lock(&SUPPRESSED_WATCH) = keep;
        }
    }
    *recover_lock(&KNOWN_COLLECTED_FLAGS) = Some(flags);
}

fn check_item_flags_lookup(raw_id: u32) -> Option<Vec<u32>> {
    CHECK_ITEM_FLAGS
        .lock()
        .unwrap()
        .as_ref()?
        .get(&raw_id)
        .cloned()
}

/// AP location ids the detour suppressed, drained by `update_live` -> `mark_checked`.
static PENDING_CHECKS: Mutex<Vec<i64>> = Mutex::new(Vec::new());

/// Resolve the inventory pointer from a game static so server/start grants don't wait for the
/// player's first in-game pickup (the long-standing UX wart). ENABLED: a 2026-06-30 run confirmed the
/// C++ pointer-slot resolver (`static_inventory_ptr_rva`, RVA 0x03D67A50) equals the pointer the game
/// hands the detour, while the typed-field `static_inventory_ptr` MISMATCHED (wrong field). The
/// one-time `inventory-ptr` confirm log in `add_item_detour` keeps verifying both each run.
const USE_STATIC_INVENTORY_PRIME: bool = true;
/// One-time guard for the static-vs-game inventory-pointer confirmation log.
static INV_PTR_CHECKED: AtomicBool = AtomicBool::new(false);

/// Set the first time the GAME itself calls AddItemFunc (a real pickup / the post-load
/// inventory being populated). Distinguishes a genuinely-live inventory from the static prime,
/// so start-item grants can wait until AFTER the save/new-game load replace (which clobbers a
/// grant made during the load screen). See patch_greenfield_start_item_clobber.py.
static REAL_PICKUP_SEEN: AtomicBool = AtomicBool::new(false);

/// True once the game has driven AddItemFunc at least once this session (inventory is live).
pub fn real_pickup_seen() -> bool {
    REAL_PICKUP_SEEN.load(Ordering::Relaxed)
}

const ADD_ITEM_FUNC_RVA: usize = 0x0056_05B0;
const ADD_ITEM_FUNC_SIG: &[u8] = &[
    0x40, 0x55, 0x56, 0x57, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57, 0x48, 0x8D, 0xAC, 0x24,
];
const ITEMBUF_ENTRY_ID_OFF: usize = 0x04;
const ITEMBUF_ENTRY_OFF: usize = 0x20; // a constructed itembuf's entry sits at buf+0x20

pub fn install() -> Result<(), Box<dyn std::error::Error>> {
    if HOOK.get().is_some() {
        return Ok(());
    }
    let target_addr =
        current_module_base().ok_or("no module base for eldenring.exe")? + ADD_ITEM_FUNC_RVA;
    if !signature_matches(target_addr) {
        return Err(format!(
            "AddItemFunc signature mismatch @ {target_addr:#x} — pinned 2.6.2.0 RVA stale for this build"
        )
        .into());
    }
    let target: AddItemFn = unsafe { std::mem::transmute::<usize, AddItemFn>(target_addr) };
    let detour = unsafe { GenericDetour::<AddItemFn>::new(target, add_item_detour)? };
    unsafe {
        detour.enable()?;
    }
    let _ = HOOK.set(detour);
    log::info!("AddItemFunc detour installed @ {target_addr:#x}");
    Ok(())
}

pub fn take_pending_checks() -> Vec<i64> {
    std::mem::take(&mut *recover_lock(&PENDING_CHECKS))
}

/// Whether the detour holds an inventory pointer that is USABLE NOW -- captured, plausible, and
/// belonging to the CURRENT world epoch. `update_live` gates server-pushed grants (and the
/// reconciler's world-loaded reading) on this so the receive watermark advances atomically.
///
/// EPOCH-AWARE since 2026-07-30: this was the verbatim pre-fix `>= 0x10000` test, so for the tick
/// or two after a world edge it reported "inventory ready" through a pointer `grant_full_id`
/// itself would refuse -- gate and enforcement disagreeing about the same question. Harmless in
/// the observed cases (every caller retries on a failed grant), but the 2026-07-24 postmortem's
/// rule is that a stale pointer is DEAD, not "probably fine", and a gate that answers from a dead
/// pointer is exactly the kind of leftover the next instance of the class grows from.
pub fn has_inventory() -> bool {
    er_logic::inv_ptr::usable(
        LAST_INVENTORY.load(Ordering::Relaxed),
        LAST_INVENTORY_EPOCH.load(Ordering::Relaxed),
        WORLD_EPOCH.load(Ordering::Relaxed),
    )
}

/// Address of `PlayerGameData.equipment.equip_inventory_data` — the structure AddItemFunc takes as
/// its inventory arg — resolved from the GameDataMan singleton (the SAME typed path `upgrades.rs`
/// walks in-world). `None` until the player is placed. SAFE to compute; whether it is the pointer the
/// game hands the detour is exactly what the confirmation log verifies.
fn static_inventory_ptr() -> Option<usize> {
    if !crate::flags::in_world() {
        return None;
    }
    let gdm = unsafe { GameDataMan::instance() }.ok()?;
    let pgd = gdm.main_player_game_data.as_ref();
    let inv = &pgd.equipment.equip_inventory_data as *const _ as usize;
    (inv >= 0x10000).then_some(inv)
}

/// SECOND inventory-resolver candidate: the pointer stored at the pinned static slot
/// `Inventory_PtrLoc_RVA` — the value the C++ client (`Inventory_PtrLoc_RVA = 0x03D67A50`) read and
/// granted through successfully on 2.6.2.0. This reads a POINTER from a static location, vs
/// `static_inventory_ptr`'s ADDRESS-of-embedded-field. The confirm log reports both so one pickup
/// identifies which (if either) equals the pointer the game hands the detour.
const INVENTORY_PTRLOC_RVA: usize = 0x03D6_7A50;
fn static_inventory_ptr_rva() -> Option<usize> {
    let slot = current_module_base()? + INVENTORY_PTRLOC_RVA;
    // SAFETY: pinned data RVA inside the loaded eldenring.exe image; reads one pointer-sized word.
    // Only called inside the one-time, in-world confirm block (mapped memory). Diagnostic only.
    let inst = unsafe { (slot as *const usize).read_unaligned() };
    (inst >= 0x10000).then_some(inst)
}

/// Tick helper (game thread): if no inventory pointer is captured yet, seed `LAST_INVENTORY` from the
/// static path so grants flush WITHOUT waiting for a pickup. No-op unless `USE_STATIC_INVENTORY_PRIME`
/// is enabled (and confirmed safe). Once a real pickup captures the game's own pointer it takes over.
pub fn prime_inventory_if_needed() {
    let epoch = WORLD_EPOCH.load(Ordering::Relaxed);
    let have = er_logic::inv_ptr::usable(
        LAST_INVENTORY.load(Ordering::Relaxed),
        LAST_INVENTORY_EPOCH.load(Ordering::Relaxed),
        epoch,
    );
    if !USE_STATIC_INVENTORY_PRIME || have {
        return;
    }
    if !crate::flags::in_world() {
        return; // the slot only holds a valid inventory instance once the player is loaded
    }
    // WARP-OUT HOLDOFF (2026-07-30): for the first teardown frames after a warp request,
    // `in_world()` still reads true and the static slot still points at the ORIGIN map's dying
    // inventory object. Priming here would recapture it in the CURRENT epoch and defeat the
    // retirement `on_warp_request` just performed -- the epoch test cannot see a free that
    // happens within an epoch; only time can. Time-bounded (PRIME_HOLDOFF_MS), so a warp that
    // never completes merely defers grants a few seconds, never permanently.
    if !er_logic::inv_ptr::may_prime(
        now_ms(),
        LAST_WARP_REQUEST_MS.load(Ordering::Relaxed),
        er_logic::inv_ptr::PRIME_HOLDOFF_MS,
    ) {
        return;
    }
    // Use the RVA pointer-slot resolver (CONFIRMED 2026-06-30); the typed-field resolver MISMATCHED.
    if let Some(inv) = static_inventory_ptr_rva() {
        LAST_INVENTORY.store(inv, Ordering::Relaxed);
        LAST_INVENTORY_EPOCH.store(WORLD_EPOCH.load(Ordering::Relaxed), Ordering::Relaxed);
        log::info!("primed inventory pointer from rva-slot @ {inv:#x} (no pickup needed)");
    }
}

/// Called on the in-world false->true edge (core.rs), beside the check_lots / enemy_drops /
/// shop_sell re-arms. Retires the cached inventory pointer: the game may have freed the object
/// during the load, and a pointer that outlives its world is a native crash the moment the
/// reconciler grants through it. The next tick re-primes from the static slot (or the player's
/// next pickup), so this costs at most a tick or two of deferred grants.
///
/// Returns the world epoch it just bumped to, so the caller can stamp its own edge lines with the
/// same number (`runes::log_sample`, world issue #259). Two edges that read the same rune count are
/// otherwise indistinguishable in a log.
pub fn on_world_edge() -> u64 {
    let e = WORLD_EPOCH.fetch_add(1, Ordering::Relaxed) + 1;
    log::info!(
        "inventory-ptr: retired at world edge (epoch {e}) -- re-priming before the next grant"
    );
    flush_pot_cap_tally(e);
    e
}

/// Report and reset the capped-grant counters (world#692).
///
/// The world edge is where the number is both stable and meaningful: the cap frees up as the player
/// consumes pots, so this boundary is also the one any future RETRY would run at. A quiet edge logs
/// nothing -- which is what makes a line that does appear worth reading.
///
/// 🛑 WARN, NOT INFO. Every item in this line was deliberately suppressed by the permanent safety
/// cap. The aggregate makes that loss visible without per-tick spam.
fn flush_pot_cap_tally(epoch: u64) {
    let Ok(mut tally) = POT_CAP_TALLY.lock() else {
        return;
    };
    if let Some(line) = tally.flush() {
        log::warn!("{line} [epoch {epoch}]");
    }
}

/// Called from the LuaWarp detour the moment ANY warp (menu or client) is REQUESTED -- the
/// warp-OUT edge `on_world_edge` cannot cover, because it only fires at ARRIVAL (in-world
/// false->true) and `in_world()` keeps reading true through the first teardown frames. Retires
/// the captured pointer (epoch bump) AND stamps the primer holdoff so the static slot cannot
/// re-seed the dying object (see `LAST_WARP_REQUEST_MS`). Same crash class, same postmortems:
/// the 2026-07-24 arrival-side pair (er_logic::inv_ptr) and the Rampart Gaol warp-out sweep.
///
/// Runs inside the game's own warp call frame: atomics + one log line, no panic path.
pub fn on_warp_request() {
    LAST_WARP_REQUEST_MS.store(now_ms(), Ordering::Relaxed);
    let e = WORLD_EPOCH.fetch_add(1, Ordering::Relaxed) + 1;
    log::info!(
        "inventory-ptr: retired at warp request (epoch {e}); primer held {}ms",
        er_logic::inv_ptr::PRIME_HOLDOFF_MS
    );
}

/// Called on the in-world true->false edge (core.rs): quit-to-menu / character switch -- the
/// exit a warp REQUEST never announces, so `on_warp_request` cannot cover it. Before this
/// retirement the captured pointer kept its epoch through the whole out-of-world window and
/// `has_inventory()` read TRUE for as long as the player sat at the menu (clients#353: 4+
/// minutes of `has_inv=true in_world=false` in the pre-crash log). Every grant path ANDs
/// `in_world()`, so nothing wrote through it -- but gate and pointer disagreeing for minutes at
/// a time is exactly the leftover the next instance of the class grows from, and the 2026-07-24
/// postmortem's rule is that a stale pointer is DEAD, not "probably fine". No primer holdoff is
/// needed here: `prime_inventory_if_needed` is itself gated on `in_world()`, so the slot cannot
/// re-seed until the next arrival, which bumps the epoch again and re-primes as today.
pub fn on_world_exit() {
    let e = WORLD_EPOCH.fetch_add(1, Ordering::Relaxed) + 1;
    log::info!(
        "inventory-ptr: retired at world exit (epoch {e}) -- menu-time ticks now read has_inventory=false"
    );
}

/// Cracked Pot FullID (GOODS | goods 9500) — the item the Chapel pot-relief guard watches.
const CRACKED_POT_FULL_ID: i32 = 0x4000_0000 | 9500;
/// Vanilla latch flag of m10_01 event 10010792 ("shop lineup: empty-pot pre-consumption"): the
/// event sets it when it completes (both branches), and `EndIf(EventFlag(10019200))` makes every
/// later run of the event inert once it is on.
const CHAPEL_POT_RELIEF_LATCH: u32 = 10_019_200;
/// Chapel of Anticipation play_region sub-id (m10_01; the fresh-character spawn map).
const CHAPEL_SUB_REGION: i32 = 10010;

/// PHANTOM-CHECK GUARD (flags 66150/66170, found 2026-07-09): vanilla m10_01 event 10010792 is a
/// patch save-migration — on its first run it waits up to 5s and, if the player ALREADY owns a
/// Cracked Pot (goods 9500), assumes a pre-patch save that bought Gostoc's pots and force-sets the
/// relocated pot-instance flags 66150/66170/66180 ("already obtained"). A fresh AP character spawns
/// in the Chapel of Anticipation (m10_01) and the start-item loadout grants 10x Cracked Pot inside
/// that window, so the migration misfires and the flag-poll reports the two Sainted Hero's Grave
/// pot locations (data.py f66150/f66170) as checked at startup, every seed.
///
/// Returns true when granting goods 9500 is SAFE: the latch is set (the armed event already
/// completed / can never re-run), or we are outside m10_01 — in which case we also set the latch
/// ourselves so a later first-load of m10_01 (Four Belfries waygate) can't run the migration
/// against pots we granted. While in the chapel pre-latch, callers get `false` and retry; the
/// event self-latches ≤6s after map start, so the pots are merely deferred a few seconds.
fn chapel_pot_relief_safe() -> bool {
    if crate::flags::get_event_flag(CHAPEL_POT_RELIEF_LATCH) {
        return true;
    }
    let in_chapel = crate::flags::play_region_id()
        .map(|pr| (if pr >= 1_000_000 { pr / 100 } else { pr }) == CHAPEL_SUB_REGION)
        // Unknown region (load screen): hold — conservative, the caller retries next tick.
        .unwrap_or(true);
    if in_chapel {
        return false;
    }
    // Outside the chapel with the latch unset: latch it so any future m10_01 first-load EndIfs
    // the migration instead of reading our granted pots as a pre-patch save.
    let _ = crate::flags::try_set_event_flag(CHAPEL_POT_RELIEF_LATCH, true);
    true
}

/// Pot goods whose HELD count trips a vanilla relief event that force-sets EVERY pot-location flag —
/// a mass phantom-check across the pot flag ranges (66000-66190 / 66400-66490 / 66700-66790).
/// common.emevd counts the bare goods row and fires at an EXACT threshold: event 1460 Goods 9500 == 20
/// -> flag 6902; 1461 Goods 9501 == 10 -> 6903; 1462 Goods 9510 == 10 -> 6904. We cap pot DELIVERIES
/// one below each threshold so the held count can never equal it. Pots are permanent reusable
/// containers (count only rises), and the pool ships ~16 Cracked Pot locations plus 10 in the start
/// loadout, so 20 is otherwise very reachable. Nobody needs 19+ pots, so the cap is invisible in play.
const POT_DELIVERY_CAPS: &[(i32, i32)] = &[
    (0x4000_0000 | 9500, 19), // Cracked Pot        (event 1460, threshold 20)
    (0x4000_0000 | 9501, 9),  // Ritual Pot         (event 1461, threshold 10)
    (0x4000_0000 | 9510, 9),  // Perfume Bottle     (event 1462, threshold 10)
    // Hefty Cracked Pot. 🛑 NOT one-below-a-threshold like the three above -- there is no
    // threshold. Its entry read `9` with the comment "DLC; threshold 10, flags 669xx" until
    // 2026-08-03. That comment was EXTRAPOLATED from the base-game pattern, not derived, and it
    // cost the player an item: Alaric's 2026-08-02 log has
    //     pot-cap: goods 0x401ea99c grant of 1 CAPPED to 0 (held 9, cap 9)
    // i.e. an AP delivery reported to the server and never given to the player (#308).
    // SEARCHED and absent: the full 589-file decompiled EMEVD corpus -- which DOES include the
    // Land of Shadow maps (m20/m21/m22/m25/m28) -- contains no
    // `StoreItemAmountHeldInEventValue(ItemType.Goods, 2009...)` and never `SetEventFlagID(66900`.
    // The base-game three are all right there and greppable; their DLC analogue is not.
    // The DLC ships EXACTLY 10 Hefty Cracked Pots (Alaric, in-game) and the world carries exactly
    // 10 checks on 66900..66990, so a cap of 9 made holding the full set impossible for no reason.
    // `EquipParamGoods.maxNum` for row 2009500 is itself 10, so this entry can no longer bind --
    // it is kept as a documented no-op rather than deleted, so the next person asking "why is
    // there no cap on the hefty pot?" finds this paragraph instead of re-deriving it.
    (0x4000_0000 | 2_009_500, 10), // Hefty Cracked Pot (no EMEVD threshold; maxNum is 10)
];

/// Total held quantity of a bare GOODS row (sums stacks). None if the inventory isn't reachable this
/// tick. Same read-only walk as `upgrades::held_scadu_fragments` / `inventory`.
fn count_held_goods_row(row: i32) -> Option<i32> {
    use eldenring::cs::{GameDataMan, ItemCategory};
    use fromsoftware_shared::FromStatic;
    let gdm = unsafe { GameDataMan::instance() }.ok()?;
    let pgd = gdm.main_player_game_data.as_ref();
    let mut total: i64 = 0;
    for entry in pgd.equipment.equip_inventory_data.items_data.items() {
        if entry.item_id.category() == ItemCategory::Goods && entry.item_id.param_id() as i32 == row
        {
            total += entry.quantity as i64;
        }
    }
    Some(total.min(i32::MAX as i64) as i32)
}

/// Clamp a pot grant so the resulting held count stays strictly below the mass-phantom-check
/// threshold. Returns the qty to actually grant (0 = at/over the cap, skip). Non-pot full_ids and an
/// unreadable inventory pass through unchanged (a transient read miss must not drop an item; the cap
/// re-checks on the next pot grant).
/// One announce-bit per `POT_DELIVERY_CAPS` row, so a capped pot says so ONCE per session instead of
/// once per grant or never.
///
/// 🛑 STILL ONE-SHOT, AND STILL THE RIGHT SHAPE FOR THIS LINE. The WARN carries the full
/// explanation of the failure mode; repeating that paragraph twice a second would be the noise it
/// was added to suppress. What it must no longer do is stand in for a COUNT -- see [`POT_CAP_TALLY`].
static POT_CAP_ANNOUNCED: AtomicU32 = AtomicU32::new(0);
/// Every capped grant since the last world edge, per row (world#692).
///
/// ⭐ THE ANNOUNCE-BIT ABOVE IS NOT A MEASUREMENT AND NEVER WAS. Its own WARN says so out loud --
/// "Further caps on this row are silent" -- so bobler's 2026-08-15 log records exactly one cap on
/// `0x40002526` and cannot say whether that was the only one or the first of fifty. #692 asks
/// whether a capped grant should be retried at the next world edge or dropped without an ack, and
/// those are opposite answers for a one-off and for a steady drip. This tally is what lets the next
/// log tell them apart.
static POT_CAP_TALLY: Mutex<er_logic::pot_cap_tally::PotCapTally> =
    Mutex::new(er_logic::pot_cap_tally::PotCapTally::new());

fn pot_capped_qty(full_id: i32, qty: i32) -> i32 {
    // `position` + index, NOT `enumerate().find(|(_, &(id, _))| ..)`: the latter mixes an explicit
    // `&` deref pattern into an implicitly-borrowing tuple pattern, which edition 2024 rejects
    // ("cannot explicitly dereference within an implicitly-borrowing pattern"). Caught by the
    // Windows CI, which is the only thing that compiles this crate.
    let Some(idx) = POT_DELIVERY_CAPS.iter().position(|&(id, _)| id == full_id) else {
        return qty;
    };
    let cap = POT_DELIVERY_CAPS[idx].1;
    match count_held_goods_row(full_id & 0x0FFF_FFFF) {
        Some(held) => {
            // All-or-nothing is the idempotence boundary. If a stack of three only has room for
            // one, placing that one and retrying the same AP entry would duplicate it. Defer the
            // complete entry until it fits instead.
            let allowed = er_logic::pot_cap_tally::deliverable_qty(held, cap, qty);
            // TELEMETRY (2026-07-30). Swallowing a grant and reporting success was precisely the
            // "polite false" CONTRIBUTING forbids. `grant_full_id` now returns false for this arm,
            // so the caller retains the entry while this counter explains the backpressure. Eldakin's
            // 2026-07-29 log shows the consequence -- start-item backfill granting 10 Hefty Cracked
            // Pots that the cap then swallowed, with nothing anywhere saying why. (That backfill
            // burst is a one-shot early-scan race on a fresh character, not a permanent re-grant.)
            // 🛑 A seed that hands out MORE start pots than the cap can never deliver the remainder;
            // this line is how that becomes visible instead of silent.
            if allowed < qty {
                // 🛑 COUNT FIRST, ANNOUNCE SECOND, and never inside the announce branch: the whole
                // defect in #692 is a number that only ever incremented on the tick a bit flipped.
                if let Ok(mut tally) = POT_CAP_TALLY.lock() {
                    tally.record(full_id, qty, allowed);
                }
                let bit = 1u32 << idx;
                if (POT_CAP_ANNOUNCED.fetch_or(bit, Ordering::Relaxed) & bit) == 0 {
                    log::warn!(
                        "pot-cap: goods {full_id:#x} grant of {qty} SUPPRESSED (held {held}, cap {cap}) \
                         -- permanent container safety cap reached; delivery watermark advanced so \
                         later AP items are not blocked. Further suppressions on this row are counted \
                         and reported at a world edge."
                    );
                }
            }
            allowed
        }
        None => qty,
    }
}

/// Grant an item (full_id = real item id | category nibble) by constructing an itembuf and calling
/// the original AddItemFunc with the captured inventory pointer. Returns false if the hook isn't
/// installed or no inventory pointer has been captured yet (no pickup this session) — caller retries.
/// MUST run on the game thread (the FrameBegin tick / update_live).
pub fn grant_full_id(full_id: i32, qty: i32) -> bool {
    !matches!(
        grant_full_id_outcome(full_id, qty),
        er_logic::start_backfill::GrantOutcome::NotReady
    )
}

/// The same grant, reporting WHAT ACTUALLY HAPPENED (#248, 2026-08-01).
///
/// `Capped` means deliberately suppressed, not placed: the permanent container count cannot fall,
/// so retrying would deadlock the receive stream forever. `grant_full_id` maps it to `true` for the
/// ledger while the separate outcome lets the start-item verifier report the missing physical item.
pub fn grant_full_id_outcome(full_id: i32, qty: i32) -> er_logic::start_backfill::GrantOutcome {
    use er_logic::start_backfill::GrantOutcome;
    if HOOK.get().is_none() {
        return GrantOutcome::NotReady;
    }
    // Chapel pot-relief guard: defer Cracked Pot grants that would trip m10_01's migration event
    // (phantom checks f66150/f66170). Every caller treats `false` as retry-next-tick, so the
    // stack simply lands a few seconds later, after the event's own latch (10019200) sets.
    if full_id == CRACKED_POT_FULL_ID && !chapel_pot_relief_safe() {
        return GrantOutcome::NotReady; // deferred, not refused -- caller retries
    }
    // Pot-delivery cap: never let a pot grant push the held count to a mass-phantom-check threshold.
    // A capped entry is acknowledged without placement. These reusable-container counts only rise,
    // so holding the watermark here would permanently block every later AP item.
    let qty = pot_capped_qty(full_id, qty);
    if qty <= 0 {
        // CAPPED/SUPPRESSED: no physical pot was added, but ledger callers advance their watermark.
        return GrantOutcome::Capped;
    }
    // The id the CALLER asked for. auto_upgrade may re-map a weapon below, but the reconciler
    // stalls on (and logs) the id it requested, so the probe must be keyed on that one.
    let requested_full_id = full_id;
    // Stage 6a: raise granted weapons to the player's current max reinforce tier (inert if off).
    let full_id = crate::upgrades::apply_auto_upgrade(full_id);
    let inv = LAST_INVENTORY.load(Ordering::Relaxed);
    if !er_logic::inv_ptr::usable(
        inv,
        LAST_INVENTORY_EPOCH.load(Ordering::Relaxed),
        WORLD_EPOCH.load(Ordering::Relaxed),
    ) {
        // Either nothing captured yet, or the capture predates a load and now points at freed
        // memory. Both mean RETRY, never "grant anyway": every caller treats false as retry, and
        // prime_inventory_if_needed re-seeds within a tick or two of the world coming back.
        return GrantOutcome::NotReady;
    }
    if let Some(ret) = grant_item(inv as *mut c_void, full_id, qty) {
        record_add_item_return(requested_full_id, ret);
    }
    // STILL `Placed`, deliberately. Nobody has RE'd what the return value means, so turning it
    // into a `Refused` outcome would bake an unverified root cause into the reconciler. This
    // change makes the datum VISIBLE; interpreting it is the follow-up, gated on a log that shows
    // what a refusal actually returns.
    GrantOutcome::Placed
}

fn call_original(inventory: *mut c_void, entry: *mut c_void, itembuf: *mut c_void, r9: u64) -> u64 {
    match HOOK.get() {
        Some(h) => unsafe { h.call(inventory, entry, itembuf, r9) },
        None => 0,
    }
}

unsafe extern "C" fn add_item_detour(
    inventory: *mut c_void,
    entry: *mut c_void,
    itembuf: *mut c_void,
    r9: u64,
) -> u64 {
    // This callback outlives individual world resources. In particular, the param repository
    // singleton remains reachable for a few teardown frames after its holders have been emptied.
    // `param_guard` makes that known case fallible; this boundary catch also guarantees that a
    // poisoned diagnostic mutex or a future Rust panic cannot unwind into Elden Ring. Passing the
    // pickup through is the least destructive fallback: crashing loses the session, while a rare
    // unsuppressed item can at worst duplicate one vanilla/placeholding delivery.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        add_item_detour_inner(inventory, entry, itembuf, r9)
    })) {
        Ok(ret) => ret,
        Err(_) => {
            let panic_count = ADD_ITEM_PANIC_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            log::error!(
                "add-item: Rust panic #{panic_count} this session contained at detour boundary; \
                 passing the game's item through"
            );
            call_original(inventory, entry, itembuf, r9)
        }
    }
}

fn add_item_detour_inner(
    inventory: *mut c_void,
    entry: *mut c_void,
    itembuf: *mut c_void,
    r9: u64,
) -> u64 {
    LAST_INVENTORY.store(inventory as usize, Ordering::Relaxed);
    LAST_INVENTORY_EPOCH.store(WORLD_EPOCH.load(Ordering::Relaxed), Ordering::Relaxed);
    REAL_PICKUP_SEEN.store(true, Ordering::Relaxed);
    // One-time: compare the pointer the game hands us against the static-resolved candidate, so we
    // can safely enable USE_STATIC_INVENTORY_PRIME (a wrong static pointer would crash on grant).
    if !INV_PTR_CHECKED.swap(true, Ordering::Relaxed) {
        let game = inventory as usize;
        match static_inventory_ptr() {
            Some(s) if s == game => {
                log::info!(
                    "inventory-ptr CONFIRM: static == game ({game:#x}) — safe to enable USE_STATIC_INVENTORY_PRIME"
                )
            }
            Some(s) => {
                log::warn!(
                    "inventory-ptr MISMATCH: static {s:#x} != game {game:#x} — keep static prime OFF (wrong field)"
                )
            }
            None => log::warn!("inventory-ptr: static unresolved at first pickup (game {game:#x})"),
        }
        // Second candidate: the C++-client RVA pointer-SLOT (proven on 2.6.2.0). One pickup thus
        // identifies which resolver — the typed embedded-field above, or this pointer-slot — equals
        // the game's pointer. Point `static_inventory_ptr` at whichever CONFIRMs, then enable the prime.
        match static_inventory_ptr_rva() {
            Some(s) if s == game => log::info!(
                "inventory-ptr CONFIRM (rva-slot): *(base+{INVENTORY_PTRLOC_RVA:#x}) == game ({game:#x}) — use the pointer-slot resolver"
            ),
            Some(s) => log::warn!("inventory-ptr rva-slot {s:#x} != game {game:#x}"),
            None => log::warn!("inventory-ptr rva-slot unresolved at first pickup"),
        }
    }
    let raw_id = unsafe { read_i32(entry, ITEMBUF_ENTRY_ID_OFF) } as u32;

    // Shop native-sell (SHOP-SYSTEM-HANDOFF.md §5): a rewritten own-world slot sells the REAL reward
    // (a non-synthetic id). Suppress its bag-add while the stock flag is unset so the single copy is
    // delivered by the AP grant, not the purchase. Checked BEFORE the synthetic/vanilla decision.
    if crate::shop_sell::should_suppress_sold(raw_id as i32, &|f| crate::flags::get_event_flag(f)) {
        return 0;
    }

    // AP PLACEHOLDER (SPEC-runtime-minibake): check lots have been repointed at ONE spare goods row, so
    // a check now hands out the placeholder instead of its vanilla ware. Suppress it UNCONDITIONALLY --
    // the row is referenced by no vanilla lot/shop/recipe, so the ONLY way to receive it is from a check
    // lot we rewrote ourselves. The flag poll reports the check; the AP grant delivers the real item.
    // Checked FIRST, before the id-keyed vanilla suppressor below (which this is progressively retiring).
    if crate::check_lots::is_placeholder(raw_id as i32) {
        log::debug!(
            "check-lots: placeholder pickup {raw_id:#x} suppressed (AP grant delivers the item)"
        );
        return 0;
    }

    if !is_synthetic_goods(raw_id) {
        // Vanilla-suppress (LIVE 2026-07-01): a vanilla id that belongs to a check location is the
        // check's ORIGINAL ware — suppress its bag-add so the AP grant delivers what the seed placed
        // there. The re-pickup discriminator is the COLLECTED set, not the live game flag: any mapped
        // flag NOT yet in KNOWN_COLLECTED_FLAGS means the check has not been reported yet, so this IS
        // the check pickup → suppress. Only pass (farmable/respawning re-pickup) once EVERY mapped
        // flag is collected. This fixes the shared-flag / early-flag-set leak where the game set the
        // acquisition flag at/before AddItem and the old live-flag test mis-read it as a re-pickup.
        if let Some(flags) = check_item_flags_lookup(raw_id) {
            let guard = recover_lock(&KNOWN_COLLECTED_FLAGS);
            // No poll yet (None) -> treat as "nothing collected" -> suppress by default (never leaks).
            let suppress = match guard.as_ref() {
                // #321: a mapped flag also counts as released once it is LIVE-SET, but only for a
                // seed whose table maps no flag to two ids (see FLAG_DISARM_LEGAL). Union with the
                // collected-set, never a replacement for it.
                Some(collected) => er_logic::vanilla_suppress::should_suppress_with_flag_disarm(
                    &flags,
                    collected,
                    FLAG_DISARM_LEGAL.load(Ordering::Relaxed),
                    &|f| crate::flags::get_event_flag(f),
                ),
                None => true,
            };
            if suppress {
                log::info!(
                    "vanilla-suppress: pickup {raw_id:#x} suppressed (check not yet collected, \
                     and its acquisition flag has not fired)"
                );
                // #759: remember it. If these flags never collect/fire, this suppression ate a
                // pickup that was not a check -- the watchdog in `set_known_collected_flags`
                // announces it with a rescue instead of letting the item vanish silently.
                recover_lock(&SUPPRESSED_WATCH).push(
                    er_logic::vanilla_suppress::SuppressedPickup {
                        raw_id,
                        mapped_flags: flags.clone(),
                        at_ms: now_ms(),
                    },
                );
                return 0;
            }
            log::info!(
                "vanilla-suppress: pickup {raw_id:#x} passed (check already collected — re-pickup)"
            );
        }
        // AUTO-UPGRADE ON PICKUP (#693, matt's-rando parity). The receipt path has always raised
        // a granted weapon to your best held tier; this applies the SAME id transform to a weapon
        // the game itself is adding -- a world pickup, a chest, or the drop-and-pick-it-back-up
        // loop players learned from matt's auto_upgrade. `apply_auto_upgrade` is raise-only,
        // cap-clamped, affinity-preserving (level rides row%100; base keeps the affinity block),
        // and identity when the option is off -- so this line is inert unless auto_upgrade is on
        // and the id is a weapon below your demonstrated tier. Writing the entry id BEFORE
        // call_original means the game adds the upgraded weapon: no remove seam needed, no
        // duplicate, and Beeno's retroactive ask (#693) gets a player-visible mechanic: put it
        // down, pick it up.
        let up = crate::upgrades::apply_auto_upgrade(raw_id as i32);
        if up != raw_id as i32 {
            log::info!(
                "auto-upgrade: pickup {raw_id:#x} raised to {up:#x} on add (drop-and-pickup / \
                 world pickup catch-up, #693)"
            );
            unsafe { write_i32(entry, ITEMBUF_ENTRY_ID_OFF, up) };
        }
        return call_original(inventory, entry, itembuf, r9);
    }

    match params::goods_row_fields(row_id_of(raw_id) as i32) {
        Some(fields) => {
            let item = decode_synthetic(&fields);
            log::info!(
                "AP check: synthetic {raw_id:#x} -> location {}",
                item.ap_location_id
            );
            recover_lock(&PENDING_CHECKS).push(item.ap_location_id);
            // own_world:true: report the check + suppress; the server echoes the item back and the
            // received-item path grants it (running progressive / region-open / notify by name).
            0 // suppress the world pickup
        }
        None => {
            log::warn!("synthetic id {raw_id:#x} but goods row unresolved; passing through");
            call_original(inventory, entry, itembuf, r9)
        }
    }
}

/// Port of the standalone `GrantItem`: 0x50-byte descriptor, entry at buf+0x20.
fn grant_item(inventory: *mut c_void, id_with_category: i32, quantity: i32) -> Option<u64> {
    if id_with_category == 0 || inventory.is_null() {
        return None;
    }
    let mut buf = [0u64; 0x50 / 8];
    let base = buf.as_mut_ptr() as *mut u8;
    unsafe {
        (base.add(0x20) as *mut i32).write_unaligned(1);
        (base.add(0x24) as *mut i32).write_unaligned(id_with_category);
        (base.add(0x28) as *mut i32).write_unaligned(quantity);
        (base.add(0x30) as *mut i32).write_unaligned(-1);
        (base.add(0x34) as *mut i32).write_unaligned(-1);
        (base.add(0x40) as *mut i64).write_unaligned(-1);
        (base.add(0x4C) as *mut i32).write_unaligned(-1);
        let entry = base.add(ITEMBUF_ENTRY_OFF) as *mut c_void;
        let itembuf = base as *mut c_void;
        // The return value is the whole point of this function's signature: dropping it is what
        // let a REFUSED add be scored as an accepted grant.
        let h = HOOK.get()?;
        Some(h.call(inventory, entry, itembuf, 0))
    }
}

fn current_module_base() -> Option<usize> {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    let hmodule = unsafe { GetModuleHandleW(None) }.ok()?;
    Some(hmodule.0 as usize)
}
fn signature_matches(addr: usize) -> bool {
    let actual = unsafe { std::slice::from_raw_parts(addr as *const u8, ADD_ITEM_FUNC_SIG.len()) };
    actual == ADD_ITEM_FUNC_SIG
}
unsafe fn read_i32(base: *const c_void, off: usize) -> i32 {
    unsafe { ((base as *const u8).add(off) as *const i32).read_unaligned() }
}

/// Write into the game's own itembuf entry BEFORE `call_original` consumes it. The only writer is
/// the auto-upgrade pickup transform; the buffer is the game's argument for THIS call, so writing
/// the id field is exactly what `grant_full_id` does when it constructs its own itembuf -- same
/// layout, same consumer, no lifetime beyond the call.
unsafe fn write_i32(base: *mut c_void, off: usize, v: i32) {
    unsafe { ((base as *mut u8).add(off) as *mut i32).write_unaligned(v) }
}

#[cfg(test)]
mod tests {
    use super::recover_lock;
    use std::sync::Mutex;

    #[test]
    fn collection_lock_recovers_after_contained_panic() {
        let collection = Mutex::new(vec![1]);
        let result = std::panic::catch_unwind(|| {
            let mut guard = collection.lock().unwrap();
            guard.push(2);
            panic!("test poison");
        });
        assert!(result.is_err());
        assert!(collection.is_poisoned());

        recover_lock(&collection).push(3);
        assert_eq!(&*recover_lock(&collection), &[1, 2, 3]);
    }
}
