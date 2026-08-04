//! Auto-equip decision logic (pure).
//!
//! When the `auto_equip` option is on, a received WEAPON, PROTECTOR or TALISMAN is equipped
//! immediately --
//! including mid-boss-fight. That is deliberate: the motivating case is the French Challenge
//! (Wretch start + randomizer + Use What You Get + permadeath + all bosses + region lock), whose
//! whole premise is that you do not choose your build. Clobbering the weapon in your hand at the
//! worst possible moment IS the feature, not a bug to guard against.
//!
//! Everything here is host-testable and holds no game state. The caller supplies the game-side
//! facts we cannot read without the game: `EQUIP_PARAM_WEAPON_ST.wep_type`,
//! `EQUIP_PARAM_PROTECTOR_ST.protectorCategory`, and -- for talismans --
//! `PlayerGameData.unlocked_talisman_slots` plus what the four accessory slots currently hold.

/// Category nibble of a FullID (`(category << 28) | row`), matching the encoding used across the
/// client. Source is `eldenring::cs::ItemCategory`: `Weapon = 0`, `Protector = 1`, `Accessory = 2`,
/// `Goods = 4`, `Gem = 8`.
const CATEGORY_MASK: u32 = 0xF000_0000;
const CATEGORY_WEAPON: u32 = 0x0000_0000;
const CATEGORY_PROTECTOR: u32 = 0x1000_0000;
const CATEGORY_ACCESSORY: u32 = 0x2000_0000;

/// `ChrAsmSlot` indices we can target. The full enum is 0..=21; these are the ten that auto-equip
/// ever writes. Confirmed against a live `chr_asm` dump on 2.6.2.0: slots 0/2/3/4/5 held unarmed
/// (`110000`) for the five idle hand slots, and 12..=15 held protector entries `0x10002710`,
/// `0x10002774`, `0x100027D8`, `0x1000283C` -- head, chest, hands, legs, in that order.
///
/// The accessory indices are read off the `ChrAsmSlot` enum in the PINNED `eldenring` crate
/// (`fromsoftware-rs` `8c67a84`, `crates/eldenring/src/cs/player_game_data.rs`), not inferred:
/// `Accessory1 = 17 .. Accessory4 = 20`. `Unused16 = 16` sits between the protectors and them --
/// which is exactly why these are NOT `15 + n`.
pub const SLOT_WEAPON_LEFT_1: u32 = 0;
pub const SLOT_WEAPON_RIGHT_1: u32 = 1;
pub const SLOT_PROTECTOR_HEAD: u32 = 12;
pub const SLOT_PROTECTOR_CHEST: u32 = 13;
pub const SLOT_PROTECTOR_HANDS: u32 = 14;
pub const SLOT_PROTECTOR_LEGS: u32 = 15;
pub const SLOT_ACCESSORY_1: u32 = 17;
pub const SLOT_ACCESSORY_2: u32 = 18;
pub const SLOT_ACCESSORY_3: u32 = 19;
pub const SLOT_ACCESSORY_4: u32 = 20;

/// The four talisman slots in unlock order. Index `i` is unlocked once the player has `i + 1`
/// talisman slots.
///
/// 🛑 `ChrAsmSlot::AccessoryCovenant = 21` is DELIBERATELY ABSENT and must never be added here.
/// It is the Great Rune slot, not a fifth talisman slot. Nothing in this module can reach it:
/// every accessory write indexes THIS array. That structural exclusion is the guard -- there is
/// no `accessoryCategory` check below, because a value-based guard would need a mapping this
/// repo has not datamined, and inventing one is the mistake `wep_type 59` already cost us.
///
/// It is also unreachable from the item pool: the apworld ships Great Runes as GOODS, not
/// accessories. `greenfield/eldenring/item_ids.py` on world `main` has all six as nibble 4
/// (`Godrick's Great Rune` = `1073742015`, rows 191..196), while all 147 nibble-2 entries are
/// talismans (rows 1000..8240). Both facts have to break before slot 21 is in play.
pub const ACCESSORY_SLOTS: [u32; 4] = [
    SLOT_ACCESSORY_1,
    SLOT_ACCESSORY_2,
    SLOT_ACCESSORY_3,
    SLOT_ACCESSORY_4,
];

/// Which primary hand a received weapon should occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hand {
    Left,
    Right,
}

/// What kind of thing a received FullID is, and therefore which param table the caller must read
/// to resolve the target slot. Anything else (goods, gems) is not auto-equipped.
///
/// 🛑 GOODS stays out on purpose, and that is the OTHER half of #295: physick tears are
/// `EQUIP_PARAM_GOODS` and do not live in `chr_asm` at all -- they go into the Flask of Wondrous
/// Physick, a separate two-slot mixture with its own persistence. None of the four-rep commit
/// this module is built on applies to them. That half is split out rather than half-built here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Equipable {
    /// Read `EQUIP_PARAM_WEAPON_ST.wep_type`, then [`slot_for_wep_type`].
    Weapon,
    /// Read `EQUIP_PARAM_PROTECTOR_ST.protectorCategory`, then [`slot_for_protector_category`].
    Protector,
    /// A TALISMAN. Read `PlayerGameData.unlocked_talisman_slots` and the current occupants of
    /// [`ACCESSORY_SLOTS`], then [`slot_for_accessory`].
    Accessory,
}

