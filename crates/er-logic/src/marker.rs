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
//! # The flag band (PLACEHOLDER — audit DONE 2026-08-21; pending only the Windows verify)
//!
//! [`FlagBand::PLACEHOLDER`] = `75000..75120`, inside the real, save-persisted legacy-bonfire flag
//! group `[71000, 76000)` (bonfire-unlock flags ARE core save data). Vanilla legacy graces occupy
//! `71000..=74351`; `75000..75999` is the unused tail, and it is disjoint from every flag this project
//! authors or reads (grace warp-unlock, region-open/lock, check flags, map-reveal). The band constant
//! is the ONLY thing that changes once (b) below lands. An
//! INVENTED (group-less) flag id would silently no-op (`er-event-flag-validity`), which is exactly why
//! the band must live inside an allocated, save-persisted group like `[71000, 76000)`.
//!
//! (a) **Flag-space audit — DONE, clean** (clients#338, 2026-08-21; reproducible via
//! `tools/audit_marker_flag_band.py` against an `elden_ring_artifacts/` tree): 589 EMEVD scripts
//! (incl. `common`/`common_func`), all 14 `*EventFlag*` verb shapes classified, 993 range/batch ops
//! span-checked — ZERO script touches of `75000..=75119`. 203 ESD talk scripts — zero band literals.
//! The only same-numeric-range literals anywhere are param cells, which cannot write event flags:
//! AtkParam_Pc row ids `75000..75106` (referenced by the `refId1..4` of Magic rows 7500/7510) and a
//! GameAreaParam `bonusSoul = 75000`. Still reasoning, not measurement, and named as such: the
//! engine-hardcoded NG+ flag reset (expressed in no script) and the binary `.dcx` flag-allocation
//! lists. An NG+ clear of the band degrades to marker-ABSENT (bounded replay), never a refusal.
//!
//! (b) **Windows persist verify — STILL OWED.** One set -> quit-to-menu -> reload -> read on a real
//! save. If the band does not survive the reload, the marker always reads ABSENT and the guard is a
//! no-op that looks armed — a quiet failure, not a lockout.
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
    /// PLACEHOLDER band — `75000..75120`. Real/save-persisted (inside `[71000, 76000)`), audited
    /// vanilla-script-free on 2026-08-21 (see the module doc), disjoint from our own flag usage.
    /// PENDING only the Windows persist verify.
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

/// Verdict for an ALREADY-ARMED reconciler when the session's identity may have moved under it.
///
/// [`decide`] answers "may this session arm?" and is evaluated ONCE, at reconciler init. That left a
/// hole: nothing re-asked the question afterwards. If the player reconnects to a DIFFERENT room
/// without restarting the game, the armed reconciler keeps running against the old room's identity
/// and the init guard never fires -- it has already run.
///
/// 2026-07-30, boblerrr: exactly that. Room `58616176906260760086` -> `87077892385581357560` on a
/// live reconnect; `229` of the old room's checks were reported into the new one before the next
/// game restart finally let [`decide`] refuse. `reconcile_io.rs`'s own note calls this outcome
/// "strictly worse than a double-grant" -- and it is not recoverable, because the checks are already
/// on someone else's server.
///
/// So this is the SAME question as [`decide`], asked at the other end: does the identity an armed
/// reconciler was built for still equal the live session's? Deliberately keyed on `(room_seed,
/// ap_slot)` via [`identity_hash`] rather than on the seed string alone, so a SLOT change is caught
/// by the same predicate -- the symptom was a seed change, but the datum is the identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArmedVerdict {
    /// The armed reconciler still belongs to this session -- keep applying.
    Keep,
    /// The armed reconciler belongs to a DIFFERENT (seed, slot). It must be disarmed and the whole
    /// pipeline gated, exactly as [`InitDecision::Refuse`] does at init.
    Disarm {
        /// The identity the armed reconciler was built for.
        armed: Identity,
        /// The identity the live connection has now.
        live: Identity,
    },
}

/// Does an armed reconciler built for `armed` still belong to the live `(room_seed, ap_slot)`?
///
/// Total and pure: the caller supplies the live room seed and slot name, this computes the live
/// identity the same way init does. See [`ArmedVerdict`].
pub fn armed_verdict(armed: Identity, room_seed: &str, ap_slot: &str) -> ArmedVerdict {
    let live = identity_hash(room_seed, ap_slot);
    if armed == live {
        ArmedVerdict::Keep
    } else {
        ArmedVerdict::Disarm { armed, live }
    }
}

