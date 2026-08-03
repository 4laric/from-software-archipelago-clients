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
    /// CO-CHECK OVERLAY (`map_v2` / `enemy_v2`): a shared getItemFlagId can drive SEVERAL lots, each
    /// its own co-firing check (SPEC-flag-lot-item-model). `map` keys the PRIMARY (lowest) lot only;
    /// this lists EVERY lot on such a flag so all of them are blanked -- otherwise the sibling
    /// co-check hands out its vanilla ware alongside its AP item. Additive: a flag absent here falls
    /// back to `map`/`enemy` (single lot), so an old table / an apworld that emits no `_v2` degrades to
    /// exactly today's lowest-lot blanking. Contract (gen_check_lots_table.py): >=2 entries per flag,
    /// lot-sorted, and `map[flag] == map_v2[flag][0]`.
    pub map_v2: HashMap<u32, Vec<(u32, Vec<u8>)>>,
    /// acquisition flag -> all ItemLotParam_enemy co-check lots (see `map_v2`).
    pub enemy_v2: HashMap<u32, Vec<(u32, Vec<u8>)>>,
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

/// Parse a co-check overlay side (`map_v2` / `enemy_v2`): `{flag: [{lot, slots}, ...]}` -> the full
/// per-flag lot list. Same per-row validation as `parse_side`; a flag with no valid rows is dropped.
fn parse_side_v2(v: Option<&Value>) -> HashMap<u32, Vec<(u32, Vec<u8>)>> {
    let mut out = HashMap::new();
    let Some(obj) = v.and_then(|v| v.as_object()) else {
        return out;
    };
    for (k, arr) in obj {
        let Ok(flag) = k.parse::<u32>() else { continue };
        let Some(rows) = arr.as_array() else { continue };
        let mut lots: Vec<(u32, Vec<u8>)> = Vec::new();
        for row in rows {
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
                lots.push((lot as u32, slots));
            }
        }
        if !lots.is_empty() {
            out.insert(flag, lots);
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
        map_v2: parse_side_v2(v.get("map_v2")),
        enemy_v2: parse_side_v2(v.get("enemy_v2")),
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
        // CO-CHECK OVERLAY WINS: a shared flag blanks EVERY co-check lot, not just the primary --
        // otherwise the sibling co-check leaks its vanilla ware. Absent from the overlay -> the
        // single-lot `map`/`enemy` (today's behavior). map[flag] == map_v2[flag][0], so the primary
        // is covered either way.
        if let Some(rows) = lots.map_v2.get(f) {
            for (lot, slots) in rows {
                m.insert(*lot, slots.clone());
            }
        } else if let Some((lot, slots)) = lots.map.get(f) {
            m.insert(*lot, slots.clone());
        }
        // NB a flag can appear in BOTH tables (5 do). Blank both -- the client only writes a row
        // that actually exists, so this cannot corrupt the table it is not in.
        if let Some(rows) = lots.enemy_v2.get(f) {
            for (lot, slots) in rows {
                e.insert(*lot, slots.clone());
            }
        } else if let Some((lot, slots)) = lots.enemy.get(f) {
            e.insert(*lot, slots.clone());
        }
    }
    (m, e)
}

/// Every lot the STATIC table associates with a flag in `seed_flags`, across both param tables and
/// both the primary and co-check overlays. The set of lots this seed can prove belong to a check
/// it actually has.
fn lots_for_flags(lots: &StaticLots, seed_flags: &[u32]) -> std::collections::HashSet<u32> {
    let mut out = std::collections::HashSet::new();
    for f in seed_flags {
        for rows in [lots.map_v2.get(f), lots.enemy_v2.get(f)]
            .into_iter()
            .flatten()
        {
            out.extend(rows.iter().map(|(lot, _)| *lot));
        }
        for one in [lots.map.get(f), lots.enemy.get(f)].into_iter().flatten() {
            out.insert(one.0);
        }
    }
    out
}

/// An `ItemLotParam` row id -> the slot indices this pass rewrites in it. The shape both the
/// apworld's `checkLot*` tables and `check_lots::configure` speak.
pub type LotSlots = HashMap<u32, Vec<u8>>;

