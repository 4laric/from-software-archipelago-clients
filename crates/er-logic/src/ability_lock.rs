//! ability_lock.rs -- the pure half of individual ability locks (SPEC-ability-lock-mode, #945).
//!
//! TEST-BUILD SCOPE: seven abilities, env-driven (`ER_ABILITY_LOCK_TEST="roll,r1,l2"`), no world
//! feature and no slot_data yet -- deliberately the same harness the gamepad build shipped, but
//! on a NEW mechanism.
//!
//! ## The change: act on LOGICAL ACTIONS, not physical buttons
//!
//! The gamepad build masked `XINPUT_GAMEPAD` button bits, which sees only the PHYSICAL pad and
//! only the default map -- so a rebind evaded it, keyboard/mouse were unreachable, and every mask
//! had to gate on a menu-open predicate because menus reuse the same buttons. All three problems
//! are the same problem: the device layer is below keybind resolution.
//!
//! ER resolves every input into `ChrActions` on the player's `CSChrActionRequestModule` -- one bit
//! per logical action (r1, jump, rolling, ...), AFTER the keybind map. Acting there is:
//!   * KEYBIND-AGNOSTIC by construction -- rebind roll to any key or button, it still sets the
//!     `rolling` action bit;
//!   * device-agnostic -- gamepad and keyboard resolve to the same bits, so one path covers both;
//!   * menu-safe with no predicate -- menu navigation does not flow through the character's action
//!     requests, so a persistent disable never locks the player out of their own inventory.
//!
//! ## What this module decides
//!
//! A locked set (`u8`, one bit per [`Ability`]) -> the `ChrActions` bitmask to disable
//! ([`chr_action_mask`]). The client ORs that into the module's `disabled_action_inputs` (the
//! game's OWN disable field) each frame, and reports which locked actions the player just pressed
//! ([`requested_locked`], fed the frame's `new_action_presses`) so the mask has the observable
//! read-back the spec requires.
//!
//! Bit positions are `ChrActions`' own (eldenring crate, `chr_ins::module::action_request`).

/// One bit per lockable ability. `Heal` is absent: its mechanism is the flask-charge clamp
/// (spec 4.1), not an action mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ability {
    Jump,
    Crouch,
    Roll,
    R1,
    R2,
    L1,
    L2,
}

impl Ability {
    pub const ALL: [Ability; 7] = [
        Ability::Jump,
        Ability::Crouch,
        Ability::Roll,
        Ability::R1,
        Ability::R2,
        Ability::L1,
        Ability::L2,
    ];

    /// This module's own one-hot bit, for the `u8` locked-set (NOT a ChrActions bit).
    pub fn bit(self) -> u8 {
        match self {
            Ability::Jump => 1 << 0,
            Ability::Crouch => 1 << 1,
            Ability::Roll => 1 << 2,
            Ability::R1 => 1 << 3,
            Ability::R2 => 1 << 4,
            Ability::L1 => 1 << 5,
            Ability::L2 => 1 << 6,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Ability::Jump => "jump",
            Ability::Crouch => "crouch",
            Ability::Roll => "roll",
            Ability::R1 => "r1",
            Ability::R2 => "r2",
            Ability::L1 => "l1",
            Ability::L2 => "l2",
        }
    }

    /// The `ChrActions` bit(s) this ability owns -- what gets disabled to lock it.
    ///
    /// Casting rides the attack buttons (a staff/seal casts through R1/L1/R2/L2), and at the
    /// action layer those are SEPARATE bits (`magic_*`), so an attack lock takes its magic twin
    /// too or the mode would let a caster ignore it -- exactly the coupling the spec calls out.
    ///
    /// 🛑 `Crouch` is the one unverified map: ER has no dedicated crouch action, so it is routed
    /// to `l3` (the stick-click) as the first guess. If the field test shows l3 is not it, this is
    /// the single line to change.
    pub fn chr_action_mask(self) -> u64 {
        match self {
            Ability::Jump => 1 << JUMP,
            Ability::Crouch => 1 << L3,
            Ability::Roll => (1 << ROLLING) | (1 << BACKSTEP),
            Ability::R1 => (1 << R1) | (1 << MAGIC_R),
            Ability::R2 => (1 << R2) | (1 << MAGIC_R2),
            Ability::L1 => (1 << L1) | (1 << MAGIC_L),
            Ability::L2 => (1 << L2) | (1 << MAGIC_L2),
        }
    }
}

// ---- ChrActions bit positions (eldenring crate, chr_ins::module::action_request::ChrActions) ----
const R1: u64 = 0;
const R2: u64 = 1;
const L1: u64 = 2;
const L2: u64 = 3;
const JUMP: u64 = 6;
const L3: u64 = 13; // stick-click; the crouch guess
const BACKSTEP: u64 = 16;
const ROLLING: u64 = 17;
const MAGIC_R: u64 = 19;
const MAGIC_L: u64 = 20;
const MAGIC_R2: u64 = 33;
const MAGIC_L2: u64 = 34;

