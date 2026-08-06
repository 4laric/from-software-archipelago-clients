//! Scadutree-blessing replay: the DLC blessing FLOOR, over a timeline.
//!
//! WHY THIS EXISTS. On 2026-07-11 `global_scadutree_blessing` was found frozen OFF in the apworld's
//! v0.2 option slim, filed under "half-built". It was not half-built -- it is finished on both sides.
//! But because the floor wire (`dlcScadutreeFloorRanges`) is emitted ONLY when the option == 2,
//! freezing it off meant the key was never emitted and this client's floor path was DEAD CODE. A DLC
//! region unlocked with no fragments handed the player to enemies tuned for blessing ~12 at blessing 0.
//!
//! 🛑 STATUS, corrected 2026-07-29. An earlier version of this header claimed "the option now ships at
//! its declared default (2 = scaled), so this decision is LIVE for every DLC seed." That is NOT true
//! and has not been since the 2026-07-18 balance call: `GlobalScadutreeBlessing.default = 0`
//! (greenfield/eldenring/features/scaling.py) and `defaults.FROZEN_OPTIONS` pins
//! `"global_scadutree_blessing": (0, "off")` (defaults.py:119). No default seed emits
//! `dlcScadutreeFloorRanges`, so the floor path this harness covers is STILL dead code in shipped
//! seeds -- it is live only for a yaml that opts in to `scaled`. The tests below are therefore
//! guarding a mechanism, not a shipped default. See docs/specs/SPEC-global-scadutree-blessing-20260729.md.
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

use crate::upgrades::{applies_globally, blessing_target, level_for_fragments, SCADU_MAX_LEVEL};

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
    /// The player RESTED AND REVERED for real at a DLC grace: the engine now refreshes vanilla
    /// rung `k` under us every tick. Outside the Land of Shadow this is 0 (no refresh loop runs).
    Revere(i32),
    /// The seed's `scaduBlessingCap` arrived on connect.
    Cap(i32),
}

#[derive(Clone, Copy, PartialEq)]
enum Policy {
    /// PRE-FIX: mode 2 behaves as mode 1 -- fragments only, floor ignored.
    FragmentsOnly,
    /// SHIPPED: fragments and floor compose as max.
    FloorComposed,
    /// SHIPPED, and the terms this tier did not cover until 2026-08-01: the seed CAP
    /// (`apply_blessing_cap`) and the player's own EARNED blessing, composed through the clone
    /// RATIO (`clone_rates`) rather than added. SPEC §3.4's double-dip rule lives here.
    CapAndEarned,
}

/// Stand-in for the vanilla ladder's attack scalar `A(n)`, strictly increasing, `A(0) = 1.0`.
///
/// 🛑 NOT FromSoft's numbers, and deliberately not a copy of them. The real `A(n)` is READ FROM THE
/// PARAM ROWS at runtime (`clone_rates` takes both values as arguments precisely so no table of
/// theirs is ever carried here -- `er-foreign-list-provenance-rule`). What this tier tests is
/// COMPOSITION: that our clone times the engine's live rung equals `A(target)` and never
/// `A(target) * A(rung)`. Any strictly-increasing curve with `A(0) = 1.0` proves or refutes that,
/// so this one is arbitrary on purpose.
fn a(n: i32) -> f32 {
    1.0 + 0.05 * n as f32
}

