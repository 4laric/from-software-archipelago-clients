//! `no_equip_load`'s roll mode: what the option can mean, and the multiplier each meaning writes.
//!
//! # Why this exists (er-archipelago#548)
//!
//! `no_equip_load` works -- client #150, confirmed in-game 2026-08-11, boblerrr measured max equip
//! load **8951.9**, exactly the 100x written to `equipWeightChangeRate`. It is **light roll only**,
//! and that is too strong. boblerrr, the moment it landed:
//!
//! > **bobler:** but its supposed to be medium roll no ?
//! > **4laric:** nah it's just light roll for now / but we can have medium roll as an option
//! > **bobler:** yeah make sense / light roll would be to op / imagine full heavy armor + light roll
//!
//! He is right: the option as it stands hands you a fast roll in full heavy armour with no
//! trade-off at all, and the equip-load budget stops being a decision.
//!
//! # 🛑 A FIXED MULTIPLIER CANNOT PIN YOU TO MEDIUM ROLL, AND THIS MODULE DOES NOT CLAIM TO
//!
//! Roll weight is a ratio of carried weight to **max** equip load: light under 30%, medium under
//! 70%, heavy under 100%. This feature multiplies the *ceiling*, so it divides that ratio by a
//! constant -- and the result still depends on what the player is actually wearing. #548's table
//! reads as though "medium roll" were one number; it is not. With a 3x ceiling, a player already
//! at 20% lands at 6.7%, which is light. There is no constant that makes every load medium.
//!
//! What a constant CAN do is put a FLOOR under the roll, and that is the honest promise:
//!
//! | vanilla load ratio | vanilla roll | at `MEDIUM` (3x) | at `LIGHT` (100x) |
//! |---|---|---|---|
//! | 0.29 | light | 0.10 light | light |
//! | 0.65 | medium | 0.22 light | light |
//! | 0.95 | heavy | 0.32 **medium** | light |
//! | 1.50 | overloaded | 0.50 **medium** | light |
//! | 2.10 | overloaded | 0.70 heavy | light |
//!
//! So `MEDIUM` means **"never worse than medium roll"**, up to 2.1x your own ceiling -- which is
//! past any kit that exists. Full heavy armour stops being punished; a light kit still rolls light,
//! exactly as it does in vanilla; and over-equipping past double your ceiling still costs you
//! something. That preserves the decision bobler was worried about losing, which the fixed
//! `LIGHT` multiplier removes entirely.
//!
//! # 🛑 `true` HAS MEANT LIGHT SINCE THE FEATURE SHIPPED, AND MUST KEEP MEANING IT
//!
//! The apworld echoes options as INTS (`core.py::_options_echo::_opt` returns `int(o.value)`), so
//! the old `Toggle` puts `1` on the wire for on. That forces the numbering here:
//!
//! ```text
//! 0 = off      (Toggle false, and the default)
//! 1 = light    (Toggle true -- UNCHANGED MEANING)
//! 2 = medium   (new; only a new yaml can produce it)
//! ```
//!
//! Numbering `medium` as 1 -- the obvious ordering -- would silently convert every yaml in the
//! wild from light roll to medium roll. Narrowing or re-typing a live option is a compat break for
//! every seed already rolled, the same discipline as `confine_foreign_progression`'s `from_any`
//! bool catch (world #513).
//!
//! # 🛑 AND IT NEEDS ITS OWN CLIENT FEATURE TAG
//!
//! `no_equip_load` deliberately ships with NO `requiresClientFeatures` tag, because the capability
//! is older than every client in circulation (`features/body_tuning.py` spells this out). That
//! reasoning covers `off` and `light` and stops dead at `medium`: an old client reading `2` through
//! [`parse_bool_option`] sees a nonzero and gives LIGHT -- the player asked for medium, got the
//! strongest possible setting, and nothing said so. That is the exact shape of #536 (`DECLARED IS
//! NOT EMITTED`) and of the `auto_equip`/spells hazard in #440.
//!
//! So `no_equip_load_roll` is in [`crate::client_features::SUPPORTED`] as of this change, and the
//! apworld must declare it **only** for a seed that asks for `medium`. `body_tuning.py`'s own
//! docstring anticipated this: *"If a future change makes the behaviour version-sensitive, the tag
//! is the right instrument then."* It is now.

use serde_json::Value;

/// What `options.no_equip_load` asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollMode {
    /// Feature off. The row is never patched and nothing is applied to the player.
    Off,
    /// Never worse than medium roll. See the module doc for why that is the honest phrasing.
    Medium,
    /// Always light roll, whatever you are wearing. What `true` has meant since the feature
    /// shipped.
    Light,
}

/// Wire value for [`RollMode::Off`].
pub const WIRE_OFF: i64 = 0;
/// Wire value for [`RollMode::Light`] -- **also what the legacy `Toggle`'s `true` puts on the
/// wire**, which is the whole reason light is 1 and medium is 2.
pub const WIRE_LIGHT: i64 = 1;
/// Wire value for [`RollMode::Medium`].
pub const WIRE_MEDIUM: i64 = 2;

