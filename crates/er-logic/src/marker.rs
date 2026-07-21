//! `marker` — persist reconnect/reconciliation state INSIDE the ER save, via a reserved band of
//! save-persisted **event flags**, so a reconnect reads GROUND TRUTH instead of INFERRING character
//! identity from `play_time`. This is built to retire [`crate::reconcile::seed_trust`] and the
//! external `reconcile.json` watermark it keys on.
//!
//! # Why this exists
//!
//! Today the ledger watermark lives in an external file keyed by `(AP slot name, ER save-slot 0-9)`.
//! The 0-9 slot index can't tell one character from a delete-and-recreate in the same slot, so
//! [`crate::reconcile::seed_trust`] has to INFER "same character resuming" from `GameDataMan.play_time`
//! (the `live*2 >= stamp` tolerance + rewind detection). That inference is the documented root of a
//! whole family of reconnect bugs (flask / great-rune / map-piece double-grants, reconnect re-snapshot
//! eating checks, reconnect-to-new-seed panic; see [`crate::reconcile`] header).
//!
//! Minibake removes the inference: the watermark and a seed/slot identity travel INSIDE the save
//! itself, written alongside the grants. On reconnect the client reads the save's own record — no
//! marker means "fresh", a matching identity means "resume from exactly here", a mismatched identity
//! means "this save belongs to a different seed/slot". Because the record rewinds WITH the inventory
//! (both live in the save), a restored backup is coherent for free: the cursor moves back with it.
//!
//! # Why event flags, not a synthetic good
//!
//! The obvious cell — a reserved good whose stack COUNT is the cursor — fails on the live client's
//! actual primitives (design review, fable 2026-07-21):
//!   * the grant path (`grant_full_id` -> `grant_item`) is **additive only** — no decrement/set, so a
//!     changing multi-digit value is impossible;
//!   * held goods cap at `EquipParamGoods.maxNum`, and the save loads against VANILLA params before the
//!     runtime param pass — a cursor could be silently clamped/truncated;
//!   * `common.emevd` fires on EXACT held counts of goods rows (`POT_DELIVERY_CAPS`) — a count that
//!     sweeps hundreds of values is a hazard the 8852-placeholder audit never covered;
//!   * every increment drives an unsuppressable acquisition popup and burns the paced grant budget;
//!   * the goods read-back walks the accessor list that is BLIND to the co-op key-items list (the
//!     Morgott's-Rune re-grant CTD), so the cursor could read 0 in co-op.
//!
//! Event flags dodge every one of those: per-character, save-persisted, idempotent, UNPACED (no popup,
//! no cap, no co-op blindness, no count-watching emevd — only flag reads, which a band audit covers),
//! and already first-class in [`GameIo`] (`get_flag`/`set_flag`).
//!
//! # The flag band (PLACEHOLDER — pending a flag-space audit + a Windows verify)
//!
//! [`FlagBand::PLACEHOLDER`] = `75000..75120`, inside the real, save-persisted legacy-bonfire flag
//! group `[71000, 76000)` (bonfire-unlock flags ARE core save data). Vanilla legacy graces occupy
//! `71000..=74351`; `75000..75999` is the unused tail, and it is disjoint from every flag this project
//! authors or reads (grace warp-unlock, region-open/lock, check flags, map-reveal). The band constant
//! is the ONLY thing that changes once (a) a flag-space audit confirms no non-grace vanilla EMEVD
//! touches it and (b) a set -> quit-to-menu -> reload -> read Windows test confirms it persists. An
//! INVENTED (group-less) flag id would silently no-op (`er-event-flag-validity`), which is exactly why
//! the band must live inside an allocated, save-persisted group like `[71000, 76000)`.
//!
//! # Layout (contiguous from `base`)
//!
//! ```text
//!   +0        PRESENT   commit sentinel. false => marker ABSENT (fresh/migrate) — never "mismatch".
//!   +1..+33   IDENT     32-bit identity hash of (room seed, AP slot name).
//!   +33       SEL       cursor register selector: false => A active, true => B active.
//!   +34..+66  CUR_A     cursor register A: u32 = watermark - START_ITEM_INDEX_BASE.
//!   +66..+98  CUR_B     cursor register B.
//! ```
//!
//! 98 flags used; 120 reserved for headroom.
//!
//! # Crash / torn-write safety
//!
//! * **Identity + present.** `PRESENT` is written STRICTLY LAST, after every `IDENT` (and the first
//!   cursor) bit confirms. A crash mid-init leaves `PRESENT` clear, so the next read is ABSENT and
//!   init simply reruns — the identity is a deterministic function of `(seed, slot)`, so rewriting the
//!   same bits is idempotent. A torn init therefore NEVER reads as a *mismatch* that would wrongly
//!   REFUSE an innocent save.
//! * **Cursor.** DOUBLE-BUFFERED. An update writes the INACTIVE register in full, then flips `SEL` in
//!   one write — the atomic commit. A crash before the flip leaves `SEL` on the old, intact register
//!   (the tail replays, bounded, absorbed by the ledger's own idempotency); a crash mid-write scrambles
//!   only the UNREACHABLE inactive register, which the next read never consults.
//!
//! This module is PURE: it speaks only [`GameIo`], so [`crate::reconcile::MockGame`] exercises the real
//! bit codec with zero Windows code and the live client gets it for free. The timeline integration with
//! the real [`crate::reconcile::Reconciler`] is proven in [`crate::marker_replay`].

