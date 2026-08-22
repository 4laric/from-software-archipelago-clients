//! ability_lock.rs — the pure half of individual ability locks (SPEC-ability-lock-mode §4.3,
//! er-archipelago#945). TEST-BUILD SCOPE: the seven input-masked abilities, gamepad only,
//! driven by an env var — no world feature, no slot_data, deliberately not blocked on the
//! SpEffect-9621 field test (the blanket path layers on later, if it proves out).
//!
//! ## The decision, in one sentence
//!
//! Mask a locked ability's physical button only while the game is IN GAMEPLAY — which the
//! 2026-08-21 probe run defined measurably: `ChrMenuFlags` reads 1 in gameplay and sets bit 2
//! or bit 3 in every observed menu state (non-pause menus 5/7, pause family 9/11/13/15, map
//! 25/27; 4,389 change-logged samples, so unseen values are evidence of absence) — and no NPC
//! conversation is active (the esd talk clock; B is "back" in dialogue).
//!
//! ## What a "mask" is
//!
//! `XINPUT_GAMEPAD` edits, applied by the existing `input.rs` hook after its whole-device
//! block: clear button bits for digital inputs, zero a trigger byte for R2/L2 (triggers are
//! analog bytes, not button bits). ER's default pad map, which is what the input layer sees:
//! A = jump, B = dodge (B is also backstep and, held, sprint — at the input layer those are
//! ONE ability and the option docs must say so), L3 = crouch, RB/LB = R1/L1, RT/LT = R2/L2.
//!
//! ## Known limits, stated once
//!
//! * GAMEPAD ONLY in this build. Keyboard/mouse masking needs the player's live binds (ER's KBM
//!   attacks are mouse buttons and shift-chords); shipping a wrong guess is worse than shipping
//!   a documented gap.
//! * Input masking sees PHYSICAL buttons: a rebind evades it. Accepted for v1 — the mode is
//!   self-imposed difficulty (spec §4.3's rebinding caveat).

/// One bit per input-masked ability. `Heal` is deliberately absent: its mechanism is the
/// flask-charge clamp (spec §4.1), not an input mask.
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
}

/// Parse a lock set like `"roll,r1, l2"` (commas and/or whitespace; case-insensitive).
/// Unknown tokens REFUSE the whole set with the offender named — a typo silently locking
/// nothing is the config failure this tool cannot have.
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
    let names: Vec<&str> = Ability::ALL
        .into_iter()
        .filter(|a| set & a.bit() != 0)
        .map(|a| a.name())
        .collect();
    names.join(",")
}

// XInput button-word bits (XINPUT_GAMEPAD wButtons).
pub const BTN_A: u16 = 0x1000; // jump
pub const BTN_B: u16 = 0x2000; // dodge / backstep / sprint
pub const BTN_LS: u16 = 0x0040; // L3: crouch
pub const BTN_RB: u16 = 0x0200; // R1
pub const BTN_LB: u16 = 0x0100; // L1

/// The menu predicate the probe measured: gameplay reads 1; every menu state sets bit 2 or
/// bit 3. Masking must stop the moment either bit is up — RB/LB tab through menus and B is
/// "back", and locking a player out of their own equipment screen is the failure mode the
/// spec's §4.2 is entirely about.
pub fn in_gameplay(chr_menu_flags: u32) -> bool {
    chr_menu_flags & 0b1100 == 0
}

/// What the XInput hook should edit this frame. Empty (`is_empty()`) when nothing is locked or
/// the context is not gameplay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GamepadMask {
    pub clear_buttons: u16,
    pub zero_left_trigger: bool,
    pub zero_right_trigger: bool,
}

impl GamepadMask {
    pub fn is_empty(&self) -> bool {
        self.clear_buttons == 0 && !self.zero_left_trigger && !self.zero_right_trigger
    }
}

/// The whole decision, pure: locked set + menu flags + the talk gate -> the mask to apply.
/// `talk_quiet` is `esd_probe::inventory_grants_safe()` — false while an NPC conversation is
/// live, during which NOTHING is masked (B is "back" in dialogue; conversations are brief and
/// no ability cheese fits inside one).
pub fn gamepad_mask(locked: u8, chr_menu_flags: u32, talk_quiet: bool) -> GamepadMask {
    if locked == 0 || !talk_quiet || !in_gameplay(chr_menu_flags) {
        return GamepadMask::default();
    }
    let mut m = GamepadMask::default();
    if locked & Ability::Jump.bit() != 0 {
        m.clear_buttons |= BTN_A;
    }
    if locked & Ability::Roll.bit() != 0 {
        m.clear_buttons |= BTN_B;
    }
    if locked & Ability::Crouch.bit() != 0 {
        m.clear_buttons |= BTN_LS;
    }
    if locked & Ability::R1.bit() != 0 {
        m.clear_buttons |= BTN_RB;
    }
    if locked & Ability::L1.bit() != 0 {
        m.clear_buttons |= BTN_LB;
    }
    if locked & Ability::R2.bit() != 0 {
        m.zero_right_trigger = true;
    }
    if locked & Ability::L2.bit() != 0 {
        m.zero_left_trigger = true;
    }
    m
}

