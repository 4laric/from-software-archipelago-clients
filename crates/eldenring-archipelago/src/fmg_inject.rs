//! fmg_inject.rs — name the synthetic AP goods by rebuilding the GoodsName MsgData (FMG entry
//! injection, allocate-extend-swap). STAGED by `MODE`; see FMG-INJECTION-PLAN.md.
//!   MODE_PARSE (0):    READ-ONLY parse + cross-check vs SearchStringTable.            [proven]
//!   MODE_IDENTITY (1): rebuild an IDENTICAL block, self-validate, swap base_array[0][10].
//!   MODE_INJECT (2):   identity + add synthetic-good entries (next stage).
//!
//! MsgData layout (RE'd from 0x266DC90): +0x0C u32 groupCount ; +0x18 u64* stringOffsetTable
//! (MsgData-relative offsets) ; +0x28 GroupRecord[count] 16B each {u32 stringIndexBase,u32 firstId,
//! u32 lastId,u32 pad}. stringIndex=(id-firstId)+stringIndexBase ; off=offsetTable[idx] ;
//! str=MsgData+off (0=>missing). repo @ base+0x3D7D4F8 ; base_array[0][10]=GoodsName MsgData ;
//! SearchStringTable @ base+0x266D3C0 (repo,group=0,category,id)->wchar*.
//!
//! SAFETY: the rebuilt block is validated in OUR memory (re-lookup the check ids) BEFORE the swap; a
//! build mismatch aborts the swap (game untouched). The new block is VirtualAlloc'd RW and leaked for
//! the process. Identity stage makes NO visible change if correct — that's the proof.
//!
//! 🔥 THE BLOCK NEEDS PADDING BELOW IT (`GUARD_BELOW`). The game reads an allocator header 8
//! bytes under the MsgData pointer, and a bare `VirtualAlloc` base has nothing mapped there. That
//! was the CTD: boblerrr crashed 14/14 sessions on 2026-07-30, every fault 8 bytes below a 64 KB
//! boundary, then 6/6 `foreign-blocks: HIT` on the instrumented build on 2026-07-31. Do not
//! "simplify" the guard away.

// This module is deliberately one big raw-pointer FFI surface: every `unsafe fn` here IS the safety
// boundary (its own doc/SAFETY notes cover the invariants), so Rust 2024's `unsafe_op_in_unsafe_fn`
// would only add noise without adding a real check. Silence it module-wide rather than wrapping each
// raw read/write in a redundant inner `unsafe {}`.
#![allow(unsafe_op_in_unsafe_fn)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

const REPO_RVA: usize = 0x3D7D4F8;
const SEARCH_RVA: usize = 0x266D3C0;
const SEARCH_SIG: &[u8] = &[
    0x3B, 0x51, 0x10, 0x73, 0x29, 0x44, 0x3B, 0x41, 0x14, 0x73, 0x23, 0x48, 0x8B, 0x41, 0x08,
];
const GOODS_CATEGORY: u32 = 10;

const MD_GROUPCOUNT: usize = 0x0C;
const MD_OFFTABLE_PTR: usize = 0x18;
const MD_GROUPS: usize = 0x28;
// Upper bound on how many UTF-16 units `read_units` will copy before giving up looking for the NUL.
// FMG strings are always NUL-terminated, so this only caps pathological/unterminated reads — but it
// MUST exceed the longest real string we copy. 128 was fine for GoodsName (short item names) but
// TRUNCATES vanilla GoodsCaption/Info (lore boxes run many hundreds of units); 4096 covers captions.
const STR_CAP: usize = 4096;

// sub[] category index for GoodsCaption (the big lore box). PINNED 2026-06-30 via the fmg_probe
// slot-map dump: for id 150, sub[10]=name "Furlcalling Finger Remedy", sub[20]=GoodsInfo short line
// "Reveals co-op and hostile summoning signs", sub[24]=the multi-line lore caption "Item for online
// play.\n(Can also be used...". So GoodsCaption = category 24. (GoodsInfo = 20 if we ever want the
// short line too.) >=0 enables `run_descriptions`.
const GOODS_CAPTION_CATEGORY: i32 = 24;

const MODE_PARSE: u8 = 0;
#[allow(dead_code)]
const MODE_IDENTITY: u8 = 1;
const MODE_INJECT: u8 = 2;
const MODE: u8 = MODE_INJECT;

const CHECK_IDS: &[u32] = &[100, 101, 109, 110, 115, 150, 8000, 10100];

type SearchFn = unsafe extern "C" fn(*mut c_void, u32, u32, u32) -> *const u16;

static DONE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
struct Group {
    string_index_base: u32,
    first_id: u32,
    last_id: u32,
}

fn plausible(p: usize) -> bool {
    (0x10000..0x7FFF_FFFF_FFFF).contains(&p)
}
fn current_module_base() -> Option<usize> {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    let h = unsafe { GetModuleHandleW(None) }.ok()?;
    Some(h.0 as usize)
}
unsafe fn read_u32(a: usize) -> u32 {
    (a as *const u32).read_unaligned()
}
unsafe fn read_u64(a: usize) -> u64 {
    (a as *const u64).read_unaligned()
}
unsafe fn read_usize(a: usize) -> usize {
    (a as *const usize).read_unaligned()
}
unsafe fn write_u64(a: usize, v: u64) {
    (a as *mut u64).write_unaligned(v);
}
unsafe fn write_u16(a: usize, v: u16) {
    (a as *mut u16).write_unaligned(v);
}
unsafe fn read_units(ptr: usize) -> Vec<u16> {
    let mut v = Vec::new();
    for i in 0..STR_CAP {
        let c = (ptr as *const u16).add(i).read_unaligned();
        if c == 0 {
            break;
        }
        v.push(c);
    }
    v
}
fn read_string(ptr: usize) -> Option<String> {
    if !plausible(ptr) {
        return None;
    }
    let u = unsafe { read_units(ptr) };
    if u.is_empty() {
        None
    } else {
        Some(String::from_utf16_lossy(&u))
    }
}
fn sig_ok(addr: usize) -> bool {
    let b = unsafe { std::slice::from_raw_parts(addr as *const u8, SEARCH_SIG.len()) };
    b == SEARCH_SIG
}

