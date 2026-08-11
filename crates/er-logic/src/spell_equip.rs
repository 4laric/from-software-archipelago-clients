//! `spell_equip` -- which MEMORY SLOT a received sorcery or incantation should be written into.
//!
//! Pure, no I/O, no game deps, deterministic over its arguments -- same discipline as
//! [`crate::auto_equip`] and [`crate::physick`]. The caller supplies the game-side facts we cannot
//! read without the game (the spell's `MagicParam` id and its `slotLength`); the slot COUNT is
//! counted off the AP received stream, not read off the live character, for the reason
//! [`crate::auto_equip::TalismanStream`] gives.
//!
//! 🛑 **NOT to be named `attunement`.** `crate::attunement` is already taken, by the region
//! attunement-release gate, which has nothing to do with spells. The collision is a trap for
//! anyone grepping.
//!
//! ## Where the slots live (MEASURED IN GAME, build 1.16.2, 2026-08-10)
//!
//! The pinned `eldenring` crate (`fromsoftware-rs`, `crates/eldenring/src/cs/player_game_data.rs`)
//! gives the layout:
//!
//! ```text
//! EquipGameData.equip_magic_data : OwnedPtr<EquipMagicData>
//! EquipMagicData { vftable; equip_game_data; entries: [EquipMagicItem; 14]; selected_slot: i32 }
//! EquipMagicItem { param_id: i32, charges: i32 }
//! ```
//!
//! and the live addresses, found by signature search and confirmed by locating the class twice in
//! one process at the same relative offset:
//!
//! ```text
//! PlayerGameData + 0x2B0 = EquipGameData          (stride 0xC00 per player slot)
//!   EquipGameData + 0x280 = OwnedPtr<EquipMagicData>
//!     EquipMagicData + 0x10 = entries[14], stride 8, param_id first
//!     EquipMagicData + 0x80 = selected_slot
//! ```
//!
//! So the live half needs no AOB, no RVA and no new RE -- it is a typed field access off a pointer
//! the client already resolves for the `GameDataMan` healthbar read.
//!
//! 🛑🛑 **An earlier revision of this file called the layout "confirmed twice, independently" and
//! gave the hop as `+0x518`. Both claims were wrong.** `+0x518` is 0x18 short and lands in an
//! object of UTF-16 strings and vtables; the Hexinton `Memory Slot 1..14` rows use it and read the
//! same garbage on 1.16.2. And the two sources never corroborated each other in the first place --
//! the crate describes a STRUCT LAYOUT, the CE table describes a POINTER PATH, so agreeing that
//! `entries` sits at `+0x10` says nothing about whether that path reaches an `EquipMagicData`. Two
//! sources describing different halves of a claim cannot disagree, so their agreement was never
//! evidence.
//!
//! 🛑 **More than one `EquipMagicData` is live.** The owner stride `0xC00` is the co-op / second
//! player slot. Resolve the instance owned by `EquipGameData` index 0, not the first one found; a
//! naive "take the first signature hit" ships a bug that only appears in co-op.
//!
//! ⭐ **The live write is ONE field: `entries[n].param_id`.** Measured, not assumed: `charges` is
//! inert (`{id, 0}` casts identically to the `{id, -1}` the game itself writes), and `selected_slot`
//! is the game's own cursor, which self-corrects without being written. A spell written this way
//! displays, casts immediately with no menu or grace round-trip, and survives a grace save plus
//! Alt+F4 and reload.
//!
//! ## ⭐⭐⭐ MEMORY SLOTS ARE ITEMS, NOT A STAT
//!
//! **Mind is FP.** It does not grant memory slots -- that is a Dark Souls 3 carry-over (Attunement
//! was both) and ER split them. Slots come from **Memory Stones**, of which there are 8, plus the
//! 2 every class starts with. The Moon of Nokstella talisman adds 2 more while equipped, for a
//! vanilla ceiling of 12 against a 14-entry array.
//!
//! That is the whole design, because a Memory Stone is an **AP item**: the slot count at any point
//! is a function of position in the received stream, exactly like the Talisman Pouch in
//! [`crate::auto_equip::TalismanStream`]. So the modulus is pure, and #342's convergence property
//! ports intact -- a reconcile replay of the received set lands on the same loadout as the live
//! pass. (An earlier draft of this module assumed Mind drove the count and concluded the modulus
//! could not be pure. It was wrong; the ruling it asked for is not owed.)
//!
//! 🛑 **The Moon of Nokstella +2 is deliberately NOT counted.** It is a talisman, so its bonus
//! applies only while equipped, and under auto-equip's own rotation a later talisman can clobber it
//! out again -- which would make `n` shrink and rearrange every spell already placed. Counting only
//! `2 + stones` keeps `n` MONOTONIC and matches the crate's own split between
//! `unlocked_magic_slots` (stones) and `effective_unlocked_magic_slots` (stones + talisman). The
//! cost is that two real slots go untargeted; the failure direction is a wasted slot, never an
//! illegal write, which is the same trade the talisman clamp already takes.
//!
//! ## Classification -- the query has now been run
//!
//! An earlier revision left `is_spell()` and `is_memory_stone()` out on purpose, because naming a
//! `goodsType` from memory is how this repo bought `wep_type 59` (which matched every staff and no
//! seal). The query has since been run against the committed `gen_inputs.db` bundle
//! (`EquipParamGoods.csv` join `Magic.csv` join `GoodsName.fmg`, base + both DLC), so the constants
//! below carry their counts and their source. See [`SPELL_GOODS_TYPES`],
//! [`MEMORY_STONE_GOODS_ROW`], and -- for the one that would have bitten us --
//! [`magic_row_for_spell_goods`].

/// `EquipMagicData.entries` length. From the crate declaration, not from a count of CE rows.
pub const MAGIC_SLOTS: usize = 14;

/// Memory slots every class starts with, before any Memory Stone.
pub const BASE_MAGIC_SLOTS: u8 = 2;

/// Memory Stones in the game. 8 stones + [`BASE_MAGIC_SLOTS`] = 10 without the Nokstella talisman.
pub const MEMORY_STONES: u8 = 8;

