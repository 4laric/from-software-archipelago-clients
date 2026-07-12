//! shop_sell.rs — runtime "mini-baker" for OWN-WORLD shop slots: rewrite each slot's
//! `ShopLineupParam.equipId`/`equipType` to its actual AP reward so the slot NATIVELY sells (and thus
//! displays) the real item — correct icon + name + description for ANY supported type (weapon, armor,
//! talisman, goods), with NO global-FMG collision (each row edited independently). Foreign items have no
//! ER counterpart, and gem/custom rewards aren't in `er_codec`'s categories, so both stay on the
//! `shop_preview`/`shop_icon` flower override.
//!
//! Field encoding (confirmed against the vanilla ShopLineupParam dump): `equipId` is the RAW item id
//! (no category nibble) and `equipType` selects the param table — 0 Weapon, 1 Protector, 2 Accessory,
//! 3 Goods (4 Gem, 5 CustomWeapon, not handled here). So equipId = `row_id_of(FullID)`, equipType =
//! FullID category.
//!
//! Because the slot now hands the player the real reward R on purchase, the redundant AP ECHO
//! grant for that check is skipped instead (`echo_skip`, consulted by the core receive loop) --
//! ECHO-DEDUP, 2026-07-03. Bag-add suppression (`should_suppress_sold`) is RETIRED: weapon-slot
//! purchases bypass the AddItemFunc detour entirely (CTD repro logs), so it could never dedup
//! them, and nulling a shop bag-add is the crash-adjacent path -- and it is now DEAD CODE
//! (SOLD_SUPPRESS is never populated, so should_suppress_sold always returns false).
//!
//! CROSS-TYPE IS OPEN (2026-07-11): SHOP_CTD_GUARD is REMOVED. It used to bail on weapon slots whose
//! reward was a non-weapon, on a 3x CTD repro from 2026-07-03 -- now believed CONFOUNDED by that same
//! bag-add nulling, which was live then and is inert now. Not proven (armor->goods also produced a
//! non-weapon bag-add and never crashed), so this is a deliberate experiment: buy out every shop and
//! see. If it CTDs, restore the guard in run().
//!
//! Because a rewritten slot sells the reward NATIVELY, the AP grant is skipped -- and `apply_auto_upgrade`
//! lives inside that grant. So the upgrade is BAKED INTO THE SOLD ID (a weapon's reinforce level is part
//! of its id); otherwise every weapon bought from a shop arrives at +0 with auto_upgrade ON.
//! Runs once in-world after shop_flags (stock flags final) + scout-ready; idempotent, re-armed on tick.

#![allow(dead_code)]

use eldenring::cs::{ShopLineupParam, SoloParamRepository};
use fromsoftware_shared::FromStatic;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// slot_data `locationFlags` (AP location id -> guarding event flag). Inverted at run to map a row's
/// `eventFlag_forStock` back to its AP location (-> scout reward). Set by net.rs.
static CONFIGURED: Mutex<Option<HashMap<i64, u32>>> = Mutex::new(None);
static DONE: AtomicBool = AtomicBool::new(false);

/// FullID of a reward we rewrote a slot to SELL -> the slot's stock flag. The detour suppresses the
/// bag-add of these while the flag is unset, so the buy doesn't double with the AP grant.
static SOLD_SUPPRESS: Mutex<Option<HashMap<i32, u32>>> = Mutex::new(None);

/// Stock flags of rewritten own-world slots whose check was still OPEN at run() time. One-shot:
/// should_suppress_sold consumes a flag on the reward's first native bag-add (the check
/// purchase), so suppression does NOT depend on when eventFlag_forStock sets. Re-armed only by
/// a fresh run().
static ARMED_SUPPRESS: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

/// ECHO-DEDUP (2026-07-03): {AP location -> stock flag} for every rewritten row whose check
/// was still OPEN at run() time. The receive loop skips the echo grant for these iff the stock
/// flag is NOW SET (the native sale really happened) -- so !collect / server-sent items for
/// un-bought checks still grant. Replaces bag-add suppression (statics above stay unpopulated).
static ECHO_SKIP: Mutex<Option<HashMap<i64, u32>>> = Mutex::new(None);

