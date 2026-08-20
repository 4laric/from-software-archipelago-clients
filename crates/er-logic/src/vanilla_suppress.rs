//! Vanilla-pickup suppression decision (pure seam).
//!
//! A vanilla item id that belongs to a check location is the check's ORIGINAL ware; its bag-add is
//! suppressed until the check is COLLECTED (reported to the server), after which a genuine re-pickup
//! of a farmable/respawning source passes through. The re-pickup discriminator is the COLLECTED-flag
//! set (the server checked-set, bridged location -> acquisition flag via slot_data `locationFlags`),
//! NOT the live game acquisition flag.
//!
//! Why not the live flag: for shared-flag multi-item lots (armor sets, NPC-corpse bundles, boss
//! remembrance drops -- 224 flags / 605 locations in the ER datapackage) the game sets the single
//! shared acquisition flag AT or BEFORE the bag-add, so a "is the flag set now?" test reads true at
//! AddItem time and passed the vanilla item through as a bogus "re-pickup" -- the leak observed on
//! Traveler's Clothes (item 0x100f90c4, flag 15007980) in the 2026-07-03 playtest.
//!
//! The collected-set is race-safe in the correct direction: a location enters it only on a flag-poll
//! tick STRICTLY AFTER its check was reported, so the FIRST pickup (flags not yet collected) always
//! suppresses, and a genuine re-pickup (flags collected on a prior, separate event) passes.

use std::collections::HashSet;

/// `true` = SUPPRESS the vanilla bag-add (this pickup IS the check itself, not yet collected).
///
/// `mapped_flags` = the picked item id's check acquisition flags (`checkItemFlags[id]`).
/// `collected` = acquisition flags of every location already in the server checked-set.
///
/// Suppress if ANY mapped flag is not yet collected; pass only once EVERY mapped flag is collected.
/// An empty `collected` (no flag-poll yet) therefore suppresses everything -> suppress-by-default
/// never leaks.
pub fn should_suppress(mapped_flags: &[u32], collected: &HashSet<u32>) -> bool {
    mapped_flags.iter().any(|f| !collected.contains(f))
}

/// `should_suppress`, plus the FLAG-SET DISARM (#321).
///
/// A flag also counts as released once it is LIVE-SET on the game, not only once the server reports
/// it collected. Suppress if ANY mapped flag is neither collected NOR live-set.
///
/// ## Why this is not the leak we removed in July
///
/// The old, deleted policy keyed on the live flag INSTEAD of the collected-set, and it leaked
/// because ~224 acquisition flags cover 605 locations: picking up Traveler's Clothes set the shared
/// flag, and every OTHER id on that flag then read as an already-done re-pickup and passed its
/// vanilla ware through. This is a UNION, not a replacement -- collected still releases on its own
/// -- and, decisively, it is only enabled for a slot_data whose `checkItemFlags` maps **no flag to
/// two ids**. See [`flags_are_unshared`]. Under that precondition a live-set mapped flag can only
/// have been set by THIS id's own check, so releasing on it releases nothing else.
///
/// ## What it buys, and what it does not
///
/// The residue after the world-side lot-coverage drop is the LOT-LESS checks (EMEVD awards), which
/// have no item lot to neutralise at the source, so the id-keyed suppressor is all they have. Today
/// they stay armed from connect until their check is collected -- and forever if the player never
/// does that check -- eating every copy from every other source in between (#321, boblerrr's
/// 2026-08-03 weapon `0x6acfc0`). The disarm caps that exposure at the window BEFORE the award
/// fires. 🛑 It does NOT eliminate it: a farmed copy picked up before you reach the check is still
/// eaten. Do not describe this as closing #321.
pub fn should_suppress_with_flag_disarm(
    mapped_flags: &[u32],
    collected: &HashSet<u32>,
    disarm_on_flag_set: bool,
    flag_set: &dyn Fn(u32) -> bool,
) -> bool {
    mapped_flags
        .iter()
        .any(|f| !collected.contains(f) && !(disarm_on_flag_set && flag_set(*f)))
}

