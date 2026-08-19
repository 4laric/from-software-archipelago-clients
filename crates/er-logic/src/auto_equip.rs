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
//! `EQUIP_PARAM_PROTECTOR_ST.protectorCategory`, and -- for talismans -- what the four accessory
//! slots currently hold.
//!
//! Talisman SLOT COUNT is the one thing that is deliberately NOT read off the live character
//! (#342). It is counted off the AP received stream instead, because the Talisman Pouch is itself
//! an AP item and the slot decision has to be a pure function of replayed inputs. See
//! [`TalismanStream`].

use crate::hook::GameHook;

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
pub const SLOT_WEAPON_LEFT_2: u32 = 2;
pub const SLOT_WEAPON_RIGHT_2: u32 = 3;
pub const SLOT_WEAPON_LEFT_3: u32 = 4;
pub const SLOT_WEAPON_RIGHT_3: u32 = 5;
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

/// All six armament slots in `ChrAsmSlot` order.
pub const WEAPON_SLOTS: [u32; 6] = [
    SLOT_WEAPON_LEFT_1,
    SLOT_WEAPON_RIGHT_1,
    SLOT_WEAPON_LEFT_2,
    SLOT_WEAPON_RIGHT_2,
    SLOT_WEAPON_LEFT_3,
    SLOT_WEAPON_RIGHT_3,
];

/// The two reserve left-hand slots the one-left-slot challenge policy clears at fresh-loadout
/// initialization (#441).
pub const LEFT_RESERVE_SLOTS: [u32; 2] = [SLOT_WEAPON_LEFT_2, SLOT_WEAPON_LEFT_3];

/// Param row the live `ChrAsm` uses for an empty armament slot on 2.6.2.0.
///
/// This is observed game state, not an invented sentinel: the live dump cited above read 110000
/// in every idle hand slot. The I/O caller still copies the complete representation from a live
/// slot that carries this row; this constant is only the predicate that locates that source.
pub const UNARMED_WEAPON_PARAM_ID: i32 = 110_000;

/// Pure plan for normalizing a starting class to the challenge's one active left-hand slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartingLeftCleanupPlan {
    /// Live slot whose complete unarmed representation should be copied to `clear_slots`.
    /// `None` when no reserve slot needs clearing.
    pub unarmed_source: Option<u32>,
    /// Populated reserve left slots to unequip back to the inventory.
    pub clear_slots: Vec<u32>,
    /// Whether the active left-hand selector must be returned to Left1.
    pub reset_selector: bool,
}

/// Why a fresh starting loadout could not be normalized this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartingLeftCleanupError {
    /// At least one reserve left slot is populated, but none of the six live armament slots carries
    /// the game's unarmed representation. Refuse rather than construct a handle from guessed bits.
    NoUnarmedSource,
}

/// Per-seed control action for the fresh-loadout cleanup. Kept pure so a missing sidecar cannot be
/// mistaken for a fresh character by the Windows glue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartingLeftInitAction {
    /// No state change this tick.
    Wait,
    /// Persist cleanup debt before the save-embedded marker can turn Fresh into Resume.
    Arm,
    /// The inventory is settled; attempt the live normalization and settle only on read-back.
    Attempt,
    /// Mark complete without touching equipment (returning character, or option off).
    PreserveAndSettle,
}

/// Decide the one-time fresh-loadout state transition.
pub fn starting_left_init_action(
    normalized: bool,
    pending: bool,
    fresh_character: Option<bool>,
    auto_equip_on: bool,
    inventory_settled: bool,
) -> StartingLeftInitAction {
    if normalized {
        return StartingLeftInitAction::Wait;
    }
    if pending {
        if !auto_equip_on {
            return StartingLeftInitAction::PreserveAndSettle;
        }
        return if inventory_settled {
            StartingLeftInitAction::Attempt
        } else {
            StartingLeftInitAction::Wait
        };
    }
    match fresh_character {
        None => StartingLeftInitAction::Wait,
        Some(_) if !auto_equip_on => StartingLeftInitAction::PreserveAndSettle,
        Some(true) => StartingLeftInitAction::Arm,
        Some(false) => StartingLeftInitAction::PreserveAndSettle,
    }
}

/// Decide how to enforce one active left-hand slot on a fresh starting loadout.
///
/// `worn` is slots 0..=5 in `ChrAsmSlot` order. Right-hand contents are only eligible to provide a
/// known-good unarmed representation; they are never returned as clear targets and are unchanged.
pub fn starting_left_cleanup_plan(
    worn: [i32; 6],
    selected_left_slot: u32,
) -> Result<StartingLeftCleanupPlan, StartingLeftCleanupError> {
    let clear_slots: Vec<u32> = LEFT_RESERVE_SLOTS
        .into_iter()
        .filter(|&slot| worn[slot as usize] != UNARMED_WEAPON_PARAM_ID)
        .collect();
    let unarmed_source = if clear_slots.is_empty() {
        None
    } else {
        WEAPON_SLOTS
            .into_iter()
            .find(|&slot| worn[slot as usize] == UNARMED_WEAPON_PARAM_ID)
            .ok_or(StartingLeftCleanupError::NoUnarmedSource)
            .map(Some)?
    };
    Ok(StartingLeftCleanupPlan {
        unarmed_source,
        clear_slots,
        reset_selector: selected_left_slot != 0,
    })
}

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
    /// A TALISMAN. Take the [`TalismanPos`] the caller's [`TalismanStream`] produced and the
    /// current occupants of [`ACCESSORY_SLOTS`], then [`slot_for_accessory`].
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

