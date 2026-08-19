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
//! The AP catalog maps each great rune to the boss-drop goods row (8148-8153), but slot-data parsing
//! normalizes those FullIDs to the restored/equippable rows (191-196) before any delivery consumer
//! sees them. This module supplies the matching restore flag and disarms the Divine-Tower award
//! event; it does not issue a second goods grant.

use crate::flags;
use std::collections::HashSet;
use std::sync::{
    Mutex,
    atomic::{AtomicU32, Ordering},
};

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
/// Bell/Knife are DIFFERENT: 60110/60130 are set by ESD/EMEVD scripts and read directly by vanilla
/// events -- there is no lot getItemFlagId to repoint -- so they must stay even though they are
/// also check flags (locs 7770012/7770014); that residual false-collect is a known, separate issue
/// (needs flagpoll-side suppression, not a lot rewrite).
const COMPANION_ACQUIRE_FLAGS: &[(&str, &[u32])] = &[
    ("Spirit Calling Bell", &[60110]),
    ("Whetstone Knife", &[60130]),
];

/// Vanilla key items whose progression gate reads an obtained event flag, not inventory -- plus the
/// six great runes, whose "restored" event flag (191-196) makes the received (already-restored-goods)
/// rune fully usable.
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

fn received_great_rune_count(received: &HashSet<String>) -> usize {
    GREAT_RUNE_NAMES
        .iter()
        .filter(|name| received.contains(**name))
        .count()
}

/// Non-location prerequisites owed to the physical Leyndell seal by the cumulative AP receive
/// stream. This is shared by the active reconciler and the runtime-fallback handler so normal
/// delivery, server `/send`, reconnect replay, and `RECONCILE_APPLY=none` cannot diverge.
pub fn leyndell_gate_flags(received: &HashSet<String>) -> Vec<u32> {
    if received_great_rune_count(received) >= 2 {
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
pub fn tick_leyndell_gate_flags(received: &HashSet<String>) {
    let rune_count = received_great_rune_count(received);
    let mut applied = Vec::new();
    for (index, flag) in leyndell_gate_flags(received).into_iter().enumerate() {
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
    entries().flat_map(|(_, fs)| fs.iter().copied())
}

/// Fast-path one-shot: set the vanilla obtained/restored flag(s) for a received item name, if any.
/// Idempotent, but BEST-EFFORT -- writes at menu/load are silently discarded (R3, SWEEP), so this
/// no longer logs success; `tick_keyitem_flags` (the reconcile tick) re-applies and owns the log.
pub fn set_acquire_flags(name: &str) {
    for (n, fs) in entries() {
        if n == name {
            for &f in fs {
                flags::set_event_flag(f, true);
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

    #[test]
    fn leyndell_fix_never_adds_location_flags_to_receive_mapping() {
        for &name in GREAT_RUNE_NAMES {
            let flags = acquire_flags(name);
            assert!(
                flags.iter().all(|f| !(171..=177).contains(f)),
                "{name}: possession/location flag leaked into receive mapping"
            );
        }
    }

    #[test]
    fn leyndell_gate_reconciles_both_non_location_prerequisites() {
        let one = HashSet::from(["Godrick's Great Rune".to_string()]);
        let two = HashSet::from([
            "Godrick's Great Rune".to_string(),
            "Great Rune of the Unborn".to_string(),
        ]);
        assert!(leyndell_gate_flags(&one).is_empty());
        assert_eq!(leyndell_gate_flags(&two), vec![105, 182]);
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
}