/// PRECONDITION for [`should_suppress_with_flag_disarm`]: no acquisition flag in the whole
/// `checkItemFlags` table is mapped by more than ONE item id.
///
/// Checked against the LIVE slot_data at connect rather than assumed from the apworld version, so
/// an older seed -- rolled before the world-side drop that makes this true -- degrades to
/// collected-set-only instead of silently reopening the shared-flag leak. The world carries the
/// same assertion as a regen gate; this is the belt to that pair of braces.
pub fn flags_are_unshared<'a, I>(entries: I) -> bool
where
    I: IntoIterator<Item = &'a [u32]>,
{
    let mut seen: HashSet<u32> = HashSet::new();
    for flags in entries {
        // A repeat WITHIN one id's list is fine -- it is still that one id's flag.
        let mut this: HashSet<u32> = HashSet::new();
        for &f in flags {
            if !this.insert(f) {
                continue;
            }
            if !seen.insert(f) {
                return false;
            }
        }
    }
    true
}

/// One suppressed vanilla bag-add, held for the #759 watchdog.
///
/// The suppressor is keyed on the item id alone and cannot see where a pickup came from, so a
/// weapon the player put on the ground with **Leave** looks exactly like the check's own world
/// placement and is eaten (er-archipelago#759). What DOES tell them apart is what happens next:
/// a real check pickup is reported and its flags enter the collected-set (or its acquisition flag
/// fires) within a poll tick or two; an eaten Leave-drop resolves NOTHING, ever. This record plus
/// [`split_unresolved`] turns that difference into a log line and a rescue.
#[derive(Debug, Clone, PartialEq)]
pub struct SuppressedPickup {
    /// The suppressed full item id (`AddItemFunc` space).
    pub raw_id: u32,
    /// The id's `checkItemFlags` acquisition flags at suppression time.
    pub mapped_flags: Vec<u32>,
    /// Monotonic client-session ms at suppression time.
    pub at_ms: u64,
}

/// Partition a suppression watch list into (still-watching, overdue).
///
/// An entry RESOLVES -- silently dropped -- once every mapped flag is collected or live-set: the
/// suppressed pickup was (or has since become) the check itself, and nothing of the player's was
/// eaten. An entry older than `grace_ms` whose flags have done NEITHER is overdue: the suppression
/// consumed a pickup that never turned into a check, which is the #759 Leave-drop signature (or a
/// lot-less check ware farmed early, #321 -- the caller's log line names both).
///
/// Direction of error, stated: under a SHARED flag another item's pickup can resolve an entry
/// that really was an eaten Leave-drop -- a MISSED warning. The reverse cannot happen: a genuine
/// check pickup's flags collect within a poll tick or two, far inside any sane `grace_ms`, so an
/// overdue entry is never a false alarm about a working check. Choose the miss, never the lie.
pub fn split_unresolved(
    watch: Vec<SuppressedPickup>,
    collected: &HashSet<u32>,
    flag_live: &dyn Fn(u32) -> bool,
    now_ms: u64,
    grace_ms: u64,
) -> (Vec<SuppressedPickup>, Vec<SuppressedPickup>) {
    let mut keep = Vec::new();
    let mut overdue = Vec::new();
    for e in watch {
        let resolved = e
            .mapped_flags
            .iter()
            .all(|f| collected.contains(f) || flag_live(*f));
        if resolved {
            continue; // the pickup was the check (or the check is done) -- nothing was eaten
        }
        if now_ms.saturating_sub(e.at_ms) >= grace_ms {
            overdue.push(e);
        } else {
            keep.push(e);
        }
    }
    (keep, overdue)
}

#[cfg(test)]
mod tests {
    use super::{
        flags_are_unshared, should_suppress, should_suppress_with_flag_disarm, split_unresolved,
        SuppressedPickup,
    };
    use std::collections::HashSet;

