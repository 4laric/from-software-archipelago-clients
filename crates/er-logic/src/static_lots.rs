//! STATIC VANILLA-SUPPRESSION: blank a check's vanilla ware for ANY apworld.
//!
//! # The problem (measured in-game, first Bedrock playtest, 2026-07-13)
//!
//! ```text
//! vanilla suppressor INERT: checkItemFlags empty/absent in slot_data
//! ```
//!
//! The client blanks a check's vanilla ware AT THE SOURCE -- it rewrites the check's own
//! `ItemLotParam` row so the game never hands the item over. But it learns WHICH rows to blank from
//! `checkLotBlankMap` / `checkLotBlankEnemy` / `checkItemFlags` in slot_data, and ONLY OUR apworld
//! emits those. Drive a foreign apworld and the tables are empty: every check pays out the VANILLA
//! item AND the AP item.
//!
//! # The insight
//!
//! The blank-list is derived from `ItemLotParam`: flag -> lot -> which slots hold a goods ware, and
//! which item ids are weapon/armor wares. **That is GAME data. It is not seed data.** It is identical
//! for every seed and every apworld.
//!
//! So it ships STATIC (`check_lots_table.json`, from `tools/gen_check_lots_table.py`). The client
//! already knows the seed's check FLAGS -- from `locationFlags`, or derived from Bedrock's matt slot
//! keys by `key_resolver`. Intersect those flags with this table and you have the blank-list, for
//! ANY apworld, with zero changes on its side. Same argument, same shape, as `shoplineup_flags.json`.
//!
//! Measured against a real Bedrock seed: **3018 of 3022 check flags suppressed (99.9%)**.
//!
//! # Two mechanisms, because the game gives us two problems
//!
//! * **GOODS** wares are blanked AT THE LOT (`map`/`enemy` -> `check_lots::configure`). Suppressing
//!   goods BY ID would be a disaster: Golden Rune [1] backs 46 checks, so every Golden Rune you ever
//!   picked up anywhere would be eaten.
//! * **WEAPON/ARMOR** wares are suppressed BY ITEM ID (`items` -> `detour::configure_check_item_flags`).
//!   Sound for them and only for them: a weapon is essentially never farmable, so it lives in the
//!   check-only set and cannot eat a legitimate source.
//!
//! Slot_data always WINS when present -- this is a FALLBACK. Our own seeds are untouched.

use std::collections::HashMap;

use serde_json::Value;

/// The shipped table: every flagged `ItemLotParam` row that carries a suppressible ware.
#[derive(Default, Debug)]
pub struct StaticLots {
    /// The one goods row the detour suppresses unconditionally (exists, unnamed, referenced nowhere).
    pub placeholder_goods: i32,
    /// acquisition flag -> (ItemLotParam_map lot id, goods slot indices 1..8)
    pub map: HashMap<u32, (u32, Vec<u8>)>,
    /// acquisition flag -> (ItemLotParam_enemy lot id, goods slot indices 1..8)
    pub enemy: HashMap<u32, (u32, Vec<u8>)>,
    /// acquisition flag -> the WEAPON/ARMOR item ids that check hands out (id-keyed suppression)
    pub items: HashMap<u32, Vec<u32>>,
}

impl StaticLots {
    pub fn is_empty(&self) -> bool {
        self.map.is_empty() && self.enemy.is_empty() && self.items.is_empty()
    }
}

fn parse_side(v: Option<&Value>) -> HashMap<u32, (u32, Vec<u8>)> {
    let mut out = HashMap::new();
    let Some(obj) = v.and_then(|v| v.as_object()) else {
        return out;
    };
    for (k, row) in obj {
        let Ok(flag) = k.parse::<u32>() else { continue };
        let Some(lot) = row.get("lot").and_then(|x| x.as_u64()) else {
            continue;
        };
        let slots: Vec<u8> = row
            .get("slots")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_u64())
                    .filter(|&s| (1..=8).contains(&s))
                    .map(|s| s as u8)
                    .collect()
            })
            .unwrap_or_default();
        if !slots.is_empty() && lot > 0 {
            out.insert(flag, (lot as u32, slots));
        }
    }
    out
}

/// Parse `check_lots_table.json`. Tolerant: malformed/missing -> empty (suppression simply stays
/// off, exactly as it is today, rather than panicking mid-connect).
pub fn parse(text: &str) -> StaticLots {
    // Tolerate NUL padding a shrinking overwrite may leave (mirrors flagpoll's table loaders).
    let text = text.trim_end_matches('\u{0}').trim();
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return StaticLots::default();
    };
    let items = v
        .get("items")
        .and_then(|x| x.as_object())
        .map(|o| {
            o.iter()
                .filter_map(|(k, ids)| {
                    let flag = k.parse::<u32>().ok()?;
                    let ids: Vec<u32> = ids
                        .as_array()?
                        .iter()
                        .filter_map(|i| i.as_u64())
                        .filter(|&i| i > 0)
                        .map(|i| i as u32)
                        .collect();
                    (!ids.is_empty()).then_some((flag, ids))
                })
                .collect()
        })
        .unwrap_or_default();
    StaticLots {
        placeholder_goods: v
            .get("placeholder_goods")
            .and_then(|x| x.as_i64())
            .unwrap_or(0) as i32,
        map: parse_side(v.get("map")),
        enemy: parse_side(v.get("enemy")),
        items,
    }
}