use crate::reconcile::{FlagId, GameIo, ItemIndex, START_ITEM_INDEX_BASE};

/// A 32-bit save/slot identity fingerprint (see [`identity_hash`]).
pub type Identity = u32;

/// The reserved contiguous flag band the marker lives in. Only [`FlagBand::base`] varies; the layout
/// offsets are fixed. Swap [`FlagBand::PLACEHOLDER`]'s base for the audited band once verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlagBand {
    /// The first flag id of the band (the `PRESENT` sentinel).
    pub base: FlagId,
}

impl FlagBand {
    /// PLACEHOLDER band — `75000..75120`. Real/save-persisted (inside `[71000, 76000)`), vanilla-free,
    /// disjoint from our own flag usage. PENDING flag-space audit + Windows persist verify.
    pub const PLACEHOLDER: FlagBand = FlagBand { base: 75_000 };
    /// Flags actually used by the layout.
    pub const WIDTH: u32 = 98;
    /// Flags reserved (WIDTH + headroom); the band must not overlap anything else in this range.
    pub const RESERVED: u32 = 120;

    const OFF_PRESENT: u32 = 0;
    const OFF_IDENT: u32 = 1; // +1..+33
    const OFF_SEL: u32 = 33;
    const OFF_CUR_A: u32 = 34; // +34..+66
    const OFF_CUR_B: u32 = 66; // +66..+98

    #[inline]
    fn present(self) -> FlagId {
        self.base + Self::OFF_PRESENT
    }
    #[inline]
    fn ident(self, bit: u32) -> FlagId {
        self.base + Self::OFF_IDENT + bit
    }
    #[inline]
    fn sel(self) -> FlagId {
        self.base + Self::OFF_SEL
    }
    /// Flag for `bit` of cursor register B (`reg_b=true`) or A (`reg_b=false`).
    #[inline]
    fn cur(self, reg_b: bool, bit: u32) -> FlagId {
        self.base
            + if reg_b {
                Self::OFF_CUR_B
            } else {
                Self::OFF_CUR_A
            }
            + bit
    }
}

