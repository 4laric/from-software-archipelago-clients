//! `sweep_view` -- what the F6 tracker says about each boss sweep group.
//!
//! MOTIVATING CASE (rule 11), 2026-08-07. bobler killed a boss, watched 49 checks arrive nearly
//! three minutes later, and asked whether anything had happened at all. Alaric: "can we display how
//! many sweep checks are attached to a boss somewhere in the f6 view?"
//!
//! A sweep is the single largest payout in the game -- 49 and 50 checks in one session -- and it was
//! completely invisible until it fired. The player could not see that a group EXISTED, how big it
//! was, or whether it was waiting on a kill or on a lock item. This module turns each group into one
//! readable row.
//!
//! ⭐ NOTHING HERE IS A SPOILER. Every number is derived from data the tracker already shows: the
//! member locations are ordinary rows in their region's list, and the region name is already the
//! header above them. This states a relationship the player could work out by hand, which is the
//! opposite of the lock-hint economy (`lock_hint_economy`), where naming the region IS the thing
//! being charged for.
//!
//! THAT LAST CLAIM HOLDS ONLY WHILE THE HEADER IS REVEALED (2026-08-10). Under
//! `lock_hint_economy::conceal_region` a LOCKED region's header reads "Locked region", so a sweep
//! row that names it hands over free exactly what the economy charges for. `region_open` closes
//! that: a region known to be locked is never named, by `group_label` or by `group_state`. Unknown
//! accessibility still reveals, which is what these rows already did.
//!
//! 🛑🛑 AND THE ROW ITSELF GOES (2026-08-12, Alaric's ruling on #171). Concealing the NAME was not
//! enough. The premise above -- "the member locations are ordinary rows in their region's list" --
//! is false for a locked region, because that region's rollup is concealed too. So the sweep row
//! became the only place the count was visible, and `Rugalea the Great Red Bear -- 0/38 checks`
//! ranks the seed's locked regions by payload without naming one of them. bobler's 2026-08-12
//! screenshot shows twenty such rows with BOTH filters ticked; Alaric: "ah its still spoiling
//! here".
//!
//! `section_rows` withholds those groups entirely and returns a COUNT instead, the same shape the
//! region rollup already uses (`Locked region 0/??`). This supersedes "concealment is not silence"
//! for the locked case only: a group whose region is unknown, or open, is unchanged, and the
//! section header still totals every group so the seed-wide numbers do not silently shrink.

/// One sweep group, as the tracker sees it. Borrowed rather than owned: the caller assembles these
/// from live tables each frame and nothing outlives the render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SweepGroupView<'a> {
    /// The boss-defeat flag that fires the group.
    pub flag: u32,
    /// Region name, resolved from a member location. `None` when the region table cannot place it.
    pub region: Option<&'a str>,
    /// Is that region reachable right now? `Some(false)` is "known locked", and the ONLY case in
    /// which the region is concealed. `None` is "unknown" and is treated as revealable, never as
    /// locked, so a missing table entry cannot invent a wall the player does not have.
    pub region_open: Option<bool>,
    /// Boss name, when `boss_defs` can supply one. Usually `None` -- seeds routinely report
    /// `0 boss-lock def(s)`, which is exactly why the flag is always shown too.
    pub boss: Option<&'a str>,
    /// Members in the group.
    pub members: usize,
    /// Members the player already has.
    pub checked: usize,
    /// Has the defeat flag fired? Read on the TICK, never in the render -- it is game memory.
    pub fired: bool,
    /// Lock item this group is held on (`sweepLockGates`), if any.
    pub gated_on: Option<&'a str>,
}

impl SweepGroupView<'_> {
    /// Members still owed.
    pub fn owed(&self) -> usize {
        self.members.saturating_sub(self.checked)
    }
}