/// Which locked abilities the player just tried to use — the read-back the spec requires
/// (`§4.3: every mechanism ships with an observable read-back ... or it does not ship`). Fed
/// with the PRE-mask state; returns the abilities whose inputs were active and masked.
pub fn suppressed(locked: u8, mask: GamepadMask, buttons: u16, lt: u8, rt: u8) -> u8 {
    let mut hit = 0u8;
    if mask.clear_buttons & BTN_A & buttons != 0 {
        hit |= Ability::Jump.bit();
    }
    if mask.clear_buttons & BTN_B & buttons != 0 {
        hit |= Ability::Roll.bit();
    }
    if mask.clear_buttons & BTN_LS & buttons != 0 {
        hit |= Ability::Crouch.bit();
    }
    if mask.clear_buttons & BTN_RB & buttons != 0 {
        hit |= Ability::R1.bit();
    }
    if mask.clear_buttons & BTN_LB & buttons != 0 {
        hit |= Ability::L1.bit();
    }
    // triggers count as "pressed" past XInput's own threshold (30): idle drift must not log
    if mask.zero_right_trigger && rt > 30 {
        hit |= Ability::R2.bit();
    }
    if mask.zero_left_trigger && lt > 30 {
        hit |= Ability::L2.bit();
    }
    hit & locked
}

#[cfg(test)]
mod tests {
    use super::*;

    // The nine ChrMenuFlags values the 2026-08-21 probe run observed, by context.
    const GAMEPLAY: u32 = 1;
    const MENUS: [u32; 8] = [5, 7, 9, 11, 13, 15, 25, 27];

    #[test]
    fn the_measured_menu_states_all_suppress_masking() {
        let locked = parse_set("jump,crouch,roll,r1,r2,l1,l2").unwrap();
        for f in MENUS {
            assert!(
                gamepad_mask(locked, f, true).is_empty(),
                "flags {f} is a MENU state (probe-measured); masking there locks the player \
                 out of their own inventory"
            );
        }
    }

    #[test]
    fn gameplay_masks_exactly_the_locked_set() {
        let locked = parse_set("roll,r2").unwrap();
        let m = gamepad_mask(locked, GAMEPLAY, true);
        assert_eq!(m.clear_buttons, BTN_B);
        assert!(m.zero_right_trigger);
        assert!(!m.zero_left_trigger);
    }

    #[test]
    fn dialogue_suppresses_everything() {
        // B is "back" in an NPC conversation; the talk gate wins over gameplay flags.
        let locked = parse_set("roll").unwrap();
        assert!(gamepad_mask(locked, GAMEPLAY, false).is_empty());
    }

    #[test]
    fn nothing_locked_is_always_empty() {
        assert!(gamepad_mask(0, GAMEPLAY, true).is_empty());
    }

    #[test]
    fn parse_refuses_a_typo_naming_it() {
        let err = parse_set("roll,r3").unwrap_err();
        assert!(err.contains("r3"), "{err}");
        assert!(parse_set("").unwrap() == 0);
        assert_eq!(
            parse_set("ROLL, r1\tl2").unwrap(),
            Ability::Roll.bit() | Ability::R1.bit() | Ability::L2.bit()
        );
    }

    #[test]
    fn suppressed_reports_only_masked_and_pressed() {
        let locked = parse_set("r1,r2").unwrap();
        let m = gamepad_mask(locked, GAMEPLAY, true);
        // R1 held + full RT pull + an UNLOCKED jump press: only the locked pair reports.
        let hit = suppressed(locked, m, BTN_RB | BTN_A, 0, 255);
        assert_eq!(hit, Ability::R1.bit() | Ability::R2.bit());
        // idle trigger drift under XInput's threshold stays silent
        assert_eq!(suppressed(locked, m, 0, 0, 20), 0);
    }

    #[test]
    fn set_names_round_trips() {
        let set = parse_set("jump,l1").unwrap();
        assert_eq!(set_names(set), "jump,l1");
    }
}
