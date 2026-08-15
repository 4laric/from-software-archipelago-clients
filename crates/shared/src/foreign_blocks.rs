//! Registry of memory WE allocated and then handed to the GAME to own.
//!
//! MOTIVATION (2026-07-30, boblerrr's log): 14 crashes in one 9-hour session, all byte-identical --
//! `ACCESS_VIOLATION (read)` at `eldenring.exe+0x251f545`, same 21-frame backtrace, thread 1. The
//! datum that makes this module exist: **all 14 fault addresses were exactly `X - 8` where `X` is
//! 64 KB-aligned** (`0x2458604fff8 + 8 = 0x24586050000`, and so on, 14/14, no exceptions).
//!
//! 64 KB is the `VirtualAlloc` allocation granularity, and a read at `[block - 8]` is what a
//! FromSoft `DLKR::DLAllocator` does -- it stores its header immediately BELOW the pointer it hands
//! back. `fmg_inject::valloc` is the ONLY place in this client that `VirtualAlloc`s a block and
//! writes the pointer into a slot the game owns (`swap_goods` / `swap_category` ->
//! `base_array[0][category]`). `VirtualAlloc`'s returned base IS the base of the reservation, so
//! the 8 bytes below it are OUTSIDE the region -- unmapped -- and any game-side header read faults
//! at an address ending `fff8`.
//!
//! STOP: **that is an inference, not a proof, and this module exists to replace it with a proof.**
//! The competing explanation is a game-side large allocation (Windows also routes >512 KB through
//! `VirtualAlloc`, so the game's own big buffers are 64 KB-aligned too) whose header read faults
//! for reasons of its own. Both stories predict exactly the observed signature. The discriminator
//! is whether the faulting address lands under a block *we* made -- which nothing currently records.
//!
//! So: [`record`] every block on the way out of our allocator, and let the crash handler
//! [`annotate`] the fault with a hit or a miss. A HIT names the block and closes the question. A
//! MISS is just as valuable -- it exonerates `fmg_inject` and sends the hunt elsewhere, which is
//! why the annotation prints on a miss too rather than staying silent.
//!
//! DISCIPLINE: read from a fatal exception handler. No allocation, no locks, no panics -- a fixed
//! static ring of atomics, `Relaxed` throughout (we are racing a dying process, not synchronizing
//! with it). Recording is wait-free so it cannot deadlock a game thread mid-swap.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Ring capacity. boblerrr's busiest session swapped 168 times, so 256 keeps a whole session of
/// blocks -- including the ABANDONED ones, which matter: every swap leaks the previous block, and a
/// stale pointer the game still holds is a live suspect.
const CAP: usize = 256;

/// How far below a block base still counts as a header read. A `DLKR::DLAllocator` header is 8-32
/// bytes depending on the allocator; widen slightly so a 16- or 32-byte header still matches
/// instead of reporting a near-miss as a miss.
const HEADER_WINDOW: usize = 64;

/// What a faulting address is, relative to the blocks we handed the game.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The fault is just BELOW one of our blocks: the game read an allocator header that is not
    /// there. This is the shape all 14 of boblerrr's crashes had.
    HeaderRead {
        base: usize,
        size: usize,
        below: usize,
    },
    /// The fault is INSIDE one of our blocks: still ours, but a layout bug rather than a header
    /// read. Must not be reported as a miss.
    Inside {
        base: usize,
        size: usize,
        offset: usize,
    },
    /// The fault matches nothing we allocated. This EXONERATES `fmg_inject` for that crash, which
    /// is why it is a verdict and not silence.
    Miss,
}

