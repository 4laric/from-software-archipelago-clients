//! `great_rune_possession` — when it is SAFE to set vanilla's Great Rune possession flags 171-177.
//!
//! # Why the flags must be set at all
//!
//! Vanilla counts Great Runes with `CountEventFlags(EventFlag, 170, 179) >= N`
//! (`common.emevd` `$Event(730)`, ~line 1110), and thefifthmatt's randomizer rune gates ("4 runes
//! for Leyndell, 3 to finish", message 20003) reuse that exact pattern with their own thresholds.
//! The band is populated by `$Event(6905)` (~line 3123), which mirrors the shardbearer defeat flags
//! into it:
//!
//! | possession | set by 6905 from | rune |
//! |---|---|---|
//! | 171 | 510010 | Godrick's |
//! | 172 | 510300 | Radahn's |
//! | 173 | 510040 | Morgott's |
//! | 174 | 510220 | Rykard's |
//! | 175 | 510120 | Mohg's |
//! | 176 | 510200 | Malenia's |
//! | 177 | 197 (Rennala) | Great Rune of the Unborn |
//!
//! An AP delivery runs neither 6905 nor a rune lot, so for a player holding six AP-delivered runes
//! the whole band reads ZERO and every gate stays shut (reported live 2026-09-13, player Zelda).
//! The restore flags 191-196 are a different thing (Divine-Tower altar state) and are not counted.
//!
//! # Why setting them unconditionally LOSES A CHECK
//!
//! 171-176 are also the `getItemFlagId` of the six boss-drop rune lots (`greenfield/flag_lots.tsv`:
//! 171 -> lot 10010 goods 8148, 172 -> 10301/8149, 173 -> 10041/8150, 174 -> 10221/8151,
//! 175 -> 10121/8152, 176 -> 10201/8153), and on current seeds they are ALSO the detection flags of
//! the six boss-rune locations (`greenfield/eldenring/tables/data.py`:
//! `Stormveil :: Godrick's Great Rune - Godrick [f171]` = 7770001 ... `Haligtree :: Malenia's Great
//! Rune - Malenia [f176]` = 7770006).
//!
//! That triple duty is the trap. If the client sets 172 when Radahn's Great Rune ARRIVES from the
//! pool, and the player has NOT yet killed Starscourge Radahn, then the boss lot is already marked
//! collected: the kill awards nothing, the flag never transitions unset -> set, and location
//! 7770002 can never be sent. The per-tick re-assert makes that permanent. The `keyitem_poll` guard
//! (#659) does not rescue this — it only stops the phantom-check direction (a client write being
//! read back as a pickup); it cannot resurrect a kill whose ONLY witness flag was pre-set.
//!
//! # The seed-aware rule
//!
//! Classify each rune at connect from the seed's own location flag table:
//!
//! * **No location in this seed carries the possession flag** -> UNCONDITIONAL. Nothing can be
//!   lost, so set on receipt and re-assert per tick. Always true of 177 (the Great Rune of the
//!   Unborn has no location; 7770007 is Rennala's Remembrance on flag 197, not 177), and true of
//!   all seven once the world-side repoint moves the six boss-rune locations onto the defeat flags
//!   510010/510300/510040/510220/510120/510200.
//! * **Some location carries it ("band-detected")** -> CONSERVATIVE. Set the flag only once the
//!   check is already banked — the location reads checked on the server, or the game's own lot flag
//!   already reads set because the player collected it in game. Never pre-set.
//!
//! Conservative mode deliberately UNDERCOUNTS: a rune received before its boss dies does not raise
//! the vanilla count until that boss is killed (or the location is otherwise checked). That is the
//! designed trade. A temporarily shut rune gate is recoverable — kill the boss, or the world-side
//! repoint lands on the next seed — whereas a location that can never be sent is a dead seed.

use std::collections::{BTreeMap, HashMap, HashSet};

/// Item name -> vanilla possession flag, the whole 171-177 band.
pub const POSSESSION_FLAGS: &[(&str, u32)] = &[
    ("Godrick's Great Rune", 171),
    ("Radahn's Great Rune", 172),
    ("Morgott's Great Rune", 173),
    ("Rykard's Great Rune", 174),
    ("Mohg's Great Rune", 175),
    ("Malenia's Great Rune", 176),
    ("Great Rune of the Unborn", 177),
];