/// What [`scope_sent_lots`] decided: the two tables to hand on, and how many rows it could PROVE
/// belong to a check this seed does not have. `dropped` is logged, not just counted -- a scoping
/// pass that silently removes rows is the same class of thing as the bug it fixes.
pub struct ScopedLots {
    pub map: LotSlots,
    pub enemy: LotSlots,
    pub dropped: usize,
}

/// Drop the lots our apworld sent that provably belong to a check this seed does NOT have (#329).
///
/// THE BUG. `features/check_lots.py` sends EVERY check lot -- it says so, and says why: *"we can
/// only scope by region here, so send every lot. A lot whose check is out of scope sits in a sealed
/// region the player cannot reach, and rewriting it is inert."* The premise is an inference about
/// GEOMETRY, and it is false. The lot is repointed at the placeholder, `detour.rs` suppresses that
/// placeholder unconditionally and correctly, and with no AP location behind the flag nothing is
/// granted either. **Reachable lot + repointed slot + suppressed placeholder + no check = the
/// player gets NOTHING.**
///
/// MOTIVATING CASE (rule 11). Two reporters, two seeds, same boss: the Summonwater Village Tibia
/// Mariner pays out nothing on a Limgrave seed. Its Deathroot reward is `f530170`, and `data.py`
/// tags that flag **Caelid** while the boss stands in Mistwood. Measured against a real seed's
/// 2090 locations: all 8 Caelid-tagged Summonwater flags absent, all 8 Limgrave-tagged ones
/// present -- a clean 8/8 split across one tile boundary (m60_45_39 vs m60_45_38).
///
/// ⭐ THE ARGUMENT IS ALREADY IN THE TREE, applied to the other apworld. The static-fallback path
/// scopes its table with `blank_tables_for(&sl, &seed_flags)` under the comment *"Scoped, NOT
/// global: blanking a lot the seed does not check would eat a legitimate vanilla pickup."* That is
/// this hazard, named, and our own path is the one that skips it.
///
/// 🛑 FAILS TOWARD TODAY'S BEHAVIOUR, and the direction is deliberate. A lot the static table does
/// not know is KEPT, not dropped, because the two errors are not symmetric:
///
///   * keep a lot we should have dropped -> the status quo, this bug, no worse than yesterday;
///   * drop a lot we should have kept    -> the check hands out its vanilla ware as well as the AP
///     item, a double-dip that `check_lots` exists to kill.
///
/// So only a lot we can PROVE belongs to an out-of-scope flag is dropped. `is_empty()` on the
/// static table means we can prove nothing about anything: pass both tables through untouched.
pub fn scope_sent_lots(
    lots: &StaticLots,
    seed_flags: &[u32],
    sent_map: LotSlots,
    sent_enemy: LotSlots,
) -> ScopedLots {
    if lots.map.is_empty() && lots.enemy.is_empty() {
        // Nothing to prove anything with -> change nothing.
        return ScopedLots {
            map: sent_map,
            enemy: sent_enemy,
            dropped: 0,
        };
    }
    // Every lot the static table knows AT ALL. A sent lot outside this set is unprovable, not
    // out-of-scope -- see the failure-direction note above.
    let mut known: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for t in [&lots.map, &lots.enemy] {
        known.extend(t.values().map(|(lot, _)| *lot));
    }
    for t in [&lots.map_v2, &lots.enemy_v2] {
        for rows in t.values() {
            known.extend(rows.iter().map(|(lot, _)| *lot));
        }
    }
    let kept = lots_for_flags(lots, seed_flags);
    let mut dropped = 0usize;
    let mut keep = |m: LotSlots| -> LotSlots {
        m.into_iter()
            .filter(|(lot, _)| {
                let out_of_scope = known.contains(lot) && !kept.contains(lot);
                dropped += usize::from(out_of_scope);
                !out_of_scope
            })
            .collect()
    };
    let map = keep(sent_map);
    let enemy = keep(sent_enemy);
    ScopedLots {
        map,
        enemy,
        dropped,
    }
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
    /// THE MOTIVATING CASE (rule 11), with the real ids. #329: the Summonwater Village Tibia
    /// Mariner pays out NOTHING on a Limgrave seed, because its Deathroot reward `f530170` is
    /// tagged **Caelid** in `data.py` while the boss stands in Mistwood. On a seed that keeps
    /// Limgrave and not Caelid the location is never created, so the flag has no check -- but the
    /// lot is repointed at the placeholder anyway and the placeholder is suppressed.
    ///
    /// Fails on the pass-everything behaviour, which is what shipped.
    #[test]
    fn an_out_of_scope_boss_lot_is_not_repointed() {
        const F_DEATHROOT_CAELID: u32 = 530_170; // out of scope on a Limgrave seed
        const F_COOKBOOK_LIMGRAVE: u32 = 68_200; // in scope, same village
        const LOT_DEATHROOT: u32 = 5_301_700;
        const LOT_COOKBOOK: u32 = 682_000;

        let mut lots = StaticLots::default();
        lots.map
            .insert(F_DEATHROOT_CAELID, (LOT_DEATHROOT, vec![1]));
        lots.map
            .insert(F_COOKBOOK_LIMGRAVE, (LOT_COOKBOOK, vec![1]));

        // what check_lots.py sends today: BOTH lots, regardless of scope
        let sent = HashMap::from([(LOT_DEATHROOT, vec![1u8]), (LOT_COOKBOOK, vec![1u8])]);
        // what the seed actually has
        let seed_flags = [F_COOKBOOK_LIMGRAVE];

        let out = scope_sent_lots(&lots, &seed_flags, sent, HashMap::new());
        let (m, dropped) = (out.map, out.dropped);

        assert_eq!(dropped, 1);
        assert!(
            !m.contains_key(&LOT_DEATHROOT),
            "the Tibia Mariner's lot has no check on this seed -- repointing it means the player \
             kills a reachable boss and receives nothing (#329)"
        );
        assert!(
            m.contains_key(&LOT_COOKBOOK),
            "an IN-SCOPE lot in the same village must still be repointed, or its vanilla ware \
             double-dips alongside the AP item"
        );
    }

    /// 🛑 The failure direction. A lot the static table has never heard of is KEPT: we cannot prove
    /// it is out of scope, and the two errors are not symmetric -- keeping one wrongly is today's
    /// behaviour, dropping one wrongly resurrects the vanilla double-dip.
    #[test]
    fn an_unprovable_lot_is_kept_not_dropped() {
        let mut lots = StaticLots::default();
        lots.map.insert(1, (100, vec![1]));
        let sent = HashMap::from([(100, vec![1u8]), (999_999, vec![1u8])]);

        let out = scope_sent_lots(&lots, &[1], sent, HashMap::new());
        let (m, dropped) = (out.map, out.dropped);
        assert_eq!(dropped, 0);
        assert!(m.contains_key(&999_999), "unknown lot must survive");
        assert!(m.contains_key(&100));
    }

    /// No static table -> we can prove nothing about anything, so change nothing. Going inert here
    /// would hand out the vanilla ware at EVERY check.
    #[test]
    fn without_the_static_table_both_tables_pass_through_untouched() {
        let sent = HashMap::from([(1u32, vec![1u8]), (2, vec![2])]);
        let sent_e = HashMap::from([(3u32, vec![3u8])]);
        let out = scope_sent_lots(&StaticLots::default(), &[7], sent.clone(), sent_e.clone());
        assert_eq!((out.map, out.enemy, out.dropped), (sent, sent_e, 0));
    }

    /// A co-check flag drives SEVERAL lots. Scoping must keep every one of them, or the sibling
    /// leaks its vanilla ware -- the same reason `blank_tables_for` consults the overlay.
    #[test]
    fn a_co_check_flags_sibling_lots_are_all_in_scope() {
        let mut lots = StaticLots::default();
        lots.map.insert(42, (1000, vec![1]));
        lots.map_v2
            .insert(42, vec![(1000, vec![1]), (1001, vec![1]), (1002, vec![1])]);
        let sent = HashMap::from([(1000, vec![1u8]), (1001, vec![1u8]), (1002, vec![1u8])]);

        let out = scope_sent_lots(&lots, &[42], sent, HashMap::new());
        assert_eq!(
            out.dropped, 0,
            "every sibling lot of a kept flag stays in scope"
        );
        let m = out.map;
        assert_eq!(m.len(), 3);
    }

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
        assert!(
            !m.contains_key(&30120000),
            "a flag this seed does NOT check must not be blanked"
        );
        assert_eq!(
            e.get(&999),
            Some(&vec![3u8]),
            "a flag in BOTH tables is blanked in both"
        );
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
        let l = parse(
            r#"{"map":{"1":{"lot":0,"slots":[1]},"2":{"lot":5,"slots":[]},
                                 "3":{"lot":5,"slots":[9,1]},"x":{"lot":5,"slots":[1]}}}"#,
        );
        assert!(!l.map.contains_key(&1), "lot 0 is not a lot");
        assert!(!l.map.contains_key(&2), "no slots => nothing to blank");
        assert_eq!(
            l.map.get(&3),
            Some(&(5u32, vec![1u8])),
            "slot 9 is out of range, dropped"
        );
        assert_eq!(l.map.len(), 1);
    }

    // ---- co-check overlay (map_v2 / enemy_v2) ----------------------------------------------------
    const T_V2: &str = r#"{
      "placeholder_goods": 8852,
      "map":    {"510460": {"lot": 10460, "slots": [1]},
                 "520110": {"lot": 20110, "slots": [1]}},
      "map_v2": {"510460": [{"lot": 10460, "slots": [1]}, {"lot": 10461, "slots": [1]}]},
      "enemy":  {"777": {"lot": 900, "slots": [2]}},
      "enemy_v2": {"777": [{"lot": 900, "slots": [2]}, {"lot": 901, "slots": [3]}]}
    }"#;

    #[test]
    fn v2_overlay_parses_and_upholds_the_primary_contract() {
        let l = parse(T_V2);
        // map[flag] == map_v2[flag][0]: the primary is the first (lowest) lot.
        assert_eq!(l.map.get(&510460), Some(&(10460u32, vec![1u8])));
        let rows = l.map_v2.get(&510460).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (10460u32, vec![1u8]));
        assert_eq!(rows[1], (10461u32, vec![1u8]));
        assert_eq!(l.enemy_v2.get(&777).unwrap().len(), 2);
    }

    #[test]
    fn co_check_flag_blanks_every_lot_not_just_the_primary() {
        // THE POINT: Messmer's flag 510460 drives lot 10460 (Rem. Impaler) AND 10461 (Kindling).
        // Blanking only the primary leaks the sibling co-check's vanilla ware. The overlay blanks both.
        let l = parse(T_V2);
        let (m, e) = blank_tables_for(&l, &[510460, 777]);
        assert_eq!(m.get(&10460), Some(&vec![1u8]), "primary lot blanked");
        assert_eq!(
            m.get(&10461),
            Some(&vec![1u8]),
            "SIBLING co-check lot blanked too"
        );
        assert_eq!(e.get(&900), Some(&vec![2u8]));
        assert_eq!(e.get(&901), Some(&vec![3u8]), "enemy sibling blanked too");
    }

    #[test]
    fn no_overlay_degrades_to_single_lot_blanking() {
        // A flag with only a `map` entry (no _v2) blanks its one lot -- exactly today's behavior, so
        // an old table / an apworld that emits no overlay is no worse than before.
        let l = parse(T_V2);
        let (m, _e) = blank_tables_for(&l, &[520110]);
        assert_eq!(m.get(&20110), Some(&vec![1u8]));
        assert_eq!(m.len(), 1, "no overlay for 520110 -> just its single lot");
    }

    #[test]
    fn absent_overlay_is_fully_backward_compatible() {
        // The pre-overlay table (no map_v2/enemy_v2 keys) parses to empty overlays and behaves as
        // it always has.
        let l = parse(T);
        assert!(l.map_v2.is_empty() && l.enemy_v2.is_empty());
        let (m, _e) = blank_tables_for(&l, &[520110]);
        assert_eq!(m.get(&20110), Some(&vec![1u8]));
    }
}