unsafe fn goods_msgdata(base: usize) -> Option<usize> {
    let repo = read_usize(base + REPO_RVA);
    if !plausible(repo) {
        return None;
    }
    let base_arr = read_usize(repo + 0x08);
    if !plausible(base_arr) {
        return None;
    }
    let sub = read_usize(base_arr);
    if !plausible(sub) {
        return None;
    }
    let md = read_usize(sub + GOODS_CATEGORY as usize * 8);
    if plausible(md) { Some(md) } else { None }
}

unsafe fn parse(md: usize) -> Option<(Vec<Group>, Vec<u64>)> {
    let count = read_u32(md + MD_GROUPCOUNT) as usize;
    if count == 0 || count > 0x10000 {
        return None;
    }
    let mut groups = Vec::with_capacity(count);
    let mut num_strings: u32 = 0;
    for i in 0..count {
        let g = md + MD_GROUPS + i * 16;
        let sib = read_u32(g);
        let fi = read_u32(g + 4);
        let li = read_u32(g + 8);
        if li < fi || li.saturating_sub(fi) > 0x10_0000 {
            return None;
        }
        groups.push(Group {
            string_index_base: sib,
            first_id: fi,
            last_id: li,
        });
        num_strings = num_strings.max(sib.saturating_add(li - fi).saturating_add(1));
    }
    if num_strings == 0 || num_strings > 0x20_0000 {
        return None;
    }
    let offtab = read_usize(md + MD_OFFTABLE_PTR);
    if !plausible(offtab) {
        return None;
    }
    let mut offsets = Vec::with_capacity(num_strings as usize);
    for s in 0..num_strings as usize {
        offsets.push(read_u64(offtab + s * 8));
    }
    Some((groups, offsets))
}

fn my_lookup(md: usize, groups: &[Group], offsets: &[u64], id: u32) -> Option<String> {
    let g = groups
        .iter()
        .find(|g| id >= g.first_id && id <= g.last_id)?;
    let si = (id - g.first_id + g.string_index_base) as usize;
    let off = *offsets.get(si)?;
    if off == 0 {
        return None;
    }
    read_string(md + off as usize)
}

/// Bytes of committed, zeroed padding placed BELOW every block we hand the game.
///
/// PROVEN 2026-07-31 by boblerrr's log, client `0.2.18 (7e03d2856019)`: SIX crashes, SIX
/// `foreign-blocks: HIT`, zero misses, every one of them `fault is 8 byte(s) BELOW our block` and
/// naming the block by address and size. The game reads an allocator header immediately under the
/// MsgData pointer -- a `DLKR::DLAllocator` convention -- and `VirtualAlloc` returns the base of the
/// RESERVATION, so those 8 bytes were unmapped. Hence 14/14 earlier faults at an address ending
/// `fff8`, i.e. 8 below a 64 KB boundary.
///
/// A whole page, not 8 or 16 bytes: the crash proves a read at -8, but nothing proves -8 is the
/// deepest the game ever reaches, and 4 KB against a ~115 KB block is free. The padding is zeroed by
/// `VirtualAlloc`, so the header reads as zeros instead of faulting.
///
/// 🛑 This makes the READ safe. It does NOT make us the owner: if the game ever tries to FREE or
/// REALLOC this block it will hand a zeroed header to its own allocator. The real fix is allocating
/// through the game's allocator so the header is genuine; that needs the allocator entry point,
/// which is not pinned yet. Keep `foreign_blocks` armed -- if this regresses, the next crash says so
/// in one line.
const GUARD_BELOW: usize = 0x1000;

unsafe fn valloc(size: usize) -> Option<usize> {
    use windows::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc};
    let p = VirtualAlloc(
        None,
        size + GUARD_BELOW,
        MEM_COMMIT | MEM_RESERVE,
        PAGE_READWRITE,
    );
    if p.is_null() {
        None
    } else {
        // Hand out a pointer INSIDE the reservation, so the bytes below it are ours and mapped.
        let p = (p as usize + GUARD_BELOW) as *mut c_void;
        let base = p as usize;
        // Stay registered even though the fault is fixed: `foreign_blocks` is what PROVED this
        // (6 crashes, 6 HITs, 0 misses on 2026-07-31), and it is the only thing that would catch a
        // regression -- if `GUARD_BELOW` is ever dropped or the game reaches deeper than a page,
        // the next crash report says so in one line instead of costing another round trip.
        shared::foreign_blocks::record(base, size);
        log::info!(
            "FMG valloc: block #{} @ {base:#x} size {size:#x} (+{:#x} guard below, mapped) -- a \
             crash reading BELOW this address would mean the game reached past the guard for an \
             allocator header",
            shared::foreign_blocks::recorded(),
            GUARD_BELOW
        );
        Some(base) // VirtualAlloc memory is zero-initialized, guard page included
    }
}

unsafe fn write_u32(a: usize, v: u32) {
    (a as *mut u32).write_unaligned(v);
}

