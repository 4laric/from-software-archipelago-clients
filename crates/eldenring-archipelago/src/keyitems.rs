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