/// `EquipParamGoods.goodsType` values that denote a memorisable spell.
///
/// **FOUR types, not two** -- each school splits attack from support. Datamined 2026-08-10, named
/// rows with `sortId != 999999`:
///
/// | goodsType | rows | school |
/// |---|---|---|
/// | 5 | 69 | sorcery, attack (Glintstone Pebble, Comet) |
/// | 17 | 15 | sorcery, support (Terra Magica, Scholar's Armament) |
/// | 16 | 87 | incantation, attack (Catch Flame, Death Lightning) |
/// | 18 | 42 | incantation, support (Flame, Cleanse Me) |
///
/// 84 sorceries + 129 incantations = **213** memorisable spells.
///
/// 🛑 A two-type guess would have silently dropped **57** spells -- the same
/// umbrella-narrower-than-its-members shape as `wep_type 59`. And the stat requirement is NOT a
/// substitute discriminator: 14 of the 69 type-5 sorceries carry a faith requirement, and 4 of the
/// 87 type-16 incantations carry an intelligence one.
pub const SPELL_GOODS_TYPES: [u8; 4] = [5, 16, 17, 18];

/// `EquipParamGoods` row for Memory Stone.
///
/// It is ONE id with `maxNum == 8`, not eight ids -- which is why [`MEMORY_STONES`] is a count and
/// [`SpellStream::push_memory_stone`] takes no argument.
pub const MEMORY_STONE_GOODS_ROW: u32 = 10_030;

/// Is this goods row a memorisable spell?
///
/// Takes the two param fields rather than reading them, exactly as [`crate::physick::is_tear`]
/// does. `sort_id` is required, not optional, for the same reason it is required there: the type
/// alone admits FromSoft's unused rows.
pub fn is_spell(goods_type: u8, sort_id: i32) -> bool {
    SPELL_GOODS_TYPES.contains(&goods_type) && sort_id != crate::physick::UNUSED_SORT_ID
}

/// Does receiving this goods row raise the memory-slot ceiling?
pub fn is_memory_stone(goods_row: u32) -> bool {
    goods_row == MEMORY_STONE_GOODS_ROW
}

/// The `MagicParam` id a memory slot must hold, given a spell's goods row.
///
/// ⭐ **It is the IDENTITY.** `EquipParamGoods.ID == Magic.ID` for all 213 spells, zero exceptions.
/// There is no reference field to follow -- `refId_default` is NOT it and resolves for none of
/// them. Confirmed against ground truth from the in-game probe: Catch Flame is goods `6000`, and
/// the live memory slot held `MagicParam` `6000`.
///
/// 🛑🛑 **The identity does NOT license the converse.** Goods `8000` is a **Stonesword Key**
/// (`goodsType 1`), and `Magic.csv` also carries a row `8000` -- so the obvious shortcut, "the
/// goods id resolves in `Magic.csv`, therefore it is a spell", files every Stonesword Key in the
/// pool as a spell. It is the ONLY such collision among named non-spell goods, which is precisely
/// what would let it survive a spot-check. Callers MUST gate on [`is_spell`] first; this function
/// is deliberately total and will happily convert a Stonesword Key.
pub fn magic_row_for_spell_goods(goods_row: u32) -> u32 {
    goods_row
}

/// Where a received spell sits in the AP received stream, and how many Memory Stones had arrived by
/// then. Mirrors [`crate::auto_equip::TalismanPos`] exactly, and for the same reason: both fields
/// are functions of position in the stream, so the slot choice replays identically.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpellPos {
    /// 0-based index of this spell among the spells received this session.
    pub ordinal: u64,
    /// Memory Stones received at or before this point in the stream.
    pub stones: u8,
}

/// Counts the received stream into [`SpellPos`] values. Mirrors
/// [`crate::auto_equip::TalismanStream`].
///
/// The caller decides what a spell and what a Memory Stone are -- this type will not guess a
/// `goodsType`. Push in received order; a stone contributes capacity to every spell after it and to
/// none before it, which is what makes the replay converge.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpellStream {
    spells: u64,
    stones: u8,
}

impl SpellStream {
    /// Record a Memory Stone. Saturates at [`MEMORY_STONES`]: a ninth stone is a modded or
    /// duplicated grant and must not widen the write range.
    pub fn push_memory_stone(&mut self) {
        self.stones = (self.stones + 1).min(MEMORY_STONES);
    }

    /// Record a received spell and return its position in the stream.
    pub fn push_spell(&mut self) -> SpellPos {
        let pos = SpellPos {
            ordinal: self.spells,
            stones: self.stones,
        };
        self.spells += 1;
        pos
    }

    /// Memory Stones seen so far.
    pub fn stones(&self) -> u8 {
        self.stones
    }
}

/// How many memory slots the rotation may target, given the stones received so far.
///
/// Deliberately excludes the Moon of Nokstella +2 -- see the module docs. Clamped to
/// [`MAGIC_SLOTS`] because the array is 14 long and a larger value could only come from a modded
/// or duplicated stone grant.
pub fn usable_magic_slots(stones: u8) -> usize {
    (BASE_MAGIC_SLOTS as usize + stones.min(MEMORY_STONES) as usize).min(MAGIC_SLOTS)
}

/// The memory-slot index a received spell should occupy, or `None` when there is nothing to do.
///
/// `slots[i]` is the `MagicParam` id currently in memory slot `i`, or `None` if that slot is empty.
///
/// ## The policy, and where it comes from
///
/// This is the French Challenge ruling applied, not a new one. *"All auto-equipped gear must stay
/// equipped ... no unequipping allowed"* means the answer to a full loadout is **where** the new
/// spell lands, never **whether** it is equipped. So there is no "leave it in the bag", no "only if
/// the player can cast it", and no better-spell comparison -- that is choosing your build with
/// extra steps.
///
/// 1. **Already memorised -> `None`.** ER will not let you put one spell in two slots from the
///    menu, so writing it twice builds a loadout the player could not have made. Same argument as
///    [`crate::auto_equip::slot_for_accessory`] rule 1.
/// 2. **Otherwise `ordinal % n`**, `n = usable_magic_slots(pos.stones)` -- a ROTATION, not "first
///    empty slot, then clobber the lowest". #342 measured what the latter costs on talismans: 21 of
///    bobler's 22 equips went to one slot, because at one unlocked slot "all slots full" is true
///    from the second item onward. The rotation does not read occupancy at all, which is exactly
///    why it converges for every stream, every stone schedule and every prefix length.
///
/// There is no capacity-zero branch, and there does not need to be: every class starts with
/// [`BASE_MAGIC_SLOTS`], so unlike a talisman-slot count this one is never 0. A spell always has
/// somewhere to go.
pub fn slot_for_spell(pos: SpellPos, slots: &[Option<i32>], magic_id: i32) -> Option<u32> {
    if already_memorised(pos, slots, magic_id) {
        return None;
    }
    Some((pos.ordinal % usable_magic_slots(pos.stones) as u64) as u32)
}