    fn set(flags: &[u32]) -> HashSet<u32> {
        flags.iter().copied().collect()
    }

    #[test]
    fn first_pickup_nothing_collected_suppresses() {
        // No flag-poll has run yet: suppress by default so a first pickup never leaks.
        assert!(should_suppress(&[15007980], &set(&[])));
    }

    #[test]
    fn traveler_clothes_regression() {
        // The exact 2026-07-03 leak: item 0x100f90c4 -> flag 15007980, uncollected at pickup.
        // Old live-flag test PASSED (flag set at/before AddItem); collected-set SUPPRESSES.
        let collected = set(&[]);
        assert!(
            should_suppress(&[15007980], &collected),
            "uncollected check must suppress"
        );

        // After the poll reports it, the same flag is collected -> a re-pickup passes.
        let collected = set(&[15007980]);
        assert!(
            !should_suppress(&[15007980], &collected),
            "collected check must pass on re-pickup"
        );
    }

    #[test]
    fn shared_flag_multi_item_lot_suppresses_before_collection() {
        // Clothes (0x100f90c4) and Manchettes (0x100f9128) are distinct item ids that share one
        // acquisition flag. Each id maps to that same flag; both must suppress on first pickup.
        let shared = 15007980u32;
        let collected = set(&[]);
        assert!(should_suppress(&[shared], &collected)); // clothes id
        assert!(should_suppress(&[shared], &collected)); // manchettes id
    }

    #[test]
    fn all_flags_collected_passes() {
        let collected = set(&[100, 200, 300]);
        assert!(!should_suppress(&[100, 200], &collected));
    }

    #[test]
    fn partial_collection_still_suppresses() {
        // A multi-flag id where only some flags are collected is still an uncollected check.
        let collected = set(&[100]);
        assert!(should_suppress(&[100, 200], &collected));
    }

    #[test]
    fn empty_mapped_flags_passes() {
        // Degenerate: an id with no mapped flags is not a check -> never suppress.
        assert!(!should_suppress(&[], &set(&[])));
    }

    // ---- #321: the FLAG-SET DISARM and its precondition -------------------------------------

    #[test]
    fn disarm_off_is_byte_for_byte_the_old_policy() {
        // The union must be inert when the disarm is off, whatever the live flags say.
        let collected = set(&[]);
        let all_set = |_f: u32| true;
        assert_eq!(
            should_suppress_with_flag_disarm(&[15007980], &collected, false, &all_set),
            should_suppress(&[15007980], &collected),
        );
    }

    #[test]
    fn a_live_set_flag_releases_when_the_disarm_is_on() {
        // The lot-less check has fired: its award set the flag. A later copy from any source must
        // now pass, instead of waiting on a collected-set entry that may never come.
        let collected = set(&[]);
        let fired = |f: u32| f == 220550;
        assert!(should_suppress_with_flag_disarm(
            &[220550],
            &collected,
            false,
            &fired
        ));
        assert!(!should_suppress_with_flag_disarm(
            &[220550],
            &collected,
            true,
            &fired
        ));
    }

    #[test]
    fn an_unset_flag_still_suppresses_under_the_disarm() {
        // 🛑 The bounded half of #321. Before the award fires, a non-check copy is STILL eaten.
        // The disarm caps the window; it does not close it. This test exists so nobody reads the
        // feature as a fix.
        let collected = set(&[]);
        let never = |_f: u32| false;
        assert!(should_suppress_with_flag_disarm(
            &[220550],
            &collected,
            true,
            &never
        ));
    }

    #[test]
    fn collected_still_releases_on_its_own() {
        // Union, not replacement: a collected flag passes even with nothing live-set.
        let collected = set(&[220550]);
        let never = |_f: u32| false;
        assert!(!should_suppress_with_flag_disarm(
            &[220550],
            &collected,
            true,
            &never
        ));
    }

