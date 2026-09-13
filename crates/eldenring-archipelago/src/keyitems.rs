//! Obtained-flag + great-rune-restore tables, ported from the standalone `features.rs`.
//!
//! Some vanilla items gate a FEATURE on an "obtained" event flag that a raw goods-grant never trips
//! (summon tutorial, whetblade affinities, the Rold lift, the Volcano drawing-room transition). When
//! such an item is RECEIVED, we set that flag so the feature actually opens. Great runes set their
//! "restored" event flag (191-196; the SetEventFlagID in Divine-Tower common event 90005110) so the
//! received rune is usable immediately (Divine Altar activation) WITHOUT the Divine Tower trip --
//! which under num_regions may sit in a sealed region. Leyndell's separate two-rune threshold is
//! reconciled from AP receipts below. All idempotent: flags are save-persisted.
//!
//! The AP catalog maps each great rune to the boss-drop goods row (8148-8153), and delivery keeps
//! those rows exactly as the seed sends them (clients#392). #392 also concluded that the restored
//! rows 191-196 CANNOT be granted -- AddItem accepts them and materialises them nowhere, Corni's
//! re-grant loop. That reading came through the client's own LEN-BOUNDED key-list walk, which
//! Tako's 2026-09-05/06 logs showed cannot see the newest key items after an NPC hand-in
//! (`er_logic::key_list_window`); whether a restored row lands is UNSETTLED and is the probe that
//! gates any delivery change (client #316). What IS settled: the grace's "Great Runes" menu entry
//! is gated on holding a goodsType-15 good (t000001000.py `PlayerHasTool(15)`), which the boss row
//! is not, so boss row + restore flag is NOT an equippable end state. This module supplies the
//! matching restore flag and disarms the Divine-Tower award event; it does not issue a second
//! goods grant.
//!
//! The pre-arm below also makes common_func event 90005110 exit on its first line for every rune,
//! so a Divine Tower shows no prompt for an AP rune by construction (#316's "no prompt").

use crate::flags;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{
    Mutex,
    atomic::{AtomicU32, Ordering},
};

/// Obtained/restored flags THIS SESSION'S client actually flipped unset -> set, i.e. the writes
/// the flag poll must not read back as pickups (`er_logic::keyitem_poll`). Only the two writers
/// below feed it, and only when the flag read UNSET first -- a flag the player had already earned
/// is never recorded, so their genuine check still reports. Session-scoped: an earlier session's
/// receive is covered by `core::flag_poll_baseline` instead.
static CLIENT_WRITTEN_FLAGS: Mutex<er_logic::keyitem_poll::ClientWrites> =
    Mutex::new(er_logic::keyitem_poll::ClientWrites::empty());

/// Restore flags for Great Runes that exist in THIS seed's item map. These are armed at slot-data
/// parse time, before receipt: event 90005110 does not check possession of the boss-drop rune before
/// awarding its vanilla restored copy, so receipt-only reconciliation loses when the altar is used
/// first (#731). Empty until configured, and cleared on a seed switch.
static SEED_GREAT_RUNE_FLAGS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Companion items whose possession is gated by a vanilla "obtained" event flag.
///
/// WHETBLADES LIVE IN `er_logic::whetblade`, NOT HERE (2026-07-30, superseding the 2026-07-30 AM
/// "derived flags only" model). Ground truth from the Hexinton CE table: the smithing menu keys
/// each affinity on ONE flag, and a whetblade's PICKUP flag (65610/65640/65660/65680/65720) IS its
/// first affinity's unlock (Iron 65610 = Heavy; Black 65720 = Occult; ...) -- common.emevd event
/// 1450 only adds the SIBLING affinities. So the previous table here, which set the siblings and
/// deliberately skipped the pickup flag (because it doubles as the world's CHECK flag -- the
/// Eldakin 2026-07-29 false-collect), shipped every pool-received whetblade missing exactly one
/// affinity. The fix is a SPLIT, not a choice: core.rs repoints those checks onto client-owned
/// flags (er_logic::whetblade::repoint_poll_flags + whetblade_lots.rs rewriting the lot's
/// getItemFlagId), after which the FULL affinity set -- pickup flag included -- is safe to set on a
/// receive: nothing polls it, and the treasure's spawn is governed by the new flag. The whetblade
/// entries below therefore come from er_logic::whetblade::WHETBLADES ([`entries`]), the same table
/// that drives the repoint, so the two mechanisms cannot drift apart.
///
/// Bell/Knife/Kit are DIFFERENT: 60110/60130/60120 are set by ESD/EMEVD scripts and read directly
/// by vanilla events -- there is no lot getItemFlagId to repoint -- so they must stay even though
/// they are also check flags (locs 7770012/7770014/7770013). The false collect they used to cause
/// is FIXED (2026-09-07), flagpoll-side as predicted, but not by suppressing those locations
/// outright: with no lot and no native shop rewrite, the vanilla flag is the ONLY signal a genuine
/// acquisition makes, so a blanket suppression would lose the real check. Instead the client
/// records the flags IT flipped unset -> set ([`client_written_flags`], fed by the two writers
/// below, both of which write only when the flag reads unset) and `er_logic::keyitem_poll` tells
/// the poll to ignore exactly those. A flag the PLAYER set first is never recorded, so their check
/// still reports; an earlier session's receive is held by `core::flag_poll_baseline`. Same guard
/// covers Rold 400001 (the reported case, loc 7770556), Drawing-Room 400072 and any restore flag
/// 191-196 a seed happens to poll.
///
/// The Kit's coupling is the vanilla grant itself: the extracted common event gives it as
/// `DirectlyGivePlayerItem(ItemType.Goods, 8500, 60120, 1)`, and the Crafting menu reads 60120, not
/// inventory -- a pool-granted kit without the flag leaves crafting disabled (client#335, playtest
/// 2026-08-20: "found it ... although i cant use it even when out of combat"; workaround was
/// `!setflag 60120 1`).
const COMPANION_ACQUIRE_FLAGS: &[(&str, &[u32])] = &[
    ("Spirit Calling Bell", &[60110]),
    ("Whetstone Knife", &[60130]),
    ("Crafting Kit", &[60120]),
];