impl RollMode {
    /// Multiplier written to `equipWeightChangeRate`.
    ///
    /// ⚠️ `MEDIUM`'s 3.0 IS PROVISIONAL AND THE READBACK IS HOW IT GETS TUNED. #548 is explicit
    /// that the honest way to pick it is one playtest in full heavy armour reading the
    /// `no_equip_load: WORKING -- max equip load X -> Y (ratio Z)` line the module prints. The
    /// arithmetic above says 3.0 covers over-equipping to 2.1x the player's own ceiling; that is
    /// past any real kit, but "past any real kit" is a claim about kits, not a measurement.
    ///
    /// `LIGHT`'s 100.0 is not provisional -- it is measured. boblerrr read 8951.9 against a
    /// vanilla 89.5.
    pub fn multiplier(self) -> f32 {
        match self {
            RollMode::Off => 1.0,
            RollMode::Medium => 3.0,
            RollMode::Light => 100.0,
        }
    }

    /// Is the feature doing anything at all?
    pub fn is_on(self) -> bool {
        !matches!(self, RollMode::Off)
    }

    /// Short label for logs. ASCII, lowercase, matches the yaml spelling.
    pub fn label(self) -> &'static str {
        match self {
            RollMode::Off => "off",
            RollMode::Medium => "medium",
            RollMode::Light => "light",
        }
    }
}

/// Outcome of reading `options.no_equip_load`, keeping "I did not understand that" apart from
/// "the player turned it off".
///
/// 🛑 The two are NOT the same thing and must not collapse. A value this build does not recognise
/// means the apworld is newer than this client, and the honest response is to degrade AND SAY SO --
/// "absence of behaviour is indistinguishable from feature turned off" is the failure the whole
/// handshake exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parsed {
    pub mode: RollMode,
    /// `Some(v)` when the wire carried a value this build cannot name; `mode` has been degraded to
    /// [`RollMode::Off`] and the caller must log it.
    pub unrecognised: Option<i64>,
}

impl Parsed {
    fn known(mode: RollMode) -> Self {
        Self {
            mode,
            unrecognised: None,
        }
    }
}