/// Deterministic, build-stable 32-bit fingerprint of `(room_seed, ap_slot)` — FNV-1a/32 over
/// `room_seed \0 ap_slot`.
///
/// This is the identity the reconnect guard compares. It keys on the ROOM SEED and the AP SLOT NAME
/// only — NOT the ER save-slot index or `play_time` (character identity is solved STRUCTURALLY: a
/// different character simply has no marker), and NOT the item layout / slot_data (that false-positives
/// on benign slot_data evolution across client upgrades and would strand innocent players). 32 bits is
/// ample: the adversary is collision among the handful of seeds one player touches, not a birthday
/// attack across all rooms.
///
/// `std::hash::DefaultHasher` is deliberately NOT used — it is not stable across toolchain versions, and
/// this value must match byte-for-byte across reconnects and client builds.
pub fn identity_hash(room_seed: &str, ap_slot: &str) -> Identity {
    const FNV_OFFSET: u32 = 0x811c_9dc5;
    const FNV_PRIME: u32 = 0x0100_0193;
    let mut h = FNV_OFFSET;
    let mut mix = |b: u8| {
        h ^= b as u32;
        h = h.wrapping_mul(FNV_PRIME);
    };
    for b in room_seed.bytes() {
        mix(b);
    }
    mix(0); // domain separator so ("ab","c") != ("a","bc")
    for b in ap_slot.bytes() {
        mix(b);
    }
    h
}

/// Encode a watermark as the u32 stored in a cursor register. The watermark is always
/// `>= START_ITEM_INDEX_BASE` (the ledger floor is 0 or the negative start-item band base) and real AP
/// indices are tiny, so `wm - base` fits a u32 with billions of headroom.
#[inline]
fn encode_cursor(wm: ItemIndex) -> u32 {
    debug_assert!(
        wm >= START_ITEM_INDEX_BASE,
        "watermark below the ledger band floor"
    );
    let biased = wm - START_ITEM_INDEX_BASE;
    debug_assert!(
        biased >= 0 && biased <= u32::MAX as i64,
        "cursor out of u32 range"
    );
    biased as u32
}

/// Inverse of [`encode_cursor`].
#[inline]
fn decode_cursor(v: u32) -> ItemIndex {
    START_ITEM_INDEX_BASE + v as i64
}

/// Read a little-endian 32-bit value out of 32 consecutive flags addressed by `at(0..32)`.
fn read_u32(io: &dyn GameIo, at: impl Fn(u32) -> FlagId) -> u32 {
    let mut v = 0u32;
    for bit in 0..32 {
        if io.get_flag(at(bit)) {
            v |= 1 << bit;
        }
    }
    v
}

/// Write a little-endian 32-bit value into 32 consecutive flags. Returns `true` iff EVERY `set_flag`
/// succeeded (the flag holder was ready). A partial write is safe for both callers here — the register
/// being written is either the fresh `CUR_A` (before `PRESENT` commits) or the INACTIVE double-buffer
/// register — so on any failure the caller just retries the whole write next tick.
fn write_u32(io: &mut dyn GameIo, at: impl Fn(u32) -> FlagId, v: u32) -> bool {
    let mut ok = true;
    for bit in 0..32 {
        let on = (v >> bit) & 1 == 1;
        if !io.set_flag(at(bit), on) {
            ok = false;
        }
    }
    ok
}

/// What the save's marker band says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerRead {
    /// `PRESENT` is clear (or the whole band is): this save has never been committed by minibake.
    /// Treat as FRESH (or a legacy migration). A torn/partial init also lands here — never a mismatch.
    Absent,
    /// A committed marker: its identity and the watermark from its active cursor register.
    Present {
        /// The `(seed, slot)` identity this save was committed under.
        identity: Identity,
        /// The persisted ledger watermark.
        watermark: ItemIndex,
    },
}

/// Read the marker out of the band. Consults `PRESENT` first, so a cleared/partial band is `Absent`.
pub fn read(io: &dyn GameIo, band: FlagBand) -> MarkerRead {
    if !io.get_flag(band.present()) {
        return MarkerRead::Absent;
    }
    let identity = read_u32(io, |b| band.ident(b));
    let reg_b = io.get_flag(band.sel());
    let watermark = decode_cursor(read_u32(io, |b| band.cur(reg_b, b)));
    MarkerRead::Present {
        identity,
        watermark,
    }
}

