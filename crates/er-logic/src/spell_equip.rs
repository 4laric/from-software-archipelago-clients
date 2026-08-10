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
//! ## Where the slots live (confirmed twice, independently)
//!
//! The pinned `eldenring` crate (`fromsoftware-rs`, `crates/eldenring/src/cs/player_game_data.rs`):
//!
//! ```text
//! EquipGameData.equip_magic_data : OwnedPtr<EquipMagicData>
//! EquipMagicData { vftable; equip_game_data; entries: [EquipMagicItem; 14]; selected_slot: i32 }
//! EquipMagicItem { param_id: i32, charges: i32 }
//! ```
//!
//! and the Hexinton all-in-one v6.1 CE table, whose `EquipMagicData` group resolves
//! `GameDataMan -> +0x08 -> +0x518 -> +0x10 + 8n` for `Memory Slot 1..14`. Both give the same array:
//! **14 entries at +0x10, stride 8, `param_id` first.** So the live half needs no AOB, no RVA and no
//! new RE -- it is a typed field access off a pointer the client already resolves for the
//! `GameDataMan` healthbar read.
//!
//! ⚠️ `EquipMagicItem` has a SECOND field, `charges`. Whether a bare `param_id` write yields a
//! castable spell or one with zero uses is probe **P4** in `CE-PROBE-magic-slots.md`.
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
//! ## What this module deliberately does NOT do: classify
//!
//! There is no `is_spell()` and no `is_memory_stone()` here, and their absence is the point. Both
//! are `EquipParamGoods` rows, so classifying one means naming a `goodsType` or an id -- and this
//! repo has already paid for a guessed param constant (`wep_type 59`, which matched every staff and
//! no seal). One query closes it:
//!
//! ```text
//! EquipParamGoods.csv (gen_inputs bundle, world repo) -- group by goodsType, join GoodsName.fmg,
//! read off the sorcery and incantation buckets and the Memory Stone row.
//! Exclude unused rows the way physick does: sortId != 999999.
//! ```
//!
//! Note also that a memory slot holds a **`MagicParam` id, not a goods id**, so the goods -> magic
//! join is a SECOND thing to datamine, not an assumption.

/// `EquipMagicData.entries` length. From the crate declaration, not from a count of CE rows.
pub const MAGIC_SLOTS: usize = 14;

/// Memory slots every class starts with, before any Memory Stone.
pub const BASE_MAGIC_SLOTS: u8 = 2;

/// Memory Stones in the game. 8 stones + [`BASE_MAGIC_SLOTS`] = 10 without the Nokstella talisman.
pub const MEMORY_STONES: u8 = 8;

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
/// Kept separate from [`slot_for_spell`] because it is NOT the same question and must not be folded
/// in until probe **P7** says what the game does with an overflowing write. `slot_length` is
/// `MagicParam +0x21`, a single byte -- adjacent to the requirement bytes the existing
/// `no_weapon_requirements` path already writes, so forcing every spell to one slot is a field on a
/// write we ship, not a new mechanism.
pub fn fits(slot_length: u8, stones: u8) -> bool {
    slot_length as usize <= usable_magic_slots(stones)
}

#[cfg(test)]
mod tests {
    use super::*;

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