/// Is this received FullID something auto-equip handles at all?
pub fn equipable(full_id: i32) -> Option<Equipable> {
    match (full_id as u32) & CATEGORY_MASK {
        CATEGORY_WEAPON => Some(Equipable::Weapon),
        CATEGORY_PROTECTOR => Some(Equipable::Protector),
        CATEGORY_ACCESSORY => Some(Equipable::Accessory),
        _ => None,
    }
}

/// Is this received FullID a weapon? Retained for callers that only care about the weapon case.
pub fn is_weapon(full_id: i32) -> bool {
    (full_id as u32) & CATEGORY_MASK == CATEGORY_WEAPON
}

/// `EQUIP_PARAM_WEAPON_ST.wep_type` values that auto-equip to the LEFT hand.
///
/// THE RULESET (Alaric's call, 2026-08-03): follow the community "French Challenge" format --
/// randomizer + auto-equip + permadeath + all bosses + region lock. Its equipment rule reads:
///
///   "Weapons auto-equip in right hand; shields/staves/seals/bows/crossbows in left hand."
///   "All auto-equipped gear must stay equipped ... No unequipping allowed."
///
/// That rules out the two alternatives considered for #301 -- never equipping catalysts, or
/// equipping them only when the player has a castable spell. The format's whole point is that you
/// do not get to opt out of what you are given; the fix is WHERE it lands, not WHETHER.
///
/// 🛑 SOURCING, stated because it is thinner than the rest of this file. The left-hand clause rests
/// on a SINGLE written transcription (a community playlist description). There is no French
/// primary document -- the rules circulate as Twitch chat commands (`!french`). The reference
/// implementation, LukeYui's item randomiser, documents only "shields will always go in your
/// left hand slot 1" -- shields ONLY -- which is exactly why staves and seals landed in the
/// main hand and disarmed the player (#301). The community rule is broader than the mod.
///
/// DATAMINE-CONFIRMED 2026-08-03 against `EquipParamWeapon` joined to `WeaponName.fmg` (base + both
/// DLC msgbnds). Named-row counts in brackets:
///
///   shields    65 Small [197] · 67 Medium [339] · 69 Greatshield [230] -- exactly the
///              shield population: no shield outside these three, nothing else inside
///   catalysts  57 Glintstone Staff [19] · 61 Sacred Seal [10]           -- #301
///   bows       50 Light Bow [5] · 51 Bow [7] · 53 Greatbow [4]
///   crossbows  55 Crossbow [8] · 56 Ballista [2]  -- Hand Ballista, Jar Cannon
///
/// Bows and crossbows are in the ruleset and are NOT a reported bug: they have the identical
/// "received item disarms you in melee" failure mode as catalysts, and following the ruleset
/// pre-empts it rather than waiting for the report. 56 (Ballista) is a judgement call -- the rule
/// says "crossbows" and a Hand Ballista is a two-handed siege crossbow; it is grouped here.
///
/// TORCH (87) STAYS RIGHT. The ruleset mentions a torch exactly once, as a narrow exception --
/// "you may equip the Sentinel Torch (left-hand slot 2) only to fight the Invisible Black Knife
/// Assassin" -- which is a permission during a fight, not an auto-equip rule. Left where it was.
///
/// ⚠️ ONE SLOT, LAST WRITER WINS. There is a single `SLOT_WEAPON_LEFT_1`, so a received staff
/// clobbers a received shield. That is faithful to "no unequipping allowed" and is not a bug here.
/// The ruleset's torch clause shows a left-hand slot 2 exists in their model, but its `ChrAsmSlot`
/// index is NOT verified and this file will not guess one (same reason ammo returns `None`).
///
/// (Datamine-confirmed is still not runtime-confirmed -- no live equip has been read back.)
const LEFT_HAND_WEP_TYPES: &[u16] = &[50, 51, 53, 55, 56, 57, 61, 65, 67, 69];

/// `wep_type` values that are AMMUNITION, not a held weapon.
///
/// DATAMINE-CONFIRMED 2026-08-03, same join: 81 = Arrow (35 named rows), 83 = Great Arrow (8),
/// 85 = Bolt (25), 86 = Ballista Bolt (5).
///
/// MOTIVATING CASE (rule 11): boblerrr's 2026-08-03 log, client `0.3.1 (f2ef85d3c920)` --
///   `auto_equip: slot 1 <- 0x031aad80 (param 52080000 ...)`
/// Param 52080000 is `wep_type` 85, "Lordsworn's Bolt", written to `SLOT_WEAPON_RIGHT_1`. Ammo is
/// `CATEGORY_WEAPON`, so it reached `hand_for_wep_type`, which had no arm for it and fell through
/// to RIGHT -- putting a crossbow bolt in the player's main hand and disarming them. The bug is an
/// UNHANDLED CLASS, not a mis-slot (#294).
pub const AMMO_WEP_TYPES: &[u16] = &[81, 83, 85, 86];

/// Is this `wep_type` ammunition?
pub fn is_ammo(wep_type: u16) -> bool {
    AMMO_WEP_TYPES.contains(&wep_type)
}