    #[test]
    fn every_mapped_flag_must_be_released_not_just_one() {
        // Multi-check ware: one released flag is not enough, exactly as in the base policy.
        let collected = set(&[100]);
        let fired = |_f: u32| false;
        assert!(should_suppress_with_flag_disarm(
            &[100, 200],
            &collected,
            true,
            &fired
        ));
    }

    #[test]
    fn unshared_precondition_accepts_a_disjoint_table() {
        let a: Vec<u32> = vec![220550];
        let b: Vec<u32> = vec![220560, 220570];
        assert!(flags_are_unshared(vec![a.as_slice(), b.as_slice()]));
    }

    #[test]
    fn unshared_precondition_rejects_the_travelers_clothes_shape() {
        // Two DISTINCT ids on one flag -- the exact shape that made live-flag keying leak. A seed
        // like this must fall back to collected-set-only.
        let clothes: Vec<u32> = vec![15007980];
        let manchettes: Vec<u32> = vec![15007980, 15007981];
        assert!(!flags_are_unshared(vec![
            clothes.as_slice(),
            manchettes.as_slice()
        ]));
    }

    #[test]
    fn unshared_precondition_tolerates_a_repeat_within_one_id() {
        // A duplicated flag inside ONE id's list is still that one id's flag, not sharing.
        let dup: Vec<u32> = vec![220550, 220550];
        assert!(flags_are_unshared(vec![dup.as_slice()]));
    }

    fn sp(raw: u32, flags: &[u32], at: u64) -> SuppressedPickup {
        SuppressedPickup {
            raw_id: raw,
            mapped_flags: flags.to_vec(),
            at_ms: at,
        }
    }

    #[test]
    fn watchdog_resolves_a_collected_entry_silently() {
        let collected = set(&[220550]);
        let never = |_f: u32| false;
        let (keep, overdue) = split_unresolved(
            vec![sp(0x1000, &[220550], 0)],
            &collected,
            &never,
            60_000,
            20_000,
        );
        assert!(
            keep.is_empty() && overdue.is_empty(),
            "a resolved entry must vanish"
        );
    }

    #[test]
    fn watchdog_resolves_on_live_flag_too() {
        let collected = set(&[]);
        let fired = |f: u32| f == 220550;
        let (keep, overdue) = split_unresolved(
            vec![sp(0x1000, &[220550], 0)],
            &collected,
            &fired,
            60_000,
            20_000,
        );
        assert!(keep.is_empty() && overdue.is_empty());
    }

    #[test]
    fn watchdog_reports_the_leave_drop_signature_after_grace() {
        // WITNESS the positive case by name: nothing collected, nothing fired, grace elapsed.
        let collected = set(&[]);
        let never = |_f: u32| false;
        let (keep, overdue) = split_unresolved(
            vec![sp(0x6acfc0, &[220550], 0)],
            &collected,
            &never,
            20_000,
            20_000,
        );
        assert!(keep.is_empty());
        assert_eq!(overdue.len(), 1, "the eaten pickup must surface");
        assert_eq!(overdue[0].raw_id, 0x6acfc0);
    }

    #[test]
    fn watchdog_waits_out_the_grace_window() {
        // A genuine check pickup's flags need a poll tick to collect; inside grace it stays quiet.
        let collected = set(&[]);
        let never = |_f: u32| false;
        let (keep, overdue) = split_unresolved(
            vec![sp(0x1000, &[220550], 0)],
            &collected,
            &never,
            19_999,
            20_000,
        );
        assert_eq!(keep.len(), 1);
        assert!(overdue.is_empty());
    }

    #[test]
    fn watchdog_partial_resolution_is_not_resolution() {
        // Multi-flag ware: one collected flag of two is still unresolved -- same rule as the
        // suppressor itself (every mapped flag, not any).
        let collected = set(&[100]);
        let never = |_f: u32| false;
        let (keep, overdue) = split_unresolved(
            vec![sp(0x1000, &[100, 200], 0)],
            &collected,
            &never,
            30_000,
            20_000,
        );
        assert!(keep.is_empty());
        assert_eq!(overdue.len(), 1);
    }
}