/// Why a session was refused — decides the WORDS, because the two cases need different actions
/// from the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// At connect: the loaded save's marker belongs to a different (seed, slot). The player has the
    /// wrong SAVE for this room. See [`InitDecision::Refuse`].
    WrongSaveAtConnect,
    /// Mid-session: the ROOM changed under a live, already-armed reconciler. See [`ArmedVerdict`].
    RoomChangedMidSession,
}

/// The on-screen words for a refused session.
///
/// 🛑 A refused session looks EXACTLY like a broken mod from the player's chair: checks stop
/// reporting, items stop arriving, and until now the only trace was a `log::warn!` nobody reads.
/// boblerrr played ~55 minutes across two sessions in that state on 2026-07-30. The guard was
/// right; it was just invisible. So these strings must name the ACTION, not the diagnosis — a
/// player cannot act on "marker identity mismatch".
///
/// Kept in er-logic (pure, host-tested) for the same reason as
/// [`crate::scaling::region_scaling_toast`]: the words are a product decision, and the client
/// should own no copy of them. The caller pushes into `toast::Deck`, whose identical-text refresh
/// makes a per-tick re-push free — which is what a persistent condition wants.
pub fn refusal_toast(refusal: Refusal) -> String {
    match refusal {
        Refusal::WrongSaveAtConnect => "Archipelago: this save belongs to a DIFFERENT seed.              Nothing will send or arrive. Quit to the main menu, then load this room's save or start a new character."
            .to_string(),
        Refusal::RoomChangedMidSession => "Archipelago: the room changed while you were playing.              Nothing will send or arrive, so this save's checks are not sent to the new room.              RESTART the game."
            .to_string(),
    }
}

/// Parse an `ap_save_<seed>_<slot>.json` file NAME into the `(seed, slot)` identity parts the
/// client hashed when it armed that room's persistence.
///
/// The split is on the FIRST `_`: room seeds are numeric, so `safe()` left them underscore-free,
/// while a slot name's own spaces/punctuation became `_` and stay part of the slot half. The
/// legacy shared file `ap_save__<slot>.json` (pre-2026-07-02, empty seed) parses to an empty seed
/// and is REJECTED: its true seed is unknowable, so it can never be named as the matching room.
///
/// 🛑 The parts are the `safe()`'d forms from the file name, not the originals -- for a slot
/// name with non-alphanumerics the re-hash in [`wrong_save_room`] simply will not match, and the
/// refusal falls back to the un-named wording. A silent miss is the designed failure mode; a
/// WRONG room name is not (a false match needs a 32-bit FNV collision).
pub fn save_file_identity_parts(file_name: &str) -> Option<(String, String)> {
    let stem = file_name.strip_prefix("ap_save_")?.strip_suffix(".json")?;
    let (seed, slot) = stem.split_once('_')?;
    if seed.is_empty() || slot.is_empty() {
        return None;
    }
    Some((seed.to_string(), slot.to_string()))
}