/// The primary hand a weapon of this `wep_type` should occupy. Shields -> LEFT; everything else ->
/// RIGHT (the main-hand slot).
pub fn hand_for_wep_type(wep_type: u16) -> Hand {
    if LEFT_HAND_WEP_TYPES.contains(&wep_type) {
        Hand::Left
    } else {
        Hand::Right
    }
}

/// The `ChrAsmSlot` index a weapon of this `wep_type` should occupy, or `None` when it does not
/// belong in a hand at all.
///
/// Returns `None` for AMMUNITION. 🛑 Deliberately `None` rather than "the quiver slot": the arrow
/// and bolt `ChrAsmSlot` indices are NOT verified here, and inventing one would write a live
/// character's equipment array from a guessed offset. Not equipping ammo is strictly better than
/// main-handing it -- the player keeps their weapon and the ammo sits in the bag exactly as it
/// would with the feature off. Routing ammo to its real quiver slot is a follow-up that needs
/// those indices read out of the pinned crate source first.
///
/// Mirrors [`slot_for_protector_category`], which has returned `Option` for the same reason since
/// the dummy-protector category was found.
pub fn slot_for_wep_type(wep_type: u16) -> Option<u32> {
    if is_ammo(wep_type) {
        return None;
    }
    Some(match hand_for_wep_type(wep_type) {
        Hand::Left => SLOT_WEAPON_LEFT_1,
        Hand::Right => SLOT_WEAPON_RIGHT_1,
    })
}

/// The `ChrAsmSlot` index for a protector, from `EQUIP_PARAM_PROTECTOR_ST.protectorCategory`.
///
/// Read the game's own field; do NOT infer the slot from the id. The folklore convention
/// `(id / 100) % 10` disagrees with `protectorCategory` on **44 of the 820 vanilla rows**, so it
/// would mis-slot 44 pieces. Category `4` is dummy data -- 41 rows, ids 1000..2100, no names, all
/// four of `headEquip`/`bodyEquip`/`armEquip`/`legEquip` clear -- and returns `None` so nothing
/// is equipped.
///
/// Known wrinkle, deliberately left alone: 11 rows in the 193xxxx..200xxxx range carry a category
/// of 1 or 3 while setting `headEquip`. `protectorCategory` wins here because it is the field the
/// game itself slots by; if one of those ever lands in the wrong slot the failure is benign and
/// the fix is a small override list.
pub fn slot_for_protector_category(protector_category: u8) -> Option<u32> {
    match protector_category {
        0 => Some(SLOT_PROTECTOR_HEAD),
        1 => Some(SLOT_PROTECTOR_CHEST),
        2 => Some(SLOT_PROTECTOR_HANDS),
        3 => Some(SLOT_PROTECTOR_LEGS),
        _ => None,
    }
}

/// How many of the four talisman slots the player has actually unlocked.
///
/// `raw` is `PlayerGameData.unlocked_talisman_slots` read straight off the live character -- we do
/// NOT track Talisman Pouch pickups ourselves, because the game already tracks them and a mod that
/// counts pouches independently is a second source of truth waiting to disagree.
///
/// ⭐ MEASURED 2026-08-03: THE FIELD COUNTS POUCHES, NOT SLOTS -- hence the `+ 1`.
///
/// It shipped clamped to `1..=4` with the zero point explicitly unverified, and the client logged
/// the raw value on every accessory equip so the first real session would settle it. It did, on the
/// first try. Alaric's log, client 0.3.2, six talisman equips:
///
///   auto_equip: talisman 0x20000fc8 -> slot 17 (unlocked_talisman_slots raw=0, worn=[Some(5050), ...])
///
/// **`raw=0` on a character who demonstrably has a working slot 1.** Under the "unlocked slots"
/// reading that field would have to be `1`. So it is a count of EXTRA slots earned -- i.e. Talisman
/// Pouches -- and the usable count is `raw + 1`.
///
/// What the old clamp cost: from the FIRST pouch onward it under-counted by one, so a player with
/// two slots only ever used one and a fully-upgraded player used three of four. Never an illegal
/// write -- the failure direction the clamp was chosen for held -- just a wasted slot.
///
/// ⚠️ ONE data point, at `raw = 0`. It is decisive against "slots" (a working slot cannot coexist
/// with a count of zero) but a reading at 1+ pouches would confirm `+ 1` directly rather than by
/// elimination. If that reading ever contradicts this, the log line is still there to catch it.
///
/// The clamp stays, and both ends still earn their keep:
///
/// * lower bound 1 -- every character has slot 1 from the first minute;
/// * upper bound 4 -- `ChrAsmSlot` 21 is the GREAT RUNE slot, and three pouches is the vanilla
///   maximum, so a larger value is a modded or garbage read and must not widen the range.
pub fn usable_accessory_slots(raw: u8) -> usize {
    (raw as usize + 1).clamp(1, ACCESSORY_SLOTS.len())
}