/// The one-line label for a group.
///
/// 🛑 THE FLAG IS ALWAYS PRESENT, for the reason `sweep_watch` exists: the boss NAME resolves only
/// when the seed shipped boss-lock defs, and seeds that report `0 boss-lock def(s)` can never name
/// one. A row that degrades to the prettiest available label instead of the most identifying one is
/// how a 49-check sweep became untraceable in bobler's log.
///
/// The region rides ALONGSIDE the boss name rather than losing to it. bobler, 2026-08-10, reading
/// nine rows: "i see many here that isnt cerulean region". Every one of them was in a region he
/// kept -- but with only a boss name on the row he had no way to place any of them, so a seed-wide
/// section read as a region-scoped one. Naming both is what makes that question answerable.
pub fn group_label(v: &SweepGroupView<'_>) -> String {
    // A region known to be LOCKED is never named here -- see the module note.
    let show_region = v.region_open != Some(false);
    let who = match (v.boss, v.region) {
        (Some(b), Some(r)) if show_region => format!("{b} ({r})"),
        (Some(b), _) => b.to_string(),
        (None, Some(r)) if show_region => r.to_string(),
        (None, Some(_)) => "locked region".to_string(),
        (None, None) => "unplaced".to_string(),
    };
    format!(
        "{who} -- {}/{} checks [flag {}]",
        v.checked, v.members, v.flag
    )
}

/// The state suffix: what this group is waiting for, in the player's terms.
///
/// ⭐ "collected" and "fired" are DIFFERENT and both are worth saying. A group can be fired with
/// members still owed (they are in flight, or another world holds them), and reading "fired" while
/// items are missing is exactly the confusion that produced "wtf it gave me nothing".
pub fn group_state(v: &SweepGroupView<'_>) -> String {
    // A group that has FIRED and paid in full is settled, whatever its region says now.
    if v.fired && v.owed() == 0 {
        return "done".to_string();
    }
    // bobler, 2026-08-10: "it thinks i can fight putrescent knight in logic btw". It did not think
    // that -- it had no way to say otherwise. Putrescent Knight sits in Stone Coffin, whose lock he
    // had never received, and the row read "waiting on the boss": character-for-character what a
    // boss he could walk to right now reads. Reachability OUTRANKS the kill, because the kill is
    // not available. The region stays unnamed -- see the module note.
    if v.region_open == Some(false) {
        return "region still locked -- you cannot reach this boss yet".to_string();
    }
    // 🛑 NOT a let-chain: er-logic is edition 2021 (the workspace is 2024) and `if let ... &&` does
    // not compile there. Same trap for anyone adding to this crate.
    if !v.fired {
        if let Some(item) = v.gated_on {
            return format!("held -- needs {item}");
        }
    }
    match (v.fired, v.owed()) {
        (true, 0) => "done".to_string(),
        (true, n) => format!("fired -- {n} still owed"),
        (false, _) => "waiting on the boss".to_string(),
    }
}

/// Is this group WITHHELD from the list? Only a region known to be locked -- `None` is "the coarse
/// table could not place it" and must never read as a wall (same rule `group_state` follows).
pub fn is_withheld(v: &SweepGroupView<'_>) -> bool {
    v.region_open == Some(false)
}

/// Has this group fired and paid in full? Settled history, and it stops taking up a row.
///
/// Alaric, 2026-08-10: "we can clear these from the view once they've paid out". This lived as an
/// inline filter at the call site in `core.rs`, where no test could reach it; it is here so that
/// its interaction with `is_withheld` is pinned rather than assumed.
pub fn is_settled(v: &SweepGroupView<'_>) -> bool {
    v.fired && v.owed() == 0
}

/// What the tracker's Boss sweeps section actually draws.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SectionRows {
    /// `(label, state)` for every group that renders, in the order given.
    pub rows: Vec<(String, String)>,
    /// How many groups were withheld because their region is known locked.
    pub withheld: usize,
}

