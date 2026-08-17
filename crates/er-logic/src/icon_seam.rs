//! Can the AP flower be spliced into Elden Ring's icon atlas at RUNTIME, so the bundle stops
//! carrying 8 MB of FromSoft's texture to deliver 25 KB of ours?
//!
//! # The question
//!
//! `me3/ap-package/menu/{hi,low}/01_common.tpf.dcx` is the game's **4096x2048 SB_Icon atlas** with
//! one cell repainted. It is ~99.9% FromSoft data by area, `docs/AP-ICON-PIPELINE.md` keeps it out
//! of the public repo for exactly that reason, and it is why the release needs a private repo, a
//! secret, and a hard packaging gate. If the flower could be written into the atlas after the game
//! loads it, all of that goes away -- and so does
//! [the me3-only gap](https://github.com/4laric/er-archipelago/issues/602), because a splice inside
//! our own DLL runs wherever the DLL loads, not only where `[[packages]]` is honoured.
//!
//! # ⭐ THE PAYLOAD HALF IS ALREADY FREE, AND IT IS AN ACCIDENT OF THE PACKING
//!
//! The layout puts the sprite at `x=2132 y=1148 w=160 h=160`. The atlas is block-compressed, and
//! **all three numbers divide by 4** -- so in BC space the sprite is a clean 40x40 grid of blocks.
//! It can be compressed ONCE, offline, from our own art, and written in with a strided memcpy: no
//! recompression, no decode, and nothing of FromSoft's in what we ship. See [`Splice`].
//!
//! 🛑 THAT ONLY HOLDS AT MIP 0. At mip 1 the sprite starts at 1066,574 and `1066 % 4 == 2`, so the
//! region straddles blocks and a splice there would need real recompression.
//! [`Splice::deepest_aligned_mip`] is that fact as a function, and the probe prints it.
//!
//! # What this module is
//!
//! The pure half. It computes the splice geometry and classifies a module list; it reads no game
//! memory. The former `eldenring_archipelago::icon_seam_probe` was the I/O half. Bobler's
//! 2026-08-17 playtest found `oo2core_6_win64.dll` loaded and confirmed the geometry below:
//! decompression is reachable, the mip-0 payload is 25,600 bytes, and no lower mip is block-aligned.
//! That default-on probe is retired; repeating those constants at every player produces no evidence.
//!
//! 🛑 THE SEAM ITSELF IS NOT REACHABLE YET, and that is the finding, not an omission.
//! `fromsoftware-rs` binds `FD4ResCap`, `FD4ResCapHolder`, `FD4ResRep` and `DLFileDeviceManager`,
//! but its Elden Ring crate exposes **32 singletons** and not one of them is a file device, a
//! resource repository or a texture manager. The types exist; the entry point does not. Getting one
//! means a pointer chase or an AOB scan. The next useful experiments are visual: whether the loader
//! accepts a non-KRAK DCX, and whether a mip-0-only flower remains correct at every UI scale.

/// Bytes per 4x4 block. BC1 is 8; every other BC format the atlases use is 16.
pub const BC_BLOCK_BYTES_DEFAULT: usize = 16;

/// Where the sprite lives in `SB_Icon_00`, from two real probe runs against the game
/// (`tools/build_ap_icon.py`, both bundles agreed). 🛑 The tool re-reads this from the layout every
/// run and so should anything that acts on it -- the atlas is arbitrarily packed and 2132 is not a
/// multiple of 160, so a grid model is wrong here whatever cell size you pick.
pub const SPRITE_X: u32 = 2132;
pub const SPRITE_Y: u32 = 1148;
pub const SPRITE_W: u32 = 160;
pub const SPRITE_H: u32 = 160;
pub const ATLAS_W: u32 = 4096;
pub const ATLAS_H: u32 = 2048;

/// A strided block-copy into a block-compressed mip surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Splice {
    /// Byte offset of the first block, from the start of the mip surface.
    pub offset: usize,
    /// Bytes to copy per block-row.
    pub row_bytes: usize,
    /// Distance between the starts of consecutive block-rows.
    pub row_stride: usize,
    /// Number of block-rows.
    pub rows: usize,
}

impl Splice {
    /// Total bytes the payload occupies -- i.e. how much of OUR art we would have to ship.
    pub fn payload_bytes(&self) -> usize {
        self.row_bytes * self.rows
    }
}

/// A sprite rect inside a block-compressed atlas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sprite {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub atlas_w: u32,
    pub block_bytes: usize,
}