/// Build a fresh MsgData block = vanilla content + a new group per RUN of `injects` ids (`injects`
/// MUST be sorted by id and hold no vanilla-covered id; they may sit ANYWHERE in the id range).
/// Header copied verbatim; groups + offset table + (deduped vanilla + injected) string data are laid
/// out fresh, all base-relative. `injects` empty => exact identity rebuild. Returns the new block.
///
/// 🔥 THE INJECTED GROUPS ARE SORTED INTO PLACE, not appended (2026-08-03, issue #300). Until now
/// this required every injected id to sort ABOVE every vanilla id, which is true of the synthetic
/// goods ids (~3.78M) and false of a spare goods row at ~9350 -- so `extend_swap_overrides` could
/// only ever REDIRECT an existing entry, and an id with no entry in the category was unwritable.
/// GoodsInfo(20)/GoodsCaption(24) cover far fewer ids than GoodsName(10), which is exactly how
/// boblerrr's `0.3.1` log got `names=53 infos=25 captions=25`. The game BINARY SEARCHES the group
/// array, so the only real constraint is that it stays ascending and disjoint -- checked here by
/// `er_logic::fmg_groups::is_ordered_disjoint` (host-tested), and a failure aborts the build rather
/// than handing the game an array whose search result depends on the array length.
unsafe fn build_block(
    vanilla: usize,
    groups: &[Group],
    offsets: &[u64],
    injects: &[(u32, Vec<u16>)],
    overrides: &[(u32, Vec<u16>)],
) -> Option<usize> {
    let num_vanilla = offsets.len();
    let num_out = num_vanilla + injects.len();
    // Overrides REPLACE the string of an EXISTING id (no new string-index slot): resolve each id to its
    // string index via the vanilla groups; ids not in any group are skipped. The redirect + the appended
    // override string happen below. (si -> new units)
    let mut ovr: Vec<(usize, &Vec<u16>)> = Vec::new();
    for (id, s) in overrides {
        if let Some(g) = groups
            .iter()
            .find(|g| *id >= g.first_id && *id <= g.last_id)
        {
            ovr.push(((*id - g.first_id + g.string_index_base) as usize, s));
        }
    }
    // Merge contiguous inject ids into RUNS -> one group per run. This keeps groups few (28 spare ids
    // cost 4 records) so the game's binary search is boundary-safe (4777 single-id groups created an
    // edge case on the lowest ids). String indices stay sequential per inject, which is why the run
    // merge and the index assignment are ONE host-tested function rather than two loops that agree
    // by inspection.
    let inj_ids: Vec<u32> = injects.iter().map(|(id, _)| *id).collect();
    let Some(runs) = er_logic::fmg_groups::runs_from_ids(&inj_ids, num_vanilla as u32) else {
        log::warn!(
            "FMG build_block: the {} injected id(s) are not strictly ascending; NOT building. A \
             run's ids must map to CONSECUTIVE offset-table slots, so an unsorted/duplicated input \
             would point records at the wrong strings.",
            inj_ids.len()
        );
        return None;
    };
    // The final record array: vanilla + injected, ASCENDING BY ID. Sorted, not appended -- an
    // injected id may sit in the middle of the vanilla range (see the header).
    //
    // 🛑 BOTH the merge and the ordering gate are skipped when there is nothing to inject, so the
    // block this produces for the identity / redirect-only path is BYTE-IDENTICAL to the one it has
    // been producing in production. A new gate belongs on the new shape: if the vanilla array ever
    // turned out not to be strictly ascending, checking it unconditionally would take down the
    // override path that has been shipping for weeks, and re-SORTING it unconditionally would hand
    // the game a different array than it had. Insertion is the only caller that needs either.
    let mut all_groups: Vec<Group> = groups.to_vec();
    if !runs.is_empty() {
        all_groups.extend(runs.iter().map(|r| Group {
            string_index_base: r.string_index_base,
            first_id: r.first_id,
            last_id: r.last_id,
        }));
        all_groups.sort_by_key(|g| g.first_id);
        let spans: Vec<er_logic::fmg_groups::Span> = all_groups
            .iter()
            .map(|g| er_logic::fmg_groups::Span::new(g.first_id, g.last_id))
            .collect();
        if !er_logic::fmg_groups::is_ordered_disjoint(&spans) {
            // Two records claiming one id give the game's binary search two answers, and WHICH it
            // returns is a function of the array length -- an intermittent wrong string or an
            // out-of-range offset-table index, not an obvious break. Refuse to allocate it. (The
            // check covers the vanilla records too, since they are in the merged array.)
            log::warn!(
                "FMG build_block: the rebuilt group array ({} vanilla + {} injected record(s)) is \
                 not ascending/disjoint; NOT building (the game binary-searches it). An injected id \
                 that a vanilla group already covers is the usual cause -- it wants a REDIRECT, not \
                 a new entry.",
                groups.len(),
                runs.len()
            );
            return None;
        }
    }
    let count = all_groups.len();
    let offtab_off = ((MD_GROUPS + count * 16) + 7) & !7;
    let strings_off = ((offtab_off + num_out * 8) + 1) & !1;

    // dedup vanilla strings by offset (multiple ids may share one string)
    let mut uniq_units: Vec<Vec<u16>> = Vec::new();
    let mut map: HashMap<u64, usize> = HashMap::new();
    for &off in offsets {
        if off == 0 || map.contains_key(&off) {
            continue;
        }
        map.insert(off, uniq_units.len());
        uniq_units.push(read_units(vanilla + off as usize));
    }
    let uniq_bytes: usize = uniq_units.iter().map(|s| (s.len() + 1) * 2).sum();
    let inj_bytes: usize = injects.iter().map(|(_, s)| (s.len() + 1) * 2).sum();
    let ovr_bytes: usize = ovr.iter().map(|(_, s)| (s.len() + 1) * 2).sum();
    let total = strings_off + uniq_bytes + inj_bytes + ovr_bytes;

    let block = valloc(total)?;
    // header (0..0x28) verbatim
    std::ptr::copy_nonoverlapping(vanilla as *const u8, block as *mut u8, MD_GROUPS);
    // group records, in the ascending/disjoint order validated above (vanilla and injected merged).
    for (i, g) in all_groups.iter().enumerate() {
        let gr = block + MD_GROUPS + i * 16;
        write_u32(gr, g.string_index_base);
        write_u32(gr + 4, g.first_id);
        write_u32(gr + 8, g.last_id);
        write_u32(gr + 12, 0);
    }
    write_u32(block + MD_GROUPCOUNT, count as u32);

    // strings: deduped vanilla (record new offsets), then injected names
    let mut wpos = strings_off;
    let mut uniq_off: Vec<u64> = Vec::with_capacity(uniq_units.len());
    for s in &uniq_units {
        let dst = block + wpos;
        for (i, &u) in s.iter().enumerate() {
            write_u16(dst + i * 2, u);
        }
        write_u16(dst + s.len() * 2, 0);
        uniq_off.push(wpos as u64);
        wpos += (s.len() + 1) * 2;
    }
    let mut inj_off: Vec<u64> = Vec::with_capacity(injects.len());
    for (_, s) in injects {
        let dst = block + wpos;
        for (i, &u) in s.iter().enumerate() {
            write_u16(dst + i * 2, u);
        }
        write_u16(dst + s.len() * 2, 0);
        inj_off.push(wpos as u64);
        wpos += (s.len() + 1) * 2;
    }
    // override strings: appended after the injected strings; record the new offset per overridden
    // string index so the offset table below can redirect that id to its longer string.
    let mut ovr_off: HashMap<usize, u64> = HashMap::new();
    for (si, s) in &ovr {
        let dst = block + wpos;
        for (i, &u) in s.iter().enumerate() {
            write_u16(dst + i * 2, u);
        }
        write_u16(dst + s.len() * 2, 0);
        ovr_off.insert(*si, wpos as u64);
        wpos += (s.len() + 1) * 2;
    }
    // offset table: vanilla indices (relocated), redirecting any overridden index to its new string,
    // then injected indices.
    for (s, &off) in offsets.iter().enumerate() {
        let v = if let Some(&o) = ovr_off.get(&s) {
            o
        } else if off == 0 {
            0
        } else {
            uniq_off[map[&off]]
        };
        write_u64(block + offtab_off + s * 8, v);
    }
    for (i, &off) in inj_off.iter().enumerate() {
        write_u64(block + offtab_off + (num_vanilla + i) * 8, off);
    }
    write_u64(block + MD_OFFTABLE_PTR, (block + offtab_off) as u64);
    Some(block)
}