/// Is `magic_id` already in a slot this character can use?
///
/// ⭐ This is the ONLY reason [`slot_for_spell`] answers `None`, factored out so a caller can NAME
/// that reason without re-deriving it. The client needs to: a `None` there is a DROP (the pending
/// queue has already been taken), and "already memorised, nothing to do" and "we failed to place a
/// spell and it is now gone" are the same silence from outside. Re-implementing the check
/// client-side would be a second source of truth for one decision, and it would agree with itself
/// right up until one of the two moved.
///
/// Occupancy is read only over the prefix that both exists and is unlocked. `n` stays the modulus
/// either way: a caller that hands us a short slice is telling us less than it knows about
/// occupancy, not less than it knows about capacity.
pub fn already_memorised(pos: SpellPos, slots: &[Option<i32>], magic_id: i32) -> bool {
    let n = usable_magic_slots(pos.stones);
    slots[..n.min(slots.len())].contains(&Some(magic_id))
}

/// Which school a spell belongs to, and therefore which catalyst can cast it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum School {
    /// Cast from a Glintstone Staff.
    Sorcery,
    /// Cast from a Sacred Seal.
    Incantation,
}

/// School from `EquipParamGoods.goodsType`.
///
/// ⭐ THE DISCRIMINATOR IS ALREADY IN THE DATA. [`SPELL_GOODS_TYPES`] is four values because each
/// school splits attack from support, so the split this needs is the same one that classifier
/// already makes -- 5/17 sorcery, 16/18 incantation. Nothing new is datamined and no id list is
/// introduced.
///
/// 🛑 NOT THE STAT REQUIREMENT. The obvious-looking shortcut ("faith requirement means
/// incantation") is measured wrong: **14 of the 69 type-5 sorceries carry a faith requirement, and
/// 4 of the 87 type-16 incantations carry an intelligence one.** That is the note already on
/// [`SPELL_GOODS_TYPES`], and it rules the shortcut out on evidence rather than on taste.
pub fn school_of(goods_type: u8) -> Option<School> {
    match goods_type {
        5 | 17 => Some(School::Sorcery),
        16 | 18 => Some(School::Incantation),
        _ => None,
    }
}

/// `EquipParamWeapon.wepType` for a Glintstone Staff. 19 rows.
pub const WEP_TYPE_STAFF: u16 = 57;
/// `EquipParamWeapon.wepType` for a Sacred Seal. 10 rows.
///
/// 🛑 61, NOT 59. `wep_type 59` has zero rows and shipping a classifier against it is a mistake
/// this repo has already made once -- see [`crate::auto_equip::hand_for_wep_type`]'s tests.
pub const WEP_TYPE_SEAL: u16 = 61;

/// What the player is holding, as far as casting is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Catalysts {
    pub staff: bool,
    pub seal: bool,
}

impl Catalysts {
    /// Fold the worn weapon rows' `wepType`s into a catalyst picture. `None` entries are slots that
    /// hold nothing, or weapons whose row could not be read.
    pub fn from_wep_types(worn: impl IntoIterator<Item = Option<u16>>) -> Self {
        let mut c = Self::default();
        for t in worn.into_iter().flatten() {
            if t == WEP_TYPE_STAFF {
                c.staff = true;
            } else if t == WEP_TYPE_SEAL {
                c.seal = true;
            }
        }
        c
    }

    /// Is the player holding nothing that casts anything? Then there is no preference to express
    /// and every spell is treated alike.
    pub fn none(self) -> bool {
        !self.staff && !self.seal
    }

    /// Can what the player is holding cast this school?
    pub fn can_cast(self, school: School) -> bool {
        match school {
            School::Sorcery => self.staff,
            School::Incantation => self.seal,
        }
    }
}

/// Order a backfill batch so spells the player can actually cast take slots first.
///
/// # What this is, and the ruling it must not break
///
/// er-archipelago#549: *"A character holding a seal should not have `auto_equip` fill its slots
/// with sorceries it cannot cast, and today nothing looks."*
///
/// 🛑 IT IS A PREFERENCE, NOT A FILTER, AND THE DIFFERENCE IS A STANDING RULING.
/// [`slot_for_spell`]'s doc rules the filter out in as many words: *"there is no 'leave it in the
/// bag', no 'only if the player can cast it', and no better-spell comparison -- that is choosing
/// your build with extra steps."* That is the French Challenge ruling, it governs the RECEIVE path,
/// and #549 asking for catalyst awareness does not repeal it.
///
/// So nothing is ever withheld. Castable spells are simply offered the free slots first, which is
/// the whole of the complaint when slots are scarce -- a seal build with two memory slots and a
/// stream full of sorceries. When there is room for everything, everything still lands, and the
/// caller names the ones the current catalyst cannot cast rather than hiding them. "Not silently"
/// was the actual ask.
///
/// ⚠️ The player can swap catalysts at any moment, which would reorder a *future* batch. That is
/// fine and is why this only reorders -- a filter would mean a spell's fate depended on what was in
/// your left hand the tick a pass happened to run.
///
/// Stable within each group, so the stream order (and therefore the ordinals) still decides ties.
/// 🛑 `None` school means "we could not read it", and it is treated as CASTABLE. Deprioritising an
/// item because a param read failed would make placement depend on a transient, and a transient
/// that only bites on a slow load is the worst kind of nondeterminism to chase.
pub fn prefer_castable<T: Copy>(batch: &[(T, Option<School>)], held: Catalysts) -> Vec<T> {
    if held.none() {
        return batch.iter().map(|(t, _)| *t).collect();
    }
    let castable = |s: &Option<School>| s.is_none_or(|s| held.can_cast(s));
    let mut out: Vec<T> = batch
        .iter()
        .filter(|(_, s)| castable(s))
        .map(|(t, _)| *t)
        .collect();
    out.extend(batch.iter().filter(|(_, s)| !castable(s)).map(|(t, _)| *t));
    out
}