/// Vanilla key items whose progression gate reads an obtained event flag, not inventory -- plus the
/// six great runes, whose "restored" event flag (191-196) is the state half of the working grant:
/// the GOODS half is the boss-drop row 8148-8153 delivered as-sent (clients#392 -- the restored
/// goods rows cannot be AddItem'd; flag + boss-drop row is the empirically working combination).
const KEY_ITEM_ACQUIRE_FLAGS: &[(&str, &[u32])] = &[
    ("Rold Medallion", &[400001]),   // Grand Lift of Rold
    ("Drawing-Room Key", &[400072]), // Volcano Manor drawing-room transition
    ("Godrick's Great Rune", &[191]),
    ("Radahn's Great Rune", &[192]),
    ("Morgott's Great Rune", &[193]),
    ("Rykard's Great Rune", &[194]),
    ("Mohg's Great Rune", &[195]),
    ("Malenia's Great Rune", &[196]),
];

/// Vanilla's Great Rune POSSESSION flags, the 170-179 band that every rune-count gate reads.
///
/// The reported bug (2026-09-13, player Zelda): six AP-delivered Great Runes, and thefifthmatt's
/// randomizer rune gates ("4 runes for Leyndell, 3 to finish", message 20003) stayed shut. Vanilla
/// counts runes with `CountEventFlags(EventFlag, 170, 179) >= N` (common.emevd $Event(730), line
/// ~1110) and matt's gates reuse that pattern with their own thresholds. These flags are the
/// `getItemFlagId` of the boss-drop rune lots (greenfield/flag_lots.tsv): 171 -> lot 10010 goods
/// 8148, 172 -> 10301/8149, 173 -> 10041/8150, 174 -> 10221/8151, 175 -> 10121/8152, 176 ->
/// 10201/8153; 177 is the Great Rune of the Unborn, which vanilla derives from flag 197 in
/// common.emevd $Event(6905) (~line 3123, the event that sets 171-177 from the boss/197 flags).
/// Nothing in an AP delivery runs 6905 or trips a lot, so the whole band read ZERO.
///
/// The restore flags 191-196 above are a DIFFERENT thing and do not substitute: they make a
/// received rune usable at a Divine Altar without the tower trip. Only 171-177 are counted.
/// `LEYNDELL_TWO_RUNES_FLAGS` writes the vanilla seal's RESULT flags 182/105 directly, which fixes
/// that ONE seal and leaves every other consumer of the band reading zero -- hence this table.
///
/// WHY THIS IS NOT IN [`entries`], AND IS SEED-AWARE. On current seeds 171-176 wear THREE hats:
/// the counted possession flag, the `getItemFlagId` of the boss-drop rune lot, and the DETECTION
/// flag of that boss-rune location (greenfield/eldenring/tables/data.py: `Stormveil :: Godrick's
/// Great Rune - Godrick [f171]` = loc 7770001 ... `Haligtree :: Malenia's ... [f176]` = 7770006).
/// Setting one on RECEIPT, before the boss dies, marks the lot collected: the kill then awards
/// nothing, the flag never transitions unset -> set, and the location can never be sent -- and the
/// per-tick re-assert makes that permanent. The #659 `keyitem_poll` guard does NOT rescue it: that
/// guard only stops the phantom-check direction, it cannot resurrect a kill whose sole witness flag
/// was pre-set. So the band cannot ride the unconditional [`entries`] path the Rold Medallion /
/// Bell / Knife / Kit take.
///
/// Instead the decision is made per rune, per seed, by `er_logic::great_rune_possession`
/// ([`configure_great_rune_possession`], [`tick_great_rune_possession_flags`]): a flag no location
/// in this seed detects is set unconditionally (always 177; all seven once the world-side repoint
/// moves the six locations onto the shardbearer defeat flags 510010/510300/510040/510220/510120/
/// 510200), and a flag that IS detected is set only once the check is banked. The flags still reach
/// [`all_acquire_flags`], so the poll guard and the shop echo-dedup exemption cover the writes the
/// unconditional path makes.
///
/// LEYNDELL HoldClosed INTERACTION. While the synthetic AP region wall is armed and the player has
/// fewer AP runes than `leyndell_runes_required`, vanilla event 730 can now DERIVE flag 182 from
/// these possession flags (its own threshold is 2) on the same tick `tick_leyndell_gate_flags`
/// clears it. We deliberately keep the possession flags TRUTHFUL and keep clearing 182: withholding
/// them would re-break matt's gates, which is the bug being fixed, and 182 is a derived result flag
/// that only the physical vanilla seal reads. Both writes are idempotent single flag writes, so the
/// steady state is a cheap clear per tick, not a correctness problem; the only real hazard was log
/// flood, and the HoldClosed branch's success line is now latched to once per session the same way
/// its warn already was.
///
/// Band-detected runes in THIS seed: possession flag -> the location id(s) detected on it.
///
/// `None` means NOT YET CLASSIFIED (pre-connect, or after a seed switch), and the writers treat
/// that as "touch nothing". An empty map is a real answer -- a repointed seed where every rune is
/// unconditional -- so the two states must not be conflated: defaulting an unclassified seed to
/// "no collisions" is exactly the pre-set that loses the check.
static GREAT_RUNE_BAND_DETECTED: Mutex<Option<BTreeMap<u32, Vec<i64>>>> = Mutex::new(None);

