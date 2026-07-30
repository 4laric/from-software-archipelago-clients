//! WHETBLADES: one vanilla flag carries TWO meanings, so the client splits them (2026-07-30).
//!
//! Ground truth (Hexinton CE table, decoded 2026-07-30; addresses `[[EventFlagMan]+28]+off`,
//! `id = off*8 + (7-bit) + 50000`): the smithing menu keys each affinity on ONE flag —
//!
//!   Iron       Heavy **65610**  Keen 65620   Quality 65630
//!   Red-Hot    Fire  **65640**  Flame Art 65650
//!   Sanctified Lightning **65660**  Sacred 65670
//!   Glintstone Magic **65680**  Frost 65690
//!   Black      Poison 65700  Blood 65710  Occult **65720**
//!
//! The BOLD flag per blade is simultaneously (a) the first affinity's unlock read by the menu and
//! (b) the blade's `ItemLotParam_map.getItemFlagId` — i.e. this world's randomized CHECK flag for
//! that location (`flag_lots.tsv`; e.g. 65610 = AP loc 7770041, `Stormveil :: Iron Whetblade`).
//! common.emevd event 1450 (slots 0-4) waits on the bold flag and sets the SIBLING flags, which is
//! why the previous fix (`6d59abd`, keyitems COMPANION_ACQUIRE_FLAGS) modeled the bold flag as
//! "pickup only, cascade derived" and skipped it on a pool receive — leaving every pool-received
//! whetblade missing exactly its FIRST affinity (Iron: no Heavy; Black: no Occult; ...).
//!
//! Setting the bold flag on receive is also wrong twice over: the flag poll reports the check as
//! collected (the Eldakin 2026-07-29 false-collect), and the game despawns the still-uncollected
//! treasure (a map lot whose `getItemFlagId` reads set does not spawn), stranding whatever the
//! multiworld placed there — the phantom-flag softlock class.
//!
//! THE SPLIT: give each meaning its own flag.
//!   * The AFFINITY meaning keeps the vanilla id: a pool receive sets the full affinity set
//!     ([`Whetblade::affinity_flags`] = bold + siblings; consumed by keyitems.rs).
//!   * The CHECK meaning moves to a client-owned [`Whetblade::check_flag`]: [`repoint_poll_flags`]
//!     rewrites the seed's poll map in place and returns the `(map_lot, check_flag)` writes that
//!     `whetblade_lots.rs` applies to `ItemLotParam_map.getItemFlagId`, so a real pickup sets the
//!     NEW flag, the poll detects it, and a client-set 65610 neither fires the check nor despawns
//!     the lot (its presence is now governed by `check_flag`).
//!
//! CHECK-FLAG VALIDITY (an invented id is an inert no-op — er-event-flag-validity): each
//! `check_flag` is `pickup_flag + 1`, an adjacent bit in the SAME allocated EventFlagMan block as a
//! flag the game demonstrably persists, so it is real and save-persisted by construction. Verified
//! unused: zero references across the full decompiled EMEVD corpus
//! (`elden_ring_artifacts/event/*.emevd.dcx.js`, all maps + common + common_func, 2026-07-30) and
//! zero rows in flag_lots / check_maps / region_map / esd_flags. The only vanilla writer anywhere
//! near the block is common event 700, which touches the TENS (65600-65790) only.
//!
//! 65600 ("Upgrade - Standard 1") is NOT part of this group: common event 700 sets it ON in every
//! normal-play branch (L0/L1) at game start, so Standard is never gated on an item.
//!
//! Does NOT generalize to Bell/Knife/Rold/Drawing-Room (60110/60130/400001/400072): those flags are
//! set by ESD/EMEVD scripts and read directly by vanilla events — there is no lot `getItemFlagId`
//! to repoint, so their check/acquire collision needs flagpoll-side suppression instead.

use std::collections::HashMap;