/// What the BACKFILL pass should do with one spell (er-archipelago#549).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backfill {
    /// It is already in a usable slot. Nothing to do, and the common case on every pass after the
    /// first -- this is what makes the pass converge.
    AlreadyMemorised,
    /// Write it to `slot`. `home` is whether that is the slot its stream ordinal names; `false`
    /// means its own slot was taken and this is the first free one instead.
    Place { slot: u32, home: bool },
    /// Every usable slot is occupied by a DIFFERENT spell. 🛑 Nothing is evicted -- see the type
    /// doc on why that is the whole point.
    NoRoom { usable: usize },
}

/// Where a spell the player ALREADY OWNS but has never memorised should go -- **filling only,
/// never evicting**.
///
/// # The defect this closes
///
/// er-archipelago#549. `auto_equip` acts on arrival, once, and the receive cursor is persisted per
/// save. boblerrr's Rotten Breath (13:14) and Ranni's Dark Moon (13:36) arrived under a 0.3.10
/// build; the 0.3.11 session that could have memorised them opened at `recv: stream=413
/// cursor=413`, already caught up, so `enqueue_spell` was never called for either. They sit in the
/// bag unmemorised and, without this, always would.
///
/// ⭐ THE ORDINAL IS NOT INVENTED, AND THAT IS THE WHOLE DESIGN. Alaric, 2026-08-11: *"we should be
/// doing the ordering based on the archipelago stream of received items, since that never
/// changes"*. He is right, and it is already computed: `core.rs`'s receive loop folds
/// [`SpellStream`] over the **whole** stream every tick and builds `spell_pos` for every spell ever
/// received -- then throws the below-watermark half away, because only items past the watermark
/// reach `enqueue_spell`. So a stranded spell's [`SpellPos`] exists, is correct, and is stable
/// forever: the server fixes the stream order, and this fold is pure over it.
///
/// That kills every alternative that was on the table. No tail-append, no `magic_id` ordering, no
/// persisted assignment, no new ordinal semantics -- and the bag is never an ORDER, only a
/// membership test, so an in-game inventory re-sort cannot move anything.
///
/// # 🛑 WHY IT NEVER EVICTS
///
/// Ruled 2026-08-11 (Alaric: *"backfill only, agreed that the major risk here is looping"*).
///
/// [`slot_for_spell`] OVERWRITES -- `ordinal % n` is the French Challenge ruling applied, and for a
/// RECEIVE that is correct: the answer to a full loadout is where the spell lands, never whether it
/// is equipped. A pass that re-ran that policy would be a different animal. Two spells whose
/// ordinals are congruent mod `n` would trade one slot back and forth on every pass, forever --
/// each pass "fixing" what the last one did. That is `residue 306` wearing a new hat, and it is the
/// failure mode this whole family has hit before.
///
/// Filling only makes convergence a property of the code rather than a thing to test for: **every
/// pass either occupies one more slot or does nothing**, occupancy is bounded by
/// [`usable_magic_slots`], so the pass terminates and then stays silent. Nothing the player had is
/// ever taken away, which also means this can never be the cause of a "my spell vanished" report.
///
/// The cost is honest and bounded: a spell whose own slot is taken lands in a free one instead
/// (`home: false`), so a backfilled spell's position is not always the one its ordinal names. The
/// receive path's policy is untouched.
pub fn backfill_slot(pos: SpellPos, slots: &[Option<i32>], magic_id: i32) -> Backfill {
    if already_memorised(pos, slots, magic_id) {
        return Backfill::AlreadyMemorised;
    }
    let n = usable_magic_slots(pos.stones).min(slots.len());
    if n == 0 {
        return Backfill::NoRoom { usable: 0 };
    }
    // Its own slot first, so a backfilled spell agrees with the receive path wherever it can.
    let home = (pos.ordinal % n as u64) as usize;
    if slots[home].is_none() {
        return Backfill::Place {
            slot: home as u32,
            home: true,
        };
    }
    // Otherwise the lowest free slot. Lowest rather than nearest so the choice is total and
    // reproducible from the slot array alone -- two clients reading the same state must agree.
    match slots[..n].iter().position(Option::is_none) {
        Some(i) => Backfill::Place {
            slot: i as u32,
            home: false,
        },
        None => Backfill::NoRoom { usable: n },
    }
}