unsafe fn swap_goods(base: usize, newblock: usize) -> bool {
    let repo = read_usize(base + REPO_RVA);
    let base_arr = read_usize(repo + 0x08);
    let sub = read_usize(base_arr);
    if !plausible(sub) {
        return false;
    }
    let slot = sub + GOODS_CATEGORY as usize * 8; // &base_array[0][10]
    (slot as *mut usize).write_unaligned(newblock);
    true
}

/// Resolve `base_array[0][category]` (the MsgData* for an arbitrary FMG category). Generalization of
/// `goods_msgdata` for the caption path; `category_msgdata(base, GOODS_CATEGORY) == goods_msgdata`.
unsafe fn category_msgdata(base: usize, category: u32) -> Option<usize> {
    let repo = read_usize(base + REPO_RVA);
    if !plausible(repo) {
        return None;
    }
    let base_arr = read_usize(repo + 0x08);
    if !plausible(base_arr) {
        return None;
    }
    let sub = read_usize(base_arr);
    if !plausible(sub) {
        return None;
    }
    let md = read_usize(sub + category as usize * 8);
    if plausible(md) { Some(md) } else { None }
}

/// Atomically point `base_array[0][category]` at a freshly-built block. Generalization of `swap_goods`.
unsafe fn swap_category(base: usize, category: u32, newblock: usize) -> bool {
    let repo = read_usize(base + REPO_RVA);
    let base_arr = read_usize(repo + 0x08);
    let sub = read_usize(base_arr);
    if !plausible(sub) {
        return false;
    }
    let slot = sub + category as usize * 8;
    (slot as *mut usize).write_unaligned(newblock);
    true
}