impl Sprite {
    /// The shipped geometry.
    pub const fn shipped() -> Self {
        Self {
            x: SPRITE_X,
            y: SPRITE_Y,
            w: SPRITE_W,
            h: SPRITE_H,
            atlas_w: ATLAS_W,
            block_bytes: BC_BLOCK_BYTES_DEFAULT,
        }
    }

    /// This sprite as it appears at mip `level` (0 = full size).
    pub fn at_mip(&self, level: u32) -> Self {
        Self {
            x: self.x >> level,
            y: self.y >> level,
            w: self.w >> level,
            h: self.h >> level,
            atlas_w: self.atlas_w >> level,
            block_bytes: self.block_bytes,
        }
    }

    /// Whether the rect lands on 4x4 block boundaries.
    ///
    /// 🛑 THIS IS THE WHOLE QUESTION. Aligned means the sprite can be replaced by copying whole
    /// blocks, so our art can be pre-compressed and no game data is ever decoded, edited or
    /// re-encoded. Unaligned means partial blocks, which means decompress-edit-recompress, which
    /// means Oodle and a completely different amount of work.
    pub fn block_aligned(&self) -> bool {
        self.x.is_multiple_of(4)
            && self.y.is_multiple_of(4)
            && self.w.is_multiple_of(4)
            && self.h.is_multiple_of(4)
    }

    /// The splice, if the rect is block-aligned. `None` when it is not -- deliberately, rather than
    /// returning a rounded-off rect that would silently corrupt the neighbouring icons.
    pub fn splice(&self) -> Option<Splice> {
        if !self.block_aligned() || self.atlas_w < 4 || self.w == 0 || self.h == 0 {
            return None;
        }
        let bpb = self.block_bytes;
        let row_stride = (self.atlas_w as usize / 4) * bpb;
        Some(Splice {
            offset: (self.y as usize / 4) * row_stride + (self.x as usize / 4) * bpb,
            row_bytes: (self.w as usize / 4) * bpb,
            row_stride,
            rows: self.h as usize / 4,
        })
    }

    /// The deepest mip level at which every level from 0 down is still block-aligned.
    ///
    /// Answers "how far can a pre-compressed payload go before we need a real encoder". For the
    /// shipped sprite the answer is **0**, which is why the probe asks whether the UI ever samples
    /// below mip 0 for these icons -- if it does not, 0 is enough and the cheap path holds.
    pub fn deepest_aligned_mip(&self, mips: u32) -> u32 {
        let mut deepest = 0;
        for level in 1..mips {
            let m = self.at_mip(level);
            if m.w < 4 || m.h < 4 || !m.block_aligned() {
                break;
            }
            deepest = level;
        }
        deepest
    }
}

/// What the loaded-module list says about the game's own Oodle.
///
/// The atlas on disk is DCX/**KRAK** -- Oodle Kraken, proprietary and not something we may ship.
/// The game must be able to read its own files, so a decompressor is already in the process; if it
/// is a separately loaded `oo2core_*.dll` we can find it, and *decompression* stops being a
/// blocker. (Recompression is a different question, and the cheaper answer to that one is to find
/// out whether the loader accepts a non-KRAK DCX at all.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Oodle {
    /// A separately loaded Oodle DLL, by file name.
    Module(String),
    /// No Oodle module. It may still be statically linked into the exe, which this cannot see --
    /// 🛑 so this is "not found as a module", NEVER "not present".
    NotAModule,
}

