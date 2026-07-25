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

/// One armed row: `(stock flag, ShopLineupParam row id, sold equipId, equipType)`. The last two are
/// what [`echo_skip`] re-reads LIVE to prove the row still sells the reward -- a reverted row no
/// longer matches, which is the whole param-revert guard, so this tuple is load-bearing and gets a
/// name rather than an `#[allow(clippy::type_complexity)]`.
type EchoArm = (u32, u32, i32, u8);

/// ECHO-DEDUP (2026-07-03): {AP location -> (stock flag, row id, sold equipId, equipType)} for
/// every rewritten row whose check was still OPEN at run() time. The receive loop skips the echo
/// grant for these iff the stock flag is NOW SET **and the row still sells that reward LIVE**
/// (PARAM-REVERT GUARD, 2026-07-24: a map load streams ShopLineupParam back in and reverts the
/// rewrite while this map survives -- the set flag then proves only a VANILLA sale, and the
/// flag-only rule ate the AP item; Kalé "Note: Waypoint Ruins" repro). !collect / server-sent
/// items for un-bought checks still grant. Replaces bag-add suppression (statics stay unpopulated).
static ECHO_SKIP: Mutex<Option<HashMap<i64, EchoArm>>> = Mutex::new(None);

/// ShopLineupParam row ids this pass REWROTE to natively sell an own-world reward. `shop_repoint`
/// must leave these alone: the world deliberately keeps their `shopPreviewGoods` at the VANILLA ware
/// (the row sells the real item, so it needs no display override), and repointing on "preview differs
/// from the ware" would therefore drag the row off its reward and back onto the vanilla good --
/// undoing the native sale AND tripping ECHO-DEDUP's param-revert guard, which re-reads the row to
/// prove the purchase delivered the reward. Rebuilt by every `run()`.
static REWROTE_ROWS: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

/// Did `run()` rewrite this row to sell an own-world reward natively? False until the first run.
pub fn sold_natively(row_id: u32) -> bool {
    REWROTE_ROWS
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|s| s.contains(&row_id))
}

/// Has `run()` completed its pass? `shop_repoint` gates on this: until it latches, `REWROTE_ROWS` is
/// incomplete and the `derived_preview` fallback below has not yet read the rows' vanilla wares.
pub fn is_done() -> bool {
    DONE.load(Ordering::Relaxed)
}

/// CLIENT-SETTABLE flags (uniqueStartGrants obtained-flags + keyitems acquire flags): flags the
/// client itself writes outside any purchase. A check detected by one of these is EXEMPT from the
/// native rewrite AND the echo-arm (er_logic::shop_echo::echo_dedup_eligible) — flag-set no longer
/// proves a native sale there, and echo-skipping on it LOSES the AP item (START-GRANT collision,
/// 2026-07-24: unique grant set 60020/60110/60130, echoes for locs 7770011/12/13 were eaten).
static EXEMPT_FLAGS: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