const GREAT_RUNE_NAMES: &[&str] = &[
    "Godrick's Great Rune",
    "Radahn's Great Rune",
    "Morgott's Great Rune",
    "Rykard's Great Rune",
    "Mohg's Great Rune",
    "Malenia's Great Rune",
    "Great Rune of the Unborn",
];

// The physical seal checks BOTH flags. Vanilla normally supplies 105 through Roundtable/Finger
// Reader progression and derives 182 from the rune-location flags, but either half can be absent
// when AP supplies the runes and starts the player past that quest sequence.
const LEYNDELL_TWO_RUNES_FLAGS: &[u32] = &[105, 182];

/// One warning bit per Leyndell prerequisite. A rejected/lost write retries every stable tick, but
/// says so once instead of flooding the log. The bit clears when readback eventually confirms.
static LEYNDELL_GATE_WARNED: AtomicU32 = AtomicU32::new(0);

pub fn received_great_rune_count(received: &HashSet<String>) -> usize {
    GREAT_RUNE_NAMES
        .iter()
        .filter(|name| received.contains(**name))
        .count()
}

/// Non-location prerequisites owed to the physical Leyndell seal by the cumulative AP receive
/// stream. This is shared by the active reconciler and the runtime-fallback handler so normal
/// delivery, server `/send`, reconnect replay, and `RECONCILE_APPLY=none` cannot diverge.
pub fn leyndell_gate_flags(received: &HashSet<String>, runes_required: usize) -> Vec<u32> {
    if er_logic::region_lock::leyndell_gate_flag_action(
        runes_required,
        received_great_rune_count(received),
    ) == er_logic::region_lock::LeyndellGateFlagAction::Open
    {
        LEYNDELL_TWO_RUNES_FLAGS.to_vec()
    } else {
        Vec::new()
    }
}

/// Dedicated self-healing backstop for the physical Leyndell seal.
///
/// This deliberately runs even when the desired-state reconciler owns ordinary flag writes. Both
/// paths derive the same two flags from [`leyndell_gate_flags`] and both are idempotent, while this
/// path gives the seal an independent readback/retry boundary. Bobler's 2026-08-18 playtest reached
/// four AP Great Runes under the active reconciler but still found the wall closed; coupling the
/// only fallback to `!owns_flags()` left no recovery or named evidence for that state.
pub fn tick_leyndell_gate_flags(received: &HashSet<String>, runes_required: usize) {
    let rune_count = received_great_rune_count(received);
    match er_logic::region_lock::leyndell_gate_flag_action(runes_required, rune_count) {
        er_logic::region_lock::LeyndellGateFlagAction::Unmanaged => return,
        er_logic::region_lock::LeyndellGateFlagAction::HoldClosed => {
            // Vanilla event 730 derives 182 from LOCAL shardbearer progress. While the synthetic
            // AP wall is armed, that must not impersonate AP Great Rune receipts. Leave flag 105
            // alone -- it is ordinary Roundtable quest state; holding only the counted-rune half
            // false is sufficient to keep the physical seal closed.
            if flags::get_event_flag(182) {
                let accepted = flags::try_set_event_flag(182, false);
                if flags::get_event_flag(182) {
                    if LEYNDELL_GATE_WARNED.fetch_or(1 << 2, Ordering::Relaxed) & (1 << 2) == 0 {
                        log::warn!(
                            "great runes: {rune_count}/{runes_required} AP-received -- vanilla \
                             Leyndell flag 182 did not clear (write accepted={accepted}); retrying"
                        );
                    }
                } else {
                    LEYNDELL_GATE_WARNED.fetch_and(!(1 << 2), Ordering::Relaxed);
                    // Since the possession flags 171-177 became truthful, vanilla event 730 can
                    // re-derive 182 (threshold 2) on EVERY tick while the AP wall holds a higher
                    // `leyndell_runes_required`. The clear stays -- it is idempotent and it is the
                    // only thing keeping the physical seal shut -- but the success line is latched
                    // to once per HoldClosed spell (bit 3) so a per-frame write fight cannot flood
                    // the log. The bit clears when the gate leaves HoldClosed.
                    if LEYNDELL_GATE_WARNED.fetch_or(1 << 3, Ordering::Relaxed) & (1 << 3) == 0 {
                        log::info!(
                            "great runes: {rune_count}/{runes_required} AP-received -- held \
                             vanilla Leyndell flag 182 closed (re-derived from possession flags \
                             171-177 by vanilla event 730; clearing every tick, logging once)"
                        );
                    }
                }
            }
            return;
        }
        er_logic::region_lock::LeyndellGateFlagAction::Open => {
            LEYNDELL_GATE_WARNED.fetch_and(!((1 << 2) | (1 << 3)), Ordering::Relaxed);
        }
    }
    let mut applied = Vec::new();
    for (index, flag) in leyndell_gate_flags(received, runes_required)
        .into_iter()
        .enumerate()
    {
        let bit = 1u32 << index;
        if flags::get_event_flag(flag) {
            LEYNDELL_GATE_WARNED.fetch_and(!bit, Ordering::Relaxed);
            continue;
        }
        let accepted = flags::try_set_event_flag(flag, true);
        if flags::get_event_flag(flag) {
            LEYNDELL_GATE_WARNED.fetch_and(!bit, Ordering::Relaxed);
            applied.push(flag);
        } else if LEYNDELL_GATE_WARNED.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
            log::warn!(
                "great runes: {rune_count} AP-received -- Leyndell prerequisite flag {flag} \
                 did not stick (write accepted={accepted}); retrying every stable in-world tick"
            );
        }
    }
    if !applied.is_empty() {
        let state: Vec<(u32, bool)> = LEYNDELL_TWO_RUNES_FLAGS
            .iter()
            .map(|&flag| (flag, flags::get_event_flag(flag)))
            .collect();
        log::info!(
            "great runes: {rune_count} AP-received -- Leyndell gate prerequisite flag(s) \
             {applied:?} applied; readback {state:?}"
        );
    }
}