/// Read `options.no_equip_load` as a roll mode.
///
/// Accepts every shape the wire has ever carried:
///
/// * absent, `false`, `0` -> [`RollMode::Off`]
/// * `true`, `1` -> [`RollMode::Light`] (the legacy `Toggle`'s meaning, preserved)
/// * `2` -> [`RollMode::Medium`]
/// * anything else -> `Off`, with `unrecognised` set so the caller can say what it saw
///
/// Degrading an unrecognised value to `Off` rather than to a guess follows the
/// `dlc_blessing_catchup` precedent, where a build without the mode-3 arm clamps an unrecognised 3
/// to 0. In practice it should be unreachable: a seed asking for a mode this build does not have
/// declares a `requiresClientFeatures` tag this build does not have, so connect REFUSES before any
/// of this runs. It is the belt to the handshake's braces.
pub fn parse(slot_data: &Value) -> Parsed {
    let Some(v) = slot_data
        .get("options")
        .and_then(|o| o.get("no_equip_load"))
    else {
        return Parsed::known(RollMode::Off);
    };
    let raw: i64 = match v {
        // A bool is the pre-int form and the fswap/Bedrock form. `true` is LIGHT, always.
        Value::Bool(true) => WIRE_LIGHT,
        Value::Bool(false) => WIRE_OFF,
        Value::Number(n) => match n.as_i64() {
            Some(i) => i,
            // A non-integer number is garbage, not a mode. Treated like any unknown.
            None => return Parsed::known(RollMode::Off),
        },
        // A string or an object is not something this key has ever carried.
        _ => return Parsed::known(RollMode::Off),
    };
    match raw {
        WIRE_OFF => Parsed::known(RollMode::Off),
        WIRE_LIGHT => Parsed::known(RollMode::Light),
        WIRE_MEDIUM => Parsed::known(RollMode::Medium),
        other => Parsed {
            mode: RollMode::Off,
            unrecognised: Some(other),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sd(v: Value) -> Value {
        json!({ "options": { "no_equip_load": v } })
    }

    // -------------------------------------------------------------------------------------------
    // 🛑 THE COMPAT RULE. Every yaml in the wild says `no_equip_load: true` and has meant LIGHT
    // since the feature shipped. If this test ever goes red, every existing seed silently changed
    // difficulty.
    // -------------------------------------------------------------------------------------------

    #[test]
    fn true_and_one_still_mean_light_roll() {
        assert_eq!(parse(&sd(json!(true))).mode, RollMode::Light);
        assert_eq!(
            parse(&sd(json!(1))).mode,
            RollMode::Light,
            "the apworld echoes options as INTS, so the legacy Toggle's `true` arrives as 1 -- \
             numbering medium as 1 would convert every yaml in the wild"
        );
    }

    #[test]
    fn off_is_off_in_every_shape_including_absent() {
        assert_eq!(parse(&sd(json!(false))).mode, RollMode::Off);
        assert_eq!(parse(&sd(json!(0))).mode, RollMode::Off);
        assert_eq!(parse(&json!({ "options": {} })).mode, RollMode::Off);
        assert_eq!(parse(&json!({})).mode, RollMode::Off);
        assert_eq!(parse(&Value::Null).mode, RollMode::Off);
    }

    #[test]
    fn two_is_the_new_medium() {
        let p = parse(&sd(json!(2)));
        assert_eq!(p.mode, RollMode::Medium);
        assert_eq!(p.unrecognised, None);
    }

    #[test]
    fn an_unknown_mode_degrades_to_off_and_says_what_it_saw() {
        let p = parse(&sd(json!(7)));
        assert_eq!(
            p.mode,
            RollMode::Off,
            "an unrecognised mode must not be guessed at -- the dlc_blessing_catchup precedent \
             clamps to off"
        );
        assert_eq!(
            p.unrecognised,
            Some(7),
            "and it must be RECOVERABLE by the caller, or the degrade is indistinguishable from \
             the player turning the option off"
        );
    }

    #[test]
    fn garbage_shapes_are_off_and_are_not_reported_as_unknown_modes() {
        // A string or an object was never a valid shape for this key; that is a malformed seed,
        // not a newer apworld, and reporting it as an unrecognised MODE would misdescribe it.
        for v in [json!("medium"), json!({}), json!([2])] {
            let p = parse(&sd(v.clone()));
            assert_eq!(p.mode, RollMode::Off, "{v:?}");
            assert_eq!(p.unrecognised, None, "{v:?}");
        }
    }

    // -------------------------------------------------------------------------------------------
    // The numbers, and the promise they make.
    // -------------------------------------------------------------------------------------------

    #[test]
    fn off_writes_the_identity_multiplier() {
        assert_eq!(
            RollMode::Off.multiplier(),
            1.0,
            "vanilla equipWeightChangeRate is 1 on 11293 of 11325 rows -- off must be the field's \
             own no-op value, so a stray write is inert rather than subtly wrong"
        );
    }

    #[test]
    fn light_is_the_measured_hundred() {
        // boblerrr, 2026-08-11: max equip load read 8951.9, exactly 100x the vanilla 89.5.
        assert_eq!(RollMode::Light.multiplier(), 100.0);
    }

    /// THE MOTIVATING CASE (rule 11), in arithmetic: full heavy armour must not roll light, and a
    /// light kit must not be dragged UP into medium.
    #[test]
    fn medium_floors_a_heavy_kit_at_medium_without_touching_a_light_one() {
        let m = RollMode::Medium.multiplier();
        // Roll boundaries, as ratios of carried weight to max equip load.
        const LIGHT_MAX: f32 = 0.30;
        const MEDIUM_MAX: f32 = 0.70;

        // Full heavy armour: vanilla ratio 0.95 (heavy roll, the thing bobler is complaining is
        // punishing). Under `medium` it must land in the medium band -- not light.
        let heavy = 0.95 / m;
        assert!(
            heavy >= LIGHT_MAX && heavy < MEDIUM_MAX,
            "a 0.95 kit must land in MEDIUM, got {heavy}"
        );

        // Over-equipped to 1.5x the ceiling: still medium, still no free light roll.
        let overloaded = 1.50 / m;
        assert!(
            overloaded >= LIGHT_MAX && overloaded < MEDIUM_MAX,
            "a 1.50 kit must still be medium, got {overloaded}"
        );

        // A light kit is NOT dragged up. This is the half the option must not break: vanilla
        // behaviour for anyone who was already rolling light.
        let light_kit = 0.25 / m;
        assert!(
            light_kit < LIGHT_MAX,
            "a light kit must stay light, got {light_kit}"
        );

        // And the ceiling of the promise, stated so it cannot drift silently: past ~2.1x your own
        // equip load you drop out of medium again.
        let way_over = 2.20 / m;
        assert!(
            way_over >= MEDIUM_MAX,
            "the promise is bounded, and the bound is ~2.1x -- got {way_over}"
        );
    }

    #[test]
    fn light_makes_even_an_absurd_load_light() {
        let l = RollMode::Light.multiplier();
        assert!(
            3.0 / l < 0.30,
            "light must mean light at any load -- that is what it has always meant"
        );
    }

    #[test]
    fn modes_are_ordered_by_strength_and_labels_are_ascii() {
        assert!(RollMode::Medium.multiplier() < RollMode::Light.multiplier());
        assert!(RollMode::Off.multiplier() < RollMode::Medium.multiplier());
        assert!(!RollMode::Off.is_on());
        assert!(RollMode::Medium.is_on());
        assert!(RollMode::Light.is_on());
        for m in [RollMode::Off, RollMode::Medium, RollMode::Light] {
            assert!(m.label().is_ascii());
        }
    }

    #[test]
    fn the_wire_constants_and_the_parser_cannot_drift() {
        assert_eq!(parse(&sd(json!(WIRE_OFF))).mode, RollMode::Off);
        assert_eq!(parse(&sd(json!(WIRE_LIGHT))).mode, RollMode::Light);
        assert_eq!(parse(&sd(json!(WIRE_MEDIUM))).mode, RollMode::Medium);
    }
}