/// The FullID the auto_equip queue must HOLD for a received item: the id the grant will put in
/// the bag, not the raw received id.
///
/// This is the #296/#302/#303 fix as a production seam. The Windows crate's `auto_equip::enqueue`
/// stores this return value, and its `tick()` later looks it up in the inventory by EXACT FullID;
/// the grant path independently runs [`crate::upgrades::apply_auto_upgrade`] on the item's way
/// into the bag. With `auto_upgrade` ON, an upgradeable weapon received as `base + 0` lands as
/// `base + N` -- queue the raw id and the lookup misses every tick for the rest of the session
/// (boblerrr: armour auto-equips fine, weapons never do). Delegating to the same predicate the
/// grant runs makes the queue and the bag agree by construction.
///
/// Why a named seam instead of the Windows crate calling `apply_auto_upgrade` inline (as the
/// first version of the fix did): the inline call lived in a crate with no test targets, and the
/// er-logic tests memorialising it compared two local aliases of one call -- deleting the inline
/// call kept the whole workspace green while the bug returned (2026-08-04 inert-test audit, F1).
/// Same repair as [`crate::check_neutralise::slot_write`]: the decision is the return value of a
/// host-tested function that production calls, so there is nothing left to disagree with.
/// `upgrades_replay`'s `auto_equip_queue_matches_bag` pins THIS function against the grant path
/// (exact ids, every held level) and goes red if either side stops applying the upgrade.
///
/// CALL SITE: `eldenring-archipelago/src/upgrades.rs enqueue_upgrade_id` (live-hook wrapper),
/// called by that crate's `auto_equip.rs enqueue` -- the only enqueue path.
pub fn enqueue_id(hook: &dyn GameHook, auto_upgrade_on: bool, full_id: i32) -> i32 {
    crate::upgrades::apply_auto_upgrade(hook, auto_upgrade_on, full_id)
}

/// Which weapon ALREADY IN THE BAG should be put in the player's hand for base row `base`?
///
/// `owned_weapon_full_ids` is every WEAPON FullID the bag currently holds. Returns the FullID to
/// queue -- the highest reinforce level of `base` the player actually owns -- or `None` if they
/// own no level of it.
///
/// MOTIVATING CASE (rule 11), boblerrr 2026-08-07 18:31:38. #101 made the Rykard equip follow the
/// FIGHT instead of the grant, and it enqueued `SERPENT_HUNTER_BASE` down the ordinary receive
/// path -- through [`enqueue_id`], which raises the id to the player's auto_upgrade target. His
/// log shows the raise happening on the equip tick:
///
/// ```text
/// boss-grant: healthbar npc_param 47101038 = chr 4710, IS Rykard | c4710 loaded = yes |
///             already holds the spear = yes -> no grant
/// auto_upgrade: 0x103db70 -> 0x103db73 (enqueue)
/// boss-grant: Rykard's healthbar is up and the spear was already in the bag -- putting it in
///             your hand
/// ```
///
/// `17030000 -> 17030003`. The drain then looks the queued id up in the bag by EXACT FullID, and
/// his spear had been granted hours earlier at `+0`, when the target still WAS `+0`. Nothing
/// matched, the entry went back on `still_pending`, and it retried in silence for the rest of the
/// session -- his log has the banner above and no `auto_equip: slot ... <-` line anywhere after it.
///
/// THE RAISE IS CORRECT FOR A RECEIVE AND WRONG HERE, AND THE DIFFERENCE IS A PREMISE, NOT A
/// NUMBER. [`enqueue_id`]'s own contract names it: "the grant path independently runs
/// [`crate::upgrades::apply_auto_upgrade`] on the item's way into the bag". The raise exists so the
/// queue agrees with what a grant is ABOUT TO DEPOSIT. This path has no grant -- the item is
/// already in the bag, banked at whatever the target was on the day it arrived -- so there is
/// nothing coming to reconcile the queue against, and `base + today's target` is a row that will
/// never exist. Ask the bag what it HAS instead of predicting what a grant WOULD put there.
///
/// HIGHEST, not nearest. `apply_auto_upgrade` is raise-only and its target is a floor, so when the
/// player owns several levels of one row the strongest is the one the auto-upgrade intent points
/// at. It is also the only tie-break that cannot depend on bag ORDER, which the caller's inventory
/// walk does not promise.
///
/// Non-weapons and out-of-range rows decode to `None` and never match, so a caller that hands this
/// a protector gets `None` rather than a wrong slot.
pub fn held_row_to_equip(
    base: i32,
    owned_weapon_full_ids: impl IntoIterator<Item = i32>,
) -> Option<i32> {
    owned_weapon_full_ids
        .into_iter()
        .filter_map(|full_id| {
            let (b, level) = crate::upgrades::decode_weapon_id(full_id)?;
            (b == base).then_some((level, full_id))
        })
        .max_by_key(|&(level, _)| level)
        .map(|(_, full_id)| full_id)
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
/// ⭐ ONE SLOT, LAST WRITER WINS (#441 ruling, 2026-08-19). A received staff replaces a received
/// shield in `SLOT_WEAPON_LEFT_1`; auto-equip never fills Left2/3. Their indices are now verified
/// from the pinned `ChrAsmSlot` enum only so fresh starting-class loadouts can be normalized by
/// [`starting_left_cleanup_plan`], not so received items can route there.
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

/// The AP FullID of `Talisman Pouch`, as the apworld ships it: `1073751864` in
/// `greenfield/eldenring/item_ids.py` on world `main` (`ITEM_CATALOG['Talisman Pouch']`), i.e.
/// GOODS (category nibble 4) row 10040. All THREE vanilla copies are randomized checks --
/// `LOCATION_ITEM` 7770025, 7770026 and 7770027 -- so a pouch cannot reach the player except
/// through the AP received stream.
///
/// 🛑 THAT IS THE ONLY REASON THIS CONSTANT EXISTS. See [`TalismanStream`] for why counting an
/// item the game already counts is the right call for this one item and the wrong call in general.
pub const TALISMAN_POUCH_FULL_ID: i32 = 1_073_751_864;

/// Is this received FullID a Talisman Pouch?
pub fn is_talisman_pouch(full_id: i32) -> bool {
    full_id == TALISMAN_POUCH_FULL_ID
}

/// Where a talisman sits in the AP RECEIVED STREAM: its ordinal among talismans, and how many
/// Talisman Pouches arrived strictly BEFORE it.
///
/// Both fields are pure functions of the replayed stream. [`slot_for_accessory`] takes this
/// instead of a loose `u8` on purpose: there is no longer a parameter a future edit can quietly
/// feed `PlayerGameData.unlocked_talisman_slots` back into.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TalismanPos {
    /// 0-based position among the TALISMANS in the stream.
    pub ordinal: u64,
    /// Talisman Pouches received before this talisman. Feeds [`usable_accessory_slots`].
    pub pouches: u8,
}