/// Every (item name, obtained flags) pair this module applies: the two local tables plus the
/// whetblade affinity sets from `er_logic::whetblade` -- ONE source shared with the check repoint,
/// so a whetblade's receive-set flags and its repointed check can never disagree by drift.
fn entries() -> impl Iterator<Item = (&'static str, &'static [u32])> {
    COMPANION_ACQUIRE_FLAGS
        .iter()
        .chain(KEY_ITEM_ACQUIRE_FLAGS)
        .copied()
        .chain(
            er_logic::whetblade::WHETBLADES
                .iter()
                .map(|w| (w.name, w.affinity_flags)),
        )
}

fn seed_great_rune_flags(names: &[String]) -> Vec<u32> {
    let names: HashSet<&str> = names.iter().map(String::as_str).collect();
    KEY_ITEM_ACQUIRE_FLAGS
        .iter()
        .filter(|(name, _)| names.contains(*name))
        .flat_map(|(_, fs)| fs.iter().copied())
        .filter(|f| (191..=196).contains(f))
        .collect()
}

/// Configure the altar-disarm set from the seed's `apIdsToItemIds` names. Deriving from the seed
/// map keeps foreign/older worlds additive: only runes the server says can arrive are touched.
pub fn configure_seed_great_runes(names: &[String]) {
    let configured = seed_great_rune_flags(names);
    log::info!(
        "great-rune altars: {} restore flag(s) armed from the seed item map",
        configured.len()
    );
    *SEED_GREAT_RUNE_FLAGS.lock().unwrap() = configured;
}

/// Clear seed-scoped configuration before parsing a different room.
pub fn reset_seed_great_runes() {
    SEED_GREAT_RUNE_FLAGS.lock().unwrap().clear();
    LEYNDELL_GATE_WARNED.store(0, Ordering::Relaxed);
    // The poll guard is seed-scoped too: a different room has a different locationFlags table, and
    // the collision set core.rs holds is rebuilt at its configure.
    CLIENT_WRITTEN_FLAGS.lock().unwrap().clear();
    // Back to UNCLASSIFIED, not to "no collisions": a different room has a different locationFlags
    // table, and until it is parsed no possession flag may be written.
    *GREAT_RUNE_BAND_DETECTED.lock().unwrap() = None;
}

/// Disarm vanilla's altar awards before the matching AP rune arrives. The event flag is the latch;
/// retry every stable tick because menu/load-time writes can be discarded by the game.
pub fn tick_seed_great_rune_altars() {
    let configured = SEED_GREAT_RUNE_FLAGS.lock().unwrap().clone();
    let mut applied = 0u32;
    for f in configured {
        if !flags::get_event_flag(f) && flags::try_set_event_flag(f, true) {
            applied += 1;
        }
    }
    if applied > 0 {
        log::info!("great-rune altars: {applied} restore flag(s) applied before vanilla award");
    }
}

