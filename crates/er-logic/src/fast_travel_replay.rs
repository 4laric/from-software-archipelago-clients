//! Fast-travel gate replay: the client must never SET a flag it does not own.
//!
//! Reproduces the 2026-07-11 Gael Tunnel bug as a timeline. The old policy, with nothing cached,
//! set whatever flag `FieldArea::enable_fast_travel_event_flag` named. In a boss dungeon that is the
//! BOSS'S DEFEAT FLAG, so the client deleted the Magma Wyrm (the game disables a boss whose defeat
//! flag is on) and then paid out that boss's 6-check sweep to itself.

#![cfg(test)]

use crate::fast_travel::{GateAction, gate_action, prime_known_good};

/// A flag the game owns and we must never write. In m32_07 the fast-travel field names 32070800 --
/// the Magma Wyrm's defeat flag.
const WYRM_DEFEAT: i32 = 32_070_800;
/// The Roundtable grace: the client sets this at spawn, so it is really on and inert to point at.
const START_GRACE: u32 = 71_190;

#[derive(Clone, Copy, PartialEq)]
enum Policy {
    /// PRE-FIX: with nothing cached, SET the flag the field names.
    SetTheFieldFlag,
    /// SHIPPED: allow / redirect / wait. Never set.
    NeverSet,
}

#[derive(Default)]
struct Sim {
    known_good: u32,
    /// Flags WE wrote. Must stay empty forever.
    flags_written: Vec<u32>,
    field: i32,
}

impl Sim {
    /// One tick. `field_on` = is the flag the field names actually set in the game?
    fn tick(&mut self, field: i32, field_on: bool, policy: Policy) {
        self.field = field;
        match policy {
            Policy::NeverSet => match gate_action(field, field_on, self.known_good) {
                GateAction::AllowAndCache(f) => self.known_good = f,
                GateAction::RedirectField(f) => self.field = f as i32, // field overwrite: no game state
                GateAction::Wait => {}
            },
            Policy::SetTheFieldFlag => {
                if field > 0 && field_on {
                    self.known_good = field as u32;
                } else if self.known_good > 0 {
                    self.field = self.known_good as i32;
                } else if field > 0 {
                    self.flags_written.push(field as u32); // <-- the bug
                }
            }
        }
    }
}

/// THE REGRESSION. Boot, walk straight into a boss dungeon, nothing cached.
#[test]
fn entering_a_boss_dungeon_must_not_set_the_bosss_defeat_flag() {
    // Pre-fix: the client writes the wyrm's defeat flag -> boss deleted, sweep paid out.
    let mut broken = Sim::default();
    broken.tick(WYRM_DEFEAT, false, Policy::SetTheFieldFlag);
    assert_eq!(
        broken.flags_written,
        vec![WYRM_DEFEAT as u32],
        "pre-fix reproduction: the client SET the Magma Wyrm's defeat flag to unblock its own travel"
    );

    // Shipped: nothing is written. Travel simply stays blocked, exactly as vanilla blocks it.
    let mut fixed = Sim::default();
    fixed.tick(WYRM_DEFEAT, false, Policy::NeverSet);
    assert!(
        fixed.flags_written.is_empty(),
        "the client must NEVER set a flag whose meaning the game owns"
    );
}

/// With a known-good flag primed from the start graces, the gate opens with zero side effects.
#[test]
fn a_primed_known_good_flag_opens_the_gate_without_writing_anything() {
    let mut s = Sim::default();
    s.known_good = prime_known_good(&[START_GRACE]).unwrap();
    s.tick(WYRM_DEFEAT, false, Policy::NeverSet);
    assert!(s.flags_written.is_empty(), "no writes");
    assert_eq!(
        s.field, START_GRACE as i32,
        "the field is pointed at an already-on, inert flag"
    );
}

/// Once travel is legitimately allowed, that flag becomes the known-good one.
#[test]
fn a_legitimately_open_gate_is_cached() {
    let mut s = Sim::default();
    s.tick(71_002, true, Policy::NeverSet);
    assert_eq!(s.known_good, 71_002);
    // ...and it is what a later block redirects to.
    s.tick(WYRM_DEFEAT, false, Policy::NeverSet);
    assert_eq!(s.field, 71_002);
    assert!(s.flags_written.is_empty());
}

/// A whole run: spawn, dungeon, out, dungeon again. Never a single write.
#[test]
fn no_write_ever_happens_across_a_run() {
    let mut s = Sim::default();
    s.known_good = prime_known_good(&[START_GRACE]).unwrap_or(0);
    for (field, on) in [
        (WYRM_DEFEAT, false), // walk into Gael Tunnel
        (32_000_800, false),  // and another dungeon
        (71_002, true),       // a real grace: gate legitimately open
        (32_020_800, false),  // straight into a third dungeon
        (0, false),           // field not placed yet (load screen)
    ] {
        s.tick(field, on, Policy::NeverSet);
    }
    assert!(
        s.flags_written.is_empty(),
        "not one flag written across the whole run"
    );
}

/// field == 0 (load screen / not placed) with nothing cached: wait, do not invent a flag.
#[test]
fn field_zero_with_nothing_cached_waits() {
    assert_eq!(gate_action(0, false, 0), GateAction::Wait);
    let mut s = Sim::default();
    s.tick(0, false, Policy::NeverSet);
    assert!(s.flags_written.is_empty());
}

#[test]
fn gate_action_is_total() {
    assert_eq!(
        gate_action(71_002, true, 0),
        GateAction::AllowAndCache(71_002)
    );
    assert_eq!(
        gate_action(WYRM_DEFEAT, false, 71_190),
        GateAction::RedirectField(71_190)
    );
    assert_eq!(gate_action(WYRM_DEFEAT, false, 0), GateAction::Wait);
    assert_eq!(
        gate_action(-1, false, 0),
        GateAction::Wait,
        "a negative field is not a flag"
    );
}