pub fn configure(location_flags: HashMap<i64, u32>) {
    log::info!(
        "shop-sell: configured {} location flag(s)",
        location_flags.len()
    );
    *CONFIGURED.lock().unwrap() = Some(location_flags);
}

/// Detour hook: suppress the bag-add of `full_id` if it's a reward a rewritten slot now sells AND the
/// slot's stock flag is still unset (check not yet completed). False until `run` populates the map.
pub fn should_suppress_sold(full_id: i32, _get_flag: &dyn Fn(u32) -> bool) -> bool {
    // Robust, timing-independent: suppress the FIRST native bag-add of a registered reward
    // whose slot-check was still OPEN when run() armed it (one-shot). That first add is the
    // check-completing purchase; the AP echo delivers the real copy (AP grants bypass this
    // detour via the original AddItem, so they never consume the arm). NOT gated on the live
    // stock flag -- eventFlag_forStock can already be set at buy time, which let the native
    // sale double with the AP grant.
    let flag = {
        let g = SOLD_SUPPRESS.lock().unwrap();
        match g.as_ref().and_then(|m| m.get(&full_id)) {
            Some(&f) => f,
            None => return false,
        }
    };
    match ARMED_SUPPRESS.lock().unwrap().as_mut() {
        Some(set) => {
            // SHOP_FIXES_PATCH: attribute every registered bag-add so a residual double-grant
            // is diagnosable from one session log (grep "shop-sell:").
            let hit = set.remove(&flag); // one-shot: consume the arm; true iff it was armed
            log::info!(
                "shop-sell: bag-add of registered ware {full_id:#x} (stock flag {flag}) -> {}",
                if hit {
                    "SUPPRESSED (arm consumed)"
                } else {
                    "PASSED (arm already consumed / never armed)"
                }
            );
            hit
        }
        None => false,
    }
}

/// ECHO-DEDUP: should the echo grant for `loc` be skipped? True iff a rewritten row sells this
/// check's reward natively AND its stock flag is now set (the purchase actually happened).
/// The flag check keeps !collect / server-sent items for un-bought checks grantable.
pub fn echo_skip(loc: i64) -> bool {
    let flag = match ECHO_SKIP.lock().unwrap().as_ref().and_then(|m| m.get(&loc)) {
        Some(&f) => f,
        None => return false,
    };
    crate::flags::get_event_flag(flag)
}

/// FullID category -> ShopLineupParam `equipType`. `None` for gem/custom (not natively sellable here).
fn equip_type_for(fid: i64) -> Option<u8> {
    match er_codec::item_category_of(fid as u32) {
        er_codec::CATEGORY_WEAPON => Some(0),
        er_codec::CATEGORY_PROTECTOR => Some(1),
        er_codec::CATEGORY_ACCESSORY => Some(2),
        er_codec::CATEGORY_GOODS => Some(3),
        _ => None,
    }
}