/// Running tally over the AP received stream, fed every received FullID in stream order.
///
/// ## 🛑 Why this counts pouches when the module used to refuse to
///
/// [`usable_accessory_slots`] used to be handed `PlayerGameData.unlocked_talisman_slots` straight
/// off the live character, and the reason was written down here:
///
/// > we do NOT track Talisman Pouch pickups ourselves, because the game already tracks them and a
/// > mod that counts pouches independently is a second source of truth waiting to disagree.
///
/// That reasoning is sound for a pickup the GAME owns, and it still governs everything the AP
/// stream never sees. It stops applying to the Talisman Pouch, and only to it, because the pouch
/// is ITSELF AN AP ITEM ([`TALISMAN_POUCH_FULL_ID`]): all three copies are randomized checks, so
/// the stream is the source of truth for when the player got one and `unlocked_talisman_slots` is
/// the DERIVED copy, updated a beat later when the grant lands. bobler's 0.3.5 log shows the two
/// in that order -- pouch received 05:05:41, field steps `raw` 0 -> 1 by 05:05:43. Counting here
/// is not a second tally of the game's number; it is reading the number UPSTREAM of the game's
/// copy. The client still logs both on every accessory equip, so a disagreement shows up in a log
/// instead of being argued about.
///
/// It has to be read upstream, because [`slot_for_accessory`] must be a pure function of replayed
/// inputs and the live field is not one -- see that function's convergence note.
///
/// ⚠️ The tally must run over the WHOLE received stream, not over the tail the client happens to
/// grant this connect. `received_through` is persisted per save, so a reconnect replays only the
/// items past it; a counter that started at zero each connect would report zero pouches to a
/// player who found three last session and pin them back to one slot. The call site therefore
/// feeds this in the same history-agnostic pass that already counts `Progressive Flask Upgrade`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TalismanStream {
    pouches: u32,
    ordinal: u64,
}

impl TalismanStream {
    /// Feed the NEXT FullID in the received stream. Returns the stream position when it is a
    /// TALISMAN -- pass that straight to [`slot_for_accessory`] -- and `None` for everything else.
    ///
    /// Pouches are counted BEFORE the talisman that follows them, which is what makes the pair
    /// (ordinal, pouches) reproduce exactly the unlock count that was live when the talisman first
    /// arrived.
    pub fn push(&mut self, full_id: i32) -> Option<TalismanPos> {
        if is_talisman_pouch(full_id) {
            self.pouches = self.pouches.saturating_add(1);
            return None;
        }
        if equipable(full_id) != Some(Equipable::Accessory) {
            return None;
        }
        let pos = TalismanPos {
            ordinal: self.ordinal,
            // Saturating, not wrapping: `usable_accessory_slots` clamps anyway, and a wrap would
            // turn a garbage stream into ONE usable slot instead of four.
            pouches: self.pouches.min(u8::MAX as u32) as u8,
        };
        self.ordinal += 1;
        Some(pos)
    }

    /// Talisman Pouches seen so far. Diagnostics only -- the decision uses [`TalismanPos`].
    pub fn pouches(&self) -> u32 {
        self.pouches
    }
}

