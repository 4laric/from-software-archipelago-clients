//! seed_ids.rs — registry of the item FullIDs THIS SEED's own tables carry, so the crash reporter
//! can answer "is the faulting value shaped like an id WE wrote?" in one line instead of one
//! archaeology session.
//!
//! MOTIVATION (client#351, crash-19968, 2026-08-21, one second after `LuaWarp hook: target
//! 1036541952`): `ACCESS_VIOLATION read at 0x240002785` with `r13 = 0x2_40002775` -- a live
//! pointer whose LOW HALF is `0x40002775`, the FullID of goods 10101, an id THIS seed's own
//! `enemyDropRoll` table wrote (9 list entries in the slot_data dump). The hypothesis under test:
//! a 32-bit FullID written through a STALE row pointer after a warp re-streamed the param tables,
//! landing on the low half of a live pointer whose high half survived -- exactly r13's shape.
//! Whether or not that holds, every future crash of this class now carries its own evidence:
//! decode the low half, and say which seed table (if any) the id appears in.
//!
//! 🛑 AN ID THAT RESOLVES IS NOT A TABLE MATCH (the issue's own caveat, kept load-bearing here):
//! `0x40002775` reading as goods-10101 can be coincidence -- the goods band alone is 1/16 of the
//! u32 space, and register low halves are noise more often than not. The MEMBERSHIP answer ("in
//! this seed's recorded tables: yes/no") is the strong half of the signal; the band decode alone
//! is the weak half. The report prints both and decides nothing.
//!
//! DISCIPLINE -- read from a fatal exception handler, same rules as `foreign_blocks`:
//!   * no locks (the crashing thread may hold any of them), no panics, no iteration over heap
//!     structures: a fixed static open-addressed table of atomics, `Relaxed` throughout -- we are
//!     racing a dying process, not synchronizing with it;
//!   * recording happens at slot_data parse (ordinary connect-time context) and is wait-free
//!     anyway, so a record cannot deadlock a game thread either;
//!   * `format!` on the ANNOTATE path allocates, as `report_inner`'s `String` already does.

use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

/// Source bits: WHICH seed table carries the id. Kept coarse on purpose -- the crash report's
/// question is "ours, and from which writer family", not a per-row provenance database.
pub const SRC_ENEMY_DROP_ROLL: u32 = 0b01;
pub const SRC_SHOP: u32 = 0b10;
pub const SRC_MINE_MATERIAL_ROLL: u32 = 0b100;

/// Table capacity (power of two). Distinct ids recorded per seed are the enemy-drops POOL (a few
/// hundred goods), the shop preview wares (~500 rows), and the infinite-stock reroll (455 rows) --
/// 8192 keeps the load factor under ~0.2 with two orders of magnitude of headroom. BSS: 64 KiB
/// for the slots + 32 KiB for the source masks, committed lazily.
const CAP: usize = 8192;
const CAP_MASK: usize = CAP - 1;

/// One slot: `(generation << 32) | full_id`. 0 is permanently empty (generation 0 never exists,
/// and FullID 0 is never recorded), so a single atomic load answers "occupied, current, and mine".
/// Packing generation and id into ONE word is what lets a crash-time reader trust the pair: no
/// torn (new id, old generation) state is observable.
static SLOT: [AtomicU64; CAP] = [const { AtomicU64::new(0) }; CAP];
/// Parallel per-slot source bitmask. Read only after SLOT says the id matches; a torn read here
/// costs a wrong source list on a line that is already advisory, never a wrong membership answer.
static SRC: [AtomicU32; CAP] = [const { AtomicU32::new(0) }; CAP];
/// Current generation. `clear()` (a new seed's slot_data parse) bumps it, retiring every slot at
/// once -- O(1), wait-free, and safe against a concurrent crash-time reader, which then simply
/// reports "no ids recorded" instead of matching LAST seed's tables.
static GEN: AtomicU32 = AtomicU32::new(1);
/// Inserted this generation (diagnostic counts for the annotate lines).
static INSERTED: AtomicUsize = AtomicUsize::new(0);
/// Inserts that found the table full. Nonzero means membership can false-MISS; annotate says so.
static DROPPED: AtomicUsize = AtomicUsize::new(0);

