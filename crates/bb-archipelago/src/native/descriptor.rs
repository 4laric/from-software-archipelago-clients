//! The native `ItemGrant` source object: 24 meaningful bytes, 32 staged.
//!
//! A faithful port of `tools/bb_native_delivery/descriptor.py` in the world
//! repo. Layout is `validated` (live inventory dump): raw descriptor at `+0x00`,
//! an internal pointer at `+0x08` (game-filled; staged zero), normalized id at
//! `+0x10`. The harness stages 32 bytes because the Cheat Engine template also
//! zeroes `+0x04` and `+0x14`; only the three named fields carry meaning.
//!
//! `normalized = 0x40000000 | goods_id` and `raw = 0xB0000000 | goods_id` hold
//! for the validated category-4 goods canaries only; they are `inferred` as a
//! general formula and must never be used to synthesise an equipment
//! descriptor. The prefixes themselves come from the contract, not this file.

use anyhow::{Result, bail};

use super::contract::DescriptorFormula;

/// Category 4 (goods): the in-frame stack source, 24 bytes materialized in the
/// consume frame. Category 0 (equipment): the persistent descriptor cell. Both
/// are validated; nothing else is.
pub const CATEGORY_GOODS: u8 = 4;
pub const CATEGORY_EQUIPMENT: u8 = 0;
pub const CATEGORY_ARMOR: u8 = 1;

/// A staged descriptor pair. `raw_id`/`normalized_id` are the two u32s the cave
/// reads; the rest of the 32-byte staging area is zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ItemGrantDescriptor {
    pub raw_id: u32,
    pub normalized_id: u32,
}

impl ItemGrantDescriptor {
    pub fn new(raw_id: u32, normalized_id: u32) -> Self {
        Self {
            raw_id,
            normalized_id,
        }
    }

    /// `(raw, normalized)` for a category-4 goods id, using the contract's
    /// prefixes. Fails closed if the id does not fit the low 28 bits.
    pub fn for_goods(formula: &DescriptorFormula, goods_id: u32) -> Result<Self> {
        if goods_id > 0x0FFF_FFFF {
            bail!("goods id {goods_id:#x} is out of range");
        }
        Ok(Self {
            raw_id: formula.goods_raw_prefix | goods_id,
            normalized_id: formula.goods_normalized_prefix | goods_id,
        })
    }

    /// The `staged_size` (32) bytes the harness writes into the descriptor cell:
    /// raw at +0x00, zero at +0x04, an eight-byte zero internal pointer at
    /// +0x08, normalized at +0x10, zero at +0x14.
    pub fn encode(&self, formula: &DescriptorFormula) -> Vec<u8> {
        let mut buffer = vec![0u8; formula.staged_size];
        buffer[0x00..0x04].copy_from_slice(&self.raw_id.to_le_bytes());
        buffer[0x10..0x14].copy_from_slice(&self.normalized_id.to_le_bytes());
        buffer
    }

    /// Recover a descriptor from at least `size` (24) staged bytes.
    pub fn decode(formula: &DescriptorFormula, data: &[u8]) -> Result<Self> {
        if data.len() < formula.size {
            bail!("need {} bytes, got {}", formula.size, data.len());
        }
        Ok(Self {
            raw_id: u32::from_le_bytes(data[0x00..0x04].try_into().unwrap()),
            normalized_id: u32::from_le_bytes(data[0x10..0x14].try_into().unwrap()),
        })
    }

    /// The Bloodborne item category this descriptor pair encodes, or `None`
    /// when the pair matches neither validated shape.
    ///
    /// The category is not a separate field on the wire: it *is* the high
    /// nibble of the normalized id, and the contract's two prefix pairs are
    /// exactly the two categories the client supports. Goods carry
    /// `normalized = goods_normalized_prefix | id` (`0x4...`, i.e. category 4)
    /// with `raw = goods_raw_prefix | id`; equipment carries a normalized id
    /// with a zero high nibble (category 0) and
    /// `raw = persistent_source_marker | id`. These are the same two pairings
    /// `NativeGrantRequest::into_command` validates against the *declared*
    /// category, so this derivation cannot disagree with the declared one for
    /// any command that reaches the delivery machine -- which is why the
    /// machine can derive it instead of carrying the byte down.
    ///
    /// Anything else is `None` and must be treated as **not stackable**: an
    /// unrecognised pair is exactly the case where guessing "stack" would add a
    /// delta into a field that is not a quantity (clients#451).
    pub fn category(&self, formula: &DescriptorFormula) -> Option<u8> {
        let raw_prefix = self.raw_id & 0xF000_0000;
        let normalized_prefix = self.normalized_id & 0xF000_0000;
        if raw_prefix == formula.goods_raw_prefix
            && normalized_prefix == formula.goods_normalized_prefix
        {
            return Some(CATEGORY_GOODS);
        }
        if raw_prefix == formula.persistent_source_marker && normalized_prefix == 0 {
            return Some(CATEGORY_EQUIPMENT);
        }
        if raw_prefix == formula.persistent_source_marker + 0x1000_0000
            && normalized_prefix == 0x1000_0000
        {
            return Some(CATEGORY_ARMOR);
        }
        None
    }