/// Pick an Oodle module out of a list of loaded module file names.
pub fn find_oodle(names: &[String]) -> Oodle {
    for n in names {
        let low = n.to_ascii_lowercase();
        if low.starts_with("oo2core") && low.ends_with(".dll") {
            return Oodle::Module(n.clone());
        }
    }
    Oodle::NotAModule
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------------------------
    // THE MOTIVATING CASE. The shipped sprite is block-aligned at mip 0, and that single fact is
    // what makes a runtime splice cheap enough to be worth investigating at all.
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn the_shipped_sprite_is_block_aligned_at_mip_zero() {
        let s = Sprite::shipped();
        assert!(s.block_aligned(), "2132/1148/160 all divide by 4");
        let sp = s.splice().expect("aligned rects yield a splice");
        // 4096/4 = 1024 blocks per row, 16 bytes each.
        assert_eq!(sp.row_stride, 16_384);
        // block row 1148/4 = 287, block col 2132/4 = 533.
        assert_eq!(sp.offset, 287 * 16_384 + 533 * 16);
        assert_eq!(sp.rows, 40);
        assert_eq!(sp.row_bytes, 40 * 16);
        assert_eq!(
            sp.payload_bytes(),
            25_600,
            "25 KB of our own art, against an 8 MB atlas that is not ours"
        );
    }

    #[test]
    fn the_full_atlas_is_the_thing_we_are_trying_not_to_ship() {
        // 1024 x 512 blocks x 16 bytes. Stated so the ratio the module doc claims is checked
        // rather than asserted in prose.
        let atlas = (ATLAS_W as usize / 4) * (ATLAS_H as usize / 4) * BC_BLOCK_BYTES_DEFAULT;
        assert_eq!(atlas, 8_388_608);
        let ours = Sprite::shipped().splice().unwrap().payload_bytes();
        assert!(
            atlas / ours > 300,
            "the atlas is >300x the payload, which is the entire argument: {atlas} vs {ours}"
        );
    }

    // ---------------------------------------------------------------------------------------------
    // 🛑 AND THE LIMIT OF IT.
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn mip_one_is_not_block_aligned_so_the_cheap_path_stops_there() {
        let m1 = Sprite::shipped().at_mip(1);
        assert_eq!((m1.x, m1.y), (1066, 574));
        assert_eq!(1066 % 4, 2, "half of 2132 is not a multiple of 4");
        assert!(!m1.block_aligned());
        assert_eq!(
            m1.splice(),
            None,
            "an unaligned rect must yield None, never a rounded rect -- rounding would overwrite \
             the neighbouring icons in the atlas"
        );
        assert_eq!(
            Sprite::shipped().deepest_aligned_mip(12),
            0,
            "mip 0 only: anything below needs a real encoder"
        );
    }

    #[test]
    fn a_sprite_that_happens_to_survive_a_mip_reports_it() {
        // Not the shipped one -- a control, so `deepest_aligned_mip` is shown to be able to return
        // something other than 0. A test that can only ever produce one answer proves nothing.
        let s = Sprite {
            x: 1024,
            y: 512,
            w: 128,
            h: 128,
            atlas_w: 4096,
            block_bytes: 16,
        };
        assert!(s.block_aligned());
        // mip 5 is the last one that works: 1024>>5=32, 512>>5=16, 128>>5=4 -- w is exactly one
        // block wide and still aligned. mip 6 gives w=2, under a block, so it stops.
        assert_eq!(s.deepest_aligned_mip(12), 5);
        assert!(s.at_mip(5).block_aligned());
        assert!(s.at_mip(6).w < 4);
    }

    #[test]
    fn bc1_halves_the_payload() {
        let mut s = Sprite::shipped();
        s.block_bytes = 8;
        assert_eq!(s.splice().unwrap().payload_bytes(), 12_800);
    }

    #[test]
    fn a_degenerate_rect_yields_no_splice_rather_than_a_bad_one() {
        for bad in [
            Sprite {
                x: 2132,
                y: 1148,
                w: 0,
                h: 160,
                atlas_w: 4096,
                block_bytes: 16,
            },
            Sprite {
                x: 2132,
                y: 1148,
                w: 160,
                h: 0,
                atlas_w: 4096,
                block_bytes: 16,
            },
            Sprite {
                x: 2130,
                y: 1148,
                w: 160,
                h: 160,
                atlas_w: 4096,
                block_bytes: 16,
            },
            Sprite {
                x: 2132,
                y: 1148,
                w: 160,
                h: 160,
                atlas_w: 0,
                block_bytes: 16,
            },
        ] {
            assert_eq!(bad.splice(), None, "{bad:?} must not produce a splice");
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Oodle.
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn oodle_is_found_by_name_case_insensitively() {
        let names = vec![
            "eldenring.exe".into(),
            "OO2CORE_9_WIN64.DLL".into(),
            "eldenring_archipelago.dll".into(),
        ];
        assert_eq!(
            find_oodle(&names),
            Oodle::Module("OO2CORE_9_WIN64.DLL".into())
        );
    }

    #[test]
    fn absence_is_reported_as_not_a_module_never_as_not_present() {
        // 🛑 The distinction is the point: Oodle may be statically linked into the exe, which a
        // module list cannot see. Reporting "absent" would be a claim we cannot support, and this
        // repo has been bitten by exactly that shape before (absence in a log is a PROMPT).
        let names = vec!["eldenring.exe".into(), "kernel32.dll".into()];
        assert_eq!(find_oodle(&names), Oodle::NotAModule);
        // A near-miss must not match.
        assert_eq!(
            find_oodle(&["oo2core_9_win64.txt".into()]),
            Oodle::NotAModule
        );
        assert_eq!(
            find_oodle(&["myoo2core_9_win64.dll".into()]),
            Oodle::NotAModule
        );
    }
}
