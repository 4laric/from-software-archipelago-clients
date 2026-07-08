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
const COMPANION_ACQUIRE_FLAGS: &[(&str, &[u32])] = &[
    ("Spirit Calling Bell", &[60110]),
    ("Whetstone Knife", &[60130]),
    ("Iron Whetblade", &[65610]),
    ("Red-Hot Whetblade", &[65640]),
    ("Sanctified Whetblade", &[65660]),
    ("Glintstone Whetblade", &[65680]),
    ("Black Whetblade", &[65720]),
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
            log::info!("key item '{n}': obtained/restored flag(s) {fs:?} applied ({applied} newly set)");
        }
    }
}