/// The session-init decision the reconnect guard makes from a [`MarkerRead`] and the identity the
/// CURRENT connection expects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitDecision {
    /// No marker: seed the reconciler fresh (or via legacy migration), then [`commit`] a marker.
    Fresh,
    /// Identity matches: resume from this exact watermark ([`crate::reconcile::Reconciler::from_persisted`]).
    Resume {
        /// The persisted watermark to resume from.
        watermark: ItemIndex,
    },
    /// Identity MISMATCH — this save belongs to a different seed/slot. REFUSE the session: the caller
    /// must gate the WHOLE pipeline (flag poll, check detection, shop rewrites), not just grants —
    /// otherwise seed-A's save flags get reported as seed-B checks, corrupting the multiworld. Do NOT
    /// [`commit`] (never mutate a save we refused). Surface a reason to the player.
    Refuse {
        /// The identity found in the save.
        stored: Identity,
        /// The identity this connection expected.
        expected: Identity,
    },
}

/// Decide what to do at session init from the save's marker and the expected identity.
pub fn decide(marker: MarkerRead, expected: Identity) -> InitDecision {
    match marker {
        MarkerRead::Absent => InitDecision::Fresh,
        MarkerRead::Present {
            identity,
            watermark,
        } => {
            if identity == expected {
                InitDecision::Resume { watermark }
            } else {
                InitDecision::Refuse {
                    stored: identity,
                    expected,
                }
            }
        }
    }
}