/// The filename-safe identity spelling used by the live `ap_save_<seed>_<slot>.json` writer.
/// Keeping it here prevents advisory sibling scans from drifting from the path they inspect.
pub fn safe_file_identity_part(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Whether this AP slot name has persisted history for a DIFFERENT room.
///
/// This is advisory only: a new seed using the same player name is legitimate, so the result must
/// never gate checks or delivery. It exists to distinguish that normal reroll from the easy support
/// mistake where a player unknowingly connects to a different room after wiping or replacing the
/// local save (clients#402). Inputs are the sanitized identity parts returned by
/// [`save_file_identity_parts`]; the current values are the original server strings and are
/// normalized here with [`safe_file_identity_part`].
pub fn has_other_room_for_slot(
    current_seed: &str,
    current_slot: &str,
    siblings: &[(String, String)],
) -> bool {
    let current_seed = safe_file_identity_part(current_seed);
    let current_slot = safe_file_identity_part(current_slot);
    siblings
        .iter()
        .any(|(seed, slot)| slot == &current_slot && seed != &current_seed)
}

/// One-shot player-facing heads-up for [`has_other_room_for_slot`]. This is deliberately not a
/// refusal: a reroll is allowed and continues normally.
pub fn other_room_history_toast() -> String {
    "Archipelago: this looks like a NEW room, but this slot name has history in another room.              If you did not reroll, check the server address. Play continues normally."
        .to_string()
}

/// The room a refused save actually belongs to (clients#337): the sibling `(seed, slot)` whose
/// [`identity_hash`] IS the save-embedded marker `stored`. `siblings` are the parsed identity
/// parts of every other `ap_save_*.json` beside the save just armed.
///
/// The CURRENT room's own file cannot false-positive here: the refusal fired precisely because
/// `stored != identity_hash(this_seed, this_slot)`.
pub fn wrong_save_room(
    stored: Identity,
    siblings: &[(String, String)],
) -> Option<(String, String)> {
    siblings
        .iter()
        .find(|(seed, slot)| identity_hash(seed, slot) == stored)
        .cloned()
}

/// [`refusal_toast`] with the clients#337 room routing when the sibling scan identified the room
/// the save belongs to. A refusal the player cannot ACT on reads as a lockout (two players read
/// it as "the new client is erroneously locking me out of my save", bobler 2026-08-20); naming
/// the room makes it a routing instruction instead of a wall.
pub fn refusal_toast_with_room(refusal: Refusal, room: Option<(&str, &str)>) -> String {
    match (refusal, room) {
        (Refusal::WrongSaveAtConnect, Some((seed, slot))) => format!(
            "Archipelago: this save belongs to a DIFFERENT seed -- it was last played on seed {seed} (save file ap_save_{seed}_{slot}.json).              Nothing will send or arrive. Connect to THAT room to continue it, or quit to the main menu, then load this room's save or start a new character."
        ),
        _ => refusal_toast(refusal),
    }
}

/// What may be done with a LATCHED refusal when the player leaves the world.
///
/// See [`release_verdict`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalRelease {
    /// Keep the session gated. Only a game restart clears this one.
    Hold,
    /// Clear the latch, so session init runs again against whatever character loads next.
    Rearm,
}