/// Classify the Great Rune possession band for THIS seed, once, at slot_data parse.
///
/// `location_flags` is the merged, already whetblade-repointed poll table core.rs is about to
/// install, i.e. exactly what the flag poll will read. Any of 171-177 appearing in it means a
/// location is detected on that flag and the client must not pre-set it; see the
/// `er_logic::great_rune_possession` module doc for the lost-check mechanism.
pub fn configure_great_rune_possession(location_flags: &HashMap<i64, u32>) {
    let detected = er_logic::great_rune_possession::band_detected(location_flags);
    log::info!(
        "{}",
        er_logic::great_rune_possession::classification_line(&detected)
    );
    *GREAT_RUNE_BAND_DETECTED.lock().unwrap() = Some(detected);
}

/// The boss-rune location ids this seed still detects on a possession flag, sorted.
///
/// core.rs asks for these so it can resolve just those few ids against the server checked set each
/// tick instead of materialising the whole set. Empty on a repointed seed and before configure.
pub fn great_rune_band_locations() -> Vec<i64> {
    let guard = GREAT_RUNE_BAND_DETECTED.lock().unwrap();
    let mut out: Vec<i64> = guard
        .iter()
        .flat_map(|d| d.values())
        .flatten()
        .copied()
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Set the possession flags a RECEIPT may set immediately — i.e. only runes in unconditional mode.
///
/// A band-detected rune is deliberately skipped here and left to
/// [`tick_great_rune_possession_flags`], which re-evaluates every tick and sets it the moment its
/// check is banked. Called from the receive dispatch beside [`set_acquire_flags`].
pub fn set_great_rune_possession_flag(name: &str) {
    let Some(flag) = er_logic::great_rune_possession::possession_flag(name) else {
        return;
    };
    let guard = GREAT_RUNE_BAND_DETECTED.lock().unwrap();
    let Some(detected) = guard.as_ref() else {
        return; // not classified yet: the tick will handle it once slot_data has been parsed
    };
    let already = flags::get_event_flag(flag);
    if already {
        return;
    }
    if !er_logic::great_rune_possession::may_set(flag, detected, already, |_| false) {
        return;
    }
    drop(guard);
    flags::set_event_flag(flag, true);
    CLIENT_WRITTEN_FLAGS.lock().unwrap().record(flag);
}

/// Per-tick reconciler for the counted Great Rune possession band (vanilla 171-177).
///
/// Re-evaluated every settled tick precisely so a LATER event can flip a rune from held-back to
/// settable: the player kills the shardbearer (the game sets the flag itself, and the poll sends the
/// location), or the location is otherwise checked on the server. `checked` is the subset of this
/// seed's band-detected location ids the server already considers checked.
///
/// BY DESIGN THIS UNDERCOUNTS. On a current (un-repointed) seed a Great Rune received before its
/// boss dies does NOT raise vanilla's `CountEventFlags(EventFlag, 170, 179)` until that boss is
/// killed, so a rune gate can stay shut while the player's AP inventory says otherwise. That is the
/// deliberate trade: the flag is the boss lot's `getItemFlagId`, so pre-setting it marks the lot
/// collected, the kill awards nothing, the flag never transitions, and the location can never be
/// sent -- an unrecoverable dead check versus a temporarily shut gate that the next kill opens. The
/// world-side repoint (moving the six locations' detection onto the shardbearer defeat flags
/// 510010/510300/510040/510220/510120/510200) removes the collision and puts new seeds on the
/// unconditional path, where there is nothing left to undercount.
pub fn tick_great_rune_possession_flags(
    received: &HashSet<String>,
    checked: &std::collections::HashSet<i64>,
) {
    let guard = GREAT_RUNE_BAND_DETECTED.lock().unwrap();
    let Some(detected) = guard.as_ref().cloned() else {
        return;
    };
    drop(guard);
    let mut applied: Vec<u32> = Vec::new();
    for &(name, flag) in er_logic::great_rune_possession::POSSESSION_FLAGS {
        if !received.contains(name) {
            continue;
        }
        // The flag is its own latch, exactly like `tick_keyitem_flags`: already set means either we
        // set it earlier or the player earned it, and either way there is nothing to write.
        if flags::get_event_flag(flag) {
            continue;
        }
        if !er_logic::great_rune_possession::may_set(flag, &detected, false, |loc| {
            checked.contains(&loc)
        }) {
            continue;
        }
        if flags::try_set_event_flag(flag, true) {
            applied.push(flag);
            // Same record as the other writers: this write is OURS, so the poll must not read it
            // back as a pickup of the boss-rune check that shares the flag (keyitem_poll, #659).
            CLIENT_WRITTEN_FLAGS.lock().unwrap().record(flag);
        }
    }
    if !applied.is_empty() {
        log::info!(
            "great-rune possession: vanilla flag(s) {applied:?} applied (counted by \
             CountEventFlags 170-179)"
        );
    }
}

/// The vanilla obtained / great-rune restored flag(s) mapped to a received item `name` (empty if
/// none). READ-ONLY companion to [`set_acquire_flags`]: the reconciler's dry-run mapper
/// (`reconcile_io::build_desired_inputs`) uses it to classify a received item as an
/// `ItemSemantics::KeyItem { goods, obtained_flags }` from the SAME table the live path applies,
/// so the two never drift.
pub fn acquire_flags(name: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for (n, fs) in entries() {
        if n == name {
            out.extend_from_slice(fs);
        }
    }
    out
}

/// EVERY flag these tables can set — i.e. every vanilla obtained/restored flag the CLIENT ITSELF
/// writes on a pool receive, outside any shop purchase. Feeds the shop_sell ECHO-DEDUP exemption
/// set (er_logic::shop_echo): a check detected by one of these flags must never be echo-armed,
/// because flag-set does not prove a native sale (START-GRANT collision, 2026-07-24).
pub fn all_acquire_flags() -> impl Iterator<Item = u32> {
    entries()
        .flat_map(|(_, fs)| fs.iter().copied())
        // The Great Rune possession band is NOT part of `entries()` (its writes are seed-gated, see
        // `GREAT_RUNE_BAND_DETECTED`), but the client does write it on the unconditional path, so it
        // still belongs in the poll-guard collision set and the shop echo-dedup exemption. Harmless
        // on a seed where no location carries these flags: `colliding_checks` finds nothing.
        .chain(er_logic::great_rune_possession::all_possession_flags())
}

/// Obtained/restored flags the client itself has flipped unset -> set THIS SESSION.
///
/// Read by the flag poll through `er_logic::keyitem_poll::poll_suppressed`: a check whose poll flag
/// is in here was set by a RECEIVE, not by the player, so reporting it would be the 2026-09-07
/// Rold-Medallion false collect. A flag the player earned first is absent by construction (both
/// writers record only a write that found the flag unset), so genuine checks still report.
pub fn client_written_flags() -> std::collections::BTreeSet<u32> {
    CLIENT_WRITTEN_FLAGS.lock().unwrap().flags().clone()
}

/// Fast-path one-shot: set the vanilla obtained/restored flag(s) for a received item name, if any.
/// Idempotent, but BEST-EFFORT -- writes at menu/load are silently discarded (R3, SWEEP), so this
/// no longer logs success; `tick_keyitem_flags` (the reconcile tick) re-applies and owns the log.
pub fn set_acquire_flags(name: &str) {
    for (n, fs) in entries() {
        if n == name {
            for &f in fs {
                // Record the write only when the flag read UNSET: that is exactly the case the
                // CLIENT caused, and the poll guard suppresses nothing else (keyitem_poll).
                let ours = !flags::get_event_flag(f);
                flags::set_event_flag(f, true);
                if ours {
                    CLIENT_WRITTEN_FLAGS.lock().unwrap().record(f);
                }
            }
        }
    }
}

/// Per-tick reconciler (R3, SWEEP; house pattern: `region::tick_reconcile_received_locks`): for
/// every RECEIVED key-item name with mapped obtained flags, try_set any flag that hasn't stuck.
/// The flag itself is the latch (unset -> attempt, set -> skip), so a one-shot write lost at
/// menu/load self-heals on the next settled tick, and once all flags read back set this is a
/// cheap no-op. Logs on the tick a flag actually lands (once per name in the normal case).
pub fn tick_keyitem_flags(received: &std::collections::HashSet<String>) {
    for (n, fs) in entries() {
        if !received.contains(n) {
            continue;
        }
        let mut applied = 0u32;
        for &f in fs {
            if !flags::get_event_flag(f) && flags::try_set_event_flag(f, true) {
                applied += 1;
                // Same latch, same record: this write is ours, so the poll must not read it back
                // as a pickup of the check that shares the flag (keyitem_poll).
                CLIENT_WRITTEN_FLAGS.lock().unwrap().record(f);
            }
        }
        if applied > 0 {
            log::info!(
                "key item '{n}': obtained/restored flag(s) {fs:?} applied ({applied} newly set)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_runes_disarm_only_their_own_altars_before_receipt() {
        let names = vec![
            "Rykard's Great Rune".to_string(),
            "Malenia's Great Rune".to_string(),
            "Rune Arc".to_string(),
        ];
        assert_eq!(seed_great_rune_flags(&names), vec![194, 196]);
    }

    #[test]
    fn ap_rune_count_includes_all_seven_identities() {
        let received: HashSet<String> = GREAT_RUNE_NAMES
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        assert_eq!(received_great_rune_count(&received), 7);
    }

    #[test]
    fn unrelated_items_and_duplicates_do_not_inflate_the_rune_count() {
        let received = HashSet::from([
            "Godrick's Great Rune".to_string(),
            "Great Rune of the Unborn".to_string(),
            "Rune Arc".to_string(),
        ]);
        assert_eq!(received_great_rune_count(&received), 2);
    }

    /// The possession band is SEED-GATED and therefore must NOT ride the unconditional
    /// [`entries`] mapping: `acquire_flags` feeds the reconciler's desired-state path, which has no
    /// idea whether this seed still detects a boss-rune location on 171-176. The gated writers
    /// (`set_great_rune_possession_flag` / `tick_great_rune_possession_flags`) own that band.
    #[test]
    fn the_receive_mapping_never_carries_a_possession_flag() {
        for &name in GREAT_RUNE_NAMES {
            assert!(
                acquire_flags(name).iter().all(|f| !(171..=177).contains(f)),
                "{name}: the counted 170-179 band must not enter the unconditional receive mapping"
            );
        }
        // The six boss runes still map their RESTORE flag, which is a different thing (Divine
        // Altar state) and carries no lot.
        assert_eq!(acquire_flags("Godrick's Great Rune"), vec![191]);
        // The Unborn rune has no restore flag and no boss lot at all.
        assert!(acquire_flags("Great Rune of the Unborn").is_empty());
        assert!(acquire_flags("Rune Arc").is_empty());
    }

    /// The seed classification core.rs installs at connect, and the location ids it hands back to
    /// the tick. Current world data keys the six boss-rune locations on 171-176; 177 never carries
    /// one (7770007 is Rennala's Remembrance on flag 197).
    #[test]
    fn connect_classifies_the_band_from_the_seed_location_table() {
        // Unclassified is not "no collisions": nothing may be written before slot_data is parsed.
        reset_seed_great_runes();
        assert!(great_rune_band_locations().is_empty());
        assert!(GREAT_RUNE_BAND_DETECTED.lock().unwrap().is_none());

        configure_great_rune_possession(&HashMap::from([
            (7_770_001i64, 171u32),
            (7_770_002, 172),
            (7_770_003, 173),
            (7_770_004, 174),
            (7_770_005, 175),
            (7_770_006, 176),
            (7_770_007, 197), // Rennala's Remembrance -- flag 197, not 177
            (7_770_099, 510_800),
        ]));
        assert_eq!(
            great_rune_band_locations(),
            vec![
                7_770_001, 7_770_002, 7_770_003, 7_770_004, 7_770_005, 7_770_006
            ]
        );
        let detected = GREAT_RUNE_BAND_DETECTED.lock().unwrap().clone().unwrap();
        assert!(!detected.contains_key(&177), "177 carries no location");

        // A post-repoint seed: the six locations detect the shardbearer DEFEAT flags instead, so
        // every rune goes unconditional and the tick has nothing to wait for.
        configure_great_rune_possession(&HashMap::from([
            (7_770_001i64, 510_010u32),
            (7_770_002, 510_300),
            (7_770_006, 510_200),
        ]));
        assert!(great_rune_band_locations().is_empty());
        assert!(
            GREAT_RUNE_BAND_DETECTED
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(BTreeMap::is_empty)
        );
        reset_seed_great_runes();
    }

    /// The safety half: because 171-176 double as this seed's boss-rune check flags (locs
    /// 7770001-7770006 in greenfield data.py), they MUST reach the client-written guard set, or a
    /// receipt on the unconditional path becomes a phantom check of the boss location.
    #[test]
    fn possession_flags_reach_the_client_written_poll_guard() {
        let settable: HashSet<u32> = all_acquire_flags().collect();
        for flag in 171..=177u32 {
            assert!(
                settable.contains(&flag),
                "possession flag {flag} must be in all_acquire_flags() for the #659 poll guard"
            );
        }
        let poll = std::collections::HashMap::from([
            (7770001i64, 171u32), // Stormveil :: Godrick's Great Rune - Godrick [f171]
            (7770006, 176),       // Haligtree :: Malenia's Great Rune - Malenia [f176]
            (7770099, 510800),    // an ordinary check -- must never be guarded
        ]);
        let collisions = er_logic::keyitem_poll::colliding_checks(&poll, &settable);
        assert_eq!(collisions, vec![(7770001, 171), (7770006, 176)]);
        let collisions: std::collections::HashMap<i64, u32> = collisions.into_iter().collect();
        let written = std::collections::BTreeSet::from([171u32]);
        assert!(
            er_logic::keyitem_poll::poll_suppressed(7770001, 171, &collisions, &written),
            "a client-written possession flag must not report as a pickup"
        );
        assert!(
            !er_logic::keyitem_poll::poll_suppressed(7770006, 176, &collisions, &written),
            "a rune the PLAYER looted must still report its check"
        );
    }

    /// Possession flags are not restore flags: the altar pre-arm set stays 191-196 only, so
    /// arming a seed cannot silently pre-set a boss-rune CHECK flag before any receipt.
    #[test]
    fn seed_altar_prearm_never_touches_the_possession_band() {
        let names: Vec<String> = GREAT_RUNE_NAMES.iter().map(|n| (*n).to_string()).collect();
        assert_eq!(
            seed_great_rune_flags(&names),
            vec![191, 192, 193, 194, 195, 196]
        );
    }

    #[test]
    fn leyndell_gate_reconciles_both_non_location_prerequisites() {
        let one = HashSet::from(["Godrick's Great Rune".to_string()]);
        let two = HashSet::from([
            "Godrick's Great Rune".to_string(),
            "Great Rune of the Unborn".to_string(),
        ]);
        assert!(leyndell_gate_flags(&one, 2).is_empty());
        assert_eq!(leyndell_gate_flags(&two, 2), vec![105, 182]);
        assert!(
            leyndell_gate_flags(&two, 0).is_empty(),
            "gate-off seeds leave vanilla flag state unmanaged"
        );
        assert!(
            LEYNDELL_TWO_RUNES_FLAGS
                .iter()
                .all(|flag| !(171..=177).contains(flag))
        );
    }

    /// The motivating case (rule 11): a pool-received whetblade must unlock ITS FULL affinity set,
    /// FIRST affinity included -- the Hexinton CE table showed the pickup flag IS that unlock, so
    /// the old "siblings only" table shipped Iron without Heavy, Black without Occult, etc.
    #[test]
    fn whetblade_receive_sets_the_full_affinity_set_including_the_first() {
        let expect: &[(&str, &[u32])] = &[
            ("Iron Whetblade", &[65610, 65620, 65630]),
            ("Red-Hot Whetblade", &[65640, 65650]),
            ("Sanctified Whetblade", &[65660, 65670]),
            ("Glintstone Whetblade", &[65680, 65690]),
            ("Black Whetblade", &[65720, 65700, 65710]),
        ];
        for (name, flags) in expect {
            assert_eq!(
                acquire_flags(name),
                flags.to_vec(),
                "{name}: must set the first affinity (pickup flag) AND the event-1450 siblings"
            );
        }
    }

    /// The safety half of the split: setting the pickup flag is only sound because the poll no
    /// longer watches it. Chain the two mechanisms at their real seam: repoint a seed's poll map
    /// exactly as core.rs does, then assert NOTHING any receive sets is still polled.
    #[test]
    fn no_receive_set_flag_survives_the_poll_repoint_as_a_check() {
        let mut poll = std::collections::HashMap::from([
            (7770041i64, 65610u32),
            (7770042, 65640),
            (7770043, 65660),
            (7770044, 65680),
            (7770045, 65720),
        ]);
        let _ = er_logic::whetblade::repoint_poll_flags(&mut poll);
        let polled: std::collections::HashSet<u32> = poll.values().copied().collect();
        for w in &er_logic::whetblade::WHETBLADES {
            for f in w.affinity_flags {
                assert!(
                    !polled.contains(f),
                    "{}: receive-set flag {f} is still a live check flag -- false collect",
                    w.name
                );
            }
        }
    }

    /// Bell/Knife keep their direct obtained flags: vanilla events READ 60110/60130 (no derived
    /// cascade exists, no lot getItemFlagId to repoint), so dropping them would break summoning /
    /// Ashes of War on a pool receive. Their check-flag collision is a known, separate issue.
    #[test]
    fn bell_and_knife_keep_their_vanilla_read_flags() {
        assert_eq!(acquire_flags("Spirit Calling Bell"), vec![60110]);
        assert_eq!(acquire_flags("Whetstone Knife"), vec![60130]);
    }

    /// The motivating case (rule 11), client#335: a pool-received Crafting Kit must set 60120 --
    /// the vanilla grant is `DirectlyGivePlayerItem(Goods, 8500, 60120, 1)` and the Crafting menu
    /// reads the FLAG, so a kit without it leaves crafting disabled out of combat. Same class as
    /// Bell/Knife: EMEVD-set, vanilla-read, and a check flag (loc 7770013) with the same known
    /// residual false-collect.
    #[test]
    fn crafting_kit_receive_sets_60120() {
        assert_eq!(acquire_flags("Crafting Kit"), vec![60120]);
    }

    /// The seam between THESE tables and the poll guard, the same way
    /// `no_receive_set_flag_survives_the_poll_repoint_as_a_check` chains the whetblade split:
    /// build a seed poll table holding the four double-booked checks plus an ordinary one, hand
    /// `all_acquire_flags()` to `er_logic::keyitem_poll`, and pin that it names exactly the four.
    /// If a future table entry adds another collision, this test makes it visible instead of
    /// letting it become a silent false collect.
    #[test]
    fn poll_guard_names_every_double_booked_check_from_these_tables() {
        let poll = std::collections::HashMap::from([
            (7770556i64, 400001u32), // Leyndell :: Rold Medallion (the 2026-09-07 report)
            (7770012, 60110),        // Limgrave :: Spirit Calling Bell
            (7770014, 60130),        // Limgrave :: Whetstone Knife
            (7770013, 60120),        // Limgrave :: Crafting Kit
            (7770099, 510800),       // an ordinary check -- must never be guarded
        ]);
        let collisions = er_logic::keyitem_poll::colliding_checks(
            &poll,
            &all_acquire_flags().collect::<HashSet<u32>>(),
        );
        assert_eq!(
            collisions,
            vec![
                (7770012, 60110),
                (7770013, 60120),
                (7770014, 60130),
                (7770556, 400001),
            ]
        );
    }

    /// Acceptance (client#335): no duplicate check or item grant. 60120 keys TWO shop rows
    /// (Kalé 100501, Twin Maiden Husks 101879), so once the client can set it, those checks must
    /// be echo-dedup EXEMPT -- the exemption set is built from `all_acquire_flags()`, and this
    /// pins that 60120 flows into it (else a native purchase's AP echo can be eaten, the
    /// 2026-07-24 start-grant collision shape).
    #[test]
    fn crafting_kit_flag_is_echo_dedup_exempt_via_all_acquire_flags() {
        assert!(
            all_acquire_flags().any(|f| f == 60120),
            "60120 must reach the shop_sell exemption set built from all_acquire_flags()"
        );
    }
}
