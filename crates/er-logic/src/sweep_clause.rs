//! Seed-truth filter for the baked sweep clause in location display names (er-archipelago#936).
//!
//! World-side, `gen_data` bakes ", may be sweep-granted by <boss> (<tile>)" into a sweep MEMBER's
//! location name at corpus-gen time (greenfield/desc_sources.py `with_sweep`) so that a hint can
//! name the boss that hands the check over (#670) -- world v0.5.2 reworded it from the older
//! ", also granted by <boss> (<tile>)", which this reader still recognises for seeds rolled on an
//! older apworld. That clause describes the CORPUS -- every
//! check a sweep COULD pay. What a seed ACTUALLY pays is `enabled_sweeps(world)` -- the rung
//! filter over the `dungeon_sweep` option plus the per-seed progression-surface cut -- and that
//! is exactly what slot_data `dungeonSweepFlags` carries. Location names ride the STATIC
//! datapackage (`location_name_to_id`), so the clause cannot be un-baked per seed server-side;
//! the reader holds the seed truth and must filter at display time.
//!
//! A clause on a check this seed's sweeps do not grant is a lie of exactly the shape Haraldwyrm
//! reported (2026-08-20): "some items say they are granted as sweep rewards when they are not,
//! like golden seeds and crystal tears" -- field-sweep members displayed in a seed whose rung
//! pays no field sweep.

use std::collections::HashSet;

/// The clause openers this reader recognises, newest first.
///
/// `CLAUSE_CURRENT` is byte-identical to greenfield/desc_sources.py `SWEEP_CLAUSE_OPENER`
/// (`f"{base}, {clause}"`). `CLAUSE_LEGACY` is the wording every datapackage generated before
/// world v0.5.2 baked, and it is NOT dead code: location names ride the static datapackage, which
/// comes from the GENERATOR, so a current client connecting to a seed rolled on an older apworld
/// is handed the old names and must still be able to take the clause back out. Both are recognised
/// on every name; a name carries at most one.
///
/// The clause always ends the descriptive text: what follows it, when anything does, is the
/// " [f<flag>]" tail the generator appends last.
const CLAUSE_CURRENT: &str = ", may be sweep-granted by ";
const CLAUSE_LEGACY: &str = ", also granted by ";
const CLAUSES: [&str; 2] = [CLAUSE_CURRENT, CLAUSE_LEGACY];

/// Byte offset of whichever recognised clause opener this name carries, or `None`.
///
/// Earliest match wins rather than first-listed: the openers are alternative spellings of the same
/// clause, so "which one is it" is never a question about priority, and scanning for the earliest
/// keeps the answer independent of the order the wordings happen to be listed in.
fn clause_start(name: &str) -> Option<usize> {
    CLAUSES.iter().filter_map(|c| name.find(c)).min()
}