/// Inject GoodsCaption (the big lore box) entries for the synthetic AP goods — the SAME
/// allocate-extend-swap as `run()`, but into the caption category instead of GoodsName, and reusing
/// the proven `build_block` / `parse` / `my_lookup` helpers so the confirmed name path is untouched.
///
/// `descriptions` = `(synthetic_id, utf16_caption)` pairs built by the caller from the scout cache via
/// `er_logic::name_override::description(game, owner, slot, kind)` — the same per-id data that feeds
/// the real name. Ids must be the synthetic goods ids (all > every vanilla caption id); this sorts
/// them so the appended groups stay ascending for the game's binary search.
///
/// Safe by construction:
///   * no-op (returns `true`) while `GOODS_CAPTION_CATEGORY` is unpinned (< 0) — cannot corrupt;
///   * no-op while `descriptions` is empty (scout cache not filled yet) — retry next tick;
///   * the rebuilt block is validated in OUR memory (vanilla sample round-trips + every injected id
///     resolves) BEFORE the swap; any mismatch aborts the swap and leaves the game untouched.
#[allow(dead_code)] // wired from tick() once the scout cache (game/owner/slot/flags per synth id) lands
pub fn run_descriptions(descriptions: &[(u32, Vec<u16>)]) -> bool {
    if GOODS_CAPTION_CATEGORY < 0 {
        log::warn!(
            "FMG-inject(caption): GOODS_CAPTION_CATEGORY unpinned; skipping (pin the sub[] index first)"
        );
        return true;
    }
    if descriptions.is_empty() {
        return true; // nothing to inject yet (scout cache not filled) — cheap retry next tick
    }
    let category = GOODS_CAPTION_CATEGORY as u32;
    let base = match current_module_base() {
        Some(b) => b,
        None => return true,
    };
    let md = match unsafe { category_msgdata(base, category) } {
        Some(m) => m,
        None => return false, // repo/category not up yet — retry next tick
    };
    let (groups, offsets) = match unsafe { parse(md) } {
        Some(p) => p,
        None => {
            log::warn!("FMG-inject(caption): parse failed");
            return true;
        }
    };

    // build_block requires injects sorted by id, all ids > every vanilla id (synthetic ids are > 3.78M
    // > vanilla caption ids, so appending keeps the group list ascending / binary-search-safe).
    let mut injects: Vec<(u32, Vec<u16>)> = descriptions.to_vec();
    injects.sort_by_key(|(id, _)| *id);

    let block = match unsafe { build_block(md, &groups, &offsets, &injects, &[]) } {
        Some(b) => b,
        None => {
            log::warn!("FMG-inject(caption): build_block failed (alloc?)");
            return true;
        }
    };
    let (g2, o2) = match unsafe { parse(block) } {
        Some(p) => p,
        None => {
            log::warn!("FMG-inject(caption): rebuilt block failed re-parse; NOT swapping");
            return true;
        }
    };

    // Validate in our own memory before the swap: a sample of vanilla caption ids must round-trip
    // unchanged, and every injected id must resolve to exactly what we wrote.
    let mut mismatch = 0;
    for g in groups.iter().take(8) {
        let id = g.first_id;
        if my_lookup(md, &groups, &offsets, id) != my_lookup(block, &g2, &o2, id) {
            mismatch += 1;
        }
    }
    for (id, s) in injects.iter() {
        let want = String::from_utf16_lossy(s);
        if my_lookup(block, &g2, &o2, *id).as_deref() != Some(want.as_str()) {
            mismatch += 1;
        }
    }
    if mismatch != 0 {
        log::warn!(
            "FMG-inject(caption): rebuilt block mismatch on {mismatch} id(s); NOT swapping (safe)"
        );
        return true;
    }

    log::info!(
        "FMG-inject(caption): validated (+{} descriptions); swapping caption sub[{category}] -> {block:#x}",
        injects.len()
    );
    unsafe { swap_category(base, category, block) };
    true
}

/// Resolve each synthetic goods id to (real name, optional caption) via the scout cache, joined by the
/// (FMG entry id, UTF-16 text) pairs -- what extend_swap_overrides takes.
type FmgInjects = Vec<(u32, Vec<u16>)>;

/// row's vagrant-encoded AP location id (`er_codec::recombine_location_id`). Cache miss => "AP#<id>"
/// name and no caption. Returns (GoodsName injects, GoodsCaption injects).
fn resolve_synth_injects(ids: &[u32]) -> (FmgInjects, FmgInjects) {
    let mut names = Vec::with_capacity(ids.len());
    let mut caps = Vec::new();
    for &id in ids {
        let mut name = format!("AP#{id}");
        if let Some(f) = crate::params::goods_row_fields(id as i32) {
            let loc = er_codec::recombine_location_id(
                f.vagrant_item_lot_id,
                f.vagrant_bonus_ene_drop_item_lot_id,
            );
            if let Some(s) = crate::scout_proof::lookup(loc) {
                name = s.name.clone();
                let cap = er_logic::name_override::description(&s.game, &s.owner, s.slot, s.kind);
                caps.push((id, cap.encode_utf16().collect::<Vec<u16>>()));
            }
        }
        names.push((id, name.encode_utf16().collect::<Vec<u16>>()));
    }
    (names, caps)
}

/// Read the LIVE FMG string for a goods `(category, id)` via `SearchStringTable` — used by shop_preview
/// to borrow an own-world reward's real GoodsName(10) / GoodsInfo(20) / GoodsCaption(24). Read-only.
/// `None` if the table/signature isn't up yet or the id has no entry.
#[allow(dead_code)] // kept as a ready-to-use live-FMG reader for shop_preview / future callers
pub fn read_goods_string(category: u32, id: u32) -> Option<String> {
    let base = current_module_base()?;
    let search_addr = base + SEARCH_RVA;
    if !sig_ok(search_addr) {
        return None;
    }
    let search: SearchFn = unsafe { std::mem::transmute::<usize, SearchFn>(search_addr) };
    let repo = unsafe { read_usize(base + REPO_RVA) };
    if !plausible(repo) {
        return None;
    }
    let ptr = unsafe { search(repo as *mut c_void, 0, category, id) } as usize;
    read_string(ptr)
}

/// One rebuild of a category block: what goes in, and whether the result must be proved against the
/// GAME's own lookup before we keep it. Bundled into a struct so `build_validate_swap` stays under
/// clippy's argument limit and so no caller can silently transpose `injects` and `overrides`.
struct Rebuild<'a> {
    /// The LIVE block we parsed from, and the block we revert to if the read-back fails.
    md: usize,
    groups: &'a [Group],
    offsets: &'a [u64],
    /// Ids in NO vanilla group: a new record + a new offset-table slot is CREATED for each.
    injects: &'a [(u32, Vec<u16>)],
    /// Ids already in a group: their existing string slot is redirected.
    overrides: &'a [(u32, Vec<u16>)],
    /// Read every written id back through `SearchStringTable` AFTER the swap and revert on any
    /// mismatch. Set only when `injects` is non-empty -- see the call site.
    verify_live: bool,
}