/// The whole instrument, as a pure function: does `addr` fall under or inside any of `blocks`
/// (`(base, size)` pairs, `base == 0` meaning an unused ring slot)?
///
/// Pure and total so it can be tested without touching the process-wide ring -- the statics below
/// are only storage, and this is the thing that can be wrong.
pub fn classify(blocks: &[(usize, usize)], addr: usize) -> Verdict {
    for &(base, size) in blocks {
        if base == 0 {
            continue;
        }
        if addr < base && base - addr <= HEADER_WINDOW {
            return Verdict::HeaderRead {
                base,
                size,
                below: base - addr,
            };
        }
        if size != 0 && addr >= base && addr < base + size {
            return Verdict::Inside {
                base,
                size,
                offset: addr - base,
            };
        }
    }
    Verdict::Miss
}

static BASE: [AtomicUsize; CAP] = [const { AtomicUsize::new(0) }; CAP];
static SIZE: [AtomicUsize; CAP] = [const { AtomicUsize::new(0) }; CAP];
/// Monotonic count of blocks ever recorded; `% CAP` is the ring slot. Never reset.
static COUNT: AtomicUsize = AtomicUsize::new(0);

/// Record a block we allocated and are about to hand to the game. Wait-free; safe from any thread.
pub fn record(base: usize, size: usize) {
    let i = COUNT.fetch_add(1, Ordering::Relaxed) % CAP;
    // Base first, then size: a concurrent reader that sees a base always sees a size for it or a
    // zero, never a size belonging to the block this slot used to hold.
    BASE[i].store(base, Ordering::Relaxed);
    SIZE[i].store(size, Ordering::Relaxed);
}

/// Total blocks recorded this session (may exceed [`CAP`]; the ring holds the newest `CAP`).
pub fn recorded() -> usize {
    COUNT.load(Ordering::Relaxed)
}

/// Snapshot the live ring slots into `out`, returning the populated prefix. Allocation-free so the
/// crash handler can call it on a dying process.
fn snapshot(out: &mut [(usize, usize); CAP]) -> usize {
    let live = COUNT.load(Ordering::Relaxed).min(CAP);
    for (slot, (b, s)) in out.iter_mut().zip(BASE.iter().zip(SIZE.iter())).take(live) {
        *slot = (b.load(Ordering::Relaxed), s.load(Ordering::Relaxed));
    }
    live
}

/// Classify a faulting DATA address against the registry and render it for the crash report.
///
/// `addr` is the AV target (`ExceptionInformation[1]`), NOT the faulting instruction.
pub fn annotate(addr: usize) -> String {
    let n = recorded();
    if n == 0 {
        return "foreign-blocks: none recorded this session (fmg_inject never allocated)\n".into();
    }
    let mut buf = [(0usize, 0usize); CAP];
    let live = snapshot(&mut buf);
    match classify(&buf[..live], addr) {
        Verdict::HeaderRead { base, size, below } => format!(
            "foreign-blocks: HIT -- fault is {below} byte(s) BELOW our block @ {base:#x} \
             (size {size:#x}); this is the game reading an allocator header under memory \
             fmg_inject::valloc handed it. {n} block(s) recorded.\n"
        ),
        Verdict::Inside { base, size, offset } => format!(
            "foreign-blocks: HIT -- fault is INSIDE our block @ {base:#x} (size {size:#x}), \
             offset {offset:#x}; the block is ours but the fault is not a header read. \
             {n} block(s) recorded.\n"
        ),
        Verdict::Miss => format!(
            "foreign-blocks: miss -- {addr:#x} matches none of the {live} block(s) in the ring \
             ({n} recorded this session); fmg_inject is NOT implicated by this fault.\n"
        ),
    }
}

#[cfg(test)]
// 🛑 THE GROUPING IS THE POINT, so the lint is refused rather than obeyed here (client#198).
// Every literal below is a fault address copied VERBATIM out of boblerrr's crash-2268.txt, and the
// 4_4_3 split is what makes `0x2458_6050_000` readable as the same address as the raw
// `0x2458604fff8` in the report. clippy's `0x0245_8605_0000` is the same number and breaks that
// correspondence -- in the one module whose whole job is to be checked against a crash log by hand.
// These grew a gate on 2026-08-15 when `--all-targets` first linted test code; they are not new.
#[allow(clippy::unusual_byte_groupings)]
mod tests {
    use super::*;

