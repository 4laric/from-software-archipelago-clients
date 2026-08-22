//! `hover_probe` -- a LOG-ONLY probe for the shop-hover selection signal (er-archipelago#937).
//!
//! ## The premise it exists to falsify
//!
//! Shop slots that share a spare goods row show the shared "Archipelago Items" label
//! (`name_override::shop_shared_label`), because a row has ONE name and the write is
//! once-per-session. The fix on the table is a per-hover rename of the detail panel
//! (GoodsInfo/GoodsCaption), and its ONE unknown is the selection signal: nothing in the client
//! sees which slot the cursor is on. The pinned crate maps `CSMenuManImp` with no shop-cursor
//! field (the buy menu is Scaleform/FE, unmapped), so the candidate that needs NO new RE is the
//! fallback: **the game's own FMG lookup stream as the hover event.** If the buy menu re-queries
//! `SearchStringTable` for GoodsName(10)/GoodsInfo(20)/GoodsCaption(24) when the cursor moves --
//! per frame, or once per move -- then hooking that lookup (an address the client already finds
//! and sig-verifies for the extend-swap read-back) yields the highlighted row for free.
//!
//! This probe buys that observation, the same shape as the 2026-08-08 ESD probe that found
//! command 22: ship a log-only hook behind a gate, run one shop session, read the log.
//!
//! ## Gate
//!
//! **OFF by default.** `ER_HOVER_PROBE=1` or `"probes": {"hover": true}` enables it. When off,
//! `install()` is a no-op and the hook is never built -- a hot-path hook does not ride a release,
//! and `SearchStringTable` backs every text lookup in the game.
//!
//! ## What the log shows
//!
//! While a shop is open (ESD command 22 dispatch, `esd_probe.rs`), goods-category lookups
//! accumulate per frame; the tick flush logs the frame's `(category, id)` sequence ONLY when it
//! differs from the previous frame's. Three readings fall out:
//!
//!   * Cursor still, sequences REPEAT (per-frame re-query): the lookup stream tracks the cursor
//!     continuously -- the hover signal exists, and info/caption entries in the sequence name the
//!     highlighted row.
//!   * Cursor still, SILENCE; cursor moves, a new sequence: edge-triggered re-query -- the signal
//!     still exists, as an on-move event.
//!   * Cursor moves, NOTHING: the menu caches its strings and the fallback is dead -- keystone 1
//!     goes back to FE-layer RE, and we know before building anything on it.
//!
//! `CSMenuMan.player_menu_ctrl.selected_goods_item` is sampled on the same flush as a long-shot
//! direct signal (it is the INVENTORY menu's field; if it tracks the shop cursor too, keystone 1
//! is solved outright).
//!
//! ## What it deliberately does not do
//!
//! It does not write ANYTHING. No FMG mutation, no row rewrite, no detour beyond the one lookup
//! hook. The per-hover in-place rewrite (keystone 2, padded fixed-capacity entries) is designed
//! against whatever this log shows, not before it.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use fromsoftware_shared::FromStatic;
use retour::GenericDetour;

/// Same signature as `fmg_inject`'s read-back call: (repo, language?, category, id) -> UTF-16.
type SearchFn = unsafe extern "C" fn(*mut c_void, u32, u32, u32) -> *const u16;

static HOOK: OnceLock<GenericDetour<SearchFn>> = OnceLock::new();
static ATTEMPTED: AtomicBool = AtomicBool::new(false);
/// Set only after `enable()` succeeds -- the gate every other entry point checks, so a
/// built-but-disabled detour (enable failed) does not read as an active probe.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Set by the ESD command-22 dispatch (`esd_probe.rs`), never cleared: the client has no
/// shop-close seam, so after the first shop open of a session every goods lookup anywhere
/// qualifies. For a bounded probe protocol (open the shop, wiggle the cursor, close, STOP) that
/// is fine -- the post-close game is quiet, and the protocol note on the issue says so.
static SHOP_OPEN: AtomicBool = AtomicBool::new(false);

/// Goods lookups since the last flush, in call order. Bounded; overflow is counted, not kept.
static FRAME: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
static LAST: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
static DROPPED: AtomicU64 = AtomicU64::new(0);
static FRAME_NO: AtomicU64 = AtomicU64::new(0);
const FRAME_CAP: usize = 8192;

const GOODS_NAME: u32 = 10;
const GOODS_INFO: u32 = 20;
const GOODS_CAPTION: u32 = 24;

/// Install the lookup hook, once, when the probe was asked for. Every failure is one log line and
/// a quiet session, never a latch the game can trip over.
pub fn install() {
    if !shared::probes::enabled("ER_HOVER_PROBE", "hover") {
        return;
    }
    if ATTEMPTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let Some(target) = crate::fmg_inject::search_string_table_addr() else {
        log::warn!(
            "hover-probe: INACTIVE -- SearchStringTable did not verify ({}), so the lookup \
             stream cannot be watched this session",
            crate::game_version_gate::measured_clause()
        );
        return;
    };
    let search: SearchFn = unsafe { std::mem::transmute::<usize, SearchFn>(target) };
    // SAFETY: `target` is the sig-verified entry of the game's own lookup; `search_detour`
    // matches `SearchFn`'s calling convention exactly.
    let hook = match unsafe { GenericDetour::<SearchFn>::new(search, search_detour) } {
        Ok(h) => h,
        Err(e) => {
            log::warn!("hover-probe: INACTIVE -- retour could not build the lookup detour: {e}");
            return;
        }
    };
    // Publish BEFORE enabling: once the patch is live, lookups route into `search_detour`
    // immediately, and its trampoline is `HOOK.get().call(...)` -- an enabled-but-unpublished
    // window would answer the game's text lookups with null. (The ESD hook can afford the other
    // order; a missed dialogue line is not a renderer reading a null string.)
    let _ = HOOK.set(hook);
    // SAFETY: patching a verified, executable entry inside the loaded image.
    if let Err(e) = unsafe { HOOK.get().expect("published above").enable() } {
        log::warn!("hover-probe: INACTIVE -- enabling the lookup detour failed: {e}");
        return;
    }
    ACTIVE.store(true, Ordering::Relaxed);
    log::info!(
        "hover-probe: lookup hook ACTIVE -- goods-category lookups are logged while a shop is \
         open. Open a merchant, move the cursor SLOWLY to the bottom of the list and back, close \
         the menu, stop playing, and post the log (issue #937)."
    );
}