/// How many of the four talisman slots the player has earned, from a POUCH COUNT.
///
/// ⭐ MEASURED 2026-08-03: THE COUNT IS POUCHES, NOT SLOTS -- hence the `+ 1`.
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
/// ✅ CONFIRMED DIRECTLY 2026-08-06, AT ONE POUCH. This carried a caveat that `+ 1` rested on a
/// single reading at `raw = 0` and was therefore established by elimination rather than by
/// observation, and asked for a reading at 1+ pouches. bobler's 0.3.5 session (13543-line log) is
/// that reading. Talisman Pouch was a randomized check in his seed and he found one mid-run:
///
///   05:05:41  bobler found their Talisman Pouch (Liurnia :: Rimed Crystal Bud - near Road to the Manor)
///   05:05:43  auto_equip: talisman 0x20000442 -> slot 18 (unlocked_talisman_slots raw=1, worn=[Some(1160), None, None, None])
///
/// Two seconds after the pouch the count steps 0 -> 1 and the resolver uses the SECOND accessory
/// slot for the first time. Session tally: `raw=0` on 9 equips, `raw=1` on 13. The caveat is
/// discharged -- `+ 1` is now observed, not inferred.
///
/// What the old clamp cost: from the FIRST pouch onward it under-counted by one, so a player with
/// two slots only ever used one and a fully-upgraded player used three of four. Never an illegal
/// write -- the failure direction the clamp was chosen for held -- just a wasted slot.
///
/// The ARGUMENT is now [`TalismanPos::pouches`], counted off the AP stream rather than read off
/// `PlayerGameData.unlocked_talisman_slots`. The units are identical -- the measurement above is
/// what says so -- and [`TalismanStream`] says why the source moved.
///
/// The clamp stays, and both ends still earn their keep:
///
/// * lower bound 1 -- every character has slot 1 from the first minute;
/// * upper bound 4 -- `ChrAsmSlot` 21 is the GREAT RUNE slot, and three pouches is the vanilla
///   maximum, so a larger value is a modded or garbage read and must not widen the range.
pub fn usable_accessory_slots(pouches: u8) -> usize {
    (pouches as usize + 1).clamp(1, ACCESSORY_SLOTS.len())
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
/// 2. **Otherwise `ACCESSORY_SLOTS[ordinal % n]`**, `n = usable_accessory_slots(pouches)` -- the
///    talisman's own position in the AP received stream, taken modulo the number of slots it was
///    earned into. Consecutive talismans therefore land on consecutive slots, which fills an empty
///    loadout 1, 2, 3, 4 exactly as the old "first empty slot" rule did, and then keeps rotating
///    instead of parking on slot 1 forever.
///
/// "All auto-equipped gear must stay equipped ... no unequipping allowed" means the answer to a
/// full loadout is WHERE the new item lands, never WHETHER it is equipped -- so "keep the ones you
/// have and leave the new talisman in the bag" is not available, and neither is a wear-value
/// comparison, which is just choosing your build with extra steps.
///
/// ## 🛑 #342: what happens AFTER the slots fill, and why rule 2 is a rotation
///
/// This shipped as *fill the first empty unlocked slot, then clobber the LOWEST one*. Its own
/// rationale ("a player who has never touched the menu ends up with the four most recent talismans
/// rather than one") inverts the moment the slots are full: from then on slot 1 is the only one
/// that ever changes and slots 2, 3, 4 freeze on whatever happened to arrive 2nd, 3rd and 4th.
/// bobler's 0.3.5 log measures how early that bites -- **21 of his 22 talisman equips went to slot
/// 17**, because at one unlocked slot "all unlocked slots full" is true from the second talisman
/// onward. The clobbered talisman is unequipped, not lost; it stays in the bag.
///
/// The physick flask hit the same freeze and [`crate::physick::slot_for_tear`] fixed it with
/// `ordinal % 2`. Porting that here needed a `% n` whose `n` did not move, and #342's blocker was
/// that `n` is live state: `unlocked_talisman_slots` grows 1 -> 4 as pouches are found, so the
/// same ordinal is taken modulo a different `n` live than on replay, and the reconciler's replay
/// of the received set silently rearranges the loadout. **That blocker is discharged by making
/// `n` a function of the stream** ([`TalismanStream`]): the pouch is an AP item, so the pouch
/// count at any point in the stream is derivable from position in it.
///
/// ⚠️ MAKING `n` PURE IS NECESSARY AND NOT SUFFICIENT, and this is the part #342 did not have.
/// A pure `n` alone does not converge, because the old rule 2 read the LIVE LOADOUT -- and the
/// live loadout is exactly what differs between the first pass (slots empty) and every replay
/// (slots full). #342's own worked example, 8 talismans with `n = 1,1,2,2,2,4,4,4`:
///
/// ```text
/// live                                          (E, D, F, H)
/// replay, n from the live field (4 throughout)  (E, F, G, H)   <- #342's divergence
/// replay, n from the stream, rule 2 KEPT        (E, D, G, H)   <- still diverges, slot 3
/// replay, n from the stream, rule 2 a ROTATION  (E, F, G, H) == live   <- converges
/// ```
///
/// F and G reached slots 3 and 4 in the live pass through the empty-slot rule; on replay those
/// slots are occupied, so they fall through to the rotation and land somewhere else. Any rule that
/// reads occupancy has that shape. The rotation does not read occupancy at all, which is why it
/// converges for every stream, every pouch schedule and every prefix length --
/// `replaying_the_received_set_converges_for_every_pouch_schedule` is the acceptance test.
///
/// Rule 1 survives because under a rotation it is never a *different* answer: a talisman in the
/// stream can only ever have been written to its own `ordinal % n`, so "already worn" fires only
/// when the item is already in the slot the rotation was about to write. It is a no-op, not a
/// diversion.
///
/// Only `usable_accessory_slots(pos.pouches)` slots are considered, so a locked slot is never read
/// for occupancy nor written -- the reason this does not simply use a constant modulus of 4, which
/// is #342's option 1 as literally filed. Returning `Some` for every talisman is deliberate and is
/// why this does not mirror the `Option` on [`slot_for_wep_type`]: ammunition has no hand at all,
/// whereas every talisman has a slot. The one `None` here means "already equipped", not "refused".
pub fn slot_for_accessory(pos: TalismanPos, slots: [Option<i32>; 4], param_id: i32) -> Option<u32> {
    let n = usable_accessory_slots(pos.pouches);
    let visible = &slots[..n];

    if visible.contains(&Some(param_id)) {
        return None;
    }
    Some(ACCESSORY_SLOTS[(pos.ordinal % n as u64) as usize])
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

    /// A stream position, spelled out at the call site so a reader can see both numbers.
    fn pos(ordinal: u64, pouches: u8) -> TalismanPos {
        TalismanPos { ordinal, pouches }
    }

    /// A talisman FullID for accessory param row `row` (category nibble 2), as the apworld sends
    /// one. `0x2000_2778` in the constants above is the real Ailment Talisman; these are synthetic
    /// rows in the same space, used only to tell eight talismans apart.
    fn tal(row: i32) -> i32 {
        (CATEGORY_ACCESSORY as i32) | row
    }

    /// Replay a whole received stream onto a starting loadout, exactly the way the client does:
    /// every received FullID goes through the production [`TalismanStream`], and a talisman is
    /// written to the slot [`slot_for_accessory`] names. The loadout holds PARAM ROWS, which is
    /// what `chr_asm.equipment_param_ids` holds.
    ///
    /// This is the whole of the reconnect model. `run(EMPTY, stream)` is the first pass;
    /// `run(run(EMPTY, stream), stream)` is what the player sees after a reconnect that replays
    /// the set. The two must be equal.
    fn run(start: [Option<i32>; 4], stream: &[i32]) -> [Option<i32>; 4] {
        let mut worn = start;
        let mut ts = TalismanStream::default();
        for &full_id in stream {
            let Some(p) = ts.push(full_id) else { continue };
            let param_id = full_id & 0x0FFF_FFFF;
            if let Some(slot) = slot_for_accessory(p, worn, param_id) {
                let i = ACCESSORY_SLOTS.iter().position(|&s| s == slot).unwrap();
                worn[i] = Some(param_id);
            }
        }
        worn
    }

    const NOTHING_WORN: [Option<i32>; 4] = [None, None, None, None];

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

    #[test]
    fn starting_class_reserve_left_slots_are_cleared_but_right_slots_are_not() {
        // #441 ruling: the challenge has one active left-hand slot. A starting class may populate
        // Left2/3, but the corresponding right slots are outside the cleanup policy.
        let worn = [
            31_330_000,
            20_000_000,
            33_090_000,
            21_000_000,
            41_000_000,
            UNARMED_WEAPON_PARAM_ID,
        ];
        let plan = starting_left_cleanup_plan(worn, 2).unwrap();
        assert_eq!(
            plan.clear_slots,
            vec![SLOT_WEAPON_LEFT_2, SLOT_WEAPON_LEFT_3]
        );
        assert_eq!(plan.unarmed_source, Some(SLOT_WEAPON_RIGHT_3));
        assert!(plan.reset_selector);
        assert!(!plan.clear_slots.contains(&SLOT_WEAPON_RIGHT_2));
        assert!(!plan.clear_slots.contains(&SLOT_WEAPON_RIGHT_3));
    }

    #[test]
    fn an_already_single_left_loadout_is_a_noop() {
        let worn = [
            31_330_000,
            20_000_000,
            UNARMED_WEAPON_PARAM_ID,
            21_000_000,
            UNARMED_WEAPON_PARAM_ID,
            22_000_000,
        ];
        let plan = starting_left_cleanup_plan(worn, 0).unwrap();
        assert!(plan.clear_slots.is_empty());
        assert_eq!(plan.unarmed_source, None);
        assert!(!plan.reset_selector);
    }

    #[test]
    fn a_non_primary_left_selector_is_normalized_even_when_reserves_are_empty() {
        let worn = [
            31_330_000,
            20_000_000,
            UNARMED_WEAPON_PARAM_ID,
            21_000_000,
            UNARMED_WEAPON_PARAM_ID,
            22_000_000,
        ];
        let plan = starting_left_cleanup_plan(worn, 1).unwrap();
        assert!(plan.clear_slots.is_empty());
        assert_eq!(plan.unarmed_source, None);
        assert!(plan.reset_selector);
    }

    #[test]
    fn a_full_six_slot_loadout_is_refused_not_cleared_with_a_guessed_handle() {
        let err = starting_left_cleanup_plan([1, 2, 3, 4, 5, 6], 0).unwrap_err();
        assert_eq!(err, StartingLeftCleanupError::NoUnarmedSource);
    }

    #[test]
    fn missing_sidecar_waits_for_the_save_embedded_fresh_verdict() {
        assert_eq!(
            starting_left_init_action(false, false, None, true, true),
            StartingLeftInitAction::Wait
        );
        assert_eq!(
            starting_left_init_action(false, false, Some(false), true, true),
            StartingLeftInitAction::PreserveAndSettle,
            "a returning character's manually curated Left2/3 must survive"
        );
    }

    #[test]
    fn fresh_cleanup_debt_survives_the_marker_changing_to_resume() {
        assert_eq!(
            starting_left_init_action(false, false, Some(true), true, false),
            StartingLeftInitAction::Arm
        );
        assert_eq!(
            starting_left_init_action(false, true, Some(false), true, false),
            StartingLeftInitAction::Wait,
            "the explicit pending bit, not the later marker verdict, owns the retry"
        );
        assert_eq!(
            starting_left_init_action(false, true, Some(false), true, true),
            StartingLeftInitAction::Attempt
        );
    }

    #[test]
    fn option_off_settles_without_touching_a_loadout() {
        for fresh in [None, Some(false), Some(true)] {
            let expected = if fresh.is_none() {
                StartingLeftInitAction::Wait
            } else {
                StartingLeftInitAction::PreserveAndSettle
            };
            assert_eq!(
                starting_left_init_action(false, false, fresh, false, true),
                expected
            );
        }
        assert_eq!(
            starting_left_init_action(false, true, Some(false), false, true),
            StartingLeftInitAction::PreserveAndSettle
        );
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
        // Both inputs are now unbounded caller-supplied numbers (a pouch count off the stream and
        // a stream ordinal), so sweep both: neither the clamp nor the modulus may leave the four.
        for pouches in 0..=255u8 {
            for ordinal in [0u64, 1, 2, 3, 4, 7, 255, u64::MAX] {
                let all_full = [Some(1), Some(2), Some(3), Some(4)];
                let pos = TalismanPos { ordinal, pouches };
                let slot = slot_for_accessory(pos, all_full, 9999).unwrap();
                assert!(
                    ACCESSORY_SLOTS.contains(&slot),
                    "pouches={pouches} ordinal={ordinal} produced slot {slot}, outside the four \
                     talisman slots"
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

        // NO POUCHES = ONE slot. Every talisman lands in slot 1 whatever its ordinal, and NOT in
        // slot 2 even though slot 2 is empty. This is the state Alaric's whole 0.3.2 session ran
        // in (six talismans, all to slot 17) and 21 of bobler's 22 equips too.
        for ordinal in 0..8u64 {
            assert_eq!(
                slot_for_accessory(pos(ordinal, 0), [None, None, None, None], 1000),
                Some(SLOT_ACCESSORY_1),
                "ordinal {ordinal} with no pouches must stay in the one unlocked slot"
            );
        }
        assert_eq!(
            slot_for_accessory(pos(1, 0), [Some(1000), None, None, None], 2000),
            Some(SLOT_ACCESSORY_1),
            "the LOCKED empties stay untouched -- a locked slot is neither read for occupancy \
             nor written"
        );
        // ONE pouch = two slots, and the rotation uses both -- never the third.
        assert_eq!(
            slot_for_accessory(pos(1, 1), [Some(1000), None, None, None], 2000),
            Some(SLOT_ACCESSORY_2)
        );
        assert_eq!(
            slot_for_accessory(pos(2, 1), [Some(1000), Some(1010), None, None], 2000),
            Some(SLOT_ACCESSORY_1),
            "both unlocked slots full -> the rotation comes back round; slot 3 is still locked"
        );
        assert_eq!(
            slot_for_accessory(pos(3, 1), [Some(1000), Some(1010), None, None], 2000),
            Some(SLOT_ACCESSORY_2),
            "...and back to slot 2. THE FREEZE (#342): under the shipped policy this was slot 1 \
             again and slot 2 never changed for the rest of the run"
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
            slot_for_accessory(pos(1, 1), [Some(1000), None, None, None], 2000),
            Some(SLOT_ACCESSORY_2),
            "with one Talisman Pouch the player has two slots -- filling one and clobbering it is \
             the old off-by-one"
        );
        // three pouches = all four slots reachable
        assert_eq!(
            slot_for_accessory(pos(3, 3), [Some(1000), Some(1010), Some(1020), None], 1030),
            Some(SLOT_ACCESSORY_4),
            "three pouches is the vanilla maximum; slot 4 must be usable"
        );
    }

    /// The stated policy, slot by slot. With four slots unlocked the rotation fills an empty
    /// loadout 1, 2, 3, 4 -- exactly what the old "first empty slot" rule did -- and then keeps
    /// going round instead of parking on slot 1.
    #[test]
    fn talismans_fill_the_slots_in_order_and_then_keep_rotating() {
        let three = 3u8; // three pouches = all four slots
        assert_eq!(
            slot_for_accessory(pos(0, three), [None, None, None, None], 1000),
            Some(SLOT_ACCESSORY_1)
        );
        assert_eq!(
            slot_for_accessory(pos(1, three), [Some(1000), None, None, None], 1010),
            Some(SLOT_ACCESSORY_2)
        );
        assert_eq!(
            slot_for_accessory(pos(2, three), [Some(1000), Some(1010), None, None], 1020),
            Some(SLOT_ACCESSORY_3)
        );
        assert_eq!(
            slot_for_accessory(
                pos(3, three),
                [Some(1000), Some(1010), Some(1020), None],
                1030
            ),
            Some(SLOT_ACCESSORY_4)
        );
        // All four worn: the fifth talisman still gets equipped. Refusing it would be "you may
        // keep the build you have", which is the one thing the ruleset forbids.
        let full = [Some(1000), Some(1010), Some(1020), Some(1030)];
        assert_eq!(
            slot_for_accessory(pos(4, three), full, 1040),
            Some(SLOT_ACCESSORY_1)
        );
        // ...and the SIXTH goes to slot 2, not slot 1 again. That single assertion is #342: under
        // the shipped policy every talisman from the fifth on went to slot 1 and slots 2, 3 and 4
        // froze on whatever arrived 2nd, 3rd and 4th.
        assert_eq!(
            slot_for_accessory(pos(5, three), full, 1050),
            Some(SLOT_ACCESSORY_2),
            "slot 2 must be reachable again once the loadout is full -- the freeze is the bug"
        );
        assert_eq!(
            slot_for_accessory(pos(6, three), full, 1060),
            Some(SLOT_ACCESSORY_3)
        );
        assert_eq!(
            slot_for_accessory(pos(7, three), full, 1070),
            Some(SLOT_ACCESSORY_4)
        );
    }

    /// 🛑 WHAT THE ROTATION GAVE UP, stated so nobody re-adds it by accident.
    ///
    /// The old rule 2 hunted for the first EMPTY unlocked slot, so a gap in the middle -- the
    /// player unequipped slot 2 by hand, or a pouch arrived after slots 1 and 3 were written --
    /// was filled before anything was clobbered. The rotation does not look at occupancy, so the
    /// gap is filled only when the ordinal comes round to it.
    ///
    /// That rule cannot come back. It reads the LIVE LOADOUT, and the live loadout is exactly what
    /// differs between the first pass (slots empty) and every replay (slots full) -- see
    /// `the_342_worked_example_diverges_until_rule_2_becomes_a_rotation`, which measures the
    /// divergence it causes even with a perfectly pure modulus.
    #[test]
    fn a_hand_made_gap_is_not_hunted_for() {
        assert_eq!(
            slot_for_accessory(pos(4, 3), [Some(1000), None, Some(1020), None], 1030),
            Some(SLOT_ACCESSORY_1),
            "ordinal 4 % 4 = slot 1; the empty slot 2 is NOT preferred"
        );
        // The gap does get used -- one ordinal later.
        assert_eq!(
            slot_for_accessory(pos(5, 3), [Some(1000), None, Some(1020), None], 1030),
            Some(SLOT_ACCESSORY_2)
        );
    }

    /// ER does not let you wear the same talisman twice, so re-equipping one already worn would
    /// build a loadout the menu cannot produce. This is the case a plain "first empty slot" policy
    /// gets wrong: with slot 2 free it would happily put a second copy there.
    #[test]
    fn a_talisman_already_worn_is_not_equipped_a_second_time() {
        assert_eq!(
            slot_for_accessory(pos(1, 3), [Some(1000), None, None, None], 1000),
            None
        );
        assert_eq!(
            slot_for_accessory(
                pos(1, 3),
                [Some(1000), Some(1010), Some(1020), Some(1030)],
                1020
            ),
            None
        );
        // ...but a copy sitting in a LOCKED slot is not "worn" -- it cannot be, so the rotation
        // over the ONE unlocked slot wins. No pouches => one unlocked slot, so the `Some(1010)` in
        // slot 2 is unreachable state the game could not have produced and must not suppress the
        // equip.
        assert_eq!(
            slot_for_accessory(pos(0, 0), [None, Some(1010), None, None], 1010),
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

    /// The Talisman Pouch has to be recognised in the received stream by its FullID, so pin the id
    /// the apworld actually ships. `greenfield/eldenring/item_ids.py` on world `main`:
    /// `'Talisman Pouch': 1073751864`, i.e. GOODS (nibble 4) row 10040. If the world ever renumbers
    /// it this fails here rather than silently reporting every player zero pouches.
    #[test]
    fn the_talisman_pouch_is_the_full_id_the_apworld_sends() {
        assert_eq!(TALISMAN_POUCH_FULL_ID, 1_073_751_864);
        assert_eq!(
            TALISMAN_POUCH_FULL_ID as u32 & CATEGORY_MASK,
            er_codec::CATEGORY_GOODS,
            "the pouch is GOODS -- an accessory-nibble id here would make it a talisman"
        );
        assert_eq!(TALISMAN_POUCH_FULL_ID as u32 & !CATEGORY_MASK, 10_040);
        assert!(is_talisman_pouch(TALISMAN_POUCH_FULL_ID));
        // ...and nothing else is. In particular it is NOT equipable, so the pouch never reaches
        // the accessory arm and cannot be mistaken for the talisman it unlocks a slot for.
        assert!(!is_talisman_pouch(ACCESSORY_TALISMAN));
        assert!(!is_talisman_pouch(GOODS_GREAT_RUNE));
        assert!(!is_talisman_pouch(GOODS_FLASK));
        assert_eq!(equipable(TALISMAN_POUCH_FULL_ID), None);
    }

    /// The seam that replaces the live `PlayerGameData.unlocked_talisman_slots` read: pouches are
    /// counted BEFORE the talismans that follow them, and both numbers come out of stream position
    /// alone.
    #[test]
    fn the_stream_says_how_many_pouches_came_before_each_talisman() {
        let mut ts = TalismanStream::default();
        let stream = [
            tal(1000),
            tal(1010),
            TALISMAN_POUCH_FULL_ID,
            tal(1020),
            WEAPON_DAGGER, // everything that is not a talisman or a pouch is ignored...
            PROTECTOR_HELM,
            GOODS_GREAT_RUNE,
            TALISMAN_POUCH_FULL_ID,
            TALISMAN_POUCH_FULL_ID,
            tal(1030),
        ];
        let seen: Vec<Option<TalismanPos>> = stream.iter().map(|&f| ts.push(f)).collect();
        assert_eq!(
            seen,
            vec![
                Some(pos(0, 0)),
                Some(pos(1, 0)),
                None, // pouch
                Some(pos(2, 1)),
                None,
                None,
                None,
                None, // pouch
                None, // pouch
                Some(pos(3, 3)),
            ]
        );
        assert_eq!(ts.pouches(), 3);
        // ...and that is exactly the unlock ladder the old live read produced: 1, 1, 2, 4 slots.
        assert_eq!(
            seen.iter()
                .flatten()
                .map(|p| usable_accessory_slots(p.pouches))
                .collect::<Vec<_>>(),
            vec![1, 1, 2, 4]
        );
    }

    /// 🛑 REJECTED VARIANTS. Kept ONLY to measure what they cost; neither is production code and
    /// neither may be promoted into one. Both keep the shipped rule 2 ("first EMPTY unlocked
    /// slot"); they differ in where the modulus comes from.
    ///
    /// * `live_n = Some(..)` supplies the modulus the way `pgd.unlocked_talisman_slots` did --
    ///   the caller passes the growing ladder for the live pass and the settled value for the
    ///   replay, which is the whole of #342's blocker.
    /// * `live_n = None` derives it from the stream, i.e. the fix #342's comment proposed.
    fn rejected_run(
        start: [Option<i32>; 4],
        stream: &[i32],
        live_n: Option<&[usize]>,
    ) -> [Option<i32>; 4] {
        let mut worn = start;
        let mut ts = TalismanStream::default();
        for &full_id in stream {
            let Some(p) = ts.push(full_id) else { continue };
            let param_id = full_id & 0x0FFF_FFFF;
            let n = match live_n {
                Some(ladder) => ladder[p.ordinal as usize],
                None => usable_accessory_slots(p.pouches),
            };
            if worn[..n].contains(&Some(param_id)) {
                continue; // rule 1, unchanged
            }
            let i = match worn[..n].iter().position(Option::is_none) {
                Some(i) => i,                            // rule 2: first EMPTY unlocked slot
                None => (p.ordinal % n as u64) as usize, // rule 3: rotate
            };
            worn[i] = Some(param_id);
        }
        worn
    }

    /// 🛑 THE MOTIVATING CASE (rule 11), and it is #342's own worked example verbatim: eight
    /// talismans A..H with Talisman Pouches arriving partway through, so the unlock ladder runs
    /// `n = 1, 1, 2, 2, 2, 4, 4, 4`. The issue's table:
    ///
    /// ```text
    /// live    (n = 1, 1, 2, 2, 2, 4, 4, 4)   ->  (E, D, F, H)
    /// replay  (n = 4 throughout)             ->  (E, F, G, H)
    ///                                             ^ diverges
    /// ```
    ///
    /// Both rows are asserted below, so the analysis this change rests on is measured and not
    /// quoted. Then the finding the issue does NOT have: making `n` a pure function of the stream
    /// -- the whole of the proposed fix -- STILL DIVERGES, at slot 3, because rule 2 reads the live
    /// loadout and the live loadout is empty on the first pass and full on every replay. Only
    /// replacing rule 2 with the rotation converges.
    ///
    /// Fails if `slot_for_accessory` ever reads occupancy again for anything but rule 1.
    #[test]
    fn the_342_worked_example_diverges_until_rule_2_becomes_a_rotation() {
        // A B [pouch] C D E [pouch] [pouch] F G H  =>  n = 1,1,2,2,2,4,4,4
        let (a, b, c, d) = (tal(1000), tal(1010), tal(1020), tal(1030));
        let (e, f, g, h) = (tal(1040), tal(1050), tal(1060), tal(1070));
        let p = TALISMAN_POUCH_FULL_ID;
        let stream = [a, b, p, c, d, e, p, p, f, g, h];
        let row = |x: i32| Some(x & 0x0FFF_FFFF);
        let ladder_live = [1usize, 1, 2, 2, 2, 4, 4, 4];
        let ladder_replay = [4usize; 8];

        // ROW 1 + 2 -- the issue's table, reproduced.
        let live = rejected_run(NOTHING_WORN, &stream, Some(&ladder_live));
        assert_eq!(
            live,
            [row(e), row(d), row(f), row(h)],
            "#342's live row is (E, D, F, H)"
        );
        let replayed = rejected_run(live, &stream, Some(&ladder_replay));
        assert_eq!(
            replayed,
            [row(e), row(f), row(g), row(h)],
            "#342's replay row is (E, F, G, H)"
        );
        assert_ne!(live, replayed, "this IS the reported divergence");

        // ROW 3 -- `n` derived from the stream, rule 2 kept. The modulus is now a pure function of
        // replayed inputs and the loadout STILL rearranges: F, which reached slot 3 through the
        // empty-slot rule, is displaced by G on replay.
        let live2 = rejected_run(NOTHING_WORN, &stream, None);
        let replayed2 = rejected_run(live2, &stream, None);
        assert_eq!(live2, [row(e), row(d), row(f), row(h)]);
        assert_eq!(replayed2, [row(e), row(d), row(g), row(h)]);
        assert_ne!(
            live2, replayed2,
            "a pure modulus is NOT sufficient -- rule 2 reads the live loadout, and that is what \
             differs between the first pass and the replay"
        );

        // ROW 4 -- what ships. Same stream, same ladder, and the loadout is a fixed point.
        let shipped = run(NOTHING_WORN, &stream);
        assert_eq!(
            shipped,
            [row(e), row(f), row(g), row(h)],
            "the rotation gives the player the four most recent talismans, which is what rule 2's \
             own rationale asked for and stopped delivering once the slots filled"
        );
        assert_eq!(
            run(shipped, &stream),
            shipped,
            "RECONNECT MUST NOT REARRANGE THE LOADOUT"
        );
        assert_eq!(run(run(shipped, &stream), &stream), shipped);
    }

    /// 🛑 THE ACCEPTANCE TEST, the same shape as `physick::replaying_the_received_set_converges`
    /// but swept over the thing #342 says cannot be swept: the pouch schedule.
    ///
    /// Every stream of 1..=8 distinct talismans, crossed with every placement of 0..=3 Talisman
    /// Pouches among them (710 cases), must be a FIXED POINT of the replay. The pouch count is a
    /// pure function of stream position, so the ladder is identical on both passes -- which is
    /// precisely what a live `unlocked_talisman_slots` read cannot promise.
    ///
    /// Fails for a modulus taken from the live field, and fails for rule 2 (see the worked example
    /// above for both).
    #[test]
    fn replaying_the_received_set_converges_for_every_pouch_schedule() {
        let mut cases = 0usize;
        for len in 1..=8usize {
            let tals: Vec<i32> = (0..len).map(|i| tal(1000 + 10 * i as i32)).collect();
            for pouches in 0..=3usize {
                // Place `pouches` pouches into the `len + 1` gaps, with repeats -- every
                // multiset of gap indices, i.e. every schedule.
                for combo in gap_combinations(len + 1, pouches) {
                    let at = |gap: usize| {
                        std::iter::repeat_n(
                            TALISMAN_POUCH_FULL_ID,
                            combo.iter().filter(|&&g| g == gap).count(),
                        )
                    };
                    let mut stream: Vec<i32> = Vec::new();
                    for (gap, &t) in tals.iter().enumerate() {
                        stream.extend(at(gap));
                        stream.push(t);
                    }
                    stream.extend(at(len));
                    let live = run(NOTHING_WORN, &stream);
                    let replayed = run(live, &stream);
                    assert_eq!(
                        live, replayed,
                        "reconnect rearranged the loadout for stream {stream:?}"
                    );
                    // ...and it must not drift on the third or fourth connect either.
                    assert_eq!(live, run(run(live, &stream), &stream));
                    cases += 1;
                }
            }
        }
        assert_eq!(cases, 710, "the sweep must not silently shrink");
    }

    /// Non-decreasing multisets of `k` values drawn from `0..slots` -- every distinct way to place
    /// `k` indistinguishable pouches into `slots` gaps.
    fn gap_combinations(slots: usize, k: usize) -> Vec<Vec<usize>> {
        if k == 0 {
            return vec![Vec::new()];
        }
        let mut out = Vec::new();
        let mut cur = vec![0usize; k];
        loop {
            out.push(cur.clone());
            // odometer over non-decreasing tuples
            let mut i = k;
            loop {
                if i == 0 {
                    return out;
                }
                i -= 1;
                if cur[i] + 1 < slots {
                    let v = cur[i] + 1;
                    for c in cur.iter_mut().skip(i) {
                        *c = v;
                    }
                    break;
                }
                if i == 0 {
                    return out;
                }
            }
        }
    }

    /// The one case the rotation does NOT converge for, stated so it is a known trade and not a
    /// surprise: the SAME talisman arriving twice in the stream. Rule 1 then fires against a slot
    /// the rotation was not about to write, and the no-op is a real diversion rather than an
    /// equivalent.
    ///
    /// Rule 1 wins that argument anyway. ER refuses duplicate talismans, so writing the row into a
    /// second slot builds a loadout the player could not have made in the menu -- a visible, wrong
    /// state -- whereas the divergence permutes items the player already has. The apworld places
    /// each talisman once, so this needs a foreign world to send ours twice.
    #[test]
    fn a_duplicate_talisman_is_still_refused_a_second_slot() {
        let t = tal(1000);
        // Worn in slot 1, two slots unlocked, ordinal points at slot 2: still None.
        assert_eq!(
            slot_for_accessory(
                pos(1, 1),
                [Some(t & 0x0FFF_FFFF), None, None, None],
                t & 0x0FFF_FFFF
            ),
            None,
            "a talisman already worn must never be given a second slot, convergence or not"
        );
    }
}