/// Parse a lock set like `"roll,r1, l2"` (commas and/or whitespace; case-insensitive). Unknown
/// tokens REFUSE the whole set with the offender named -- a typo silently locking nothing is the
/// config failure this tool cannot have.
pub fn parse_set(s: &str) -> Result<u8, String> {
    let mut set = 0u8;
    for tok in s.split([',', ' ', '\t']).filter(|t| !t.is_empty()) {
        let t = tok.to_ascii_lowercase();
        let a = Ability::ALL
            .into_iter()
            .find(|a| a.name() == t)
            .ok_or_else(|| {
                format!("unknown ability {tok:?} -- valid: jump, crouch, roll, r1, r2, l1, l2")
            })?;
        set |= a.bit();
    }
    Ok(set)
}

pub fn set_names(set: u8) -> String {
    Ability::ALL
        .into_iter()
        .filter(|a| set & a.bit() != 0)
        .map(|a| a.name())
        .collect::<Vec<_>>()
        .join(",")
}

/// The `ChrActions` bitmask to disable for a locked set -- the union of each locked ability's
/// own action bits. `0` when nothing is locked (the client then does nothing).
pub fn chr_action_mask(locked: u8) -> u64 {
    Ability::ALL
        .into_iter()
        .filter(|a| locked & a.bit() != 0)
        .fold(0u64, |m, a| m | a.chr_action_mask())
}

/// Read-back: which locked abilities the player pressed THIS frame, given the module's
/// `new_action_presses` (raw `ChrActions` u64). An ability reports if ANY of its action bits is
/// newly pressed -- the observable proof the mask is doing something, per spec 4.3.
pub fn requested_locked(locked: u8, new_action_presses: u64) -> u8 {
    Ability::ALL
        .into_iter()
        .filter(|a| locked & a.bit() != 0 && a.chr_action_mask() & new_action_presses != 0)
        .fold(0u8, |h, a| h | a.bit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_refuses_a_typo_naming_it() {
        let err = parse_set("roll,r3").unwrap_err();
        assert!(err.contains("r3"), "{err}");
        assert_eq!(parse_set("").unwrap(), 0);
        assert_eq!(
            parse_set("ROLL, r1\tl2").unwrap(),
            Ability::Roll.bit() | Ability::R1.bit() | Ability::L2.bit()
        );
    }

    #[test]
    fn set_names_round_trips() {
        assert_eq!(set_names(parse_set("jump,l1").unwrap()), "jump,l1");
    }

    #[test]
    fn nothing_locked_masks_nothing() {
        assert_eq!(chr_action_mask(0), 0);
        assert_eq!(requested_locked(0, u64::MAX), 0);
    }

    #[test]
    fn attack_locks_take_their_magic_twin() {
        // Locking R1 disables the light attack AND casting through it, or a caster ignores the mode.
        let m = chr_action_mask(Ability::R1.bit());
        assert_ne!(m & (1 << R1), 0);
        assert_ne!(m & (1 << MAGIC_R), 0, "R1 lock must also disable magic_r (casting rides R1)");
        // L2 likewise pairs with the charged/left magic twin.
        let l2 = chr_action_mask(Ability::L2.bit());
        assert_ne!(l2 & (1 << L2), 0);
        assert_ne!(l2 & (1 << MAGIC_L2), 0);
    }

    #[test]
    fn roll_covers_both_the_roll_and_the_neutral_backstep() {
        let m = chr_action_mask(Ability::Roll.bit());
        assert_ne!(m & (1 << ROLLING), 0);
        assert_ne!(m & (1 << BACKSTEP), 0, "a directionless B-tap is a backstep, still an evade");
    }

    #[test]
    fn the_union_is_exactly_the_locked_abilities() {
        let locked = parse_set("jump,roll").unwrap();
        let m = chr_action_mask(locked);
        assert_eq!(m, (1u64 << JUMP) | (1 << ROLLING) | (1 << BACKSTEP));
        // an unlocked ability's bits are absent
        assert_eq!(m & (1 << R1), 0);
    }

    #[test]
    fn read_back_reports_only_locked_and_pressed() {
        let locked = parse_set("r1,r2").unwrap();
        // player pressed R1 (bit 0), an UNLOCKED jump (bit 6), nothing else
        let presses = (1u64 << R1) | (1u64 << JUMP);
        assert_eq!(requested_locked(locked, presses), Ability::R1.bit());
        // casting through the locked R1 (magic_r) also counts as an R1 attempt
        assert_eq!(requested_locked(locked, 1u64 << MAGIC_R), Ability::R1.bit());
        // no locked press -> silent
        assert_eq!(requested_locked(locked, 1u64 << JUMP), 0);
    }
}