/// Build one category block, validate it in OUR memory, swap it in, and (optionally) prove the GAME
/// reads it correctly. Returns how many entries landed, or `None` with the game left untouched
/// (either never swapped, or swapped and reverted).
unsafe fn build_validate_swap(base: usize, category: u32, r: &Rebuild<'_>) -> Option<usize> {
    let block = match build_block(r.md, r.groups, r.offsets, r.injects, r.overrides) {
        Some(b) => b,
        None => {
            log::warn!("FMG extend-swap(cat {category}): build_block failed (alloc/ordering?)");
            return None;
        }
    };
    let (g2, o2) = match parse(block) {
        Some(p) => p,
        None => {
            log::warn!(
                "FMG extend-swap(cat {category}): rebuilt block failed re-parse; NOT swapping"
            );
            return None;
        }
    };

    // A SAMPLE of ids we did NOT write, which must still resolve to exactly their vanilla string.
    // The first 16 groups are the historical sample; on their own they are useless for an INSERT,
    // because a spare goods row sits ~9000 ids in and a mis-sorted record corrupts the lookups of
    // its NEIGHBOURS, not of the lowest ids in the file. So the records either side of every
    // inserted id are sampled too -- the guard has to be able to fail when the bug is present.
    let mut sample: Vec<u32> = r.groups.iter().take(16).map(|g| g.first_id).collect();
    for (id, _) in r.injects.iter() {
        if let Some(g) = r.groups.iter().rev().find(|g| g.last_id < *id) {
            sample.push(g.first_id);
            sample.push(g.last_id);
        }
        if let Some(g) = r.groups.iter().find(|g| g.first_id > *id) {
            sample.push(g.first_id);
            sample.push(g.last_id);
        }
    }
    sample.sort_unstable();
    sample.dedup();

    let written: std::collections::HashSet<u32> = r
        .overrides
        .iter()
        .chain(r.injects.iter())
        .map(|(id, _)| *id)
        .collect();
    let mut mismatch = 0;
    for id in sample.iter().copied() {
        if written.contains(&id) {
            continue; // written on purpose; checked below
        }
        if my_lookup(r.md, r.groups, r.offsets, id) != my_lookup(block, &g2, &o2, id) {
            mismatch += 1;
        }
    }
    for (id, txt) in r.overrides.iter().chain(r.injects.iter()) {
        let want = String::from_utf16_lossy(txt);
        if my_lookup(block, &g2, &o2, *id).as_deref() != Some(want.as_str()) {
            mismatch += 1;
        }
    }
    if mismatch != 0 {
        log::warn!(
            "FMG extend-swap(cat {category}): rebuilt block mismatch on {mismatch} id(s); NOT swapping (safe)"
        );
        return None;
    }

    if !swap_category(base, category, block) {
        // The repo pointer stopped being plausible between the parse and here. Nothing was written,
        // so say so rather than returning a count for a swap that did not happen.
        log::warn!(
            "FMG extend-swap(cat {category}): swap REFUSED (repo pointer not plausible); the block \
             was built and validated but nothing was published"
        );
        return None;
    }
    if !r.verify_live {
        return Some(r.overrides.len() + r.injects.len());
    }

    // POST-SWAP READ-BACK THROUGH THE GAME'S OWN LOOKUP. Everything above is our parser agreeing
    // with our builder -- it cannot detect a group array the GAME walks differently, which is the
    // one thing a mid-array insert newly risks (it binary-searches; we scan). `SearchStringTable`
    // takes the repo, not a block, so this is only possible AFTER the swap; the window is a few
    // microseconds on the game thread and a failure reverts to the block we parsed from.
    let repo_addr = read_usize(base + REPO_RVA);
    if !sig_ok(base + SEARCH_RVA) || !plausible(repo_addr) {
        // Verification was PROMISED (verify_live) and cannot be delivered, so the swap does not get
        // to stand on "probably fine". Revert; the caller falls back to redirect-only.
        log::error!(
            "FMG extend-swap(cat {category}): swapped a block with {} inserted record(s) but the \
             read-back is unavailable (SearchStringTable sig / repo pointer); REVERTING to {:#x}",
            r.injects.len(),
            r.md
        );
        swap_category(base, category, r.md);
        return None;
    }
    let search: SearchFn = std::mem::transmute::<usize, SearchFn>(base + SEARCH_RVA);
    let repo = repo_addr as *mut c_void;
    let mut bad: Vec<u32> = Vec::new();
    for (id, txt) in r.injects.iter().chain(r.overrides.iter()) {
        let want = String::from_utf16_lossy(txt);
        if read_string(search(repo, 0, category, *id) as usize).as_deref() != Some(want.as_str()) {
            bad.push(*id);
        }
    }
    for id in sample.iter().copied() {
        if written.contains(&id) {
            continue;
        }
        let vanilla = my_lookup(r.md, r.groups, r.offsets, id);
        if vanilla.is_some() && read_string(search(repo, 0, category, id) as usize) != vanilla {
            bad.push(id);
        }
    }
    if !bad.is_empty() {
        let head: Vec<u32> = bad.iter().copied().take(8).collect();
        log::error!(
            "FMG extend-swap(cat {category}): the GAME reads {} of the ids in the swapped block \
             wrongly (ids: {head:?}); REVERTING to the previous block {:#x}. The block was built \
             with {} NEW record(s) sorted into the vanilla array -- that is the shape being \
             blamed. Nothing is lost that was working before this call.",
            bad.len(),
            r.md,
            r.injects.len()
        );
        swap_category(base, category, r.md);
        return None;
    }
    Some(r.overrides.len() + r.injects.len())
}

/// Insertion (CREATING an FMG entry for an id no vanilla group covers) is the one shape here whose
/// correctness depends on the game's binary search over a record array we reordered. It is proved
/// per swap by a read-back; the first failure disables it for the session so we do not build, swap
/// and revert a block on every world edge for the rest of the run.
static INSERT_UNSAFE: AtomicBool = AtomicBool::new(false);