/// May a latched refusal be RELEASED now that the player has left the world?
///
/// # The bug this exists for (2026-08-10, Alaric)
///
/// [`refusal_toast`] tells a wrong-save player to "start a fresh character", and until this
/// predicate existed that instruction could not work. The refusal is a process-lifetime latch
/// (`reconcile_io::REFUSED`), and `core`'s `reconcile_inited` is set the moment `init` RETURNS --
/// including the [`InitDecision::Refuse`] path, which returns before building anything. So the
/// player quit to the menu, rolled a brand-new character, and got a save that was gated, silent and
/// permanently inert: no checks reported, no items granted, the same toast still on screen. The
/// only recovery was restarting the game, and nothing on screen said so.
///
/// # Why this is a predicate and not just "clear the flag"
///
/// The two refusals are NOT symmetric, and clearing both would reintroduce the 229-check corruption
/// [`ArmedVerdict`] exists to stop:
///
/// * [`Refusal::WrongSaveAtConnect`] fires INSIDE init, before a driver is constructed. There is
///   nothing armed, nothing built for the wrong identity, and nothing that has reported a check.
///   Re-running init is a genuine first init, so this one releases.
/// * [`Refusal::RoomChangedMidSession`] fires against an ALREADY-ARMED reconciler built for the old
///   room. `reconcile_io`'s `DRIVER` is a `OnceLock` and cannot be replaced, so "clear the latch and
///   re-init" would silently keep the OLD driver while looking correct. Fail closed: hold, and let
///   the toast's RESTART instruction stand.
///
/// `driver_armed` is the caller's answer to "does a driver exist right now" (`DRIVER.get().is_some()`).
/// It is a parameter rather than an assumption because the first bullet's "there is nothing armed"
/// is an invariant of today's `init`, not a law -- if a future init ever builds a driver before
/// refusing, this holds instead of releasing a session onto a driver built for someone else.
pub fn release_verdict(refusal: Refusal, driver_armed: bool) -> RefusalRelease {
    match refusal {
        // An armed driver belongs to an identity we can no longer serve, and a `OnceLock` cannot be
        // replaced -- so a release here would re-arm nothing and un-gate everything.
        Refusal::RoomChangedMidSession => RefusalRelease::Hold,
        Refusal::WrongSaveAtConnect if driver_armed => RefusalRelease::Hold,
        Refusal::WrongSaveAtConnect => RefusalRelease::Rearm,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::MockGame;

    const B: FlagBand = FlagBand::PLACEHOLDER;

    #[test]
    fn band_fits_reserved_headroom() {
        // Both operands are consts, so this is checked at COMPILE time rather than at test time:
        // a band that outgrew its reserved window can no longer wait for someone to run the suite.
        const _: () = assert!(FlagBand::WIDTH <= FlagBand::RESERVED);
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

    // -----------------------------------------------------------------------------------------
    // armed_verdict — the mid-session half of the reconnect guard (2026-07-30, boblerrr)
    // -----------------------------------------------------------------------------------------

    /// THE MOTIVATING CASE, pinned to the wild numbers. boblerrr's client logged
    /// `save marker identity 0x45d35730 != this session 0x9911460c` after reconnecting from room
    /// `58616176906260760086` to room `87077892385581357560` as slot `bobler1`. Both identities are
    /// reproduced here from the seed strings alone, so this test also pins `identity_hash` against a
    /// value observed in the field -- if the hash ever changes, every player's marker silently stops
    /// matching, and this reds.
    #[test]
    fn boblerrrs_live_room_switch_disarms_the_armed_reconciler() {
        const ROOM_A: &str = "58616176906260760086";
        const ROOM_B: &str = "87077892385581357560";
        let armed = identity_hash(ROOM_A, "bobler1");
        assert_eq!(
            armed, 0x45d3_5730,
            "identity_hash drifted from the observed marker"
        );
        assert_eq!(
            identity_hash(ROOM_B, "bobler1"),
            0x9911_460c,
            "identity_hash drifted from the observed session identity"
        );
        assert_eq!(
            armed_verdict(armed, ROOM_B, "bobler1"),
            ArmedVerdict::Disarm {
                armed: 0x45d3_5730,
                live: 0x9911_460c
            }
        );
    }

    /// The negative case that keeps the guard from becoming a bug: a reconnect to the SAME room must
    /// keep the reconciler armed. `is_seed_change` already refuses to fire here, but this predicate
    /// is evaluated independently, so it has to be right on its own.
    #[test]
    fn a_same_room_reconnect_keeps_the_reconciler_armed() {
        let armed = identity_hash("58616176906260760086", "bobler1");
        assert_eq!(
            armed_verdict(armed, "58616176906260760086", "bobler1"),
            ArmedVerdict::Keep
        );
    }

    /// The symptom was a SEED change, but the datum is the IDENTITY: a slot change on the same room
    /// is the same failure (the save belongs to the other slot) and must disarm too. If this ever
    /// reds, someone has narrowed the predicate back to the symptom.
    #[test]
    fn a_slot_change_on_the_same_room_also_disarms() {
        let armed = identity_hash("58616176906260760086", "bobler1");
        assert!(matches!(
            armed_verdict(armed, "58616176906260760086", "boblerHK"),
            ArmedVerdict::Disarm { .. }
        ));
    }

    /// `armed_verdict` must agree with `decide` -- they are the same question asked at two moments,
    /// and a client that disarmed mid-session but would Resume at init (or vice versa) would flap.
    #[test]
    fn armed_verdict_agrees_with_the_init_decision() {
        let armed = identity_hash("A", "slot");
        for (room, slot) in [("A", "slot"), ("B", "slot"), ("A", "other")] {
            let live = identity_hash(room, slot);
            let init = decide(
                MarkerRead::Present {
                    identity: armed,
                    watermark: 7,
                },
                live,
            );
            match armed_verdict(armed, room, slot) {
                ArmedVerdict::Keep => {
                    assert_eq!(init, InitDecision::Resume { watermark: 7 }, "{room}/{slot}")
                }
                ArmedVerdict::Disarm { armed: a, live: l } => {
                    assert_eq!(
                        init,
                        InitDecision::Refuse {
                            stored: a,
                            expected: l
                        },
                        "{room}/{slot}"
                    )
                }
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // refusal_toast — the on-screen half of the guard (2026-07-30)
    // -----------------------------------------------------------------------------------------

    /// Both refusals must tell the player what to DO. A player cannot act on "identity mismatch",
    /// which is all the log said while boblerrr played ~55 minutes into a gated session.
    #[test]
    fn both_refusal_toasts_name_an_action_the_player_can_take() {
        let wrong = refusal_toast(Refusal::WrongSaveAtConnect);
        let changed = refusal_toast(Refusal::RoomChangedMidSession);
        for t in [&wrong, &changed] {
            assert!(t.contains("Archipelago:"), "{t}");
            // The consequence, so a silent game is explained rather than merely flagged.
            assert!(t.contains("Nothing will send or arrive"), "{t}");
        }
        assert!(wrong.to_lowercase().contains("new character"), "{wrong}");
        assert!(changed.to_lowercase().contains("restart"), "{changed}");
    }

    /// The wrong-save toast names the MENU step, because that is the step that does the work.
    /// `release_verdict` is only consulted when the player leaves the world, so "start a new
    /// character" without "quit to the main menu" describes an action the player cannot take from
    /// where they are standing -- which is how this instruction was wrong for the whole of v0.3.
    #[test]
    fn the_wrong_save_toast_names_the_menu_step_that_actually_releases_the_latch() {
        let wrong = refusal_toast(Refusal::WrongSaveAtConnect).to_lowercase();
        assert!(wrong.contains("main menu"), "{wrong}");
    }

    /// In-game strings render through the game's own font path and must stay ASCII
    /// (er-toast-strings-are-ascii-only). The em dashes elsewhere in this file are DOC comments,
    /// which never reach a screen; these do.
    #[test]
    fn refusal_toasts_are_ascii_only() {
        for r in [Refusal::WrongSaveAtConnect, Refusal::RoomChangedMidSession] {
            let t = refusal_toast(r);
            assert!(t.is_ascii(), "{t}");
            // The room-enriched variant (clients#337) obeys the same rule, hint or no hint.
            let h = refusal_toast_with_room(r, Some(("88641793823048305365", "bobler")));
            assert!(h.is_ascii(), "{h}");
        }
    }

    // -----------------------------------------------------------------------------------------
    // clients#337 — the wrong-save refusal NAMES the room the save belongs to (2026-08-21)
    // -----------------------------------------------------------------------------------------

    /// THE MOTIVATING CASE, bobler 2026-08-20: the guard fired correctly -- stored marker
    /// 0x56c299ee is exactly identity_hash("88641793823048305365", "bobler") -- but the refusal
    /// named no room and read as a lockout. The sibling scan must recover that room.
    #[test]
    fn the_sibling_scan_names_the_room_the_save_belongs_to() {
        let stored = 0x56c299ee;
        let siblings: Vec<(String, String)> = [
            "ap_save_88641793823048305365_bobler.json", // the room he played five hours earlier
            "ap_save_77770000000000000001_bobler.json", // the NEW room he connected to
            "ap_save_88641793823048305365_carla.json",  // same seed, another slot
        ]
        .iter()
        .map(|n| save_file_identity_parts(n).unwrap())
        .collect();
        // Guard the motivating hash itself: if identity_hash ever drifts, THIS is the alarm.
        assert_eq!(identity_hash("88641793823048305365", "bobler"), stored);
        let room = wrong_save_room(stored, &siblings).expect("a sibling matches");
        assert_eq!(
            room,
            ("88641793823048305365".to_string(), "bobler".to_string())
        );
        // ...and the enriched toast turns the refusal into a routing instruction.
        let toast = refusal_toast_with_room(
            Refusal::WrongSaveAtConnect,
            Some((room.0.as_str(), room.1.as_str())),
        );
        assert!(toast.contains("seed 88641793823048305365"), "{toast}");
        assert!(
            toast.contains("ap_save_88641793823048305365_bobler.json"),
            "{toast}"
        );
        assert!(toast.contains("Connect to THAT room"), "{toast}");
        // The standing instructions survive the enrichment.
        assert!(toast.contains("Nothing will send or arrive"), "{toast}");
        assert!(toast.to_lowercase().contains("main menu"), "{toast}");
    }

    #[test]
    fn no_matching_sibling_keeps_the_original_wording() {
        let stored = identity_hash("no-such-seed", "no-such-slot");
        let siblings: Vec<(String, String)> =
            [save_file_identity_parts("ap_save_12345_bobler.json").unwrap()].into();
        assert_eq!(wrong_save_room(stored, &siblings), None);
        assert_eq!(
            refusal_toast_with_room(Refusal::WrongSaveAtConnect, None),
            refusal_toast(Refusal::WrongSaveAtConnect)
        );
    }

    #[test]
    fn save_file_name_parsing_splits_on_the_first_underscore_only() {
        // Slot names with spaces were safe()'d to underscores and must survive the split whole.
        assert_eq!(
            save_file_identity_parts("ap_save_12345_My_Slot_Name.json"),
            Some(("12345".to_string(), "My_Slot_Name".to_string()))
        );
        // The legacy shared file (pre-2026-07-02) has an empty seed: unknowable, never named.
        assert_eq!(save_file_identity_parts("ap_save__bobler.json"), None);
        // Not a save file at all.
        assert_eq!(save_file_identity_parts("reconcile.json"), None);
        assert_eq!(save_file_identity_parts("ap_save_12345.json"), None);
    }

    #[test]
    fn room_history_heads_up_only_for_the_same_slot_in_another_room() {
        let siblings = vec![
            ("111".to_string(), "bobler".to_string()),
            ("222".to_string(), "carla".to_string()),
            ("333".to_string(), "bobler".to_string()),
        ];
        assert!(has_other_room_for_slot("999", "bobler", &siblings));
        assert!(!has_other_room_for_slot("222", "carla", &siblings));
        assert!(!has_other_room_for_slot("111", "nobody", &siblings));
        assert!(has_other_room_for_slot(
            "999",
            "bob ler",
            &[("111".to_string(), "bob_ler".to_string())]
        ));

        let toast = other_room_history_toast();
        assert!(toast.contains("NEW room"), "{toast}");
        assert!(toast.contains("Play continues normally"), "{toast}");
        assert!(toast.is_ascii(), "{toast}");
    }

    #[test]
    fn a_room_changed_refusal_is_never_room_enriched() {
        // The hint only exists for WrongSaveAtConnect; the mid-session wording must not change.
        assert_eq!(
            refusal_toast_with_room(
                Refusal::RoomChangedMidSession,
                Some(("88641793823048305365", "bobler"))
            ),
            refusal_toast(Refusal::RoomChangedMidSession)
        );
    }

    // -----------------------------------------------------------------------------------------
    // release_verdict — clearing a latched refusal (2026-08-10)
    // -----------------------------------------------------------------------------------------

    /// THE MOTIVATING CASE, at the predicate level. A wrong-save refusal happens inside `init`,
    /// before a driver exists; quitting to the menu must let the next character arm. Before this
    /// predicate the latch was permanent and a brand-new character was silently inert until the
    /// player restarted the game.
    #[test]
    fn a_wrong_save_refusal_releases_once_the_player_leaves_the_world() {
        assert_eq!(
            release_verdict(Refusal::WrongSaveAtConnect, false),
            RefusalRelease::Rearm
        );
    }

    /// The 229-check corruption guard must NOT be releasable. `DRIVER` is a `OnceLock`, so a
    /// released mid-session disarm would un-gate the pipeline while keeping the OLD room's
    /// reconciler -- the exact outcome `ArmedVerdict` was written to prevent.
    #[test]
    fn a_mid_session_room_change_never_releases() {
        for armed in [false, true] {
            assert_eq!(
                release_verdict(Refusal::RoomChangedMidSession, armed),
                RefusalRelease::Hold
            );
        }
    }

    /// Fail CLOSED on the combination today's `init` cannot produce. "Refused implies no driver" is
    /// an invariant of one function, not a law; if it ever stops holding, releasing would hand the
    /// session a driver built for somebody else's identity.
    #[test]
    fn a_wrong_save_refusal_holds_while_a_driver_is_armed() {
        assert_eq!(
            release_verdict(Refusal::WrongSaveAtConnect, true),
            RefusalRelease::Hold
        );
    }

    /// The two cases need DIFFERENT actions — restarting does not fix a wrong save, and loading
    /// another save does not fix a room that moved. Collapsing them would give bad advice.
    #[test]
    fn the_two_refusals_do_not_give_the_same_advice() {
        assert_ne!(
            refusal_toast(Refusal::WrongSaveAtConnect),
            refusal_toast(Refusal::RoomChangedMidSession)
        );
    }

    /// A toast is re-pushed every tick while refused and deduped by `toast::Deck` on EXACT text —
    /// so the words must be stable for a given refusal, or the deck stacks a new toast per frame.
    #[test]
    fn the_words_are_stable_so_the_decks_dedup_holds() {
        for r in [Refusal::WrongSaveAtConnect, Refusal::RoomChangedMidSession] {
            assert_eq!(refusal_toast(r), refusal_toast(r));
        }
    }
}