#[inline]
fn home(full_id: u32) -> usize {
    // Fibonacci hashing; the high bits carry the entropy for a power-of-two table.
    (full_id.wrapping_mul(0x9E37_79B1) >> 19) as usize & CAP_MASK
}

/// Record one FullID this seed's tables carry. Called at slot_data parse (core.rs) for
/// `enemyDropRoll`, `shopPreviewGoods` and `shopInfiniteStock`. Wait-free; idempotent -- recording
/// the same id again just ORs the source bits.
pub fn record(full_id: u32, src: u32) {
    if full_id == 0 || src == 0 {
        return;
    }
    let generation = GEN.load(Ordering::Relaxed) as u64;
    let want = generation << 32 | full_id as u64;
    let mut h = home(full_id);
    for _ in 0..CAP {
        let cur = SLOT[h].load(Ordering::Relaxed);
        if cur == want {
            SRC[h].fetch_or(src, Ordering::Relaxed);
            return;
        }
        // Empty (0) or a previous generation's entry: claim it. A losing CAS just means a
        // concurrent writer got here first -- re-read and continue probing.
        if cur >> 32 != generation
            && SLOT[h]
                .compare_exchange(cur, want, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            SRC[h].store(src, Ordering::Relaxed);
            INSERTED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        h = (h + 1) & CAP_MASK;
    }
    DROPPED.fetch_add(1, Ordering::Relaxed);
}

/// Is `full_id` one of the ids recorded for THIS seed? Returns its source bitmask. Crash-safe:
/// bounded probes, no locks, no allocation.
pub fn lookup(full_id: u32) -> Option<u32> {
    if full_id == 0 {
        return None;
    }
    let generation = GEN.load(Ordering::Relaxed) as u64;
    let want = generation << 32 | full_id as u64;
    let mut h = home(full_id);
    for _ in 0..CAP {
        let cur = SLOT[h].load(Ordering::Relaxed);
        if cur == want {
            return Some(SRC[h].load(Ordering::Relaxed));
        }
        if cur == 0 {
            return None; // empty slot on the probe path: not present
        }
        h = (h + 1) & CAP_MASK;
    }
    None
}

/// Retire every recorded id (a new seed's slot_data is about to be parsed). O(1): the generation
/// bump orphans every slot, which `record` then lazily reclaims.
pub fn clear() {
    GEN.fetch_add(1, Ordering::Relaxed);
    INSERTED.store(0, Ordering::Relaxed);
    DROPPED.store(0, Ordering::Relaxed);
}

/// Ids recorded for the current seed (diagnostic line fodder).
pub fn registered() -> usize {
    INSERTED.load(Ordering::Relaxed)
}

/// Decode the FromSoft FullID category nibble (`er_codec`'s scheme, inherited DS3 -> ER: top 4
/// bits category, low 28 the param row id). Deliberately EXCLUDES the weapon band (nibble 0):
/// `0x0000_2775`-shaped values are indistinguishable from ordinary small integers, which is every
/// second register low half -- naming those "weapon 10101" would manufacture confidence, not
/// evidence. Row id 0 is no item either.
///
/// The category values mirror `er_codec::CATEGORY_*`; they live here because `shared` is the
/// game-agnostic crate and must not take an ER-specific dependency for a crash-time decode that
/// DS3's client may someday want too.
pub fn decode_band(low32: u32) -> Option<(&'static str, u32)> {
    let row = low32 & 0x0FFF_FFFF;
    if row == 0 {
        return None;
    }
    let name = match low32 & 0xF000_0000 {
        0x1000_0000 => "protector",
        0x2000_0000 => "accessory",
        0x4000_0000 => "goods",
        0x8000_0000 => "gem/ash-of-war",
        _ => return None,
    };
    Some((name, row))
}

/// Render the source bitmask as table names for the report line.
fn src_names(src: u32) -> String {
    let mut names: Vec<&str> = Vec::new();
    if src & SRC_ENEMY_DROP_ROLL != 0 {
        names.push("enemyDropRoll");
    }
    if src & SRC_SHOP != 0 {
        names.push("shop (shopPreviewGoods/shopInfiniteStock)");
    }
    if src & SRC_MINE_MATERIAL_ROLL != 0 {
        names.push("mineMaterialRoll");
    }
    if names.is_empty() {
        format!("unknown bits {src:#x}")
    } else {
        names.join(" + ")
    }
}

/// The one-value instrument, pure over [`lookup`]/[`decode_band`]: what does `value`'s low half
/// say? `what` labels the value in the report ("fault target", "register r13"). `None` = the low
/// half is neither in a FullID band nor a recorded id, so there is nothing worth a line.
pub fn annotate_value(what: &str, value: u64) -> Option<String> {
    let low = value as u32;
    let band = decode_band(low);
    let hit = lookup(low);
    match (band, hit) {
        (Some((cat, row)), Some(src)) => Some(format!(
            "seed-ids: {what} low half {low:#010x} = {cat} FullID row {row}; PRESENT in this \
             seed's tables: {} -- an id OUR writers handle sitting inside a pointer-shaped value \
             is the #351 shape\n",
            src_names(src)
        )),
        (Some((cat, row)), None) => Some(format!(
            "seed-ids: {what} low half {low:#010x} = {cat} FullID row {row}; not in any table \
             this seed recorded (a band match alone is NOT evidence -- 1/16 of all u32s are in a \
             band)\n"
        )),
        (None, Some(src)) => Some(format!(
            "seed-ids: {what} low half {low:#010x} is not in a FullID band but MATCHES a recorded \
             id: {}\n",
            src_names(src)
        )),
        (None, None) => None,
    }
}

/// Annotate the fault TARGET (`ExceptionInformation[1]`), always emitting a line: the empty-
/// registry case is itself triage information ("never connected" vs "connected but no match").
pub fn annotate_fault(addr: u64) -> String {
    let n = registered();
    let dropped = DROPPED.load(Ordering::Relaxed);
    if n == 0 {
        return "seed-ids: no seed ids recorded this session (never connected, or the seed's \
                enemyDropRoll/shop tables were all empty)\n"
            .into();
    }
    let mut s = annotate_value("fault target", addr).unwrap_or_else(|| {
        format!(
            "seed-ids: fault target low half {:#010x} is not id-shaped; {n} id(s) registered\n",
            addr as u32
        )
    });
    if dropped != 0 {
        s.push_str(&format!(
            "seed-ids: WARNING -- the registry FILLED and dropped {dropped} id(s); a miss above \
             can be a false miss\n"
        ));
    }
    s
}

/// Annotate the general-purpose registers. crash-19968's id-shaped value sat in r13's LOW HALF,
/// not in the fault target, so the registers get the same question. Duplicate low halves (a value
/// copied across registers is the common case) print once.
pub fn annotate_registers(regs: &[(&str, u64)]) -> String {
    let mut seen: Vec<u32> = Vec::new();
    let mut out = String::new();
    for &(name, v) in regs {
        let low = v as u32;
        if seen.contains(&low) {
            continue;
        }
        if let Some(line) = annotate_value(&format!("register {name}"), v) {
            seen.push(low);
            out.push_str(&line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE MOTIVATING VALUE, verbatim from client#351: crash-19968's r13 was
    /// `0x00000002_40002775` -- a live pointer whose low half is the FullID of goods 10101, an id
    /// the session's own enemyDropRoll table carried. The decode must read exactly that.
    #[test]
    fn the_observed_r13_low_half_decodes_as_goods_10101() {
        let r13 = 0x00000002_40002775u64;
        assert_eq!(decode_band(r13 as u32), Some(("goods", 10101)));
    }

    /// The fault target too: `0x240002785`'s low half is `0x40002785` = goods 10117. (Both
    /// halves landing in the goods band is the coincidence-stacking the issue warns about -- the
    /// MEMBERSHIP answer is the strong signal, and it is tested below.)
    #[test]
    fn the_observed_fault_target_low_half_is_also_goods_band() {
        assert_eq!(decode_band(0x40002785), Some(("goods", 10117)));
    }

    /// The weapon band (nibble 0) is EXCLUDED: `0x0000_2775` is indistinguishable from the small
    /// integers ordinary code leaves in registers, and naming it "weapon 10101" would manufacture
    /// evidence out of noise.
    #[test]
    fn the_weapon_band_and_row_zero_are_not_decoded() {
        assert_eq!(decode_band(0x0000_2775), None);
        assert_eq!(decode_band(0x4000_0000), None);
        assert_eq!(decode_band(0x3000_2775), None); // no such category
    }

    #[test]
    fn the_other_known_bands_decode() {
        assert_eq!(decode_band(0x1000_04D2), Some(("protector", 1234)));
        assert_eq!(decode_band(0x2000_04D2), Some(("accessory", 1234)));
        assert_eq!(decode_band(0x8000_04D2), Some(("gem/ash-of-war", 1234)));
    }

    /// A value that is neither band-shaped nor recorded must produce NO line -- the register scan
    /// runs over 15 noise-filled registers per report, and a report that prints 15 non-findings
    /// buries the one that matters.
    #[test]
    fn an_uninteresting_value_is_silence_not_a_line() {
        assert_eq!(annotate_value("register rax", 0x0000_0000_DEAD), None);
        assert_eq!(annotate_value("register rax", 0x7000_0001), None);
    }

    /// ONE stateful test, deliberately: the registry is process-wide statics, so parallel tests
    /// each doing record/clear would race. All statics-touching assertions live in this one
    /// sequential body; everything else above is pure.
    #[test]
    fn the_registry_round_trips_merges_and_clears() {
        // Ids chosen to be unique to this test so no other test's records can alias them.
        let goods_10101 = 0x4000_2775u32;
        let goods_10999 = 0x4000_2AF7u32;
        clear();
        assert_eq!(lookup(goods_10101), None, "present before any record");

        record(goods_10101, SRC_ENEMY_DROP_ROLL);
        assert_eq!(lookup(goods_10101), Some(SRC_ENEMY_DROP_ROLL));

        // Re-recording under a second source MERGES, and the annotate line names both tables.
        record(goods_10101, SRC_SHOP);
        assert_eq!(lookup(goods_10101), Some(SRC_ENEMY_DROP_ROLL | SRC_SHOP));
        let line =
            annotate_value("register r13", 0x2_4000_2775).expect("a recorded id must annotate");
        assert!(line.contains("goods FullID row 10101"), "{line}");
        assert!(line.contains("PRESENT"), "{line}");
        assert!(line.contains("enemyDropRoll"), "{line}");

        record(goods_10101, SRC_MINE_MATERIAL_ROLL);
        let line = annotate_value("register r13", 0x2_4000_2775).unwrap();
        assert!(line.contains("mineMaterialRoll"), "{line}");

        // A band-shaped id nobody recorded annotates WITHOUT the membership claim (the weak half
        // of the signal, labeled as such).
        let miss = annotate_value("fault target", 0x2_4000_2785).expect("band decode");
        assert!(miss.contains("goods FullID row 10117"), "{miss}");
        assert!(miss.contains("not in any table"), "{miss}");

        // clear() (the next seed's slot_data parse) retires everything: LAST seed's ids must not
        // vouch for THIS seed's crash.
        record(goods_10999, SRC_ENEMY_DROP_ROLL);
        assert!(lookup(goods_10999).is_some());
        clear();
        assert_eq!(lookup(goods_10101), None, "stale id survived clear()");
        assert_eq!(lookup(goods_10999), None, "stale id survived clear()");
        assert_eq!(registered(), 0);
    }
}