/// Persist `(identity, watermark)` into the band. Idempotent and safe to call every tick; returns
/// `true` iff the marker is FULLY committed and would read back this watermark.
///
/// * FRESH band (`PRESENT` clear): write `SEL=A` + `CUR_A` + `IDENT`, then set `PRESENT` LAST. Any
///   failure leaves `PRESENT` clear, so the save still reads [`MarkerRead::Absent`] and the write is
///   retried next tick.
/// * ESTABLISHED band (`PRESENT` set): if the ACTIVE cursor already equals `watermark`, no-op. Else
///   write the INACTIVE register in full, then flip `SEL` — the atomic cursor commit.
///
/// The caller MUST NOT call this on an identity [`InitDecision::Refuse`]: committing would mutate a
/// save we just refused to touch.
pub fn commit(
    io: &mut dyn GameIo,
    band: FlagBand,
    identity: Identity,
    watermark: ItemIndex,
) -> bool {
    if !io.get_flag(band.present()) {
        // FRESH: register A holds the first cursor; SEL points at A (false). PRESENT commits last.
        let mut ok = io.set_flag(band.sel(), false);
        ok &= write_u32(io, |b| band.cur(false, b), encode_cursor(watermark));
        ok &= write_u32(io, |b| band.ident(b), identity);
        if !ok {
            return false; // PRESENT stays clear -> reads Absent -> retried; never a partial "Present"
        }
        io.set_flag(band.present(), true) // COMMIT
    } else {
        // ESTABLISHED: double-buffered cursor update.
        let reg_b = io.get_flag(band.sel());
        let active = decode_cursor(read_u32(io, |b| band.cur(reg_b, b)));
        if active == watermark {
            return true; // already current — no write, no churn
        }
        let inactive = !reg_b;
        if !write_u32(io, |b| band.cur(inactive, b), encode_cursor(watermark)) {
            return false; // holder not ready; SEL still points at the valid active register
        }
        io.set_flag(band.sel(), inactive) // FLIP = atomic commit of the new cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::MockGame;

    const B: FlagBand = FlagBand::PLACEHOLDER;

    #[test]
    fn band_fits_reserved_headroom() {
        assert!(FlagBand::WIDTH <= FlagBand::RESERVED);
        // last used flag stays inside the reserved window
        assert!(B.cur(true, 31) < B.base + FlagBand::RESERVED);
    }

    #[test]
    fn identity_hash_is_deterministic_and_discriminating() {
        let a = identity_hash("ROOMSEED-abc", "Alaric");
        assert_eq!(a, identity_hash("ROOMSEED-abc", "Alaric")); // stable
        assert_ne!(a, identity_hash("ROOMSEED-xyz", "Alaric")); // seed matters
        assert_ne!(a, identity_hash("ROOMSEED-abc", "Bob")); // slot matters
                                                             // domain separation: the \0 boundary keeps ("ab","c") from colliding with ("a","bc")
        assert_ne!(identity_hash("ab", "c"), identity_hash("a", "bc"));
    }

    #[test]
    fn cursor_roundtrips_across_the_band_floor() {
        for wm in [
            START_ITEM_INDEX_BASE,
            START_ITEM_INDEX_BASE + 7,
            -1,
            0,
            1,
            158,
            100_000,
        ] {
            assert_eq!(decode_cursor(encode_cursor(wm)), wm, "wm={wm}");
        }
    }

    #[test]
    fn absent_when_band_is_clear() {
        let g = MockGame::stable();
        assert_eq!(read(&g, B), MarkerRead::Absent);
        assert_eq!(decide(read(&g, B), 42), InitDecision::Fresh);
    }

    #[test]
    fn fresh_commit_roundtrips() {
        let mut g = MockGame::stable();
        let id = identity_hash("seed", "slot");
        assert!(commit(&mut g, B, id, 158));
        assert_eq!(
            read(&g, B),
            MarkerRead::Present {
                identity: id,
                watermark: 158
            }
        );
        assert_eq!(
            decide(read(&g, B), id),
            InitDecision::Resume { watermark: 158 }
        );
    }

    #[test]
    fn present_is_written_last_so_a_stalled_init_reads_absent() {
        // Holder not ready: no flag write lands, so PRESENT never sets -> Absent, not a torn Present.
        let mut g = MockGame::stable();
        g.flag_ready = false;
        assert!(!commit(&mut g, B, 7, 3));
        assert_eq!(read(&g, B), MarkerRead::Absent);
    }

    #[test]
    fn cursor_update_is_double_buffered() {
        let mut g = MockGame::stable();
        let id = identity_hash("seed", "slot");
        assert!(commit(&mut g, B, id, 10)); // fresh -> register A, SEL=false
        assert!(!g.get_flag(B.sel()));
        assert!(commit(&mut g, B, id, 25)); // established -> writes B, flips SEL
        assert!(g.get_flag(B.sel()));
        assert_eq!(
            read(&g, B),
            MarkerRead::Present {
                identity: id,
                watermark: 25
            }
        );
        assert!(commit(&mut g, B, id, 40)); // flips back to A
        assert!(!g.get_flag(B.sel()));
        assert_eq!(
            read(&g, B),
            MarkerRead::Present {
                identity: id,
                watermark: 40
            }
        );
    }

    #[test]
    fn a_torn_inactive_register_never_corrupts_the_active_cursor() {
        let mut g = MockGame::stable();
        let id = identity_hash("seed", "slot");
        assert!(commit(&mut g, B, id, 10)); // A active (SEL=false)
                                            // Simulate a crash mid-update: scramble the INACTIVE register (B) but DON'T flip SEL.
        for bit in 0..32 {
            g.set_flag(B.cur(true, bit), bit % 2 == 0);
        }
        // SEL still points at A -> the committed value is intact.
        assert_eq!(
            read(&g, B),
            MarkerRead::Present {
                identity: id,
                watermark: 10
            }
        );
    }

    #[test]
    fn established_same_watermark_is_a_noop() {
        let mut g = MockGame::stable();
        let id = identity_hash("seed", "slot");
        assert!(commit(&mut g, B, id, 10));
        let before = g.flags.clone();
        assert!(commit(&mut g, B, id, 10)); // same wm -> no writes
        assert_eq!(g.flags, before);
    }

    #[test]
    fn mismatch_decides_refuse() {
        let mut g = MockGame::stable();
        let stored = identity_hash("seedA", "slot");
        assert!(commit(&mut g, B, stored, 5));
        let expected = identity_hash("seedB", "slot");
        assert_eq!(
            decide(read(&g, B), expected),
            InitDecision::Refuse { stored, expected }
        );
    }
}