/// `name` with the sweep clause removed when this seed's active sweep members do not include
/// `ap_id`; the name is unchanged when the seed DOES sweep-grant it, when there is no clause, or
/// when `active` is `None` (seed truth unknown -- never strip on a guess).
///
/// `active` is the union of `dungeonSweepFlags` member lists as the seed emitted them (already
/// rung-filtered and surface-cut world-side), so an empty set -- a seed with sweeps OFF --
/// strips every clause, which is the truth for that seed.
pub fn seed_scoped_name(name: &str, ap_id: u64, active: Option<&HashSet<u64>>) -> String {
    let Some(members) = active else {
        return name.to_string();
    };
    if members.contains(&ap_id) {
        return name.to_string();
    }
    let Some(start) = clause_start(name) else {
        return name.to_string();
    };
    // Keep the flag tail: it is the check's identity in logs and issue reports.
    match name[start..].find(" [f") {
        Some(tail) => format!("{}{}", &name[..start], &name[start + tail..]),
        None => name[..start].to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::seed_scoped_name;
    use std::collections::HashSet;

    fn set(ids: &[u64]) -> HashSet<u64> {
        ids.iter().copied().collect()
    }

    // v0.5.2 WORDING (er-archipelago#936, colombius' report verbatim). The datapackage now bakes
    // ", may be sweep-granted by <boss> (<tile>)"; the strip is identical and the [f...] tail
    // survives. This is the rule-11 acceptance case for the rename.
    const COLOMBIUS: &str = "Mountaintops of the Giants :: Golden Seed - near Foot of the Forge, by two snow trolls, may be sweep-granted by Fire Giant (m60_52_52) [f1052537800]";
    const COLOMBIUS_ID: u64 = 7773183;

    #[test]
    fn current_wording_is_stripped_when_the_seed_does_not_grant_it() {
        let active = set(&[]);
        assert_eq!(
            seed_scoped_name(COLOMBIUS, COLOMBIUS_ID, Some(&active)),
            "Mountaintops of the Giants :: Golden Seed - near Foot of the Forge, by two snow trolls [f1052537800]"
        );
    }

    #[test]
    fn current_wording_is_kept_when_the_seed_does_grant_it() {
        let active = set(&[COLOMBIUS_ID]);
        assert_eq!(
            seed_scoped_name(COLOMBIUS, COLOMBIUS_ID, Some(&active)),
            COLOMBIUS
        );
    }

    #[test]
    fn current_wording_never_strips_on_unknown_seed_truth() {
        assert_eq!(seed_scoped_name(COLOMBIUS, COLOMBIUS_ID, None), COLOMBIUS);
    }

    #[test]
    fn current_wording_with_parens_in_the_boss_name() {
        // Trigger names are not unique and several carry a parenthesised weapon; the cut is keyed
        // on the opener, so the boss' own parens are irrelevant under the new wording too.
        let active = set(&[]);
        assert_eq!(
            seed_scoped_name(
                "Consecrated Snowfield :: Golden Seed - near Consecrated Snowfield, may be sweep-granted by Night's Cavalry (Glaive) (m60_48_55) [f1049557800]",
                1,
                Some(&active)
            ),
            "Consecrated Snowfield :: Golden Seed - near Consecrated Snowfield [f1049557800]"
        );
    }

    #[test]
    fn a_name_asserting_a_grant_is_no_longer_what_the_generator_writes() {
        // The rename's point: the CURRENT wording claims eligibility, not a grant. Guard the
        // constant itself so a future edit cannot quietly re-assert one.
        assert!(super::CLAUSE_CURRENT.starts_with(", may be "));
        assert!(!super::CLAUSE_CURRENT.contains("also granted by"));
        assert_ne!(super::CLAUSE_CURRENT, super::CLAUSE_LEGACY);
    }

    // LEGACY WORDING (pre-v0.5.2 datapackages). A current client can be handed these names by any
    // seed rolled on an older apworld -- the datapackage comes from the GENERATOR -- so the
    // recogniser must keep both. The cases below are the originals, unchanged.
    // The acceptance cases are Haraldwyrm's two named examples, verbatim from
    // greenfield/eldenring/data.py (name, ap id): a field-sweep Golden Seed and a field-sweep
    // Crystal Tear. In a seed whose rung pays no field sweep neither is sweep-granted, so the
    // clause must not show.
    const GOLDEN_SEED: &str =
        "Altus :: Golden Seed - On a tree near the road, also granted by Night's Cavalry (m60_39_51) [f1039517400]";
    const GOLDEN_SEED_ID: u64 = 7772843;
    const CRYSTAL_TEAR: &str = "Altus :: Crimson Crystal Tear - near Hermit Merchant's Shack (region unconfirmed), also granted by Deathbird (m60_44_53) [f65030]";
    const CRYSTAL_TEAR_ID: u64 = 7770028;

    #[test]
    fn clause_stripped_when_seed_does_not_sweep_grant_the_check() {
        let active = set(&[]);
        assert_eq!(
            seed_scoped_name(GOLDEN_SEED, GOLDEN_SEED_ID, Some(&active)),
            "Altus :: Golden Seed - On a tree near the road [f1039517400]"
        );
        assert_eq!(
            seed_scoped_name(CRYSTAL_TEAR, CRYSTAL_TEAR_ID, Some(&active)),
            "Altus :: Crimson Crystal Tear - near Hermit Merchant's Shack (region unconfirmed) [f65030]"
        );
    }

    #[test]
    fn clause_kept_when_seed_sweep_grants_the_check() {
        // The rung includes the field sweep and the surface cut spared this member: the clause
        // is true and stays -- it is the answer to "which boss do I kill" (#670).
        let active = set(&[GOLDEN_SEED_ID]);
        assert_eq!(
            seed_scoped_name(GOLDEN_SEED, GOLDEN_SEED_ID, Some(&active)),
            GOLDEN_SEED
        );
    }

    #[test]
    fn unknown_seed_truth_never_strips() {
        // No slot_data yet (or no flag-poll): the corpus name is the best we have.
        assert_eq!(
            seed_scoped_name(GOLDEN_SEED, GOLDEN_SEED_ID, None),
            GOLDEN_SEED
        );
    }

    #[test]
    fn name_without_clause_is_untouched() {
        let active = set(&[]);
        let plain = "Liurnia :: Imbued Sword Key - near The Four Belfries [f1033477020]";
        assert_eq!(seed_scoped_name(plain, 1, Some(&active)), plain);
    }

    #[test]
    fn clause_without_flag_tail_strips_to_end() {
        let active = set(&[]);
        assert_eq!(
            seed_scoped_name(
                "Caelid :: Golden Seed - Roadside Phantom Tree, also granted by Night's Cavalry (m60_49_37)",
                1,
                Some(&active)
            ),
            "Caelid :: Golden Seed - Roadside Phantom Tree"
        );
    }

    #[test]
    fn boss_name_parens_do_not_confuse_the_cut() {
        // The disambiguating tile is parenthesised AND the boss name itself can carry parens;
        // the cut is keyed on the clause opener, so neither matters.
        let active = set(&[]);
        assert_eq!(
            seed_scoped_name(
                "Consecrated Snowfield :: Golden Seed - near Consecrated Snowfield, also granted by Night's Cavalry (Glaive) (m60_48_55) [f1049557800]",
                1,
                Some(&active)
            ),
            "Consecrated Snowfield :: Golden Seed - near Consecrated Snowfield [f1049557800]"
        );
    }
}
