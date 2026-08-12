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

/// The header for the whole section: how much is parked behind bosses right now.
///
/// This is the number bobler actually wanted -- not "is there a sweep" but "how much am I owed".
pub fn section_header(groups: &[SweepGroupView<'_>]) -> String {
    let (owed, total) = check_totals(groups);
    format!(
        "Boss sweeps -- {} group(s), {owed} of {total} check(s) still behind a boss",
        groups.len()
    )
}

/// Is this group hidden while the tracker's `in-logic only` filter is on?
///
/// MOTIVATING CASE (rule 11), Alaric 2026-08-12, looking at the F6 Boss sweeps panel: 24 groups
/// listed, 21 of them reading "region still locked -- you cannot reach this boss yet". "He wants to
/// see only the ones he can actually go and fight."
///
/// 🛑 THIS KEYS ON `region_open == Some(false)` AND NOTHING ELSE, and `None` MUST STAY VISIBLE.
/// `Some(false)` is a fact somebody established -- the lock governing that region has not been
/// received. `None` is the ABSENCE of a fact: the coarse table could not place the group at all.
/// Hiding on `None` would silently drop groups nobody has shown to be unreachable, which is the
/// difference between hiding the 21 rows Alaric asked about and hiding a row he needed. It is the
/// same asymmetry `group_state` already runs on -- an unknown never invents a wall -- and this is
/// the single most important decision in the filter.
///
/// Nothing else in the view is consulted, either. `fired`, `gated_on` and `owed()` answer "what is
/// this group waiting for", not "can I walk there"; folding any of them in would give one checkbox
/// two meanings, and the checkbox is shared with the per-region rollups below.
pub fn hidden_by_in_logic_only(v: &SweepGroupView<'_>) -> bool {
    v.region_open == Some(false)
}

/// Check totals for a group list, as `(owed, total)`.
///
/// Shared by both headers on purpose: the filter hides ROWS, it must never touch the arithmetic, and
/// two copies of this sum are two chances for the filtered header to disagree with the plain one.
fn check_totals(groups: &[SweepGroupView<'_>]) -> (usize, usize) {
    let total: usize = groups.iter().map(|g| g.members).sum();
    let owed: usize = groups.iter().map(SweepGroupView::owed).sum();
    (owed, total)
}

/// The header when the `in-logic only` filter may be hiding rows.
///
/// 🛑 A HEADER MUST NOT LIE ABOUT WHAT IS UNDER IT. `N group(s)` read above three visible rows is
/// worse than the noise it replaced -- the player cannot tell a seed with three sweeps from a seed
/// with twenty-four and a filter on. So when the filter bites, the header says how many it kept, how
/// many it hid, and why it hid them.
///
/// ⭐ THE TOTALS STILL COUNT EVERY GROUP, which is the property the already-paid row filter in
/// `core.rs` was built with ("The HEADER still counts it, so the section's totals do not silently
/// shrink"). A hidden group is still owed to the player; a section that stopped counting it would
/// answer bobler's original question -- "how much am I owed" -- with a smaller number every time he
/// lost a region.
pub fn section_header_filtered(groups: &[SweepGroupView<'_>], in_logic_only: bool) -> String {
    let hidden = groups.iter().filter(|g| hidden_by_in_logic_only(g)).count();
    // Filter off, or on and biting nothing: the plain header, character for character. "24
    // reachable, 0 behind locked regions" announces a filter that did not do anything, which is
    // its own species of noise.
    if !in_logic_only || hidden == 0 {
        return section_header(groups);
    }
    let (owed, total) = check_totals(groups);
    format!(
        "Boss sweeps -- {} reachable, {hidden} behind locked regions, {owed} of {total} check(s) still behind a boss",
        groups.len() - hidden
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
    fn every_string_is_ascii() {
        // These are imgui labels, not game-font toasts, but the project rule is ASCII everywhere a
        // user might copy the text out of.
        let mut g = v(20000800, Some("Shadow Keep"), 49, 3, true);
        g.gated_on = Some("Shadow Keep Lock");
        g.boss = Some("Messmer");
        for s in [group_label(&g), group_state(&g), section_header(&[g])] {
            assert!(s.is_ascii(), "{s:?}");
        }
    }

    #[test]
    fn a_known_locked_region_is_hidden_by_the_in_logic_filter() {
        // THE ROW ALARIC IS DROWNING IN: 21 of his 24 groups read "region still locked -- you
        // cannot reach this boss yet". If this returned false the checkbox would appear to do
        // nothing and the panel would stay unreadable.
        let mut g = v(22000800, Some("Stone Coffin"), 8, 0, false);
        g.region_open = Some(false);
        assert!(hidden_by_in_logic_only(&g));
    }

    #[test]
    fn a_reachable_region_survives_the_in_logic_filter() {
        // The three rows he actually asked for. Hiding these would empty the section he wanted.
        let g = v(2048380850, Some("Cerulean"), 19, 0, false);
        assert_eq!(g.region_open, Some(true));
        assert!(!hidden_by_in_logic_only(&g));
    }

    #[test]
    fn an_unknown_region_is_shown_because_unknown_is_not_locked() {
        // 🛑 THE LOAD-BEARING CASE. `None` means the coarse table could not place the group -- it
        // is the ABSENCE of a verdict, not a verdict of "locked". Hiding it would delete a row
        // nobody established was unreachable, and the player would never learn it existed: the
        // filter is a view, so a wrongly-hidden row leaves no trace anywhere. Wrongly SHOWING an
        // unreachable group costs one line of noise; wrongly hiding a reachable one costs the
        // sweep. `group_state` already refuses to invent a wall out of `None` for the same reason.
        let mut g = v(1, Some("Belurat"), 10, 0, false);
        g.region_open = None;
        assert!(!hidden_by_in_logic_only(&g));
    }

    #[test]
    fn nothing_but_region_open_can_flip_the_verdict() {
        // The one-checkbox-one-meaning rule, as a corpus. `fired`, `gated_on` and the check counts
        // all answer "what is this waiting for", and any of them leaking into "can I walk there"
        // would give the shared `in-logic only` checkbox a second meaning nobody asked for.
        let mut corpus = Vec::new();
        for fired in [false, true] {
            for gate in [None, Some("Stone Coffin Lock")] {
                for checked in [0, 8] {
                    let mut g = v(1, Some("Stone Coffin"), 8, checked, fired);
                    g.gated_on = gate;
                    g.boss = Some("Putrescent Knight");
                    corpus.push(g);
                }
            }
        }
        // WITNESS: an empty corpus would make every assertion below vacuous.
        assert_eq!(corpus.len(), 8, "corpus must be built before it is judged");
        for g in &corpus {
            let mut open = *g;
            open.region_open = Some(true);
            let mut unknown = *g;
            unknown.region_open = None;
            let mut locked = *g;
            locked.region_open = Some(false);
            assert!(!hidden_by_in_logic_only(&open), "{:?}", open);
            assert!(!hidden_by_in_logic_only(&unknown), "{:?}", unknown);
            assert!(hidden_by_in_logic_only(&locked), "{:?}", locked);
        }
    }

    #[test]
    fn the_unfiltered_header_wording_is_pinned() {
        // PINNED ON PURPOSE. The filtered header is a new string; this one is what players have
        // been reading since 2026-08-07, and adding a filter must not reword it by accident.
        let groups = [
            v(1, Some("Shadow Keep"), 49, 49, true),
            v(2, Some("Belurat"), 50, 10, false),
        ];
        assert_eq!(
            section_header(&groups),
            "Boss sweeps -- 2 group(s), 40 of 99 check(s) still behind a boss"
        );
        // Filter off must be byte-identical to no filter at all.
        assert_eq!(
            section_header_filtered(&groups, false),
            section_header(&groups)
        );
    }

    #[test]
    fn the_filtered_header_names_the_hidden_count_and_why() {
        // Reading "3 group(s)" above 3 rows while 21 are hidden is worse than the noise: it makes
        // a filtered seed look like a small one. The header has to admit what it is not showing.
        let mut locked_a = v(2, Some("Stone Coffin"), 8, 0, false);
        locked_a.region_open = Some(false);
        let mut locked_b = v(3, Some("Enir Ilim"), 12, 2, false);
        locked_b.region_open = Some(false);
        let groups = [v(1, Some("Cerulean"), 19, 4, false), locked_a, locked_b];
        assert_eq!(
            section_header_filtered(&groups, true),
            "Boss sweeps -- 1 reachable, 2 behind locked regions, 33 of 39 check(s) still behind a boss"
        );
    }

    #[test]
    fn a_filter_that_hides_nothing_does_not_claim_it_hid_something() {
        // On, but biting nothing. "2 reachable, 0 behind locked regions" would be true and useless
        // -- it advertises a filter with no effect and pushes the number that matters to the right.
        let groups = [
            v(1, Some("Shadow Keep"), 49, 49, true),
            v(2, Some("Belurat"), 50, 10, false),
        ];
        let s = section_header_filtered(&groups, true);
        assert_eq!(s, section_header(&groups));
        assert!(!s.contains("behind locked regions"), "{s}");
    }

    #[test]
    fn the_filter_hides_rows_and_never_the_arithmetic() {
        // 🛑 A HIDDEN GROUP IS STILL OWED. bobler's question was "how much am I owed"; if the
        // totals shrank with the view, losing a region would look like being owed less. Same
        // property the already-paid row filter in core.rs was built with.
        let mut locked = v(2, Some("Stone Coffin"), 8, 0, false);
        locked.region_open = Some(false);
        let groups = [v(1, Some("Cerulean"), 19, 4, false), locked];
        let on = section_header_filtered(&groups, true);
        let off = section_header_filtered(&groups, false);
        assert!(on.contains("23 of 27 check(s)"), "{on}");
        assert!(off.contains("23 of 27 check(s)"), "{off}");
        assert_ne!(
            on, off,
            "the filtered header must still say it is filtering"
        );
    }

    #[test]
    fn the_filtered_header_is_ascii_too() {
        // Same rule as `every_string_is_ascii`: imgui labels a player may copy out.
        let mut locked = v(2, Some("Stone Coffin"), 8, 0, false);
        locked.region_open = Some(false);
        let groups = [v(1, Some("Cerulean"), 19, 4, false), locked];
        let s = section_header_filtered(&groups, true);
        assert!(s.is_ascii(), "{s:?}");
    }
}