/// The possession flag for a received item name, or `None` when the name is not a Great Rune.
pub fn possession_flag(name: &str) -> Option<u32> {
    POSSESSION_FLAGS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, f)| *f)
}

/// Every flag the band can contain, whatever the seed — the set that must reach the shop-echo
/// exemption and the `keyitem_poll` collision computation.
pub fn all_possession_flags() -> impl Iterator<Item = u32> {
    POSSESSION_FLAGS.iter().map(|(_, f)| *f)
}

/// Per-rune classification, computed once at connect from the seed's (already whetblade-repointed)
/// location flag table: possession flag -> the location id(s) in THIS seed detected on it.
///
/// A flag absent from the result is unconditional; a flag present is band-detected and takes the
/// conservative path. Locations are sorted so the log line is stable.
pub fn band_detected(location_flags: &HashMap<i64, u32>) -> BTreeMap<u32, Vec<i64>> {
    let band: HashSet<u32> = all_possession_flags().collect();
    let mut out: BTreeMap<u32, Vec<i64>> = BTreeMap::new();
    for (&loc, &flag) in location_flags {
        if band.contains(&flag) {
            out.entry(flag).or_default().push(loc);
        }
    }
    for locs in out.values_mut() {
        locs.sort_unstable();
    }
    out
}

/// The one line logged at connect, naming how many of the seven flags still carry a location.
pub fn classification_line(detected: &BTreeMap<u32, Vec<i64>>) -> String {
    let total = POSSESSION_FLAGS.len();
    if detected.is_empty() {
        return format!(
            "great-rune possession: 0/{total} flag(s) still carry a location in this seed -> \
             unconditional"
        );
    }
    let list = detected
        .iter()
        .map(|(flag, locs)| {
            let locs = locs
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join("/");
            format!("{flag} ({locs})")
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "great-rune possession: {}/{total} flag(s) still carry a location in this seed -> \
         conservative mode for those: {list}",
        detected.len()
    )
}

/// How a single possession flag is handled in this seed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// No location detects this flag: set on receipt and re-assert freely.
    Unconditional,
    /// These locations detect this flag: the flag may only be set once the check is banked.
    Conservative(Vec<i64>),
}

/// Classify one flag against the connect-time table.
pub fn mode(flag: u32, detected: &BTreeMap<u32, Vec<i64>>) -> Mode {
    match detected.get(&flag) {
        None => Mode::Unconditional,
        Some(locs) => Mode::Conservative(locs.clone()),
    }
}