    /// True only for a category the inventory actually *stacks*, and so the
    /// only case where the cave's existing-stack delta branch is meaningful.
    ///
    /// Category 4 (goods) stacks. Category 0 (equipment) does not: a weapon is
    /// an instance record whose "quantity" position is not a count, so a second
    /// Hunter Pistol is a second INSTANCE, never `+1` on the first -- which is
    /// the point of the Uncanny/Lost design, and is equally true of a plain
    /// duplicate. Armour, when it arrives, joins the equipment side of this
    /// test, not the goods side. Unknown pairs fail closed to non-stackable.
    pub fn is_stackable_category(&self, formula: &DescriptorFormula) -> bool {
        self.category(formula) == Some(CATEGORY_GOODS)
    }

    /// True when the cave takes the `lea rsi,[descriptor]` (persistent,
    /// equipment) branch: `raw & 0xF0000000 == persistent_source_marker`.
    pub fn uses_persistent_source(&self, formula: &DescriptorFormula) -> bool {
        matches!(
            self.category(formula),
            Some(CATEGORY_EQUIPMENT) | Some(CATEGORY_ARMOR)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::contract::contract;
    use super::*;

    fn formula() -> DescriptorFormula {
        contract().descriptor
    }

    #[test]
    fn goods_descriptor_uses_the_contract_prefixes() {
        // Pebble 0x4CE is a validated category-4 canary.
        let d = ItemGrantDescriptor::for_goods(&formula(), 0x4CE).unwrap();
        assert_eq!(d.raw_id, 0xB000_04CE);
        assert_eq!(d.normalized_id, 0x4000_04CE);
        assert!(!d.uses_persistent_source(&formula()));
    }

    #[test]
    fn out_of_range_goods_id_is_refused() {
        assert!(ItemGrantDescriptor::for_goods(&formula(), 0x1000_0000).is_err());
    }

    #[test]
    fn encode_matches_the_24_meaningful_plus_8_zero_staging() {
        let f = formula();
        let d = ItemGrantDescriptor::new(0xB000_0384, 0x4000_0384); // Bullets
        let bytes = d.encode(&f);
        assert_eq!(bytes.len(), 32);
        assert_eq!(&bytes[0x00..0x04], &0xB000_0384u32.to_le_bytes());
        assert_eq!(&bytes[0x04..0x08], &[0, 0, 0, 0]); // padding
        assert_eq!(&bytes[0x08..0x10], &[0; 8]); // internal pointer staged zero
        assert_eq!(&bytes[0x10..0x14], &0x4000_0384u32.to_le_bytes());
        assert_eq!(&bytes[0x14..0x20], &[0; 12]);
        // Round-trips.
        assert_eq!(ItemGrantDescriptor::decode(&f, &bytes).unwrap(), d);
    }

    #[test]
    fn saw_spear_is_the_persistent_equipment_branch() {
        // Saw Spear raw 0x806C5660 is the sole validated equipment row.
        let d = ItemGrantDescriptor::new(0x806C_5660, 0x006C_5660);
        assert!(d.uses_persistent_source(&formula()));
    }

    #[test]
    fn reviewed_armor_shape_is_persistent_and_non_stackable() {
        let f = formula();
        let armor = ItemGrantDescriptor::new(0x9000_2AF8, 0x1000_2AF8);
        assert_eq!(armor.category(&f), Some(CATEGORY_ARMOR));
        assert!(armor.uses_persistent_source(&f));
        assert!(!armor.is_stackable_category(&f));
    }

    #[test]
    fn category_is_derived_from_the_descriptor_pair() {
        let f = formula();
        let goods = ItemGrantDescriptor::for_goods(&f, 0x4CE).unwrap();
        assert_eq!(goods.category(&f), Some(CATEGORY_GOODS));
        assert!(goods.is_stackable_category(&f));

        // Saw Spear, the validated equipment row.
        let weapon = ItemGrantDescriptor::new(0x806C_5660, 0x006C_5660);
        assert_eq!(weapon.category(&f), Some(CATEGORY_EQUIPMENT));
        assert!(!weapon.is_stackable_category(&f));
    }

    #[test]
    fn an_unrecognised_pair_is_never_stackable() {
        let f = formula();
        // Goods raw prefix with an equipment-shaped normalized id: neither
        // validated pairing, so it must not be allowed onto the delta lane.
        let odd = ItemGrantDescriptor::new(0xB000_0384, 0x0000_0384);
        assert_eq!(odd.category(&f), None);
        assert!(!odd.is_stackable_category(&f));
    }

    #[test]
    fn decode_rejects_a_short_buffer() {
        assert!(ItemGrantDescriptor::decode(&formula(), &[0u8; 8]).is_err());
    }
}