/// The ESD command-22 dispatch tells us a buy menu opened, and over which ShopLineupParam rows
/// (the rows map slots to goods ids at analysis time).
pub fn on_shop_open(begin: i32, end: i32) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    SHOP_OPEN.store(true, Ordering::Relaxed);
    let _ = LAST.lock().map(|mut l| l.clear());
    log::info!("hover-probe: SHOP OPEN over rows {begin}..={end} -- cursor trace starts");
}

/// The trampoline ALWAYS runs; the observation is caught so a poisoned mutex or a formatting
/// panic can neither take the game down nor skip the game's own lookup.
unsafe extern "C" fn search_detour(
    repo: *mut c_void,
    lang: u32,
    category: u32,
    id: u32,
) -> *const u16 {
    let r = match HOOK.get() {
        // SAFETY: the trampoline is the game's own lookup, called with the caller's arguments
        // unchanged; retour preserves the original bytes for it.
        Some(h) => unsafe { h.call(repo, lang, category, id) },
        None => std::ptr::null(),
    };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if !SHOP_OPEN.load(Ordering::Relaxed) {
            return;
        }
        if !matches!(category, GOODS_NAME | GOODS_INFO | GOODS_CAPTION) {
            return;
        }
        if let Ok(mut b) = FRAME.lock() {
            if b.len() < FRAME_CAP {
                b.push((category, id));
            } else {
                DROPPED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }));
    r
}

/// Frame flush, called from the client tick. Logs the frame's lookup sequence only when it
/// differs from the previous frame's, so a still cursor is silence (or a repeated pattern, if the
/// menu re-queries every frame) and a cursor move is a line.
pub fn tick() {
    if !ACTIVE.load(Ordering::Relaxed) || !SHOP_OPEN.load(Ordering::Relaxed) {
        return;
    }
    let frame = FRAME_NO.fetch_add(1, Ordering::Relaxed) + 1;
    let cur = match FRAME.lock().map(|mut b| std::mem::take(&mut *b)) {
        Ok(c) => c,
        Err(_) => return,
    };
    let dropped = DROPPED.swap(0, Ordering::Relaxed);
    let changed = match LAST.lock() {
        Ok(mut last) => {
            let c = cur != *last;
            *last = cur.clone();
            c
        }
        Err(_) => false,
    };
    if changed {
        if cur.is_empty() {
            log::info!("hover-probe: frame {frame}: <no goods lookups>");
        } else {
            log::info!("hover-probe: frame {frame}: {}", fmt_seq(&cur));
        }
    }
    if dropped > 0 {
        log::warn!(
            "hover-probe: frame {frame}: dropped {dropped} lookup(s) past the {FRAME_CAP}-entry \
             frame cap -- the sequence above is truncated"
        );
    }
    sample_selected_goods(frame);
}

/// `10:[row,row,...] 20:[...] 24:[...]`, preserving call order within each category.
fn fmt_seq(seq: &[(u32, u32)]) -> String {
    let mut out = String::new();
    for cat in [GOODS_NAME, GOODS_INFO, GOODS_CAPTION] {
        let ids: Vec<String> = seq
            .iter()
            .filter(|(c, _)| *c == cat)
            .map(|(_, id)| id.to_string())
            .collect();
        if !ids.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&format!("{cat}:[{}]", ids.join(",")));
        }
    }
    out
}

/// The long-shot direct signal: the INVENTORY menu's selected-goods field, sampled once per
/// frame while the shop is open. Logged on change only. If it moves in lockstep with the shop
/// cursor, keystone 1 is solved with no inference at all; if it sits at NONE or a stale value,
/// that is the expected (and still useful) negative.
fn sample_selected_goods(frame: u64) {
    static LAST_SEL: Mutex<Option<Option<u32>>> = Mutex::new(None);
    let sel = std::panic::catch_unwind(|| {
        let mm = unsafe { eldenring::cs::CSMenuManImp::instance() }.ok()?;
        Some(mm.player_menu_ctrl.selected_goods_item.param_id())
    })
    .ok()
    .flatten();
    let Some(sel) = sel else {
        return; // singleton not up / unsupported build -- say nothing rather than guess
    };
    if let Ok(mut last) = LAST_SEL.lock()
        && *last != Some(sel)
    {
        *last = Some(sel);
        log::info!("hover-probe: frame {frame}: selected_goods_item = {sel:?}");
    }
}

/// Session teardown (disconnect/reset): close the trace so a later session starts clean.
pub fn reset() {
    SHOP_OPEN.store(false, Ordering::Relaxed);
    if let Ok(mut b) = FRAME.lock() {
        b.clear();
    }
    if let Ok(mut l) = LAST.lock() {
        l.clear();
    }
}