/// The `ChrAsmSlot` index a received TALISMAN should occupy, or `None` when there is nothing to do.
///
/// `slots[i]` is the accessory param row currently in `ACCESSORY_SLOTS[i]`, or `None` if that slot
/// is empty. The caller decides "empty" by asking the param table, not by testing a sentinel: the
/// empty-accessory value in `equipment_param_ids` is not verified here, so a slot counts as empty
/// whenever its entry does not resolve to an `EQUIP_PARAM_ACCESSORY_ST` row. That is correct for
/// `-1`, for `0`, and for any other sentinel FromSoft might use, without this file guessing which.
///
/// THE POLICY, and where it comes from. #295 called the choice of slot "a design decision, not an
/// implementation detail". It is -- but it is a decision the French Challenge ruling already made
/// (see [`LEFT_HAND_WEP_TYPES`]), so this is that ruling applied, not a new one:
///
/// 1. **Already worn -> `None`.** Not an optimisation: ER refuses duplicate talismans, so writing
///    the same row into a second slot builds a loadout the player could not have made in the menu.
/// 2. **First EMPTY unlocked slot.** Fills 1, 2, 3, 4 as they come, so a player who has never
///    touched the menu ends up with the four most recent talismans rather than one.
/// 3. **All unlocked slots full -> alternate**, `ordinal % n`. Last writer wins -- "all auto-
///    equipped gear must stay equipped ... no unequipping allowed" means the answer to a full
///    loadout is WHERE the new item lands, never WHETHER it is equipped, so "leave the new talisman
///    in the bag" is not available and neither is a wear-value comparison, which is just choosing
///    your build with extra steps. WHICH slot it lands in is the part this rule decides.
///
/// ## What happens AFTER the slots fill -- issue #342
///
/// Rule 3 was `the LOWEST unlocked slot, always`. That made **slot 1 the only slot that ever
/// changed again**: slots 2, 3 and 4 froze on whatever happened to arrive 2nd, 3rd and 4th, for the
/// rest of the run. Rule 2's own rationale above argues against exactly that outcome -- it holds
/// during the fill and inverts the moment the slots are full, leaving the player with ONE recent
/// talisman and three stale ones. Alaric hit the two-slot version on the physick flask (#334) and
/// it is more pronounced with four slots, not less.
///
/// ## 🛑 Why `n` comes from the STREAM, not from the live field
///
/// The reconciler replays the WHOLE received set on every reconnect, so the policy must be a pure
/// function of things that replay identically. #48 established that for physick with `ordinal % 2`.
/// The obvious port -- `ordinal % usable_accessory_slots(raw)` -- **does not work**, because that
/// `n` grows from 1 to 4 as the player finds Talisman Pouches, so live it is evaluated against a
/// different modulus than on replay. MEASURED over 329,760 interleaved pouch/talisman streams: the
/// live-`n` form fails to be a replay fixed point in **8.9%** of them. #342 read that as proof the
/// mechanism does not port.
///
/// It ports. `n` is only live because it was being read at the wrong MOMENT. The Talisman Pouch is
/// itself an AP item ([`TALISMAN_POUCH_FULL_ID`]), so "pouches earned by stream position `i`" is a
/// pure function of the stream -- captured at RECEIVE, exactly like the ordinal, it replays
/// identically. Same 329,760 streams, `n` stream-derived: **0** failures.
///
/// `stream_slots` is that count ([`stream_accessory_slots`]); `unlocked_talisman_slots` remains the
/// authority on what may be WRITTEN. Keeping both is the answer to the objection recorded on
/// [`usable_accessory_slots`] -- "a mod that counts pouches independently is a second source of
/// truth waiting to disagree". They can disagree in exactly one direction that matters: a pouch
/// SENT but never granted (the #308 capped-grant shape) would have the stream claim a slot the
/// player has not earned. The `clamp` resolves that toward the GAME, so a locked slot is still
/// never read for occupancy nor written -- and the caller logs when it fires, because a silent
/// clamp is the "polite `false`" that *Runtime visibility* forbids.
///
/// ⚠️ What this does NOT promise is `live == replay`. Rule 2 is state-dependent, so replaying a
/// stream onto the state it produced can land a talisman differently (5.6% of the streams above).
/// **#48 has the identical property at 2.0%** and for the identical reason; the guarantee both make
/// is that replay is a FIXED POINT -- it settles in one pass and never drifts again. Returning `Some` for every talisman is deliberate and is
/// why this does not mirror the `Option` on [`slot_for_wep_type`]: ammunition has no hand at all,
/// whereas every talisman has a slot. The one `None` here means "already equipped", not "refused".
pub fn slot_for_accessory(
    unlocked_talisman_slots: u8,
    stream_slots: usize,
    slots: [Option<i32>; 4],
    param_id: i32,
    ordinal: u64,
) -> Option<u32> {
    // The GAME's field is the hard bound on what may be written; the STREAM only picks among the
    // slots the game already allows. In agreement (the normal case) the min is the stream value.
    let n = stream_slots.clamp(1, usable_accessory_slots(unlocked_talisman_slots));
    let visible = &slots[..n];

    if visible.contains(&Some(param_id)) {
        return None;
    }
    match visible.iter().position(Option::is_none) {
        Some(i) => Some(ACCESSORY_SLOTS[i]),
        // All unlocked slots full -> alternate by received-stream position, so no slot freezes.
        // `ordinal % n < n <= usable_accessory_slots(..)`, so this cannot name a locked slot.
        None => Some(ACCESSORY_SLOTS[(ordinal % n as u64) as usize]),
    }
}