    /// THE MOTIVATING CASE, verbatim from boblerrr's crash-2268.txt: fault `0x2458604fff8` against
    /// the block base its arithmetic implies (`fault + 8`, 64 KB-aligned). If this stops reporting
    /// a header read, the instrument is broken and the next crash report proves nothing.
    #[test]
    fn the_observed_fault_is_eight_bytes_below_a_recorded_block() {
        let blocks = [(0x2458_6050_000usize, 0x8000usize)];
        assert_eq!(
            classify(&blocks, 0x2458_604f_ff8),
            Verdict::HeaderRead {
                base: 0x2458_6050_000,
                size: 0x8000,
                below: 8
            }
        );
    }

    /// All 14 of boblerrr's fault addresses, so a future edit cannot pass the one case above by
    /// accident. Each is paired with the block base `fault + 8` implies.
    #[test]
    fn every_one_of_the_fourteen_observed_faults_classifies_as_a_header_read() {
        for fault in [
            0x2458604fff8usize,
            0x177f669fff8,
            0x2827a2dfff8,
            0x1ee15fbfff8,
            0x22afcf0fff8,
            0x15b5384fff8,
            0x1dcd5d7fff8,
            0x1c5a2f2fff8,
            0x272f412fff8,
            0x2789ebcfff8,
            0x1a05822fff8,
            0x1ee837efff8,
            0x18ffb05fff8,
            0x1a37de0fff8,
        ] {
            let base = fault + 8;
            assert_eq!(base % 0x1_0000, 0, "{base:#x} is not 64KB-aligned");
            assert!(
                matches!(
                    classify(&[(base, 0x8000)], fault),
                    Verdict::HeaderRead { below: 8, .. }
                ),
                "{fault:#x}"
            );
        }
    }

    #[test]
    fn an_unrelated_address_is_a_miss_not_silence() {
        assert_eq!(
            classify(&[(0x1000_0000, 0x1000)], 0xdead_0000),
            Verdict::Miss
        );
    }

    #[test]
    fn an_address_inside_a_block_is_a_hit_but_not_a_header_read() {
        assert_eq!(
            classify(&[(0x2000_0000, 0x1000)], 0x2000_0800),
            Verdict::Inside {
                base: 0x2000_0000,
                size: 0x1000,
                offset: 0x800
            }
        );
    }

    /// A fault far below a block must NOT be dragged in by the header window -- otherwise the
    /// instrument manufactures a HIT for any address that happens to sit under our heap.
    #[test]
    fn a_fault_far_below_a_block_is_not_pulled_in_by_the_header_window() {
        let base = 0x2458_6050_000usize;
        assert_eq!(
            classify(&[(base, 0x8000)], base - HEADER_WINDOW - 1),
            Verdict::Miss
        );
        assert!(matches!(
            classify(&[(base, 0x8000)], base - HEADER_WINDOW),
            Verdict::HeaderRead { .. }
        ));
    }

    /// Empty ring slots (`base == 0`) must be skipped, not treated as a block at address 0.
    #[test]
    fn unused_ring_slots_are_skipped() {
        assert_eq!(classify(&[(0, 0), (0, 0)], 0x20), Verdict::Miss);
    }

    /// The ring itself: wait-free recording, wraps at CAP, and `recorded()` keeps counting past it.
    #[test]
    fn the_ring_wraps_and_keeps_counting() {
        let before = recorded();
        for k in 1..=(CAP + 8) {
            record(0x8000_0000 + k * 0x1_0000, 0x100);
        }
        assert!(recorded() >= before + CAP + 8);
        let mut buf = [(0usize, 0usize); CAP];
        assert_eq!(snapshot(&mut buf), CAP);
    }
}