/// Run once in-world + scout-ready (after shop_flags): rewrite each own-world check row to sell its
/// reward natively. Returns false (retry) until slot_data + the scout cache + the param repo are up.
pub fn run() -> bool {
    if DONE.load(Ordering::Relaxed) {
        return true;
    }
    let loc_flags = {
        let g = CONFIGURED.lock().unwrap();
        match g.as_ref() {
            Some(m) => m.clone(),
            None => return false, // wait for slot_data parse (net.rs)
        }
    };
    if !crate::scout_proof::cache_ready() {
        return false; // need the rewards
    }
    // invert: stock flag -> AP location
    let mut flag_to_loc: HashMap<u32, i64> = HashMap::with_capacity(loc_flags.len());
    for (&loc, &flag) in loc_flags.iter() {
        if flag != 0 {
            flag_to_loc.insert(flag, loc);
        }
    }
    // SAFETY: FD4 singleton; game thread, in-world (caller gates). rows/get_mut on the live RW table.
    let repo = match unsafe { SoloParamRepository::instance_mut() } {
        Ok(r) => r,
        Err(_) => return false, // repo not up yet — retry next tick
    };

    // Scan immutably -> plan the rewrites, then apply (avoids holding a row borrow across get_mut).
    let mut plan: Vec<(u32, i32, u8)> = Vec::new(); // (row id, new equipId, equipType)
    let mut echo_skip: HashMap<i64, u32> = HashMap::new(); // AP location -> stock flag (ECHO-DEDUP)
    for (id, row) in repo.rows::<ShopLineupParam>() {
        let f = row.event_flag_for_stock();
        if f == 0 {
            continue;
        }
        let Some(&loc) = flag_to_loc.get(&f) else {
            continue;
        };
        let Some(s) = crate::scout_proof::lookup(loc) else {
            continue;
        };
        let Some(fid) = s.er_sell_id else { continue }; // own-world sellable category only
        let Some(etype) = equip_type_for(fid) else {
            continue;
        };
        // SHOP_CTD_GUARD REMOVED 2026-07-11 (Alaric). It bailed on WEAPON-category slots rewritten to
        // a NON-WEAPON reward, on a 3x CTD repro from 2026-07-03 (Longbow->Tear, Great Arrow->Smithing
        // Stone, Gostoc arrows->Talisman Pouch). That repro is now believed CONFOUNDED by the bag-add
        // nulling that was live at the time: `should_suppress_sold` returned 0 from the AddItemFunc
        // detour to suppress the native ware, and nulling a shop bag-add is the crash-adjacent path.
        // It is now DEAD CODE -- SOLD_SUPPRESS is never populated, so should_suppress_sold always
        // returns false and detour.rs can no longer null a shop add. The crash signature fits: a weapon
        // slot selling a NON-weapon reward is exactly the case that produces a non-weapon bag-add out of
        // a weapon purchase, i.e. the one add that could hit the nulling. (weapon->weapon never crashed,
        // and weapon-slot purchases bypass AddItemFunc entirely -- no add, no null, no crash.)
        // NOT PROVEN: `armor->goods is fine` also produces a non-weapon bag-add and did not crash, so
        // the theory has a hole. Opened anyway, deliberately, to settle it: Alaric is buying out every
        // shop next playtest. If it CTDs, restore the two-line guard here and we have our answer.
        // AUTO_UPGRADE (fixes the +0 weapon bug Alaric caught 2026-07-11, and opening the guard above
        // makes it WORSE -- more weapon slots now sell natively). `apply_auto_upgrade` lives inside
        // detour.rs `grant_full_id`, and ECHO-DEDUP deliberately SKIPS that grant for a rewritten slot
        // (the game already handed you the item), so a weapon bought from a rewritten slot never passes
        // through the only code that upgrades it -- it arrives at +0 even with auto_upgrade ON.
        // A weapon's reinforce level is encoded in its id (base + level), so bake the upgrade into the
        // id the slot SELLS. The shop then natively hands over an already-upgraded weapon and the grant
        // path is not needed. Inert when auto_upgrade is off (apply_auto_upgrade is identity).
        // Re-run on tier change: `run()` is idempotent and the tick re-arms it, so the stock tracks the
        // player's max reinforce tier as it climbs.
        let sell_fid = crate::upgrades::apply_auto_upgrade(fid as i32);
        let new_eid = er_codec::row_id_of(sell_fid as u32) as i32;
        if row.equip_id() != new_eid {
            plan.push((id, new_eid, etype));
        }
        // ECHO-DEDUP: this row sells the exact reward natively from here on, so a FUTURE
        // purchase must skip its echo grant. Checks already completed (flag set) are NOT
        // recorded -- e.g. a pre-rewrite-window buy sold the VANILLA ware and still needs
        // its echo to deliver the reward.
        if !crate::flags::get_event_flag(f) {
            echo_skip.insert(loc, f);
        }
    }
    let n = plan.len();
    for (id, eid, etype) in &plan {
        if let Some(row) = repo.get_mut::<ShopLineupParam>(*id) {
            row.set_equip_id(*eid);
            row.set_equip_type(*etype);
        }
    }
    let skip_count = echo_skip.len();
    *ECHO_SKIP.lock().unwrap() = Some(echo_skip);
    // Bag-add suppression RETIRED (ECHO-DEDUP): SOLD_SUPPRESS / ARMED_SUPPRESS stay
    // unpopulated, so should_suppress_sold() short-circuits false and the detour never nulls
    // a shop bag-add again. Native sale + echo-skip is the whole dedup now.
    log::info!(
        "shop-sell: rewrote {n} own-world slot(s) to natively sell their reward ({skip_count} echo-skip, cross-type OPEN, auto_upgrade baked)"
    );
    DONE.store(true, Ordering::Relaxed);
    true
}
