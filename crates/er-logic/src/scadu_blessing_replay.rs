//! Scadutree-blessing replay: the DLC blessing FLOOR, over a timeline.
//!
//! WHY THIS EXISTS. On 2026-07-11 `global_scadutree_blessing` was found frozen OFF in the apworld's
//! v0.2 option slim, filed under "half-built". It was not half-built -- it is finished on both sides.
//! But because the floor wire (`dlcScadutreeFloorRanges`) is emitted ONLY when the option == 2,
//! freezing it off meant the key was never emitted and this client's floor path was DEAD CODE. A DLC
//! region unlocked with no fragments handed the player to enemies tuned for blessing ~12 at blessing 0.
//! The option now ships at its declared default (2 = scaled), so this decision is LIVE for every DLC
//! seed -- and it had no test at all.
//!
//! The decision is a timeline, not a tick: fragments arrive over the run, the player crosses region
//! boundaries (so the floor changes under them), the bag walk can transiently fail, and a reconnect
//! re-runs the whole thing. Each of those is a chance to write the WRONG level -- or worse, to LOWER a
//! real blessing the player earned. So it gets a replay harness, per CONTRIBUTING: a pure predicate
//! (`upgrades::blessing_target`) plus a model that can express "later".
//!
//! The failing-without-the-fix / passing-with-it pair is the `policy` flag: `Policy::FragmentsOnly`
//! reproduces the pre-fix behaviour (mode 2 treated as an alias of mode 1 -- which is exactly what the
//! client's own comment still claimed) and `Policy::FloorComposed` is the shipped one.

#![cfg(test)]

use crate::upgrades::{blessing_target, level_for_fragments, SCADU_MAX_LEVEL};

/// What the client does each throttle window.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Ev {
    /// The player now holds this many Scadutree Fragments.
    Fragments(i32),
    /// The player moved; the floor for the CURRENT play_region is now this (0 = not in a DLC bucket).
    EnterRegion(i32),
    /// The inventory walk failed this tick (bag realloc raced us). The client must NOT write.
    BagUnreadable,
    /// Server reconnect: slot_data is re-applied and the tick loop starts over.
    Reconnect,
    /// The game itself set a blessing (e.g. the player consumed real Revered Ash outside our path).
    GameSetBlessing(i32),
}

#[derive(Clone, Copy, PartialEq)]
enum Policy {
    /// PRE-FIX: mode 2 behaves as mode 1 -- fragments only, floor ignored.
    FragmentsOnly,
    /// SHIPPED: fragments and floor compose as max.
    FloorComposed,
}

/// Minimal model of the stored blessing byte + the writer's raise-only rule.
struct Sim {
    mode: i32,
    stored: i32,
    frags: i32,
    floor: i32,
    bag_ok: bool,
    writes: Vec<i32>,
}

impl Sim {
    fn new(mode: i32) -> Self {
        Sim {
            mode,
            stored: 0,
            frags: 0,
            floor: 0,
            bag_ok: true,
            writes: vec![],
        }
    }

    /// One throttle window. Mirrors `upgrades::tick_global_scadu`: bail when off / bag unreadable,
    /// compute the target, then RAISE ONLY.
    fn tick(&mut self, policy: Policy) {
        if self.mode == 0 || !self.bag_ok {
            return;
        }
        let target = match policy {
            Policy::FloorComposed => blessing_target(self.mode, self.frags, self.floor),
            Policy::FragmentsOnly => {
                if self.mode == 0 {
                    None
                } else {
                    Some(level_for_fragments(self.frags))
                }
            }
        };
        let Some(t) = target else { return };
        if t > self.stored {
            self.stored = t;
            self.writes.push(t);
        }
    }
}

fn replay(mode: i32, events: &[Ev], policy: Policy) -> Sim {
    let mut s = Sim::new(mode);
    for e in events {
        match *e {
            Ev::Fragments(n) => {
                s.frags = n;
                s.bag_ok = true;
            }
            Ev::EnterRegion(f) => s.floor = f,
            Ev::BagUnreadable => s.bag_ok = false,
            Ev::Reconnect => { /* slot_data re-applied; stored byte survives in the save */ }
            Ev::GameSetBlessing(v) => s.stored = v,
        }
        s.tick(policy);
    }
    s
}

// ---------------------------------------------------------------------------------------------
// The bug this tier exists to catch.
// ---------------------------------------------------------------------------------------------

/// THE REGRESSION. Walk into a DLC region (floor 12) holding ZERO fragments.
/// Pre-fix (mode 2 == mode 1): blessing stays 0 and the area's enemies delete you.
/// Shipped: the floor lifts you to 12.
#[test]
fn dlc_region_with_no_fragments_is_floored_not_left_at_zero() {
    let evs = [Ev::Fragments(0), Ev::EnterRegion(12)];

    let broken = replay(2, &evs, Policy::FragmentsOnly);
    assert_eq!(
        broken.stored, 0,
        "pre-fix reproduction: floor ignored, player arrives at blessing 0"
    );

    let fixed = replay(2, &evs, Policy::FloorComposed);
    assert_eq!(
        fixed.stored, 12,
        "the DLC floor must lift a fragment-less player to the area's expectation"
    );
}