/// Extend-swap OVERRIDES: give a set of ids in `base_array[0][category]` new AP strings, rebuilding
/// from the LIVE block so any prior swap (e.g. this module's synthetic-goods appends) is preserved.
/// Used by shop_preview / check_lots for names, info lines and captions that don't fit the packed
/// vanilla entry in place. Returns how many entries landed. No-op on empty / category-not-up.
///
/// TWO shapes, and the second one is new (2026-08-03, issue #300):
///
///   * REDIRECT -- the id already lives in a vanilla group, so it has a string slot; point that slot
///     at our longer string. An EMPTY vanilla entry is fine (that is the `[ERROR]` render).
///   * INSERT   -- the id is in NO group, so it has no slot and the menu renders the `?GoodsInfo?`
///     tag. Create a record for it, sorted into place. This USED to be impossible ("build_block's
///     inject path cannot take it either, injected ids must sort above every vanilla id" -- the
///     apworld's own datamine script says so), and the cost was that the three goods categories
///     have DIFFERENT id sets: boblerrr's `0.3.1` log named 53 shop slots and described 25 of them,
///     because GoodsInfo(20)/GoodsCaption(24) carry no entry for 28 of those spare goods rows.
///     Refusing to name them could never have fixed those 28 -- the tag renders whether or not we
///     write anything -- so the writer had to learn to CREATE an entry.
///
/// Validated in OUR memory before the swap (a sample of untouched ids round-trips + every written id
/// resolves), and when anything was INSERTED, re-read through the game's own `SearchStringTable`
/// after it, reverting on any disagreement.
pub fn extend_swap_overrides(category: u32, overrides: &[(u32, Vec<u16>)]) -> usize {
    if overrides.is_empty() {
        return 0;
    }
    let base = match current_module_base() {
        Some(b) => b,
        None => return 0,
    };
    let md = match unsafe { category_msgdata(base, category) } {
        Some(m) => m,
        None => return 0, // repo/category not up yet — caller retries next tick
    };
    let (groups, offsets) = match unsafe { parse(md) } {
        Some(p) => p,
        None => {
            log::warn!("FMG extend-swap(cat {category}): parse failed");
            return 0;
        }
    };
    // REDIRECT vs INSERT, decided by the category's existing coverage (er-logic, host-tested against
    // the 25/28 split from the log).
    let spans: Vec<er_logic::fmg_groups::Span> = groups
        .iter()
        .map(|g| er_logic::fmg_groups::Span::new(g.first_id, g.last_id))
        .collect();
    let ids: Vec<u32> = overrides.iter().map(|(id, _)| *id).collect();
    let split = er_logic::fmg_groups::split_by_coverage(&spans, &ids);
    let text = |want: u32| -> Vec<u16> {
        overrides
            .iter()
            .find(|(id, _)| *id == want)
            .map(|(_, t)| t.clone())
            .unwrap_or_default()
    };
    let resolvable: Vec<(u32, Vec<u16>)> =
        split.redirect.iter().map(|&id| (id, text(id))).collect();
    let mut injects: Vec<(u32, Vec<u16>)> = split.insert.iter().map(|&id| (id, text(id))).collect();

    // FAIL CLOSED on the new shape. An insert is only kept if the game could be CAUGHT reading it
    // wrongly, and that read-back needs `SearchStringTable`; without the signature (a game patch
    // moved it) we cannot check, so we do not insert. Same once an insert has already been caught.
    if !injects.is_empty() {
        let latched = INSERT_UNSAFE.load(Ordering::Relaxed);
        if latched || !sig_ok(base + SEARCH_RVA) {
            let sample: Vec<u32> = injects.iter().map(|(id, _)| *id).take(8).collect();
            log::warn!(
                "FMG extend-swap(cat {category}): {} of {} id(s) are in NO vanilla group and entry \
                 INSERTION is unavailable ({}), so they have no string slot and will render the \
                 `?GoodsName?` / `?GoodsInfo?` tag in game. ids: {sample:?}",
                injects.len(),
                overrides.len(),
                if latched {
                    "disabled after a failed read-back earlier this session"
                } else {
                    "SearchStringTable signature mismatch, so a swap cannot be verified"
                }
            );
            injects.clear();
        }
    }
    if resolvable.is_empty() && injects.is_empty() {
        return 0;
    }

    if !injects.is_empty() {
        let inserted = injects.len();
        let redirected = resolvable.len();
        let r = Rebuild {
            md,
            groups: &groups,
            offsets: &offsets,
            injects: &injects,
            overrides: &resolvable,
            verify_live: true,
        };
        if let Some(n) = unsafe { build_validate_swap(base, category, &r) } {
            log::info!(
                "FMG extend-swap(cat {category}): swapped (+{redirected} redirected, \
                 +{inserted} NEW entries created for ids in no vanilla group; \
                 read-back through the game's own lookup OK)"
            );
            return n;
        }
        INSERT_UNSAFE.store(true, Ordering::Relaxed);
        log::warn!(
            "FMG extend-swap(cat {category}): entry INSERTION did not verify; DISABLED for the rest \
             of the session and retrying redirect-only. {inserted} of {} id(s) keep rendering the \
             `?GoodsInfo?`-style tag; the other {redirected} are unaffected.",
            overrides.len()
        );
        if resolvable.is_empty() {
            return 0;
        }
    }

    let r = Rebuild {
        md,
        groups: &groups,
        offsets: &offsets,
        injects: &[],
        overrides: &resolvable,
        verify_live: false,
    };
    match unsafe { build_validate_swap(base, category, &r) } {
        Some(n) => {
            log::info!("FMG extend-swap(cat {category}): swapped (+{n} overrides)");
            n
        }
        None => 0,
    }
}