/// One whetblade's two-meaning flag split. Fields are GAME facts (lot row, vanilla flag, event-1450
/// siblings) plus the one client-owned choice (`check_flag`); provenance in the module doc.
pub struct Whetblade {
    /// Received-item name, as the apworld ships it (keyitems.rs matches on this).
    pub name: &'static str,
    /// `ItemLotParam_map` row of the vanilla pickup (flag_lots.tsv).
    pub map_lot: u32,
    /// Vanilla `getItemFlagId` == the FIRST affinity's menu unlock (Hexinton CE table).
    pub pickup_flag: u32,
    /// Client-owned check-detection flag the lot is repointed to (`pickup_flag + 1`; see doc).
    pub check_flag: u32,
    /// Full unlock set a pool receive must apply: the first affinity (`pickup_flag`) plus the
    /// event-1450 siblings. Order: bold flag first, then siblings in id order.
    pub affinity_flags: &'static [u32],
}

/// The five whetblades. `flag_lots.tsv` rows 65610/65640/65660/65680/65720 + common.emevd 1450.
pub const WHETBLADES: [Whetblade; 5] = [
    Whetblade {
        name: "Iron Whetblade",
        map_lot: 10000420,
        pickup_flag: 65610,
        check_flag: 65611,
        affinity_flags: &[65610, 65620, 65630],
    },
    Whetblade {
        name: "Red-Hot Whetblade",
        map_lot: 1051360070,
        pickup_flag: 65640,
        check_flag: 65641,
        affinity_flags: &[65640, 65650],
    },
    Whetblade {
        name: "Sanctified Whetblade",
        map_lot: 11001010,
        pickup_flag: 65660,
        check_flag: 65661,
        affinity_flags: &[65660, 65670],
    },
    Whetblade {
        name: "Glintstone Whetblade",
        map_lot: 14000500,
        pickup_flag: 65680,
        check_flag: 65681,
        affinity_flags: &[65680, 65690],
    },
    Whetblade {
        name: "Black Whetblade",
        map_lot: 12020010,
        pickup_flag: 65720,
        check_flag: 65721,
        affinity_flags: &[65720, 65700, 65710],
    },
];

