//! Obtained-flag + great-rune-restore tables, ported from the standalone `features.rs`.
//!
//! Some vanilla items gate a FEATURE on an "obtained" event flag that a raw goods-grant never trips
//! (summon tutorial, whetblade affinities, the Rold lift, the Volcano drawing-room transition). When
//! such an item is RECEIVED, we set that flag so the feature actually opens. Great runes set their
//! "restored" event flag (191-196; the SetEventFlagID in Divine-Tower common event 90005110) so the
//! received rune is usable immediately (Divine Altar activation) WITHOUT the Divine Tower trip --
//! which under num_regions may sit in a sealed region. All idempotent: flags are save-persisted.
//!
//! NOTE: the AP catalog already maps each great rune to its RESTORED goods row (FullID
//! 0x40000000 | 191..196), so the base item grant already gives the usable rune. We therefore ONLY
//! set the flag here -- we must NOT additively grant the goods a second time (that double-granted the
//! rune -> in-game "maximum allowed in inventory").

use crate::flags;

/// Companion items whose possession is gated by a vanilla "obtained" event flag.
///
/// WHETBLADES SET THE **DERIVED** AFFINITY FLAGS, NEVER THE PICKUP FLAG (2026-07-30). A
/// whetblade's pickup flag (65610/65640/65660/65680/65720) is an item-LOT flag -- which is this
/// world's randomized CHECK flag for that location (e.g. 65610 = `Stormveil :: Iron Whetblade -
/// near Rampart Tower`, AP loc 7770041). Setting it on a pool RECEIVE falsely reported the check
/// as collected (seen live: Eldakin 2026-07-29, a received Glintstone Whetblade auto-completed
/// f65680 within the same second). Vanilla itself never keys the affinity menu on the pickup flag:
/// common.emevd event 1450 (slots 0-4) WAITS on each pickup flag and then sets these derived
/// flags, which are what the grace "Ashes of War" menu consumes -- and the derived flags are not
/// lot/check/region flags anywhere in the world's corpus. So we set what event 1450 would have
/// set, and the pickup flag stays untouched for the real location check. Mapping (common.emevd):
///   65610 -> 65620, 65630   (Iron)
///   65640 -> 65650          (Red-Hot)
///   65660 -> 65670          (Sanctified)
///   65680 -> 65690          (Glintstone)
///   65720 -> 65700, 65710   (Black)
/// Bell/Knife are DIFFERENT: 60110/60130 are read directly by vanilla events (no derived cascade
/// exists), so they must stay even though they are also check flags (locs 7770012/7770014) --
/// that residual false-collect is a known, separate issue.
const COMPANION_ACQUIRE_FLAGS: &[(&str, &[u32])] = &[
    ("Spirit Calling Bell", &[60110]),
    ("Whetstone Knife", &[60130]),
    ("Iron Whetblade", &[65620, 65630]),
    ("Red-Hot Whetblade", &[65650]),
    ("Sanctified Whetblade", &[65670]),
    ("Glintstone Whetblade", &[65690]),
    ("Black Whetblade", &[65700, 65710]),
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

/// The vanilla obtained / great-rune restored flag(s) mapped to a received item `name` (empty if
/// none). READ-ONLY companion to [`set_acquire_flags`]: the reconciler's dry-run mapper
/// (`reconcile_io::build_desired_inputs`) uses it to classify a received item as an
/// `ItemSemantics::KeyItem { goods, obtained_flags }` from the SAME table the live path applies,
/// so the two never drift.
pub fn acquire_flags(name: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for (n, fs) in COMPANION_ACQUIRE_FLAGS.iter().chain(KEY_ITEM_ACQUIRE_FLAGS) {
        if *n == name {
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
    COMPANION_ACQUIRE_FLAGS
        .iter()
        .chain(KEY_ITEM_ACQUIRE_FLAGS)
        .flat_map(|(_, fs)| fs.iter().copied())
}

/// Fast-path one-shot: set the vanilla obtained/restored flag(s) for a received item name, if any.
/// Idempotent, but BEST-EFFORT -- writes at menu/load are silently discarded (R3, SWEEP), so this
/// no longer logs success; `tick_keyitem_flags` (the reconcile tick) re-applies and owns the log.
pub fn set_acquire_flags(name: &str) {
    for (n, fs) in COMPANION_ACQUIRE_FLAGS.iter().chain(KEY_ITEM_ACQUIRE_FLAGS) {
        if *n == name {
            for &f in *fs {
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
    for (n, fs) in COMPANION_ACQUIRE_FLAGS.iter().chain(KEY_ITEM_ACQUIRE_FLAGS) {
        if !received.contains(*n) {
            continue;
        }
        let mut applied = 0u32;
        for &f in *fs {
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

    /// The five whetblade PICKUP flags. Each is the world's randomized CHECK flag for the
    /// corresponding vanilla location, so the client must never set one as an acquire flag: doing
    /// so reports a location the player never touched (the 2026-07-29/30 false-collect).
    const WHETBLADE_PICKUP_FLAGS: [u32; 5] = [65610, 65640, 65660, 65680, 65720];

    #[test]
    fn no_acquire_flag_is_a_whetblade_pickup_flag() {
        for f in all_acquire_flags() {
            assert!(
                !WHETBLADE_PICKUP_FLAGS.contains(&f),
                "acquire flag {f} is a randomized check flag; setting it on receive \
                 falsely collects that location -- set the event-1450 derived flags instead"
            );
        }
    }

    /// The motivating case, at the seam the live path uses: receiving a whetblade must yield the
    /// DERIVED affinity flags common.emevd event 1450 would have set from the pickup flag.
    #[test]
    fn whetblade_receive_maps_to_the_event_1450_derived_flags() {
        let expect: &[(&str, &[u32])] = &[
            ("Iron Whetblade", &[65620, 65630]),
            ("Red-Hot Whetblade", &[65650]),
            ("Sanctified Whetblade", &[65670]),
            ("Glintstone Whetblade", &[65690]),
            ("Black Whetblade", &[65700, 65710]),
        ];
        for (name, flags) in expect {
            assert_eq!(
                acquire_flags(name),
                flags.to_vec(),
                "{name}: acquire flags must be exactly the event-1450 derived set"
            );
        }
    }

    /// Bell/Knife keep their direct obtained flags: vanilla events READ 60110/60130 (no derived
    /// cascade exists), so dropping them would break summoning / Ashes of War on a pool receive.
    #[test]
    fn bell_and_knife_keep_their_vanilla_read_flags() {
        assert_eq!(acquire_flags("Spirit Calling Bell"), vec![60110]);
        assert_eq!(acquire_flags("Whetstone Knife"), vec![60130]);
    }
}