/// Split the groups into the rows that render and a count of the ones withheld (#171).
///
/// ⭐ ORDER MATTERS AND IT IS TESTED. Settled is checked FIRST, so a group that fired and paid in
/// full is dropped outright rather than counted as withheld -- otherwise a locked region the player
/// has already been paid out of would inflate a number whose whole job is to say "there is more
/// here you cannot see yet".
pub fn section_rows(groups: &[SweepGroupView<'_>]) -> SectionRows {
    let mut out = SectionRows::default();
    for v in groups {
        if is_settled(v) {
            continue;
        }
        if is_withheld(v) {
            out.withheld += 1;
            continue;
        }
        out.rows.push((group_label(v), group_state(v)));
    }
    out
}

/// The single line that stands in for every withheld group.
///
/// It says the NUMBER and nothing else. A count of locked groups is not orderable into a route the
/// way a per-region member count is, which is the whole difference #171 turns on.
pub fn withheld_line(n: usize) -> String {
    format!("{n} locked group(s) -- hidden until you can reach them")
}

/// The header for the whole section: how much is parked behind bosses right now.
///
/// This is the number bobler actually wanted -- not "is there a sweep" but "how much am I owed".
pub fn section_header(groups: &[SweepGroupView<'_>]) -> String {
    let total: usize = groups.iter().map(|g| g.members).sum();
    let owed: usize = groups.iter().map(SweepGroupView::owed).sum();
    format!(
        "Boss sweeps -- {} group(s), {owed} of {total} check(s) still behind a boss",
        groups.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v<'a>(
        flag: u32,
        region: Option<&'a str>,
        members: usize,
        checked: usize,
        fired: bool,
    ) -> SweepGroupView<'a> {
        SweepGroupView {
            flag,
            region,
            boss: None,
            members,
            checked,
            fired,
            gated_on: None,
            region_open: Some(true),
        }
    }

    #[test]
    fn the_label_always_carries_the_flag_even_when_nothing_can_be_named() {
        // The seed shape that made bobler's 49-check sweep untraceable: `0 boss-lock def(s)`, so
        // `boss` is None for every group in the session. The flag is the only stable handle.
        let s = group_label(&v(20000800, Some("Shadow Keep"), 49, 0, false));
        assert!(s.contains("20000800"), "{s}");
        assert!(s.contains("0/49"), "{s}");
        assert!(s.contains("Shadow Keep"), "{s}");

        let unplaced = group_label(&v(20000800, None, 49, 0, false));
        assert!(unplaced.contains("20000800"), "{unplaced}");
        assert!(unplaced.contains("unplaced"), "{unplaced}");
    }

    #[test]
    fn a_named_boss_carries_its_region_too_and_the_flag_still_shows() {
        // The boss used to REPLACE the region, which left nine rows unplaceable for bobler.
        let mut g = v(20000800, Some("Shadow Keep"), 49, 0, false);
        g.boss = Some("Messmer");
        let s = group_label(&g);
        assert!(s.contains("Messmer"), "{s}");
        assert!(s.contains("Shadow Keep"), "the row must be placeable: {s}");
        assert!(
            s.contains("20000800"),
            "the flag survives a resolvable name: {s}"
        );
    }

    #[test]
    fn a_locked_region_is_never_named_by_either_string() {
        // THE LEAK THIS GUARDS: `lock_hint_economy` charges for a locked region's name and the
        // rollup header above conceals it. A sweep row spelling it out would sell it for free.
        let mut g = v(22000800, Some("Stone Coffin"), 8, 0, false);
        g.boss = Some("Putrescent Knight");
        g.region_open = Some(false);
        let label = group_label(&g);
        let state = group_state(&g);
        assert!(!label.contains("Stone Coffin"), "{label}");
        assert!(!state.contains("Stone Coffin"), "{state}");
        // The boss and the flag still identify the row -- concealment is not silence.
        assert!(label.contains("Putrescent Knight"), "{label}");
        assert!(label.contains("22000800"), "{label}");
    }

    #[test]
    fn a_boss_you_cannot_reach_does_not_read_as_one_you_can() {
        // MOTIVATING CASE (rule 11), bobler 2026-08-10: "it thinks i can fight putrescent knight
        // in logic btw". Stone Coffin Lock was never received in that seed, yet the row was
        // indistinguishable from a boss standing in a region he had open.
        let mut locked = v(22000800, Some("Stone Coffin"), 8, 0, false);
        locked.region_open = Some(false);
        let open = v(2048380850, Some("Cerulean"), 19, 0, false);
        let locked_state = group_state(&locked);
        assert_ne!(locked_state, group_state(&open));
        assert_eq!(group_state(&open), "waiting on the boss");
        assert!(locked_state.contains("locked"), "{locked_state}");
    }

    #[test]
    fn unknown_accessibility_never_invents_a_wall() {
        // `None` means the coarse table could not place it. Reporting that as locked would tell a
        // player they cannot go somewhere they can, which is worse than saying nothing.
        let mut g = v(1, Some("Belurat"), 10, 0, false);
        g.region_open = None;
        assert_eq!(group_state(&g), "waiting on the boss");
        assert!(group_label(&g).contains("Belurat"));
    }

    #[test]
    fn fired_with_members_still_owed_is_not_done() {
        // 🛑 THE DISTINCTION THAT COST A TRIAGE. "wtf it gave me nothing" came from a player who
        // could not tell a sweep that had not fired from one that had fired and was still
        // delivering. Collapsing these two into "done" would rebuild that confusion in the UI.
        assert_eq!(group_state(&v(1, None, 49, 49, true)), "done");
        assert_eq!(
            group_state(&v(1, None, 49, 30, true)),
            "fired -- 19 still owed"
        );
        assert_eq!(
            group_state(&v(1, None, 49, 0, false)),
            "waiting on the boss"
        );
    }

    #[test]
    fn a_gate_names_the_item_it_is_waiting_for() {
        // Held on a lock item is a THIRD state, and telling the player "waiting on the boss" when
        // the boss is already dead would send them to re-kill something.
        let mut g = v(1, None, 10, 0, false);
        g.gated_on = Some("Shadow Keep Lock");
        assert_eq!(group_state(&g), "held -- needs Shadow Keep Lock");
    }

    #[test]
    fn a_fired_group_is_never_reported_as_held() {
        // Once it has fired the gate is behind us; still calling it held would be stale.
        let mut g = v(1, None, 10, 10, true);
        g.gated_on = Some("Shadow Keep Lock");
        assert_eq!(group_state(&g), "done");
    }

    #[test]
    fn checked_above_members_cannot_underflow() {
        // `checked` is counted against a live location set and `members` against slot_data; a
        // reconnect replay could in principle disagree. Saturating, because a panicking tracker
        // row is far worse than an odd number.
        assert_eq!(v(1, None, 3, 9, true).owed(), 0);
        assert_eq!(group_state(&v(1, None, 3, 9, true)), "done");
    }

    #[test]
    fn the_header_totals_what_is_still_behind_a_boss() {
        // The number actually asked for: not "is there a sweep" but "how much am I owed".
        let groups = [
            v(1, Some("Shadow Keep"), 49, 49, true),
            v(2, Some("Belurat"), 50, 10, false),
        ];
        let s = section_header(&groups);
        assert!(s.contains("2 group(s)"), "{s}");
        assert!(s.contains("40 of 99"), "{s}");
    }

    #[test]
    fn an_empty_seed_says_so_without_dividing_by_anything() {
        let s = section_header(&[]);
        assert!(s.contains("0 group(s)"), "{s}");
        assert!(s.contains("0 of 0"), "{s}");
    }

    #[test]
    fn a_locked_group_does_not_render_at_all_it_is_counted() {
        // MOTIVATING CASE (rule 11), bobler 2026-08-12 with BOTH filters ticked. Twenty rows like
        // "Rugalea the Great Red Bear -- 0/38 checks [flag 2044470800] -- region still locked".
        // The region was already concealed and that was not enough: 0/38 ranks the locked regions
        // by payload, and the rollup those members belong to is concealed, so this row was the
        // ONLY place the number appeared. Alaric: "ah its still spoiling here".
        let mut rugalea = v(2044470800, Some("Rauh Base"), 38, 0, false);
        rugalea.boss = Some("Rugalea the Great Red Bear");
        rugalea.region_open = Some(false);
        let mut midra = v(28000800, Some("Abyss"), 7, 0, false);
        midra.boss = Some("Midra, Lord of Frenzied Flame");
        midra.region_open = Some(false);
        let open = v(2046450800, Some("Belurat"), 20, 1, false);

        let out = section_rows(&[rugalea, midra, open]);
        assert_eq!(out.withheld, 2);
        assert_eq!(out.rows.len(), 1, "only the reachable group renders");
        let rendered = format!("{:?}", out.rows);
        for leak in ["Rugalea", "Midra", "0/38", "0/7", "2044470800", "28000800"] {
            assert!(!rendered.contains(leak), "{leak} survived into {rendered}");
        }
        assert!(rendered.contains("Belurat"), "{rendered}");
    }

    #[test]
    fn the_withheld_line_carries_the_count_and_nothing_else() {
        let s = withheld_line(20);
        assert!(s.contains("20"), "{s}");
        assert!(s.is_ascii(), "{s:?}");
        // Nothing orderable: no region, no boss, no per-group size.
        assert!(!s.contains('/'), "a fraction would rank them again: {s}");
    }

    #[test]
    fn a_settled_group_is_dropped_not_counted_as_withheld() {
        // ORDER MATTERS. A locked region the player has already been paid out of must not inflate
        // "N locked group(s)" -- that number exists to say there is more here you cannot see yet.
        let mut paid = v(1, Some("Abyss"), 9, 9, true);
        paid.region_open = Some(false);
        let out = section_rows(&[paid]);
        assert_eq!(out.withheld, 0, "settled outranks withheld");
        assert!(out.rows.is_empty());
        assert!(is_settled(&paid));
        assert!(
            is_withheld(&paid),
            "the predicates disagree on purpose; the ORDER resolves it"
        );
    }

    #[test]
    fn an_unplaceable_group_still_renders() {
        // `None` is "the coarse table could not place it". Withholding it would hide a group the
        // player can walk to right now -- the same rule `group_state` already follows.
        let mut g = v(1, Some("Belurat"), 10, 0, false);
        g.region_open = None;
        let out = section_rows(&[g]);
        assert_eq!(out.withheld, 0);
        assert_eq!(out.rows.len(), 1);
    }

    #[test]
    fn the_header_still_totals_the_groups_the_list_withholds() {
        // The precedent is the settled filter: "The HEADER still counts it, so the section's
        // totals do not silently shrink." A seed-wide total does not rank anything, so it stays.
        let mut locked = v(1, Some("Abyss"), 38, 0, false);
        locked.region_open = Some(false);
        let open = v(2, Some("Belurat"), 20, 0, false);
        let groups = [locked, open];
        let out = section_rows(&groups);
        assert_eq!(out.rows.len(), 1);
        assert_eq!(out.withheld, 1);
        let h = section_header(&groups);
        assert!(h.contains("2 group(s)"), "{h}");
        assert!(h.contains("58 of 58"), "{h}");
    }

    #[test]
    fn a_seed_whose_every_group_is_locked_renders_no_rows_but_says_so() {
        // The section must not vanish: "0 rows" and "20 groups you cannot see" are different
        // facts, and the caller gates the whole header on one of them.
        let mut g = v(1, Some("Abyss"), 7, 0, false);
        g.region_open = Some(false);
        let out = section_rows(&[g, g, g]);
        assert!(out.rows.is_empty());
        assert_eq!(out.withheld, 3);
    }

    #[test]
    fn every_string_is_ascii() {
        // These are imgui labels, not game-font toasts, but the project rule is ASCII everywhere a
        // user might copy the text out of.
        let mut g = v(20000800, Some("Shadow Keep"), 49, 3, true);
        g.gated_on = Some("Shadow Keep Lock");
        g.boss = Some("Messmer");
        for s in [
            group_label(&g),
            group_state(&g),
            section_header(&[g]),
            withheld_line(20),
        ] {
            assert!(s.is_ascii(), "{s:?}");
        }
    }
}