/// May the client set `flag` right now for a rune it has RECEIVED?
///
/// `flag_already_set` is the live game read: a set flag means the player collected the rune in
/// game, so there is nothing left to lose and nothing left to write either — callers latch on the
/// flag anyway, and this returning `true` keeps the two paths from disagreeing.
///
/// The conservative arm requires EVERY location detected on the flag to be checked already. In
/// practice there is exactly one, but demanding all of them keeps the rule safe if a seed ever
/// shares a detection flag between two locations.
pub fn may_set(
    flag: u32,
    detected: &BTreeMap<u32, Vec<i64>>,
    flag_already_set: bool,
    is_checked: impl Fn(i64) -> bool,
) -> bool {
    if flag_already_set {
        return true;
    }
    match detected.get(&flag) {
        None => true,
        Some(locs) => locs.iter().copied().all(is_checked),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This seed's six boss-rune locations, keyed on the possession flags (current world data).
    fn current_seed() -> HashMap<i64, u32> {
        HashMap::from([
            (7_770_001, 171),
            (7_770_002, 172),
            (7_770_003, 173),
            (7_770_004, 174),
            (7_770_005, 175),
            (7_770_006, 176),
            // Rennala's Remembrance — flag 197, NOT 177. The Unborn rune has no location.
            (7_770_007, 197),
            // an ordinary check, the control
            (7_770_099, 510_800),
        ])
    }

    /// A post-repoint seed: the six locations detect the shardbearer DEFEAT flags instead.
    fn repointed_seed() -> HashMap<i64, u32> {
        HashMap::from([
            (7_770_001, 510_010),
            (7_770_002, 510_300),
            (7_770_003, 510_040),
            (7_770_004, 510_220),
            (7_770_005, 510_120),
            (7_770_006, 510_200),
            (7_770_007, 197),
            (7_770_099, 510_800),
        ])
    }

    #[test]
    fn every_rune_name_maps_to_its_band_flag() {
        assert_eq!(possession_flag("Godrick's Great Rune"), Some(171));
        assert_eq!(possession_flag("Malenia's Great Rune"), Some(176));
        assert_eq!(possession_flag("Great Rune of the Unborn"), Some(177));
        assert_eq!(possession_flag("Rune Arc"), None);
        assert_eq!(possession_flag(""), None);
        let all: Vec<u32> = all_possession_flags().collect();
        assert_eq!(all, vec![171, 172, 173, 174, 175, 176, 177]);
    }

    /// (e) the connect-time classification, from a seed location table.
    #[test]
    fn current_seeds_classify_the_six_boss_runes_and_never_177() {
        let detected = band_detected(&current_seed());
        assert_eq!(
            detected.keys().copied().collect::<Vec<_>>(),
            vec![171, 172, 173, 174, 175, 176]
        );
        assert_eq!(detected[&172], vec![7_770_002]);
        assert!(
            !detected.contains_key(&177),
            "177 carries no location: 7770007 is Rennala's Remembrance on flag 197"
        );
        let line = classification_line(&detected);
        assert!(line.contains("6/7"), "{line}");
        assert!(line.contains("conservative"), "{line}");
        assert!(line.contains("172 (7770002)"), "{line}");
    }

    #[test]
    fn a_repointed_seed_classifies_every_rune_unconditional() {
        let detected = band_detected(&repointed_seed());
        assert!(detected.is_empty());
        let line = classification_line(&detected);
        assert!(line.contains("0/7"), "{line}");
        assert!(line.contains("unconditional"), "{line}");
        for flag in all_possession_flags() {
            assert_eq!(mode(flag, &detected), Mode::Unconditional);
        }
    }

    #[test]
    fn mode_names_the_colliding_locations() {
        let detected = band_detected(&current_seed());
        assert_eq!(mode(172, &detected), Mode::Conservative(vec![7_770_002]));
        assert_eq!(mode(177, &detected), Mode::Unconditional);
    }

    /// (a) unconditional path: on a repointed seed a receipt may set immediately.
    #[test]
    fn unconditional_runes_may_be_set_on_receipt() {
        let detected = band_detected(&repointed_seed());
        for flag in all_possession_flags() {
            assert!(
                may_set(flag, &detected, false, |_| false),
                "flag {flag} carries no location in a repointed seed and must set on receipt"
            );
        }
    }

    /// (b) THE REGRESSION: Radahn's rune arrives before Radahn dies. Setting 172 here would mark
    /// lot 10301 collected, the kill would award nothing, and 7770002 could never be sent.
    #[test]
    fn a_band_detected_rune_is_not_set_before_its_check() {
        let detected = band_detected(&current_seed());
        assert!(
            !may_set(172, &detected, false, |_| false),
            "pre-setting 172 loses location 7770002 forever"
        );
        // ... and a sibling's check landing does not unlock it either: the guard is per-flag.
        assert!(!may_set(172, &detected, false, |loc| loc == 7_770_001));
    }

    /// (c) once the boss-rune location IS checked, the flag may be set, so a later tick raises the
    /// vanilla count and the rune gates open.
    #[test]
    fn a_band_detected_rune_is_set_once_its_location_is_checked() {
        let detected = band_detected(&current_seed());
        assert!(may_set(172, &detected, false, |loc| loc == 7_770_002));
        // The other half: the player looted it in game, so the game's own lot flag already reads
        // set. Nothing to lose, nothing to write.
        assert!(may_set(172, &detected, true, |_| false));
    }

    /// (d) 177 is unconditional even on a current seed — no location has ever carried it.
    #[test]
    fn the_unborn_rune_is_always_unconditional() {
        let detected = band_detected(&current_seed());
        assert_eq!(mode(177, &detected), Mode::Unconditional);
        assert!(may_set(177, &detected, false, |_| false));
    }

    /// A seed sharing one detection flag between two locations needs BOTH banked.
    #[test]
    fn a_shared_detection_flag_needs_every_location_checked() {
        let detected = band_detected(&HashMap::from([(7_770_001, 171), (7_770_500, 171)]));
        assert_eq!(
            mode(171, &detected),
            Mode::Conservative(vec![7_770_001, 7_770_500])
        );
        assert!(!may_set(171, &detected, false, |loc| loc == 7_770_001));
        assert!(may_set(171, &detected, false, |_| true));
    }
}