/// How many talisman slots the received STREAM says the player has earned, given how many Talisman
/// Pouches have been seen in it so far. See [`slot_for_accessory`] for why this exists alongside
/// [`usable_accessory_slots`] rather than replacing it.
///
/// The Talisman Pouch is a real AP item in this world -- three checks, `7770025/26/27`, all
/// awarding GOODS row **10040** (FullID `1073751864`), flags 60500/60510/60520. Cited to
/// `greenfield/eldenring/item_ids.py` and independently to `greenfield/shop_rows.tsv:463`, which
/// names row 10040 `Talisman Pouch` at the Twin Maiden Husks. No id is guessed here.
pub const TALISMAN_POUCH_FULL_ID: i32 = 1_073_751_864;

/// Slots earned after seeing `pouches` Talisman Pouches in the received stream.
///
/// Deliberately the same shape as [`usable_accessory_slots`] -- both are "pouches + 1, clamped" --
/// because they are two measurements of the SAME quantity from two sources. They agree except when
/// a pouch was sent but never landed; [`slot_for_accessory`] resolves that disagreement toward the
/// game.
pub fn stream_accessory_slots(pouches: u32) -> usize {
    (pouches as usize + 1).clamp(1, ACCESSORY_SLOTS.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEAPON_DAGGER: i32 = 0x0000_2710; // category 0, row 10000
    const GOODS_FLASK: i32 = 0x4000_03E9; // category 4 (goods), row 1001
    const PROTECTOR_HELM: i32 = 0x1000_2710; // category 1 (armor)
    const GEM_ASH: i32 = 0x8000_0064u32 as i32; // category 8 (ash of war)
    /// Ailment Talisman -- `greenfield/eldenring/item_ids.py` ships it as 536879032, i.e.
    /// category 2, row 10104. A real entry from the real pool, not a constructed one.
    const ACCESSORY_TALISMAN: i32 = 0x2000_2778;
    /// Godrick's Great Rune as the apworld actually sends it: 1073742015 = category 4, row 191.
    /// GOODS, not accessory -- which is why no Great Rune ever reaches the accessory arm.
    const GOODS_GREAT_RUNE: i32 = 0x4000_00BF;

    #[test]
    fn only_weapons_are_weapons() {
        assert!(is_weapon(WEAPON_DAGGER));
        assert!(!is_weapon(GOODS_FLASK));
        assert!(!is_weapon(PROTECTOR_HELM));
        assert!(!is_weapon(GEM_ASH));
    }

    #[test]
    fn weapons_protectors_and_talismans_are_equipable_nothing_else_is() {
        assert_eq!(equipable(WEAPON_DAGGER), Some(Equipable::Weapon));
        assert_eq!(equipable(PROTECTOR_HELM), Some(Equipable::Protector));
        assert_eq!(equipable(ACCESSORY_TALISMAN), Some(Equipable::Accessory));
        assert_eq!(equipable(GOODS_FLASK), None);
        assert_eq!(equipable(GEM_ASH), None);
    }

    /// THE MOTIVATING CASE (rule 11), twice reported by the same player and named in his words:
    ///
    ///   2026-08-02, on 0.3.1: "however talismans and crystal tears are not changing"
    ///   2026-08-03, on 0.3.2: "talisman didn't swap after killing a boss"
    ///
    /// A talisman FullID reached `equipable()` and got `None`, so `enqueue` dropped it on the
    /// floor and nothing downstream ever ran. This fails on the two-arm classifier that shipped
    /// 0.3.2. The GOODS half of his sentence -- crystal tears -- is still `None` here ON PURPOSE
    /// and is a separate issue: they are not in `chr_asm` at all.
    #[test]
    fn a_received_talisman_is_no_longer_dropped_on_the_floor() {
        assert_eq!(
            equipable(536_879_032),
            Some(Equipable::Accessory),
            "Ailment Talisman (item_ids.py 536879032) must classify as an accessory -- \
             returning None here is the whole of #295"
        );
        assert_eq!(
            equipable(GOODS_GREAT_RUNE),
            None,
            "a Great Rune ships as GOODS and must NOT enter the accessory arm"
        );
        assert_eq!(equipable(GOODS_FLASK), None); // physick tear: separate issue, still None
    }

    #[test]
    fn shields_go_left_weapons_go_right() {
        assert_eq!(hand_for_wep_type(65), Hand::Left); // small shield
        assert_eq!(hand_for_wep_type(67), Hand::Left); // medium shield
        assert_eq!(hand_for_wep_type(69), Hand::Left); // greatshield
        assert_eq!(hand_for_wep_type(1), Hand::Right); // straight sword
        assert_eq!(hand_for_wep_type(57), Hand::Left); // glintstone staff -- French Challenge
        assert_eq!(hand_for_wep_type(61), Hand::Left); // sacred seal (61, NOT 59: 59 has 0 rows)
        assert_eq!(hand_for_wep_type(87), Hand::Right); // torch stays main-hand (see the const)
    }

    #[test]
    fn weapon_slots_follow_the_hand() {
        assert_eq!(slot_for_wep_type(67), Some(SLOT_WEAPON_LEFT_1));
        assert_eq!(slot_for_wep_type(1), Some(SLOT_WEAPON_RIGHT_1));
    }

    /// The bug, by name: a bolt reached SLOT_WEAPON_RIGHT_1 in boblerrr's 2026-08-03 log because
    /// ammo is CATEGORY_WEAPON and nothing claimed it. Fails without the AMMO arm.
    #[test]
    fn ammo_is_never_routed_to_a_hand() {
        for &t in AMMO_WEP_TYPES {
            assert!(is_ammo(t), "wep_type {t} should be ammo");
            assert_eq!(
                slot_for_wep_type(t),
                None,
                "wep_type {t} is ammunition and must not be equipped to a hand -- \
                 param 52080000 (wep_type 85, Lordsworn's Bolt) reached the MAIN HAND this way"
            );
        }
        // and the classes either side of it stay in a hand
        assert_eq!(slot_for_wep_type(87), Some(SLOT_WEAPON_RIGHT_1)); // torch
        assert_eq!(slot_for_wep_type(69), Some(SLOT_WEAPON_LEFT_1)); // greatshield
        assert!(!is_ammo(57) && !is_ammo(61)); // staff / seal are held, not ammo
    }

    /// The #301 motivating case, by name (rule 11): boblerrr received a staff/seal, it went to the
    /// MAIN hand, and with no sorceries he was effectively unarmed. Under the French Challenge
    /// ruleset every one of these belongs in the LEFT hand. Fails on the pre-2026-08-03 list
    /// `[65, 67, 69]`, which is exactly what shipped the bug.
    #[test]
    fn catalysts_bows_and_crossbows_go_to_the_left_hand_like_shields() {
        for (t, what) in [
            (57u16, "glintstone staff"),
            (61, "sacred seal"),
            (50, "light bow"),
            (51, "bow"),
            (53, "greatbow"),
            (55, "crossbow"),
            (56, "ballista"),
        ] {
            assert_eq!(
                hand_for_wep_type(t),
                Hand::Left,
                "wep_type {t} ({what}) must auto-equip LEFT -- main-handing it takes the player's \
                 weapon away and leaves them unable to fight (#301)"
            );
            assert_eq!(slot_for_wep_type(t), Some(SLOT_WEAPON_LEFT_1));
        }
        // and the classes that must NOT have moved
        assert_eq!(hand_for_wep_type(1), Hand::Right); // straight sword
        assert_eq!(hand_for_wep_type(41), Hand::Right); // colossal weapon
        assert_eq!(hand_for_wep_type(87), Hand::Right); // torch -- ruleset does not move it
        assert_eq!(slot_for_wep_type(85), None); // ammo still not equipped at all (#294)
    }

    /// This module keeps its OWN copy of the category nibbles rather than importing `er_codec`'s.
    /// That duplication predates #295, but adding a third arm to it makes a silent split more
    /// expensive, so pin the two together behaviourally: if either table is ever edited alone,
    /// this fails instead of one classifier quietly disagreeing with the other.
    #[test]
    fn the_local_category_nibbles_agree_with_er_codec() {
        assert_eq!(
            equipable((er_codec::CATEGORY_WEAPON | 10_000) as i32),
            Some(Equipable::Weapon)
        );
        assert_eq!(
            equipable((er_codec::CATEGORY_PROTECTOR | 10_000) as i32),
            Some(Equipable::Protector)
        );
        assert_eq!(
            equipable((er_codec::CATEGORY_ACCESSORY | 1_000) as i32),
            Some(Equipable::Accessory)
        );
        assert_eq!(equipable((er_codec::CATEGORY_GOODS | 1_000) as i32), None);
        assert_eq!(equipable((er_codec::CATEGORY_GEM | 1_000) as i32), None);
    }

    /// ⚠️ THE INDICES ARE NOT `15 + n`. `ChrAsmSlot::Unused16 = 16` sits between the protector
    /// block and the accessories, so an off-by-one derived from "armour ends at 15" writes the
    /// unused slot and then walks one short of `Accessory4`. Pinned against the enum in the
    /// `eldenring` crate this workspace actually builds against.
    #[test]
    fn accessory_slots_are_the_pinned_chrasm_indices() {
        assert_eq!(ACCESSORY_SLOTS, [17, 18, 19, 20]);
        assert_eq!(SLOT_ACCESSORY_1, 17);
        assert_eq!(SLOT_PROTECTOR_LEGS + 1, 16); // Unused16 -- NOT an accessory
    }

    /// 🛑 The one write this module must never make. `AccessoryCovenant = 21` is the GREAT RUNE
    /// slot; an off-by-one at the top of the range puts a talisman in it. Nothing reads a slot
    /// index from anywhere but `ACCESSORY_SLOTS`, so asserting the array is asserting the bound.
    #[test]
    fn the_great_rune_slot_is_never_a_target() {
        const ACCESSORY_COVENANT: u32 = 21;
        assert!(!ACCESSORY_SLOTS.contains(&ACCESSORY_COVENANT));
        for unlocked in 0..=255u8 {
            for ordinal in 0..16u64 {
                let all_full = [Some(1), Some(2), Some(3), Some(4)];
                let n = stream_accessory_slots(unlocked as u32);
                let slot = slot_for_accessory(unlocked, n, all_full, 9999, ordinal).unwrap();
                assert!(
                    ACCESSORY_SLOTS.contains(&slot),
                    "unlocked={unlocked} ordinal={ordinal} produced slot {slot}, outside the four \
                     talisman slots"
                );
                // ...and inside the slots this character has actually EARNED.
                let earned = &ACCESSORY_SLOTS[..usable_accessory_slots(unlocked)];
                assert!(
                    earned.contains(&slot),
                    "unlocked={unlocked} ordinal={ordinal} produced slot {slot}, which is locked"
                );
            }
        }
    }

    /// The unlock count is progression-gated by Talisman Pouches, so the resolver must never
    /// consider a slot the player has not earned. Both readings of the field (slots or pouches)
    /// are clamped into 1..=4, and a garbage value cannot widen the range.
    #[test]
    fn locked_slots_are_never_written() {
        // MEASURED (Alaric, 0.3.2, 2026-08-03): the field counts POUCHES. `raw=0` was logged on a
        // character with a working slot 1, which is impossible under the "unlocked slots" reading.
        assert_eq!(usable_accessory_slots(0), 1); // no pouches -> the one slot everyone starts with
        assert_eq!(usable_accessory_slots(1), 2);
        assert_eq!(usable_accessory_slots(2), 3);
        assert_eq!(usable_accessory_slots(3), 4); // three pouches = the vanilla maximum
        assert_eq!(usable_accessory_slots(4), 4); // clamped: ChrAsmSlot 21 is the GREAT RUNE slot
        assert_eq!(usable_accessory_slots(9), 4); // clamped, not wrapped
        assert_eq!(usable_accessory_slots(255), 4);

        // NO POUCHES (raw=0) = ONE slot. Empty -> slot 1, and NOT slot 2 even though 2 is empty
        // too. This is the state Alaric's whole session ran in: six talismans, all to slot 17.
        assert_eq!(
            slot_for_accessory(0, 1, [None, None, None, None], 1000, 0),
            Some(SLOT_ACCESSORY_1)
        );
        // ...and once that one slot is full, clobber it. The LOCKED empties stay untouched, which
        // is the assertion that matters: a locked slot is neither read for occupancy nor written.
        // With ONE usable slot the alternation has nowhere to go: `ordinal % 1` is 0 for every
        // ordinal, so rule 3 degenerates to "clobber slot 1" and never names slot 2.
        for ordinal in 0..8u64 {
            assert_eq!(
                slot_for_accessory(0, 1, [Some(1000), None, None, None], 2000, ordinal),
                Some(SLOT_ACCESSORY_1),
                "ordinal {ordinal} escaped the single unlocked slot"
            );
        }
        // ONE pouch = two slots; first full -> the second, never the third.
        assert_eq!(
            slot_for_accessory(1, 2, [Some(1000), None, None, None], 2000, 0),
            Some(SLOT_ACCESSORY_2)
        );
        // Both unlocked slots full -> alternate between them. Slot 3 is still locked and must not
        // be named by EITHER parity.
        assert_eq!(
            slot_for_accessory(1, 2, [Some(1000), Some(1010), None, None], 2000, 2),
            Some(SLOT_ACCESSORY_1)
        );
        assert_eq!(
            slot_for_accessory(1, 2, [Some(1000), Some(1010), None, None], 2000, 3),
            Some(SLOT_ACCESSORY_2)
        );
    }

    /// 🛑 THE BUG THE MEASUREMENT FOUND. Shipped, `usable_accessory_slots` clamped the raw field to
    /// `1..=4` on the assumption it might already BE a slot count. It is a POUCH count, so from the
    /// first pouch onward the player had one fewer usable slot than they had earned -- two slots
    /// filled one, and a fully-upgraded four filled three. Never an illegal write; just a slot that
    /// silently went unused for the rest of the run.
    ///
    /// Fails on the shipped `(raw).clamp(1, 4)`.
    #[test]
    fn a_pouch_earns_a_slot_and_the_slot_gets_used() {
        // one pouch = two slots: the second talisman must NOT clobber the first
        assert_eq!(
            slot_for_accessory(1, 2, [Some(1000), None, None, None], 2000, 0),
            Some(SLOT_ACCESSORY_2),
            "with one Talisman Pouch the player has two slots -- filling one and clobbering it is \
             the old off-by-one"
        );
        // three pouches = all four slots reachable
        assert_eq!(
            slot_for_accessory(3, 4, [Some(1000), Some(1010), Some(1020), None], 1030, 0),
            Some(SLOT_ACCESSORY_4),
            "three pouches is the vanilla maximum; slot 4 must be usable"
        );
    }

    /// The stated policy, slot by slot: fill empties in order, then ALTERNATE by stream position.
    #[test]
    fn talismans_fill_empty_slots_then_alternate() {
        let full = 4u8;
        let worn = [Some(1000), Some(1010), Some(1020), Some(1030)];
        for (ordinal, expect) in [
            (0, SLOT_ACCESSORY_1),
            (1, SLOT_ACCESSORY_2),
            (2, SLOT_ACCESSORY_3),
            (3, SLOT_ACCESSORY_4),
        ] {
            let mut slots = worn;
            for s in slots.iter_mut().skip(ordinal as usize) {
                *s = None;
            }
            assert_eq!(
                slot_for_accessory(full, 4, slots, 1040, ordinal),
                Some(expect),
                "fill phase: ordinal {ordinal} should take the first empty slot"
            );
        }
        // All four worn: the fifth talisman still gets equipped. Refusing it would be "you may
        // keep the build you have", which is the one thing the ruleset forbids. WHERE it lands now
        // walks the slots instead of pinning slot 1.
        for (ordinal, expect) in [
            (4u64, SLOT_ACCESSORY_1),
            (5, SLOT_ACCESSORY_2),
            (6, SLOT_ACCESSORY_3),
            (7, SLOT_ACCESSORY_4),
            (8, SLOT_ACCESSORY_1),
        ] {
            assert_eq!(
                slot_for_accessory(full, 4, worn, 1040, ordinal),
                Some(expect),
                "full loadout: ordinal {ordinal} landed in the wrong slot"
            );
        }
    }

    /// 🛑 THE MOTIVATING CASE for #342 (CONTRIBUTING rule 11), stated as the freeze itself rather
    /// than as one call: run eight talismans into a fully-unlocked character and every slot must
    /// have churned. Under the shipped `clobber the lowest` rule this ends
    /// `[H, B, C, D]` -- slots 2, 3 and 4 frozen on the 2nd, 3rd and 4th arrivals for the rest of
    /// the run, which is the opposite of rule 2's own stated rationale.
    ///
    /// Fails on `None => Some(ACCESSORY_SLOTS[0])`.
    #[test]
    fn a_full_loadout_churns_every_slot_instead_of_freezing_three() {
        let stream: [i32; 8] = [1000, 1010, 1020, 1030, 1040, 1050, 1060, 1070];
        let mut slots = [None; 4];
        for (ordinal, &param_id) in stream.iter().enumerate() {
            if let Some(slot) = slot_for_accessory(3, 4, slots, param_id, ordinal as u64) {
                let i = ACCESSORY_SLOTS.iter().position(|&x| x == slot).unwrap();
                slots[i] = Some(param_id);
            }
        }
        assert_eq!(
            slots,
            [Some(1040), Some(1050), Some(1060), Some(1070)],
            "the four most recent talismans should be worn, not the 1st, 2nd, 3rd and last"
        );
        // The specific tell Alaric would see: nothing from the first four survives.
        for stale in [1000, 1010, 1020, 1030] {
            assert!(
                !slots.contains(&Some(stale)),
                "talisman {stale} froze in its slot"
            );
        }
    }

    /// A gap in the middle is filled before anything is clobbered -- the player unequipped slot 2
    /// by hand, or a pouch arrived after slots 1 and 3 were written.
    #[test]
    fn the_first_empty_slot_wins_even_out_of_order() {
        assert_eq!(
            slot_for_accessory(4, 4, [Some(1000), None, Some(1020), None], 1030, 7),
            Some(SLOT_ACCESSORY_2)
        );
    }

    /// ER does not let you wear the same talisman twice, so re-equipping one already worn would
    /// build a loadout the menu cannot produce. This is the case a plain "first empty slot" policy
    /// gets wrong: with slot 2 free it would happily put a second copy there.
    #[test]
    fn a_talisman_already_worn_is_not_equipped_a_second_time() {
        assert_eq!(
            slot_for_accessory(4, 4, [Some(1000), None, None, None], 1000, 0),
            None
        );
        assert_eq!(
            slot_for_accessory(
                4,
                4,
                [Some(1000), Some(1010), Some(1020), Some(1030)],
                1020,
                5
            ),
            None
        );
        // ...but a copy sitting in a LOCKED slot is not "worn" -- it cannot be, so slot 1 it is.
        // raw=0 (no pouches) => ONE unlocked slot, so the `Some(1010)` in slot 2 is unreachable
        // state the game could not have produced and must not suppress the equip.
        assert_eq!(
            slot_for_accessory(0, 1, [None, Some(1010), None, None], 1010, 0),
            Some(SLOT_ACCESSORY_1)
        );
    }

    #[test]
    fn protector_categories_map_to_the_four_armour_slots() {
        assert_eq!(slot_for_protector_category(0), Some(SLOT_PROTECTOR_HEAD));
        assert_eq!(slot_for_protector_category(1), Some(SLOT_PROTECTOR_CHEST));
        assert_eq!(slot_for_protector_category(2), Some(SLOT_PROTECTOR_HANDS));
        assert_eq!(slot_for_protector_category(3), Some(SLOT_PROTECTOR_LEGS));
    }

    /// Category 4 is the 41-row dummy block (ids 1000..2100, no name, no equip flags). Equipping
    /// one would put a nameless placeholder on the player.
    #[test]
    fn dummy_protector_category_is_refused() {
        assert_eq!(slot_for_protector_category(4), None);
        assert_eq!(slot_for_protector_category(255), None);
    }

    /// The id convention `(id / 100) % 10` is NOT a substitute for the param field: it disagrees
    /// on 44 vanilla rows. 1000 is one of them -- convention says head, the game says dummy.
    #[test]
    fn id_convention_is_not_the_slot_source() {
        let conventional = (1000i32 / 100) % 10; // == 0, i.e. "head"
        assert_eq!(conventional, 0);
        assert_eq!(slot_for_protector_category(4), None); // the game's answer for row 1000
    }
}
