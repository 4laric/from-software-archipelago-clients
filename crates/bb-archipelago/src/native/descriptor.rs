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

    /// True when the cave takes the `lea rsi,[descriptor]` (persistent,
    /// equipment) branch: `raw & 0xF0000000 == persistent_source_marker`.
    pub fn uses_persistent_source(&self, formula: &DescriptorFormula) -> bool {
        (self.raw_id & 0xF000_0000) == formula.persistent_source_marker
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
    fn decode_rejects_a_short_buffer() {
        assert!(ItemGrantDescriptor::decode(&formula(), &[0u8; 8]).is_err());
    }
}