pub fn run() -> bool {
    if DONE.load(Ordering::Relaxed) {
        return true;
    }
    let base = match current_module_base() {
        Some(b) => b,
        None => return true,
    };
    let md = match unsafe { goods_msgdata(base) } {
        Some(m) => m,
        None => return false,
    };
    let search_addr = base + SEARCH_RVA;
    if !sig_ok(search_addr) {
        // R10 (SWEEP): latch DONE -- without it this warned EVERY tick for the whole session
        // (a game patch shifts the RVA; one warn then a permanent no-op is the right degrade).
        log::warn!(
            "FMG-inject: SearchStringTable sig mismatch; abort (latched -- synthetic naming OFF this session)"
        );
        DONE.store(true, Ordering::Relaxed);
        return true;
    }
    let search: SearchFn = unsafe { std::mem::transmute::<usize, SearchFn>(search_addr) };
    let repo = unsafe { read_usize(base + REPO_RVA) } as *mut c_void;

    let (groups, offsets) = match unsafe { parse(md) } {
        Some(p) => p,
        None => {
            log::warn!("FMG-inject: parse failed");
            return true;
        }
    };
    log::info!(
        "FMG-inject: GoodsName md={md:#x} groups={} offsetTableLen={} (MODE={MODE})",
        groups.len(),
        offsets.len()
    );

    if MODE == MODE_PARSE {
        let mut ok = 0;
        for &id in CHECK_IDS {
            let mine = my_lookup(md, &groups, &offsets, id);
            let theirs = read_string(unsafe { search(repo, 0, GOODS_CATEGORY, id) as usize });
            let m = if mine == theirs {
                ok += 1;
                "MATCH"
            } else {
                "MISMATCH"
            };
            log::info!("FMG-inject:   id={id} mine={mine:?} game={theirs:?} [{m}]");
        }
        log::info!("FMG-inject: === parse {ok}/{} match ===", CHECK_IDS.len());
        DONE.store(true, Ordering::Relaxed);
        return true;
    }

    // MODE_IDENTITY / MODE_INJECT: build a fresh block, validate it in OUR memory, then swap.
    // For MODE_INJECT, names + captions are resolved from the scout cache (joined to each synthetic row
    // by its vagrant-encoded AP location id); a cache miss falls back to "AP#<id>" / no caption.
    let mut descs: Vec<(u32, Vec<u16>)> = Vec::new();
    let injects: Vec<(u32, Vec<u16>)> = if MODE == MODE_INJECT {
        let ids = crate::params::synthetic_goods_ids();
        // Wait for the scout reply before naming so we get real names, not AP#<id>. No synthetics =>
        // nothing to wait for; proceed and inject nothing (solo/base-game).
        if !ids.is_empty() && !crate::scout_proof::cache_ready() {
            return false; // retry next tick once LocationScouts has populated the cache
        }
        let (names, caps) = resolve_synth_injects(&ids);
        log::info!(
            "FMG-inject: {} synthetic goods ids; {} resolved to real name+caption from scout cache (rest -> AP#<id>)",
            ids.len(),
            caps.len()
        );
        descs = caps;
        names
    } else {
        Vec::new()
    };

    let block = match unsafe { build_block(md, &groups, &offsets, &injects, &[]) } {
        Some(b) => b,
        None => {
            log::warn!("FMG-inject: build_block failed (alloc?)");
            DONE.store(true, Ordering::Relaxed);
            return true;
        }
    };
    let (g2, o2) = match unsafe { parse(block) } {
        Some(p) => p,
        None => {
            log::warn!("FMG-inject: rebuilt block failed re-parse; NOT swapping");
            DONE.store(true, Ordering::Relaxed);
            return true;
        }
    };
    let mut mismatch = 0;
    for &id in CHECK_IDS {
        if my_lookup(md, &groups, &offsets, id) != my_lookup(block, &g2, &o2, id) {
            mismatch += 1;
        }
    }
    for (id, name) in injects.iter().take(4) {
        let want = String::from_utf16_lossy(name);
        let got = my_lookup(block, &g2, &o2, *id);
        let good = got.as_deref() == Some(want.as_str());
        log::info!(
            "FMG-inject:   synth-check id={id} want={want:?} got={got:?} [{}]",
            if good { "OK" } else { "BAD" }
        );
        if !good {
            mismatch += 1;
        }
    }
    if mismatch != 0 {
        log::warn!("FMG-inject: rebuilt block mismatch on {mismatch} id(s); NOT swapping (safe)");
        DONE.store(true, Ordering::Relaxed);
        return true;
    }
    log::info!(
        "FMG-inject: block validated (+{} injected); swapping GoodsName -> {block:#x}",
        injects.len()
    );
    if unsafe { swap_goods(base, block) } {
        for &id in CHECK_IDS {
            let theirs = read_string(unsafe { search(repo, 0, GOODS_CATEGORY, id) as usize });
            log::info!("FMG-inject:   post-swap vanilla id={id} game={theirs:?}");
        }
        for (id, _) in injects.iter().take(4) {
            let theirs = read_string(unsafe { search(repo, 0, GOODS_CATEGORY, *id) as usize });
            log::info!("FMG-inject:   post-swap SYNTH id={id} game={theirs:?}");
        }
        log::info!("FMG-inject: === GoodsName swap done (MODE={MODE}); synthetic goods named ===");
    }
    // GoodsCaption (cat 24): write each resolved synthetic good's description (game / owner / class).
    // Independent allocate-extend-swap; no-op if nothing resolved (e.g. solo seed) or category unpinned.
    if MODE == MODE_INJECT && !descs.is_empty() {
        run_descriptions(&descs);
    }
    DONE.store(true, Ordering::Relaxed);
    true
}