/// Move every whetblade CHECK off its double-booked vanilla flag, in place.
///
/// For each whetblade whose `pickup_flag` appears as a detection flag in [`poll`] (the seed uses
/// that location as a check — greenfield `locationFlags` or a matt key's token 1 both yield the
/// lot's `getItemFlagId`), replace the value with the client-owned `check_flag` and emit the
/// `(map_lot, check_flag)` rewrite that must be applied to `ItemLotParam_map.getItemFlagId` so the
/// in-game pickup reports on the SAME new flag. A whetblade absent from the seed (num_regions
/// dropped its region) is left alone: no poll entry, no rewrite — and a receive-set `pickup_flag`
/// then despawns a lot that is NOT a check, which is vanilla-correct ("obtained").
///
/// After this runs, no poll value equals any flag a whetblade receive sets, which is the whole
/// safety argument for keyitems setting `affinity_flags` unconditionally.
pub fn repoint_poll_flags(poll: &mut HashMap<i64, u32>) -> Vec<(u32, u32)> {
    let mut rewrites = Vec::new();
    for w in &WHETBLADES {
        let mut hit = false;
        for v in poll.values_mut() {
            if *v == w.pickup_flag {
                *v = w.check_flag;
                hit = true;
            }
        }
        if hit {
            rewrites.push((w.map_lot, w.check_flag));
        }
    }
    rewrites
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn seed_poll() -> HashMap<i64, u32> {
        // The five whetblade locations as data.py ships them, plus an unrelated check.
        HashMap::from([
            (7770041, 65610),
            (7770042, 65640),
            (7770043, 65660),
            (7770044, 65680),
            (7770045, 65720),
            (7770099, 510800), // Leonine Misbegotten — must never be touched
        ])
    }

    /// Table self-consistency: the bold flag leads its own affinity set, the check flag is the
    /// adjacent same-block bit, and no whetblade's check flag collides with any affinity flag.
    #[test]
    fn table_is_consistent() {
        let all_affinity: HashSet<u32> = WHETBLADES
            .iter()
            .flat_map(|w| w.affinity_flags.iter().copied())
            .collect();
        for w in &WHETBLADES {
            assert_eq!(
                w.affinity_flags[0], w.pickup_flag,
                "{}: the pickup flag IS the first affinity unlock (Hexinton CE table)",
                w.name
            );
            assert_eq!(
                w.check_flag,
                w.pickup_flag + 1,
                "{}: check flag must stay the adjacent same-block bit (validity argument)",
                w.name
            );
            assert!(
                !all_affinity.contains(&w.check_flag),
                "{}: check flag {} collides with an affinity flag",
                w.name,
                w.check_flag
            );
        }
    }

    /// After the repoint, the poll map shares NO flag with anything a whetblade receive sets —
    /// the invariant that makes the unconditional affinity write safe.
    #[test]
    fn repointed_poll_is_disjoint_from_every_receive_set_flag() {
        let mut poll = seed_poll();
        let rewrites = repoint_poll_flags(&mut poll);
        assert_eq!(rewrites.len(), 5, "all five whetblade checks repointed");
        let polled: HashSet<u32> = poll.values().copied().collect();
        for w in &WHETBLADES {
            for f in w.affinity_flags {
                assert!(
                    !polled.contains(f),
                    "{}: receive-set flag {f} is still polled as a check",
                    w.name
                );
            }
        }
        assert_eq!(poll[&7770099], 510800, "unrelated checks untouched");
        assert_eq!(poll[&7770041], 65611, "Iron check now detected on 65611");
        assert!(
            rewrites.contains(&(10000420, 65611)),
            "Iron lot rewrite emitted"
        );
    }

    /// A seed without a whetblade location (num_regions dropped it) gets no rewrite for it: the
    /// lot is not a check there, so the receive-set pickup flag despawning it is vanilla-correct.
    #[test]
    fn absent_location_means_no_rewrite() {
        let mut poll = HashMap::from([(7770042, 65640u32), (7770099, 510800u32)]);
        let rewrites = repoint_poll_flags(&mut poll);
        assert_eq!(rewrites, vec![(1051360070, 65641)]);
        assert_eq!(poll[&7770042], 65641);
    }

    /// THE MOTIVATING CASE (rule 11 — Eldakin false-collect + the missing first affinity), as a
    /// replay over a minimal game: flag store + poll map + the lot's live `getItemFlagId`.
    ///
    /// A pool-received Iron Whetblade must unlock Heavy (65610), Keen (65620) AND Quality (65630),
    /// must NOT send check 7770041, and must NOT despawn the treasure; the real pickup afterwards
    /// must send exactly that check once.
    #[test]
    fn iron_whetblade_receive_unlocks_all_three_and_sends_no_check_replay() {
        let mut flags: HashSet<u32> = HashSet::new();
        let mut poll = seed_poll();

        // Connect: repoint the poll + apply the lot writes (whetblade_lots.rs in production).
        let rewrites = repoint_poll_flags(&mut poll);
        let mut lot_flag: HashMap<u32, u32> = HashMap::from([(10000420, 65610)]);
        for (lot, f) in &rewrites {
            if let Some(slot) = lot_flag.get_mut(lot) {
                *slot = *f;
            }
        }

        // Receive the pool Iron Whetblade: keyitems sets the full affinity set.
        let iron = WHETBLADES
            .iter()
            .find(|w| w.name == "Iron Whetblade")
            .unwrap();
        flags.extend(iron.affinity_flags.iter().copied());
        // ... and common event 1450 (waits on 65610, sets siblings) firing changes nothing:
        flags.extend([65620, 65630]);

        assert!(flags.contains(&65610), "Heavy unlocked");
        assert!(flags.contains(&65620), "Keen unlocked");
        assert!(flags.contains(&65630), "Quality unlocked");
        let fired: Vec<i64> = poll
            .iter()
            .filter(|&(_, f)| flags.contains(f))
            .map(|(&l, _)| l)
            .collect();
        assert!(
            fired.is_empty(),
            "receive must send NO check, got {fired:?}"
        );
        // The treasure still spawns: its presence is governed by the LIVE getItemFlagId.
        assert!(
            !flags.contains(&lot_flag[&10000420]),
            "lot despawned by the receive — the check is stranded"
        );

        // The real pickup: the game sets the lot's live flag.
        flags.insert(lot_flag[&10000420]);
        let fired: Vec<i64> = poll
            .iter()
            .filter(|&(_, f)| flags.contains(f))
            .map(|(&l, _)| l)
            .collect();
        assert_eq!(
            fired,
            vec![7770041],
            "the pickup sends exactly the Iron check"
        );
    }
}