/// Build the `{lot: slots}` blank tables for THIS seed: the static table, scoped to the flags the
/// seed actually checks. A flag the seed does not use is left alone -- we never blank a lot that is
/// not a check in this seed, or we would eat a legitimate vanilla pickup.
pub fn blank_tables_for(
    lots: &StaticLots,
    seed_flags: &[u32],
) -> (HashMap<u32, Vec<u8>>, HashMap<u32, Vec<u8>>) {
    let (mut m, mut e) = (HashMap::new(), HashMap::new());
    for f in seed_flags {
        if let Some((lot, slots)) = lots.map.get(f) {
            m.insert(*lot, slots.clone());
        }
        // NB a flag can appear in BOTH tables (5 do). Blank both -- the client only writes a row
        // that actually exists, so this cannot corrupt the table it is not in.
        if let Some((lot, slots)) = lots.enemy.get(f) {
            e.insert(*lot, slots.clone());
        }
    }
    (m, e)
}

/// Build `checkItemFlags` ({item id -> the check flags that hand it out}) for THIS seed. This is the
/// INVERSE of the shipped `items` map, which is keyed by flag.
pub fn check_item_flags_for(lots: &StaticLots, seed_flags: &[u32]) -> HashMap<u32, Vec<u32>> {
    let mut out: HashMap<u32, Vec<u32>> = HashMap::new();
    for f in seed_flags {
        if let Some(ids) = lots.items.get(f) {
            for id in ids {
                let e = out.entry(*id).or_default();
                if !e.contains(f) {
                    e.push(*f);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = r#"{
      "placeholder_goods": 8852,
      "map":   {"520110": {"lot": 20110, "slots": [1]},
                "30127000": {"lot": 30120000, "slots": [1,2]}},
      "enemy": {"520110": {"lot": 999, "slots": [3]}},
      "items": {"510030": [11110000], "520100": [23020000, 11110000]}
    }"#;

    #[test]
    fn parses_all_three_halves() {
        let l = parse(T);
        assert_eq!(l.placeholder_goods, 8852);
        assert_eq!(l.map.get(&520110), Some(&(20110u32, vec![1u8])));
        assert_eq!(l.map.get(&30127000), Some(&(30120000u32, vec![1u8, 2])));
        assert_eq!(l.enemy.get(&520110), Some(&(999u32, vec![3u8])));
        assert_eq!(l.items.get(&510030), Some(&vec![11110000u32]));
        assert!(!l.is_empty());
    }

    #[test]
    fn only_the_seeds_own_flags_are_blanked() {
        // THE SAFETY PROPERTY. Blanking a lot the seed does not check would eat a legitimate
        // vanilla pickup -- the exact bug the id-keyed suppressor used to have (Golden Rune [1]
        // backs 46 checks; every one you found anywhere was eaten).
        let l = parse(T);
        let (m, e) = blank_tables_for(&l, &[520110]);
        assert_eq!(m.get(&20110), Some(&vec![1u8]));
        assert!(!m.contains_key(&30120000), "a flag this seed does NOT check must not be blanked");
        assert_eq!(e.get(&999), Some(&vec![3u8]), "a flag in BOTH tables is blanked in both");
    }

    #[test]
    fn check_item_flags_is_the_inverse_and_merges_shared_ids() {
        let l = parse(T);
        let cif = check_item_flags_for(&l, &[510030, 520100]);
        // 11110000 is handed out by BOTH checks -> both flags, or picking it up at one check
        // would not clear the other.
        let mut got = cif.get(&11110000).cloned().unwrap();
        got.sort_unstable();
        assert_eq!(got, vec![510030u32, 520100]);
        assert_eq!(cif.get(&23020000), Some(&vec![520100u32]));
    }

    #[test]
    fn garbage_and_absent_degrade_to_empty_not_panic() {
        assert!(parse("").is_empty());
        assert!(parse("{ not json").is_empty());
        assert!(parse("{}").is_empty());
        // NUL-padded overwrite (the mount/me3 failure mode) must still parse.
        let padded = format!("{}\u{0}\u{0}\u{0}", T);
        assert!(!parse(&padded).is_empty());
    }

    #[test]
    fn malformed_rows_are_skipped_not_trusted() {
        let l = parse(r#"{"map":{"1":{"lot":0,"slots":[1]},"2":{"lot":5,"slots":[]},
                                 "3":{"lot":5,"slots":[9,1]},"x":{"lot":5,"slots":[1]}}}"#);
        assert!(!l.map.contains_key(&1), "lot 0 is not a lot");
        assert!(!l.map.contains_key(&2), "no slots => nothing to blank");
        assert_eq!(l.map.get(&3), Some(&(5u32, vec![1u8])), "slot 9 is out of range, dropped");
        assert_eq!(l.map.len(), 1);
    }
}
