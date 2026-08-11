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
    let n = usable_magic_slots(pos.stones);
    // Occupancy is read only over the prefix that both exists and is unlocked. `n` stays the
    // modulus either way: a caller that hands us a short slice is telling us less than it knows
    // about occupancy, not less than it knows about capacity.
    let visible = &slots[..n.min(slots.len())];

    if visible.contains(&Some(magic_id)) {
        return None;
    }
    Some((pos.ordinal % n as u64) as u32)
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
        // 57 of the 213 live in these two types; a "sorceries and incantations" guess drops them.
        assert!(is_spell(17, 300_000), "type 17 (support sorcery) must classify");
        assert!(is_spell(18, 300_000), "type 18 (support incantation) must classify");
    }

    #[test]
    fn catch_flame_classifies_and_maps_to_the_id_seen_in_game() {
        // goods 6000 Catch Flame: goodsType 16, sortId 306000. The memory slot held 6000.
        assert!(is_spell(16, 306_000));
        assert_eq!(magic_row_for_spell_goods(6000), 6000);
    }

    #[test]
    fn unused_rows_are_rejected_the_way_physick_rejects_them() {
        for gt in SPELL_GOODS_TYPES {
            assert!(!is_spell(gt, crate::physick::UNUSED_SORT_ID), "goodsType {gt}");
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
}