/// Fragments still count ABOVE the floor -- the floor is a floor, not a cap.
#[test]
fn collected_fragments_still_count_above_the_floor() {
    // 26 fragments = level 12 by the vanilla curve; a floor of 5 must not hold us down.
    let s = replay(
        2,
        &[Ev::EnterRegion(5), Ev::Fragments(26)],
        Policy::FloorComposed,
    );
    assert_eq!(s.stored, level_for_fragments(26));
    assert!(
        s.stored > 5,
        "the floor must not cap a player who earned more"
    );
}

/// Leaving the DLC (floor -> 0) must NOT lower a blessing already granted. Raise-only.
#[test]
fn leaving_the_dlc_never_lowers_the_blessing() {
    let s = replay(
        2,
        &[Ev::Fragments(0), Ev::EnterRegion(12), Ev::EnterRegion(0)],
        Policy::FloorComposed,
    );
    assert_eq!(
        s.stored, 12,
        "floor dropping to 0 outside the DLC must never write a LOWER blessing"
    );
    assert_eq!(
        s.writes,
        vec![12],
        "and it must not write at all on the way out"
    );
}

/// A real, higher blessing the game set must never be stomped by our floor.
#[test]
fn a_higher_real_blessing_is_never_stomped() {
    let s = replay(
        2,
        &[
            Ev::GameSetBlessing(18),
            Ev::EnterRegion(12),
            Ev::Fragments(0),
        ],
        Policy::FloorComposed,
    );
    assert_eq!(s.stored, 18);
    assert!(
        s.writes.is_empty(),
        "already above the target -> no write at all"
    );
}

/// A transient bag-walk failure must be INERT -- never a write, and never a flicker to 0.
#[test]
fn a_transient_bag_miss_never_writes() {
    let s = replay(
        2,
        &[Ev::Fragments(26), Ev::BagUnreadable, Ev::EnterRegion(12)],
        Policy::FloorComposed,
    );
    assert_eq!(
        s.stored,
        level_for_fragments(26),
        "the bag miss must not disturb the standing blessing"
    );
    assert_eq!(
        s.writes.len(),
        1,
        "exactly one write (the real one); the miss tick wrote nothing"
    );
}

/// Reconnect re-applies slot_data and re-runs the loop: idempotent, no second write.
#[test]
fn reconnect_is_idempotent() {
    let s = replay(
        2,
        &[
            Ev::Fragments(0),
            Ev::EnterRegion(12),
            Ev::Reconnect,
            Ev::Reconnect,
        ],
        Policy::FloorComposed,
    );
    assert_eq!(s.stored, 12);
    assert_eq!(
        s.writes,
        vec![12],
        "reconnect must not re-write a level already stored"
    );
}

/// mode 0 (off) is total: never a write, whatever happens.
#[test]
fn off_never_writes() {
    let s = replay(
        0,
        &[Ev::Fragments(50), Ev::EnterRegion(20)],
        Policy::FloorComposed,
    );
    assert_eq!(s.stored, 0);
    assert!(s.writes.is_empty());
}

/// mode 1 (player_only) ignores the floor by DESIGN -- that is the difference between the modes, and
/// it must stay true, or mode 1 silently becomes mode 2.
#[test]
fn mode_1_ignores_the_floor_by_design() {
    let s = replay(
        1,
        &[Ev::Fragments(0), Ev::EnterRegion(12)],
        Policy::FloorComposed,
    );
    assert_eq!(s.stored, 0, "player_only must not apply the DLC floor");
}

// ---------------------------------------------------------------------------------------------
// The pure predicate itself.
// ---------------------------------------------------------------------------------------------

#[test]
fn blessing_target_is_max_of_fragments_and_floor_and_is_clamped() {
    assert_eq!(
        blessing_target(0, 50, 20),
        None,
        "off => no decision at all"
    );
    assert_eq!(
        blessing_target(1, 0, 12),
        Some(0),
        "mode 1 ignores the floor"
    );
    assert_eq!(blessing_target(2, 0, 12), Some(12), "mode 2 floors");
    assert_eq!(
        blessing_target(2, 26, 5),
        Some(level_for_fragments(26)),
        "fragments win when higher"
    );
    assert_eq!(
        blessing_target(2, 999, 999),
        Some(SCADU_MAX_LEVEL),
        "clamped to the curve's max"
    );
    assert_eq!(
        blessing_target(9, 10, 10),
        None,
        "an unknown mode must not write"
    );
}

#[test]
fn level_for_fragments_matches_the_vanilla_curve() {
    assert_eq!(level_for_fragments(0), 0);
    assert_eq!(level_for_fragments(1), 1);
    assert_eq!(
        level_for_fragments(2),
        1,
        "below the next threshold -> stays"
    );
    assert_eq!(level_for_fragments(50), 20, "the full set is max level");
    assert_eq!(
        level_for_fragments(1000),
        20,
        "never past the top of the curve"
    );
}