/// Minimal model of the stored blessing byte + the writer's raise-only rule.
struct Sim {
    mode: i32,
    stored: i32,
    frags: i32,
    floor: i32,
    bag_ok: bool,
    writes: Vec<i32>,
    /// Seed cap (`scaduBlessingCap`). 0 = absent => the ladder ceiling, never 0.
    cap: i32,
    /// The vanilla rung the ENGINE is refreshing under us. Non-zero only inside the Land of
    /// Shadow, and only once the player has revered for real. This is `k` in `A(t)/A(k)`.
    rung: i32,
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
            cap: 0,
            rung: 0,
        }
    }

    /// The power our CLONE actually delivers on top of whatever the engine is already applying.
    ///
    /// Effective attack = (what our row carries) x (what the engine's live rung carries). The
    /// double-dip rule is exactly the claim that this product equals `A(target)` and never
    /// `A(target) * A(rung)`.
    fn effective_attack(&self, policy: Policy) -> f32 {
        let target = self.stored;
        let (clone_attack, _cut) = match policy {
            // PRE-FIX shape: the clone carries the FULL A(t), so inside the DLC it multiplies with
            // the engine's live rung. This is the bug §3.4 forbids.
            Policy::FragmentsOnly | Policy::FloorComposed => (a(target), 1.0 / a(target)),
            Policy::CapAndEarned => crate::upgrades::clone_rates(a(target), a(self.rung)),
        };
        clone_attack * a(self.rung)
    }

    /// One throttle window. Mirrors `upgrades::tick_global_scadu`: bail when off / bag unreadable,
    /// compute the target, then RAISE ONLY.
    fn tick(&mut self, policy: Policy) {
        if self.mode == 0 || !self.bag_ok {
            return;
        }
        let target = match policy {
            Policy::CapAndEarned => blessing_target(self.mode, self.frags, self.floor)
                .map(|t| crate::upgrades::apply_blessing_cap(t, self.cap)),
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
            Ev::Revere(k) => {
                s.rung = k;
                // Revering for real also raises the stored byte: the player EARNED that level.
                if k > s.stored {
                    s.stored = k;
                }
            }
            Ev::Cap(c) => s.cap = c,
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

/// MODE 3 -- the combination the single `global_scadutree_blessing` Choice could not express, and
/// the reason the option was split: vanilla SCOPE with the DLC catch-up floor.
///
/// THE MOTIVATING CASE, stated as the acceptance test (CONTRIBUTING rule 11): the fill scatters
/// Scadutree Fragments across the multiworld, so a player can be handed Shadow Keep holding none of
/// them and be brutalised for a decision that was never theirs. Mode 3 says "fix that, and nothing
/// else" -- no Limgrave power curve.
#[test]
fn mode_3_is_the_floor_alone_and_never_global() {
    assert_eq!(
        blessing_target(3, 0, 10),
        Some(10),
        "arriving at Shadow Keep with zero fragments must still meet its floor"
    );
    // 🛑 THE FRAGMENTS ARE NOT OURS TO COUNT IN THIS MODE. The game still runs its own ladder --
    // you revere at a grace and it applies the rung -- so folding the received count in would give
    // blessing WITHOUT revering, which is `anywhere`'s semantics smuggled into the mode whose whole
    // promise is that it does not do that. Outside a DLC bucket the floor is 0, so this is also
    // what makes mode 3 inert in the base game.
    assert_eq!(
        blessing_target(3, 50, 7),
        Some(7),
        "a full fragment purse does not raise mode 3's contribution above the floor"
    );
    assert_eq!(
        blessing_target(3, 50, 0),
        Some(0),
        "no floor here (a base-game bucket) => mode 3 contributes nothing"
    );
    assert_eq!(
        blessing_target(3, 0, 999),
        Some(SCADU_MAX_LEVEL),
        "a nonsense floor from foreign slot_data is still clamped to the curve"
    );

    // The predicate the tick reads to decide whether to touch the clone row at all.
    assert!(!applies_globally(3), "mode 3 must NEVER drive the clone row");
    assert!(!applies_globally(0));
    assert!(applies_globally(1));
    assert!(applies_globally(2));
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

// ---------------------------------------------------------------------------------------------
// SPEC §3.4 -- the double-dip rule, and the seed CAP. Added 2026-08-01: this file modelled the
// FLOOR term and nothing else, so two of the three terms in
// `max(curve(fragments), region floor, stored blessing)` -- plus the cap -- had no timeline.
// ---------------------------------------------------------------------------------------------

/// THE DOUBLE-DIP. Inside the Land of Shadow the engine refreshes a REAL rung under us. If our
/// clone carried the full `A(t)` the two would MULTIPLY and the player would silently get
/// `A(t) * A(k)`.
///
/// Timeline: collect fragments in the base game, walk into the DLC, then REVERE FOR REAL at a
/// grace -- the exact sequence a DLC seed produces. Pre-fix (clone carries `A(t)`) the effective
/// power overshoots; shipped (`clone_rates` -> `A(t)/A(k)`) it lands exactly on `A(t)`.
#[test]
fn revering_for_real_while_we_hold_a_clone_never_double_dips() {
    // 🛑 THE RUNG MUST SIT BELOW OUR TARGET, or this test is vacuous. `clone_rates` short-circuits
    // to NOOP when `a_active >= a_target` (raise-only), so a timeline where the player has already
    // revered TO our target never reaches the ratio at all -- the first version of this test did
    // exactly that, stayed green under a mutation that made the clone carry the full `A(t)`, and
    // only the unit test caught it. Fragments buy 12; the player has revered to 5.
    let events = [
        Ev::Cap(12),
        Ev::Fragments(26), // -> level 12, the cap
        Ev::EnterRegion(0),
        Ev::Revere(5), // the player has earned rung 5 for real; the engine refreshes THAT
    ];

    let broken = replay(2, &events, Policy::FloorComposed);
    let fixed = replay(2, &events, Policy::CapAndEarned);
    assert_eq!(
        broken.stored, fixed.stored,
        "both reach the same LEVEL; only the rate differs"
    );
    assert_eq!(fixed.stored, 12, "fragments buy the cap");
    assert_eq!(
        fixed.rung, 5,
        "and the engine is refreshing a LOWER real rung underneath"
    );

    let want = a(fixed.stored);
    let got_fixed = fixed.effective_attack(Policy::CapAndEarned);
    let got_broken = broken.effective_attack(Policy::FloorComposed);

    assert!(
        (got_fixed - want).abs() < 1e-4,
        "shipped: effective attack {got_fixed} should equal A(target) {want}"
    );
    assert!(
        got_broken > want + 1e-3,
        "pre-fix must OVERSHOOT (that is the double-dip): {got_broken} vs A(target) {want}"
    );
}

/// The composition is `max`, never a subtraction: a player whose OWN blessing already beats our
/// target must never be debuffed by our clone.
#[test]
fn a_real_blessing_above_our_target_is_never_debuffed() {
    let events = [Ev::Cap(12), Ev::Fragments(1), Ev::Revere(15)];
    let s = replay(2, &events, Policy::CapAndEarned);
    let eff = s.effective_attack(Policy::CapAndEarned);
    assert!(
        eff >= a(15) - 1e-4,
        "player earned rung 15; our clone must not pull them below it (got {eff}, want >= {})",
        a(15)
    );
}

/// The seed CAP had unit tests (`upgrades::apply_blessing_cap`) but never entered a timeline.
/// Fragments enough for level 20, cap 12 -> the writer stops at 12 and stays there.
#[test]
fn the_seed_cap_binds_over_a_timeline() {
    let events = [Ev::Cap(12), Ev::Fragments(50), Ev::EnterRegion(0)];
    let s = replay(2, &events, Policy::CapAndEarned);
    assert_eq!(
        s.stored, 12,
        "cap 12 must bind even though 50 fragments buy level 20"
    );
    assert!(
        s.writes.iter().all(|&w| w <= 12),
        "no write may exceed the seed cap: {:?}",
        s.writes
    );
}

/// 🛑 An ABSENT cap must mean "the ladder ceiling", never 0 -- the failure that would ship the
/// whole feature inert for the second time. `apply_blessing_cap` pins this as a unit; here it is
/// over a timeline, because absence arrives on the wire (an old apworld sends no key at all).
#[test]
fn an_absent_cap_does_not_pin_the_blessing_to_zero() {
    let events = [Ev::Fragments(26), Ev::EnterRegion(0)]; // no Ev::Cap at all -> cap stays 0
    let s = replay(2, &events, Policy::CapAndEarned);
    assert_eq!(
        s.stored, 12,
        "an absent cap must fall back to the ceiling, not clamp to 0"
    );
}

/// The floor still composes under the new policy -- the term this file already covered must not
/// regress when the cap and earned terms join it.
#[test]
fn the_floor_still_lifts_a_fragmentless_player_under_the_new_policy() {
    let events = [Ev::Cap(12), Ev::EnterRegion(12), Ev::Fragments(0)];
    let s = replay(2, &events, Policy::CapAndEarned);
    assert_eq!(
        s.stored, 12,
        "floor 12 with zero fragments must still lift to 12"
    );
}
