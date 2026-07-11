//! Fast-travel gate: what to do when the game says travel is blocked.
//!
//! THE BUG THIS EXISTS TO KILL (found in-game 2026-07-11, Gael Tunnel).
//!
//! `FieldArea::enable_fast_travel_event_flag` is the flag VANILLA uses to gate warping out of an
//! area. In a boss dungeon that flag IS THE BOSS'S DEFEAT FLAG -- vanilla will not let you warp out
//! until the boss is dead. The client's old fallback, when it had no cached known-good flag, was to
//! `set_event_flag(field, true)` -- i.e. to SET whatever flag the field happened to name. Walking into
//! Gael Tunnel it set flag 32070800, which is the Magma Wyrm's defeat flag, and so:
//!
//!   * the game ran `if (EventFlag(32070800)) DisableCharacter(32070800)` -- the boss never spawned;
//!   * the client's own sweep watcher saw the flag flip and paid out the boss's 6-check sweep.
//!
//! The client killed the boss to unblock its own fast travel, then rewarded itself for the kill.
//!
//! THE RULE: never SET a flag whose meaning you do not own. The gate can be opened with ZERO side
//! effects by pointing the field at a flag that is ALREADY on and inert -- which is what the
//! known-good path always did. So the decision is: allow / redirect / wait. There is no "set".

/// What the tick should do about the fast-travel gate. Pure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateAction {
    /// Travel is genuinely allowed; cache `flag` as the known-good on-flag.
    AllowAndCache(u32),
    /// Blocked, but we hold a flag that is really on: point the field at it. No game state changes.
    RedirectField(u32),
    /// Blocked and we have nothing safe to point at. DO NOTHING -- vanilla also blocks here, and the
    /// only way to "fix" it would be to set a flag we do not own, which is what deleted a boss.
    Wait,
}

/// `field`      -- FieldArea::enable_fast_travel_event_flag as the game currently has it.
/// `field_on`   -- is that flag actually set?
/// `known_good` -- a flag we have previously OBSERVED to be on (0 = none yet).
pub fn gate_action(field: i32, field_on: bool, known_good: u32) -> GateAction {
    if field > 0 && field_on {
        return GateAction::AllowAndCache(field as u32);
    }
    if known_good > 0 {
        return GateAction::RedirectField(known_good);
    }
    GateAction::Wait
}

/// Seed the known-good flag from flags the client itself has already turned on this session (the
/// start graces -- it sets them at spawn, so they are on and pointing the gate field at one is inert).
/// Removes the only situation the destructive fallback existed for: booting straight into a dungeon
/// with nothing cached.
pub fn prime_known_good(start_graces: &[u32]) -> Option<u32> {
    start_graces.iter().copied().find(|&f| f > 0)
}