/// Does a spell of cost `slot_length` fit a character with `stones` Memory Stones at all?
///
/// ✅ **RULING 2026-08-10: the client writes `slot_length = 1` across all 213 spells** -- one byte
/// at `MagicParam +0x21`, adjacent to the requirement bytes the existing `no_weapon_requirements`
/// path already writes. In a shipped run this therefore returns `true` for every spell and the
/// modulus in [`slot_for_spell`] is exact, with no bin-packing.
///
/// It is kept, rather than deleted, because it is the honest predicate for an UNPATCHED character
/// -- 24 of the 213 cost more than one slot, the maximum being 3 (Comet Azur, Placidusax's Ruin,
/// Scarlet Aeonia) -- and because that param write is a deliberate GLOBAL one whose absence should
/// fail loudly here rather than silently misplace a spell.
///
/// 🛑 Measured: the game itself does NOT enforce capacity. A spell written to slot 5 on a
/// two-slot character is accepted, castable, and survives a reload. The clamp is entirely ours.
pub fn fits(slot_length: u8, stones: u8) -> bool {
    slot_length as usize <= usable_magic_slots(stones)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE MOTIVATING CASE (CONTRIBUTING rule 11). Goods `8000` is a Stonesword Key, and
    /// `Magic.csv` carries a row `8000` as well. The obvious classifier -- "the goods id resolves
    /// in `Magic.csv`" -- files every Stonesword Key in the item pool as a spell.
    #[test]
    fn a_stonesword_key_is_not_a_spell_despite_colliding_with_a_magic_row() {
        // goods 8000 Stonesword Key: goodsType 1, sortId 203090.
        assert!(!is_spell(1, 203_090));
        // The identity is total and WILL convert it, which is exactly why is_spell() is the gate.
        assert_eq!(magic_row_for_spell_goods(8000), 8000);
    }

    #[test]
    fn the_two_support_schools_are_not_forgotten() {
        // 57 of the 213 live in these two types -- a "sorceries and incantations"
        // guess drops every one of them.
        assert!(is_spell(17, 300_000), "support sorcery");
        assert!(is_spell(18, 300_000), "support incantation");
    }

    #[test]
    fn catch_flame_classifies_and_maps_to_the_id_seen_in_game() {
        // goods 6000 Catch Flame: goodsType 16, sortId 306000. The memory slot held 6000.
        assert!(is_spell(16, 306_000));
        assert_eq!(magic_row_for_spell_goods(6000), 6000);
    }

    #[test]
    fn unused_rows_are_rejected_the_way_physick_rejects_them() {
        let unused = crate::physick::UNUSED_SORT_ID;
        for gt in SPELL_GOODS_TYPES {
            assert!(!is_spell(gt, unused), "goodsType {gt}");
        }
    }

    #[test]
    fn non_spell_goods_types_are_rejected() {
        for gt in [0u8, 1, 2, 3, 7, 8, 9, 10, 11, 12, 14, 15] {
            assert!(!is_spell(gt, 100), "goodsType {gt} classified as a spell");
        }
    }

    #[test]
    fn memory_stone_is_one_id_held_eight_times() {
        assert!(is_memory_stone(MEMORY_STONE_GOODS_ROW));
        assert!(!is_memory_stone(8000));
        let mut s = SpellStream::default();
        for _ in 0..20 {
            s.push_memory_stone();
        }
        assert_eq!(s.stones(), MEMORY_STONES, "the stone count must saturate");
    }

    fn empty(n: usize) -> Vec<Option<i32>> {
        vec![None; n]
    }

    fn pos(ordinal: u64, stones: u8) -> SpellPos {
        SpellPos { ordinal, stones }
    }

    /// 🛑 THE POINT OF FACTORING IT OUT: the client logs "already memorised, nothing to do" vs
    /// "DROPPED, nothing retries it" by asking `already_memorised`, while the placement decision is
    /// made by `slot_for_spell`. If those two ever disagree, the log tells a player the opposite of
    /// what happened -- the worst outcome available, because a wrong line is trusted where silence
    /// is merely unhelpful. This pins them to one another over the whole small space.
    #[test]
    fn already_memorised_is_exactly_when_no_slot_is_returned() {
        let ids = [-1_i32, 100, 200];
        let mut checked = 0;
        for stones in 0..=MEMORY_STONES {
            for ordinal in 0..6u64 {
                for a in ids {
                    for b in ids {
                        for c in ids {
                            let slots: Vec<Option<i32>> =
                                [a, b, c].iter().map(|&i| (i != -1).then_some(i)).collect();
                            let p = SpellPos { ordinal, stones };
                            for magic_id in [100, 200, 300] {
                                assert_eq!(
                                    slot_for_spell(p, &slots, magic_id).is_none(),
                                    already_memorised(p, &slots, magic_id),
                                    "disagreed at stones={stones} ordinal={ordinal} \
                                     slots={slots:?} magic_id={magic_id}"
                                );
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
        // WITNESS (test_gf_vacuous_pass's rule, applied by hand in Rust): a loop that ran zero
        // times would pass every assertion above and prove nothing.
        assert!(checked > 1000, "only {checked} cases -- the scan collapsed");
    }

    #[test]
    fn every_character_starts_with_two_slots() {
        // Mind is FP; slots are items. A fresh character has 2, never 0, so there is no
        // "nowhere to put it" case for spells the way there is for a locked talisman slot.
        assert_eq!(usable_magic_slots(0), 2);
        assert_eq!(slot_for_spell(pos(0, 0), &empty(14), 3000), Some(0));
        assert_eq!(slot_for_spell(pos(1, 0), &empty(14), 3001), Some(1));
        assert_eq!(slot_for_spell(pos(2, 0), &empty(14), 3002), Some(0));
    }

    #[test]
    fn each_stone_adds_exactly_one_slot() {
        for s in 0..=MEMORY_STONES {
            assert_eq!(usable_magic_slots(s), 2 + s as usize, "stones {s}");
        }
        assert_eq!(usable_magic_slots(MEMORY_STONES), 10);
    }

    #[test]
    fn a_ninth_stone_cannot_widen_the_write_range() {
        assert_eq!(usable_magic_slots(9), 10);
        assert_eq!(usable_magic_slots(255), 10);
    }

    #[test]
    fn the_nokstella_bonus_is_not_counted() {
        // Vanilla ceiling with the talisman is 12, but the rotation targets 10. Documented
        // undershoot: the failure direction is a wasted slot, never an out-of-range write.
        assert_eq!(usable_magic_slots(MEMORY_STONES), 10);
        assert!(usable_magic_slots(MEMORY_STONES) < MAGIC_SLOTS);
    }

    #[test]
    fn consecutive_spells_fill_consecutive_slots() {
        let slots = empty(14);
        for i in 0..6u64 {
            assert_eq!(
                slot_for_spell(pos(i, 4), &slots, 3000 + i as i32),
                Some(i as u32)
            );
        }
    }

    #[test]
    fn the_rotation_wraps_instead_of_parking_on_slot_one() {
        // THE MOTIVATING CASE (CONTRIBUTING rule 11), ported from #342: "first empty, else clobber
        // the lowest" froze 21 of bobler's 22 talismans into one slot. A rotation alternates.
        let slots = empty(14);
        let seq: Vec<u32> = (0..6)
            .map(|i| slot_for_spell(pos(i, 0), &slots, 3000 + i as i32).unwrap())
            .collect();
        assert_eq!(seq, vec![0, 1, 0, 1, 0, 1]);
    }

    #[test]
    fn a_spell_already_memorised_is_left_alone() {
        let mut slots = empty(14);
        slots[1] = Some(3000);
        assert_eq!(slot_for_spell(pos(0, 2), &slots, 3000), None);
    }

    #[test]
    fn occupancy_beyond_the_unlocked_count_is_never_read() {
        let mut slots = empty(14);
        slots[9] = Some(3000);
        assert_eq!(
            slot_for_spell(pos(0, 0), &slots, 3000),
            Some(0),
            "slot 9 is outside the two base slots and must be invisible"
        );
    }

    #[test]
    fn a_locked_slot_is_never_written() {
        let slots = empty(14);
        for stones in 0..=MEMORY_STONES {
            let n = usable_magic_slots(stones);
            for i in 0..40u64 {
                let got = slot_for_spell(pos(i, stones), &slots, 3000).unwrap();
                assert!(
                    (got as usize) < n,
                    "stones {stones}, ordinal {i} wrote slot {got}"
                );
            }
        }
    }

    #[test]
    fn the_stream_counts_stones_only_for_spells_after_them() {
        let mut s = SpellStream::default();
        let a = s.push_spell();
        s.push_memory_stone();
        let b = s.push_spell();
        assert_eq!(a, pos(0, 0));
        assert_eq!(b, pos(1, 1));
        assert_eq!(s.stones(), 1);
    }

    #[test]
    fn the_stream_saturates_at_eight_stones() {
        let mut s = SpellStream::default();
        for _ in 0..20 {
            s.push_memory_stone();
        }
        assert_eq!(s.stones(), MEMORY_STONES);
        assert_eq!(usable_magic_slots(s.push_spell().stones), 10);
    }

    #[test]
    fn slot_length_gate_is_separate_from_the_slot_choice() {
        assert!(fits(1, 0));
        assert!(fits(2, 0));
        assert!(!fits(3, 0)); // two base slots
        assert!(fits(3, 1));
    }

    /// ⭐⭐⭐ THE ACCEPTANCE TEST. Because a Memory Stone is an AP item, `n` is a function of the
    /// stream, so replaying the received set reproduces the live loadout exactly -- the property
    /// #342 had to engineer for talismans, inherited here for free.
    ///
    /// The live pass sees stones arrive partway through; the replay sees the same stream from the
    /// start. Both must agree, for every interleaving.
    #[test]
    fn replaying_the_received_set_converges_for_every_stone_schedule() {
        // `schedule[i]` = how many stones have arrived before spell i.
        let schedules: [&[u8]; 5] = [
            &[0, 0, 0, 0, 0, 0, 0, 0],
            &[0, 0, 1, 1, 2, 2, 3, 3],
            &[0, 1, 2, 3, 4, 5, 6, 7],
            &[8, 8, 8, 8, 8, 8, 8, 8],
            &[0, 0, 0, 0, 8, 8, 8, 8],
        ];
        for schedule in schedules {
            // Live: slots fill up as we go.
            let mut live_slots = empty(14);
            let mut live = Vec::new();
            for (i, &stones) in schedule.iter().enumerate() {
                let magic = 3000 + i as i32;
                let got = slot_for_spell(pos(i as u64, stones), &live_slots, magic);
                if let Some(s) = got {
                    live_slots[s as usize] = Some(magic);
                }
                live.push(got);
            }
            // Replay: the reconciler walks the same stream again from an empty loadout.
            let mut replay_slots = empty(14);
            let mut replay = Vec::new();
            for (i, &stones) in schedule.iter().enumerate() {
                let magic = 3000 + i as i32;
                let got = slot_for_spell(pos(i as u64, stones), &replay_slots, magic);
                if let Some(s) = got {
                    replay_slots[s as usize] = Some(magic);
                }
                replay.push(got);
            }
            assert_eq!(live, replay, "schedule {schedule:?}");
            assert_eq!(
                live_slots, replay_slots,
                "final loadout, schedule {schedule:?}"
            );
        }
    }

    // ---------------------------------------------------------------------------------------
    // BACKFILL (er-archipelago#549) -- fill only, never evict.
    // ---------------------------------------------------------------------------------------

    /// THE MOTIVATING CASE (rule 11). boblerrr's Rotten Breath and Ranni's Dark Moon arrived under
    /// 0.3.10; the 0.3.11 session opened with the cursor already caught up, so nothing ever placed
    /// them. Their SpellPos was computed on every tick of that session and thrown away.
    #[test]
    fn a_spell_stranded_below_the_watermark_is_placed_at_the_slot_its_ordinal_names() {
        let slots = [None, None, None, None];
        assert_eq!(
            backfill_slot(pos(0, 2), &slots, 4001),
            Backfill::Place {
                slot: 0,
                home: true
            }
        );
        assert_eq!(
            backfill_slot(pos(3, 2), &slots, 4002),
            Backfill::Place {
                slot: 3,
                home: true
            }
        );
    }

    #[test]
    fn a_taken_home_slot_falls_to_the_lowest_free_one_and_says_it_did() {
        // Ordinal 1 wants slot 1; something else is there.
        let slots = [None, Some(9999), None, None];
        assert_eq!(
            backfill_slot(pos(1, 2), &slots, 4001),
            Backfill::Place {
                slot: 0,
                home: false
            },
            "a backfilled spell may land off its ordinal -- that is the stated cost of never evicting"
        );
    }

    /// 🛑 THE ANTI-LOOP PROPERTY. If this ever returns Place over an occupied slot, two spells whose
    /// ordinals are congruent mod n trade it forever and the pass never converges (`residue 306`).
    #[test]
    fn a_full_rack_is_left_completely_alone() {
        // 🛑 `stones` is the MEMORY STONE COUNT, not a slot count: BASE_MAGIC_SLOTS is 2, so
        // `stones: 0` is two usable slots and `stones: 2` is four. Getting that backwards is how
        // this test was wrong on its first run.
        let two_full = [Some(1), Some(2), None, None];
        assert_eq!(
            backfill_slot(pos(0, 0), &two_full, 4001),
            Backfill::NoRoom { usable: 2 },
            "no stones = two usable slots, so the free slots BEYOND them are not room"
        );
        let slots = [Some(1), Some(2), Some(3), Some(4)];
        assert_eq!(
            backfill_slot(pos(0, 2), &slots, 4001),
            Backfill::NoRoom { usable: 4 }
        );
        assert_eq!(
            backfill_slot(pos(7, 2), &slots, 4001),
            Backfill::NoRoom { usable: 4 }
        );
    }

    #[test]
    fn an_already_memorised_spell_is_a_no_op_which_is_what_makes_the_pass_settle() {
        let slots = [Some(4001), None, None, None];
        assert_eq!(
            backfill_slot(pos(1, 2), &slots, 4001),
            Backfill::AlreadyMemorised
        );
        // ... and only within the slots this character can actually USE. A spell parked in a slot
        // beyond the unlocked count is not memorised for this character.
        let far = [None, None, Some(4001), None];
        assert!(matches!(
            backfill_slot(pos(0, 0), &far, 4001),
            Backfill::Place { .. }
        ));
    }

    /// ⭐⭐⭐ THE CONVERGENCE GATE. #549's fourth acceptance case: run the pass ten times with
    /// nothing new and get ZERO writes. Non-convergence is the failure mode this family has hit
    /// before, so it is asserted directly rather than hoped for.
    #[test]
    fn ten_passes_over_the_same_stream_write_once_and_then_never_again() {
        // Eight stranded spells, four usable slots (2 stones), ordinals chosen so several are
        // congruent mod 4 -- the exact shape that would oscillate under an evicting policy.
        let stream: Vec<(i32, SpellPos)> =
            (0..8u64).map(|i| (4000 + i as i32, pos(i, 2))).collect();
        let mut slots = [None::<i32>; MAGIC_SLOTS];
        let mut writes_per_pass = Vec::new();

        for _ in 0..10 {
            let mut writes = 0;
            for (magic_id, p) in &stream {
                match backfill_slot(*p, &slots, *magic_id) {
                    Backfill::Place { slot, .. } => {
                        assert!(
                            slots[slot as usize].is_none(),
                            "backfill must NEVER be handed an occupied slot -- that is the loop"
                        );
                        slots[slot as usize] = Some(*magic_id);
                        writes += 1;
                    }
                    Backfill::AlreadyMemorised | Backfill::NoRoom { .. } => {}
                }
            }
            writes_per_pass.push(writes);
        }

        assert_eq!(
            writes_per_pass[0], 4,
            "the first pass fills the four usable slots"
        );
        assert!(
            writes_per_pass[1..].iter().all(|&w| w == 0),
            "passes 2..10 must be SILENT, got {writes_per_pass:?}"
        );
    }

    /// The same property stated as an invariant rather than a count: occupancy only ever grows, and
    /// no slot's occupant is ever replaced. A future change that reintroduces eviction fails here
    /// even if it happens to converge for the case above.
    #[test]
    fn backfill_is_monotonic_over_every_ordering_and_never_displaces_an_occupant() {
        for stones in 0..=MEMORY_STONES {
            let n = usable_magic_slots(stones);
            let mut slots = [None::<i32>; MAGIC_SLOTS];
            // Seed a couple of slots so the pass has to work around real occupants.
            slots[0] = Some(7001);
            if n > 2 {
                slots[2] = Some(7002);
            }
            let before = slots;
            let mut occupied = slots[..n].iter().filter(|s| s.is_some()).count();
            for i in 0..40u64 {
                let magic_id = 4000 + i as i32;
                if let Backfill::Place { slot, .. } =
                    backfill_slot(pos(i, stones), &slots, magic_id)
                {
                    assert!(
                        slots[slot as usize].is_none(),
                        "stones={stones} displaced an occupant"
                    );
                    slots[slot as usize] = Some(magic_id);
                    occupied += 1;
                }
                assert!(occupied <= n, "stones={stones} occupied {occupied} of {n}");
            }
            // The pre-existing occupants are untouched, whatever else happened.
            assert_eq!(slots[0], before[0], "stones={stones}");
            if n > 2 {
                assert_eq!(slots[2], before[2], "stones={stones}");
            }
        }
    }

    /// The receive path is deliberately NOT changed by any of this: it still overwrites, because
    /// that is the French Challenge ruling. Pinned so the two policies cannot quietly merge.
    #[test]
    fn the_receive_path_still_overwrites_while_backfill_does_not() {
        // No stones -> exactly two usable slots, both occupied.
        let slots = [Some(9001), Some(9002), None, None];
        assert_eq!(
            slot_for_spell(pos(0, 0), &slots, 4001),
            Some(0),
            "receive still lands on ordinal % n even when that slot is taken"
        );
        assert_eq!(
            backfill_slot(pos(0, 0), &slots, 4001),
            Backfill::NoRoom { usable: 2 },
            "backfill refuses the same write -- this divergence IS the fix"
        );
    }

    // ---------------------------------------------------------------------------------------
    // CATALYST-AWARE ROUTING (er-archipelago#549).
    // ---------------------------------------------------------------------------------------

    #[test]
    fn every_spell_goods_type_has_a_school_and_nothing_else_does() {
        assert_eq!(school_of(5), Some(School::Sorcery));
        assert_eq!(school_of(17), Some(School::Sorcery));
        assert_eq!(school_of(16), Some(School::Incantation));
        assert_eq!(school_of(18), Some(School::Incantation));
        // 🛑 THE COVERAGE GATE. SPELL_GOODS_TYPES is the classifier the rest of the client uses; a
        // type in it with no school would route silently wrong, which is the exact
        // umbrella-narrower-than-its-members shape that shipped `wep_type 59`.
        for t in SPELL_GOODS_TYPES {
            assert!(
                school_of(t).is_some(),
                "goodsType {t} is a spell with no school"
            );
        }
        for t in [0u8, 1, 2, 4, 6, 10, 15, 19, 255] {
            assert_eq!(school_of(t), None, "goodsType {t} is not a spell");
        }
    }

    #[test]
    fn a_catalyst_is_read_from_wep_type_not_from_an_id_list() {
        let seal = Catalysts::from_wep_types([Some(WEP_TYPE_SEAL), None]);
        assert!(seal.seal && !seal.staff);
        assert!(seal.can_cast(School::Incantation));
        assert!(!seal.can_cast(School::Sorcery));

        let staff = Catalysts::from_wep_types([None, Some(WEP_TYPE_STAFF)]);
        assert!(staff.can_cast(School::Sorcery) && !staff.can_cast(School::Incantation));

        // Both hands full: a hybrid casts everything and has no preference to express.
        let both = Catalysts::from_wep_types([Some(WEP_TYPE_STAFF), Some(WEP_TYPE_SEAL)]);
        assert!(both.can_cast(School::Sorcery) && both.can_cast(School::Incantation));
        assert!(!both.none());

        // A greatsword is not a catalyst, and neither is an unreadable row.
        let melee = Catalysts::from_wep_types([Some(3), None]);
        assert!(melee.none(), "wep_type 3 is not a catalyst");
        assert!(Catalysts::from_wep_types([None, None]).none());
        // 🛑 59 is the wrong seal type -- it has zero rows and must classify as nothing.
        assert!(Catalysts::from_wep_types([Some(59), None]).none());
    }

    /// THE MOTIVATING CASE (rule 11), #549 acceptance 3: a character holding a SEAL receives a
    /// sorcery and an incantation, and the incantation is the one that gets the slot.
    #[test]
    fn a_seal_build_fills_its_slots_with_incantations_first() {
        let batch = [
            (4001, Some(School::Sorcery)),
            (4002, Some(School::Incantation)),
            (4003, Some(School::Sorcery)),
            (4004, Some(School::Incantation)),
        ];
        let seal = Catalysts::from_wep_types([Some(WEP_TYPE_SEAL), None]);
        assert_eq!(
            prefer_castable(&batch, seal),
            vec![4002, 4004, 4001, 4003],
            "incantations first, and stream order preserved WITHIN each group"
        );
    }

    /// 🛑 THE RULING THIS MUST NOT BREAK. slot_for_spell's doc: "there is no 'leave it in the bag',
    /// no 'only if the player can cast it'". Preference is allowed; withholding is not.
    #[test]
    fn nothing_is_ever_withheld_however_the_catalyst_is_held() {
        let batch = [
            (1, Some(School::Sorcery)),
            (2, Some(School::Incantation)),
            (3, Some(School::Sorcery)),
        ];
        for held in [
            Catalysts::from_wep_types([Some(WEP_TYPE_SEAL), None]),
            Catalysts::from_wep_types([Some(WEP_TYPE_STAFF), None]),
            Catalysts::from_wep_types([Some(WEP_TYPE_STAFF), Some(WEP_TYPE_SEAL)]),
            Catalysts::default(),
        ] {
            let out = prefer_castable(&batch, held);
            assert_eq!(out.len(), batch.len(), "a spell was DROPPED for {held:?}");
            let mut sorted = out.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, vec![1, 2, 3], "the set changed for {held:?}");
        }
    }

    #[test]
    fn an_unreadable_school_is_treated_as_castable_never_deprioritised() {
        let batch = [
            (1, None),
            (2, Some(School::Sorcery)),
            (3, Some(School::Incantation)),
        ];
        let seal = Catalysts::from_wep_types([Some(WEP_TYPE_SEAL), None]);
        assert_eq!(
            prefer_castable(&batch, seal),
            vec![1, 3, 2],
            "an unread school keeps its place among the castable -- a failed param read must not \
             decide a loadout"
        );
    }

    #[test]
    fn no_catalyst_means_no_reordering_at_all() {
        let batch = [
            (9, Some(School::Sorcery)),
            (8, Some(School::Incantation)),
            (7, Some(School::Sorcery)),
        ];
        assert_eq!(
            prefer_castable(&batch, Catalysts::default()),
            vec![9, 8, 7],
            "a player holding no catalyst has no preference to express, so the stream order stands"
        );
    }

    #[test]
    fn a_hybrid_reorders_nothing_because_it_can_cast_everything() {
        let batch = [
            (9, Some(School::Sorcery)),
            (8, Some(School::Incantation)),
            (7, Some(School::Sorcery)),
        ];
        let both = Catalysts::from_wep_types([Some(WEP_TYPE_STAFF), Some(WEP_TYPE_SEAL)]);
        assert_eq!(prefer_castable(&batch, both), vec![9, 8, 7]);
    }

    /// Preference + fill-only still converges: reordering changes WHICH spell takes a slot, never
    /// how many passes it takes.
    #[test]
    fn preference_does_not_reintroduce_the_loop() {
        let batch: Vec<(i32, Option<School>)> = (0..6)
            .map(|i| {
                (
                    4000 + i,
                    Some(if i % 2 == 0 {
                        School::Sorcery
                    } else {
                        School::Incantation
                    }),
                )
            })
            .collect();
        let seal = Catalysts::from_wep_types([Some(WEP_TYPE_SEAL), None]);
        let order = prefer_castable(&batch, seal);
        let mut slots = [None::<i32>; MAGIC_SLOTS];
        let mut writes = Vec::new();
        for _ in 0..10 {
            let mut w = 0;
            for (ord, magic_id) in order.iter().enumerate() {
                let p = SpellPos {
                    ordinal: ord as u64,
                    stones: 0,
                };
                if let Backfill::Place { slot, .. } = backfill_slot(p, &slots, *magic_id) {
                    assert!(slots[slot as usize].is_none());
                    slots[slot as usize] = Some(*magic_id);
                    w += 1;
                }
            }
            writes.push(w);
        }
        assert_eq!(writes[0], 2, "two usable slots with no stones");
        assert!(
            writes[1..].iter().all(|&w| w == 0),
            "must still settle: {writes:?}"
        );
        // And the two that got them are the castable ones.
        assert_eq!(slots[0], Some(4001));
        assert_eq!(slots[1], Some(4003));
    }
}