pub fn configure(location_flags: HashMap<i64, u32>, exempt_flags: HashSet<u32>) {
    log::info!(
        "shop-sell: configured {} location flag(s), {} client-settable exempt flag(s)",
        location_flags.len(),
        exempt_flags.len()
    );
    *EXEMPT_FLAGS.lock().unwrap() = Some(exempt_flags);
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
/// check's reward natively, its stock flag is now set (the purchase actually happened), AND the
/// live row STILL carries the reward (er_logic::shop_echo::echo_skip_decision). The flag check
/// keeps !collect / server-sent items for un-bought checks grantable; the live-row check is the
/// PARAM-REVERT GUARD -- a map load can stream the vanilla ware back into the row while this map
/// survives, and a set flag then proves a vanilla sale, not a reward delivery. Unprovable
/// delivery (repo down, row missing, ware mismatched) GRANTS: a rare duplicate beats a lost item.
pub fn echo_skip(loc: i64) -> bool {
    let (flag, row_id, eid, etype) =
        match ECHO_SKIP.lock().unwrap().as_ref().and_then(|m| m.get(&loc)) {
            Some(&t) => t,
            None => return false,
        };
    let flag_set = crate::flags::get_event_flag(flag);
    if !flag_set {
        return false; // un-bought: echo must grant (!collect / server-sent)
    }
    // SAFETY: FD4 singleton, read-only use; called from the core tick (game thread), same
    // context run() writes rows from.
    let row_still_sells_reward = unsafe { SoloParamRepository::instance_mut() }
        .ok()
        .and_then(|repo| repo.get_mut::<ShopLineupParam>(row_id))
        .map(|row| row.equip_id() == eid && row.equip_type() == etype)
        .unwrap_or(false);
    let skip = er_logic::shop_echo::echo_skip_decision(flag_set, row_still_sells_reward);
    if !skip {
        log::warn!(
            "shop-sell: echo-skip REFUSED for loc {loc} -- stock flag {flag} is set but row \
             {row_id} no longer sells the reward (equipId {eid}/type {etype} expected; param \
             revert?). The sale delivered the vanilla ware; the echo grants the AP item."
        );
    }
    skip
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

/// Re-arm the pass: a map load streams ShopLineupParam back in, silently reverting every rewrite
/// while the DONE latch (and ECHO_SKIP) survive. Called by core.rs on the in_world false->true
/// edge, exactly like check_lots::reset / enemy_drops::reset (the 2026-07-21 DLC-leak fix this
/// pass was missing from -- the 2026-07-24 "vanilla ware sold, echo eaten" bug). ECHO_SKIP is NOT
/// cleared here: echo_skip() self-guards against the stale window via the live-row check, and the
/// next run() rebuild replaces it (dropping now-completed checks per the flag gate).
pub fn reset() {
    DONE.store(false, Ordering::Relaxed);
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
    let exempt_flags = EXEMPT_FLAGS.lock().unwrap().clone().unwrap_or_default();
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
    // Every row this pass sells natively -- see the note at the `owned.insert` below for why this is
    // NOT derived from `plan`.
    let mut owned: HashSet<u32> = HashSet::new();
    // AP location -> (stock flag, row id, sold equipId, equipType) (ECHO-DEDUP + revert guard)
    let mut echo_skip: HashMap<i64, EchoArm> = HashMap::new();
    // SKIP TALLY -- why a check row did NOT get a native rewrite. On a SOLO seed a large `no_scout` or
    // `no_sell_id` means the scout cache / apIdsToItemIds is THINNER than the check set, not that the
    // rewards are genuinely un-sellable. Do not reason about the rewrite count without this breakdown:
    // the 2026-07-13 Bedrock run rewrote 84 of ~410 shop checks and nothing in the log said why.
    let (mut check_rows, mut no_scout) = (0u32, 0u32);
    let (mut no_sell_id, mut no_equip_type) = (0u32, 0u32);
    let mut exempt_rows = 0u32; // client-settable flag -> no rewrite, no echo-arm
    // shopPreviewGoods FALLBACK (see core.rs). The pair shop_preview/shop_icon need is
    // (AP location -> the VANILLA ware in that shop row), and the vanilla ware is right here in the
    // row we are already reading. Capture it for every check row we do NOT rewrite. This MUST be read
    // inside this loop, before the rewrite below lands: afterwards the row's equipId is the AP reward,
    // not the vanilla ware, and the fallback would preview the item onto itself.
    let mut derived_preview: Vec<(i64, i32)> = Vec::new();
    let need_preview_fallback = !crate::shop_preview::is_configured();
    // Which configured flags actually landed on a live ShopLineupParam row. A flag in flag_to_loc with
    // NO live row is the interesting case: the location says "this check is a shop purchase guarded by
    // flag F", and no shop row in the game is guarded by F. Either the row is not in ShopLineupParam
    // (recipes / a different param), or the flag never came from a shop row at all.
    let mut matched_flags: HashSet<u32> = HashSet::new();
    let mut live_rows_with_flag = 0u32;
    for (id, row) in repo.rows::<ShopLineupParam>() {
        let f = row.event_flag_for_stock();
        if f == 0 {
            continue;
        }
        live_rows_with_flag += 1;
        let Some(&loc) = flag_to_loc.get(&f) else {
            continue;
        };
        check_rows += 1;
        matched_flags.insert(f);
        let vanilla = er_codec::full_id_from_equip_type(row.equip_type(), row.equip_id());
        // START-GRANT COLLISION FIX (2026-07-24): a check whose detection flag the CLIENT can set
        // outside a purchase (unique start grant / keyitems pool receive) is exempt from BOTH the
        // rewrite and the echo-arm -- together, never split: arming without rewriting eats the AP
        // item (the reproduced bug), rewriting without arming double-grants a genuine purchase.
        // The row keeps its vanilla ware; its echo always grants (pre-ECHO-DEDUP behaviour, which
        // never loses an item). Falls through to the shop_preview display-override like foreign/gem.
        if !er_logic::shop_echo::echo_dedup_eligible(f, &exempt_flags) {
            exempt_rows += 1;
            log::info!(
                "shop-sell: row {id} (stock flag {f}, loc {loc}) EXEMPT from native rewrite + \
                 echo-dedup -- flag is client-settable (start grant / key item receive), so \
                 flag-set does not prove a sale; echo will grant normally"
            );
            if let Some(v) = vanilla {
                derived_preview.push((loc, v));
            }
            continue;
        }
        let Some(s) = crate::scout_proof::lookup(loc) else {
            no_scout += 1;
            continue;
        };
        let Some(fid) = s.er_sell_id else {
            // foreign reward, or an own-world reward in a category we cannot sell as a shop ware
            // (gem/custom). Both fall through to the shop_preview display-override.
            no_sell_id += 1;
            if let Some(v) = vanilla {
                derived_preview.push((loc, v));
            }
            continue;
        };
        let Some(etype) = equip_type_for(fid) else {
            no_equip_type += 1;
            if let Some(v) = vanilla {
                derived_preview.push((loc, v));
            }
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
        // OWNERSHIP is recorded here, not from `plan`. `plan` holds only the rows that still NEED a
        // write, so on an idempotent re-run (the common case: the tick re-arms this pass and every
        // row already sells its reward) it is EMPTY -- and an ownership set built from it would say
        // this pass owns NOTHING. shop_repoint gates on that set, so it would then repoint 354 rows
        // straight off their native sale and onto a cosmetic placeholder. Observed doing exactly
        // that in Alaric's 2026-07-25 log: `shop-repoint: repointed 354 ... 0 owned by shop_sell`.
        // A row this pass has DECIDED to sell natively is owned whether or not the write was needed.
        owned.insert(id);
        if row.equip_id() != new_eid {
            plan.push((id, new_eid, etype));
        }
        // ECHO-DEDUP: this row sells the exact reward natively from here on, so a FUTURE
        // purchase must skip its echo grant. Checks already completed (flag set) are NOT
        // recorded -- e.g. a pre-rewrite-window buy sold the VANILLA ware and still needs
        // its echo to deliver the reward. The row identity + sold ware ride along so
        // echo_skip() can re-verify the row LIVE at echo time (PARAM-REVERT GUARD).
        if !crate::flags::get_event_flag(f) {
            echo_skip.insert(loc, (f, id, new_eid, etype));
        }
    }
    let n = plan.len();
    let mut overrides_cleared = 0u32;
    for (id, eid, etype) in &plan {
        if let Some(row) = repo.get_mut::<ShopLineupParam>(*id) {
            row.set_equip_id(*eid);
            row.set_equip_type(*etype);
            // CLEAR THE ROW'S OWN NAME OVERRIDE. `ShopLineupParam.nameMsgId` lets a shop row label
            // itself instead of using the ware's name, and rewriting equipId/equipType leaves that
            // override pointing at the ware we just replaced. The menu prefers it, so the slot keeps
            // showing the OLD item's name -- or, when the id does not resolve in the NEW ware's FMG
            // category, the `?ProtectorName?` / `?GoodsName?` tag.
            //
            // Alaric, playtest 2026-07-25: a slot reading `?ProtectorName?` over real armour stats
            // that paid out Blackflame Monk Greaves correctly, and the same Greaves displaying its
            // name perfectly IN THE INVENTORY. The item, the routing and the FMG were all fine; the
            // ROW was labelling itself. Only some vanilla rows carry an override, which is why most
            // slots looked right and a handful did not.
            //
            // -1 is the assumed "no override" sentinel. The OLD value is logged for every row that
            // had one, so if -1 turns out to be wrong the log says exactly what it was.
            if row.name_msg_id() != -1 {
                log::debug!(
                    "shop-sell: row {id} nameMsgId {} -> -1 (was labelling itself)",
                    row.name_msg_id()
                );
                row.set_name_msg_id(-1);
                overrides_cleared += 1;
            }
        }
    }
    if overrides_cleared > 0 {
        log::info!(
            "shop-sell: cleared {overrides_cleared} row-level nameMsgId override(s) -- those slots \
             were showing the name of the ware they USED to sell (or its tag)"
        );
    }
    let skip_count = echo_skip.len();
    *ECHO_SKIP.lock().unwrap() = Some(echo_skip);
    // Publish the rows we own BEFORE latching DONE, so shop_repoint (which gates on is_done) can
    // never observe a latched-but-empty set and repoint a row this pass owns.
    let owned_n = owned.len();
    *REWROTE_ROWS.lock().unwrap() = Some(owned);
    // Bag-add suppression RETIRED (ECHO-DEDUP): SOLD_SUPPRESS / ARMED_SUPPRESS stay
    // unpopulated, so should_suppress_sold() short-circuits false and the detour never nulls
    // a shop bag-add again. Native sale + echo-skip is the whole dedup now.
    log::info!(
        "shop-sell: rewrote {n} own-world slot(s) to natively sell their reward ({owned_n} owned in \
         total -- rewritten this pass plus already-correct; shop_repoint must not touch any of them) \
         ({skip_count} echo-skip, cross-type OPEN, auto_upgrade baked)"
    );
    log::info!(
        "shop-sell: skip tally -- {check_rows} check row(s) seen, {exempt_rows} exempt \
         (client-settable flag), {no_scout} no scout entry, {no_sell_id} no er_sell_id \
         (foreign/gem), {no_equip_type} unsellable category"
    );
    let unmatched: Vec<u32> = flag_to_loc
        .keys()
        .copied()
        .filter(|f| !matched_flags.contains(f))
        .collect();
    log::info!(
        "shop-sell: live ShopLineupParam rows with a stock flag = {live_rows_with_flag}; \
         {} configured flag(s) matched a live row, {} did NOT",
        matched_flags.len(),
        unmatched.len()
    );
    if need_preview_fallback {
        log::info!(
            "shop-preview/icon: derived {} (loc -> vanilla ware) pair(s) from live ShopLineupParam \
             (slot_data had no shopPreviewGoods)",
            derived_preview.len()
        );
        crate::shop_preview::configure(derived_preview.clone());
        crate::shop_icon::configure(derived_preview);
    }
    DONE.store(true, Ordering::Relaxed);
    true
}
