//! shop_preview.rs — name/description override for FOREIGN (and gem/custom) shop slots only.
//!
//! Own-world rewards are rewritten by `shop_sell` to natively sell the real item (correct name, lore
//! and icon), so this only handles the slots shop_sell can't: FOREIGN items (no ER counterpart) and
//! gem/custom rewards. For those the vanilla good's FMG name + info + caption are overwritten with
//! the AP routing block ("AP: <item> / For: <owner> (<game>) / <kind>").
//!
//! Mechanism: EXTEND-SWAP via fmg_inject::extend_swap_overrides (rebuilds the category block from the
//! LIVE pointer so any length fits; validated before the atomic swap). Runs AFTER fmg_inject. The
//! override is GLOBAL per good id, so we dedup by good id (the shared FMG entry shows one reward).
//!
//! THREE CATEGORIES, THREE ID SETS -- not one id set read three times. A goods row that has a
//! GoodsName(10) entry very often has no GoodsInfo(20) / GoodsCaption(24) entry, and the world's
//! spare-preview pool only has 25 rows with all three (`greenfield/spare_goods.tsv`, `fmg_full`
//! column). A seed needing more distinct previews than that spends them and falls through to
//! name-only rows, whose description panel renders `?GoodsInfo?` -- whether or not this module
//! writes anything, because the entry does not exist to be redirected. Since 2026-08-03
//! `extend_swap_overrides` CREATES the missing entry rather than dropping the id; the completeness
//! check at the bottom of `run()` is what makes a regression to the old behaviour audible.
//!
//! TWO bugs fixed here (2026-07-12):
//!
//!  1. WRONG KEY -- it keyed the FMG override by the ER FullID (`good as u32`, category nibble and
//!     all: 0x40000000|row ~= 1.07e9) instead of the EquipParamGoods ROW id the FMG is actually keyed
//!     by. So every override landed at an id no menu ever reads: the AP name/caption has NEVER been
//!     displayed, and extend_swap merely grew the block with dead entries. `shop_icon` strips the
//!     nibble (er_codec::row_id_of) and says so in a comment; this module was never given the same fix.
//!
//!  2. GLOBAL IDENTITY THEFT -- and fixing (1) alone would have made things WORSE, by finally
//!     activating a global write. The FMG entry is shared: renaming the good behind a shop slot renames
//!     EVERY copy of that good the player will ever hold. 11 vanilla shop rows sell smithing stones, so
//!     one foreign/custom reward landing on one of them would rename the player's whole stone economy
//!     to "AP: <something>". `shop_icon` had exactly this bug on the ICON side and it is what Alaric saw
//!     in the 2026-07-12 playtest (telescope icons on every smithing stone, in the world AND in the
//!     inventory). So the same REAL_GOODS guard applies here: never repaint a good the seed can grant.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use eldenring::cs::{ShopLineupParam, SoloParamRepository};
use fromsoftware_shared::FromStatic;

const GOODS_NAME_CAT: u32 = 10;
const GOODS_INFO_CAT: u32 = 20; // the "Item Effect" line the buy menu renders
const GOODS_CAPTION_CAT: u32 = 24;

/// ER GOODS row ids the seed can actually GRANT (from apIdsToItemIds). Never repaint one of these:
/// the FMG entry is shared, so renaming the good behind a shop slot renames every copy the player holds.
static REAL_GOODS: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

static CONFIGURED: Mutex<Vec<(i64, i32)>> = Mutex::new(Vec::new());
static CONFIGURED_SET: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);

/// Ticks spent waiting for the MSG repo before we stop retrying the name override. A BOUND, not a
/// timeout: `extend_swap_overrides` returns 0 both for "repo not up yet" (retry) and for "not one of
/// these ids is writable" (never retry), and it cannot tell the caller which. Without a cap the
/// second case spins an FMG parse every tick forever. ~10s at 60fps.
static NAME_RETRIES: AtomicU32 = AtomicU32::new(0);
const NAME_RETRY_LIMIT: u32 = 600;

/// #937 repaint state. `PER_LOC` is what the baseline fold learned per slot -- AP location ->
/// (goods row, this slot's OWN label) -- kept so a shop open can rewrite each visible row to the
/// claimant actually on that shelf (the world's coloring reuses rows across menus; the baseline
/// fold can only write ONE name per row seed-wide, so shared rows carry the honest shared label
/// until the first open repaints them). `MY_BLOCKS` holds the block address each of the three
/// category extend-swaps published: `fmg_inject::rewrite_in_place` refuses any other block, which
/// is the whole safety story -- a load or a sibling's re-dress replaces the block, the repaint
/// sees `StaleBlock`, clears `DONE`, and the baseline (padded) pass re-arms first.
static PER_LOC: Mutex<Vec<(i64, u32, er_logic::name_override::ShopLabel)>> = Mutex::new(Vec::new());
/// (name, info, caption) published-block addresses, in category-call order.
static MY_BLOCKS: Mutex<[Option<usize>; 3]> = Mutex::new([None; 3]);
/// The last `OpenRegularShop` range not yet repainted. One deep on purpose: menus do not stack.
static PENDING_RANGE: Mutex<Option<(i32, i32)>> = Mutex::new(None);
/// The range most recently repainted against the CURRENT blocks -- reopening the same shelf with
/// nothing changed is a no-op, not a rewrite storm.
static PAINTED: Mutex<Option<(i32, i32)>> = Mutex::new(None);
/// Rows the repaint walks at most, whatever the ESD claimed (mirrors shop_hints' own bound).
const REPAINT_MAX_ROWS: i32 = 2048;

pub fn set_real_goods(rows: HashSet<u32>) {
    log::info!(
        "shop-preview: {} real goods row(s) protected from the global FMG override",
        rows.len()
    );
    *REAL_GOODS.lock().unwrap() = Some(rows);
}

/// Has slot_data (or the shop_sell runtime fallback) supplied the (loc -> vanilla good) pairs yet?
/// `run()` waits on this: an apworld that emits no `shopPreviewGoods` must NOT latch DONE on an empty
/// set, or the fallback derived from the live params arrives too late to be used.
pub fn is_configured() -> bool {
    CONFIGURED_SET.load(Ordering::Relaxed)
}

pub fn configure(pairs: Vec<(i64, i32)>) {
    log::info!("shop-preview: configured {} shop slot(s)", pairs.len());
    *CONFIGURED.lock().unwrap() = pairs;
    CONFIGURED_SET.store(true, Ordering::Relaxed);
    // Fresh budget per connect: a reconnect must not inherit an exhausted one and latch instantly.
    NAME_RETRIES.store(0, Ordering::Relaxed);
    DONE.store(false, Ordering::Relaxed);
}

/// The (loc -> preview good) pairs, or `None` before either source has supplied them. `shop_repoint`
/// reads them here rather than being plumbed separately, so it is fed by BOTH the slot_data
/// `shopPreviewGoods` path and the runtime ShopLineupParam fallback shop_sell installs for a foreign
/// apworld that omits the key -- there is one place a pair can enter the client, so there is one
/// place to read it from.
pub fn configured_pairs() -> Option<Vec<(i64, i32)>> {
    if !CONFIGURED_SET.load(Ordering::Relaxed) {
        return None;
    }
    Some(CONFIGURED.lock().unwrap().clone())
}

/// Region-lock item NAMES (the `regionOpenFlags` slot_data keys, e.g. "Ensis Lock"). A shop slot whose
/// scouted reward is one of these is a REGION UNLOCK: it gets a distinct "REGION UNLOCK" label AND is
/// forced past the real-good FMG protection (a region key is worth renaming one shared note FMG, unlike
/// a stone economy). Without this a lock in a shop reads as its vanilla good ("Note: Sealed Spiritsprings").
static LOCK_NAMES: Mutex<Option<HashSet<String>>> = Mutex::new(None);

pub fn configure_locks(names: HashSet<String>) {
    log::info!(
        "shop-preview: {} region-lock name(s) armed for shop marking",
        names.len()
    );
    *LOCK_NAMES.lock().unwrap() = Some(names);
}

/// Re-arm after a map load (or a reconnect / new seed).
///
/// 🛑 THE FOURTH WRITER TO MAKE THIS MISTAKE -- after shop_sell (2026-07-24), shop_icon and
/// shop_stock (2026-07-29). Until 2026-08-03 this module had NO reset() at all, so its FMG name /
/// info / caption overrides applied once on connect and never again.
///
/// The mechanism is worth stating, because it is NOT "a load reverts our write and we do not
/// re-apply" -- it is worse. `fmg_inject::extend_swap_overrides` rebuilds the category block from
/// whatever the category CURRENTLY points at. A load reverts that pointer to vanilla; `check_lots`
/// then re-dresses its placeholder (it IS re-armed) and publishes a fresh block containing ONLY the
/// placeholder -- actively discarding these overrides. So a sibling's correct re-arm is what erases
/// us, and the busier the session the more reliably it happens.
///
/// MEASURED, Alaric's playtest 2026-08-02 (`0.3.1 (f2ef85d3c920)`, 1h40m, 6769 lines):
///   153x `swapped (+1 overrides)`  <- check_lots, 51 world edges x 3 categories
///     3x `FMG extend-swap ... swapped (+5 overrides)`   <- this module, ONE edge x 3 categories
/// Named at 22:13:46. Session ran to 23:52. Fifty more edges, zero re-applications: every
/// foreign/gem shop slot rendered `[ERROR]` for 96 of the session's 100 minutes.
///
/// Clears the LATCHES only. `CONFIGURED` / `CONFIGURED_SET` / `REAL_GOODS` / `LOCK_NAMES` are
/// seed-scoped (set from slot_data at connect) and a map load does not invalidate them -- clearing
/// them here would make `run()` wait forever on `is_configured()`.
pub fn reset() {
    DONE.store(false, Ordering::Relaxed);
    NAME_RETRIES.store(0, Ordering::Relaxed);
    // #937 repaint state: the load edge that armed this reset also reverted the category pointers,
    // so the recorded blocks are history and any pending shelf belongs to a menu that cannot
    // survive a load. (`PER_LOC` is seed-scoped and re-filled by the next run(); left alone.)
    *MY_BLOCKS.lock().unwrap() = [None; 3];
    PENDING_RANGE.lock().unwrap().take();
    PAINTED.lock().unwrap().take();
}

pub fn run() -> bool {
    if DONE.load(Ordering::Relaxed) {
        return true;
    }
    if !CONFIGURED_SET.load(Ordering::Relaxed) {
        return false; // wait for slot_data parse (net.rs)
    }
    if !crate::scout_proof::cache_ready() {
        return false; // wait for the scout reply
    }
    let pairs = CONFIGURED.lock().unwrap().clone();
    if pairs.is_empty() {
        DONE.store(true, Ordering::Relaxed);
        return true;
    }
    // Fail CLOSED: without the protected set we cannot tell a shop-only curio from a Smithing Stone,
    // and guessing wrong renames a real item globally for the whole run. Wait instead.
    let real: HashSet<u32> = match REAL_GOODS.lock().unwrap().clone() {
        Some(r) => r,
        None => return false,
    };

    // FOREIGN / gem slots only — own-world slots are sold natively by shop_sell. Per-category override
    // maps (name 10, info 20, caption 24) deduped by good id (the FMG entry is global).
    // goods row -> every label the slots pointing at it produced. See the fold below for why this
    // is not a `HashMap<u32, Label>` with an insert.
    PER_LOC.lock().unwrap().clear();
    let mut by_row: HashMap<u32, Vec<er_logic::name_override::ShopLabel>> = HashMap::new();
    let mut nmap: HashMap<u32, Vec<u16>> = HashMap::new();
    let mut imap: HashMap<u32, Vec<u16>> = HashMap::new();
    let mut cmap: HashMap<u32, Vec<u16>> = HashMap::new();
    let lock_names = LOCK_NAMES.lock().unwrap().clone().unwrap_or_default();
    let (mut overridden, mut native, mut protected, mut locks) = (0u32, 0u32, 0u32, 0u32);
    for (loc, good) in &pairs {
        let Some(s) = crate::scout_proof::lookup(*loc) else {
            continue;
        };
        // A REGION LOCK reward: mark it as a region unlock and force it PAST the native/real-good skips
        // below (a region key is worth renaming its shared note FMG). Locks are own-world with no
        // apIdsToItemIds entry, so er_sell_id is already None -- but check first so intent is explicit.
        let is_lock = lock_names.contains(&s.name);
        if s.er_sell_id.is_some() && !is_lock {
            native += 1;
            continue; // own-world: shop_sell sells it natively
        }
        // The FMG is keyed by the EquipParamGoods ROW id, not the ER FullID. Strip the category
        // nibble exactly as shop_sell / shop_icon do; a non-GOODS ware has its name in a different FMG
        // category, and reusing a weapon row id as a goods row id would rename the WRONG good.
        let full = *good as u32;
        if er_codec::item_category_of(full) != er_codec::CATEGORY_GOODS {
            continue;
        }
        let gid = er_codec::row_id_of(full);
        // THE GUARD (see the module header): the FMG entry is shared, so renaming the good behind this
        // slot renames EVERY copy the player can hold -- globally, for the whole run. If the seed can
        // grant this good, leave the slot showing its vanilla name; one slot lying about one reward
        // beats mislabeling every copy of a real good in the player's inventory.
        // Region locks are NOT exempt (2026-07-20): a lock ware that collides with a grantable real
        // good produced "9 Leyndell Locks" in a player's bag (the ware was real good row 9510). An
        // unmarked shop slot is the lesser evil; the honest fix is repointing the lock slot at a
        // dedicated placeholder good (the 8852 pattern) so it can be marked without hijacking a real
        // row -- see the shop-placeholder follow-up.
        if real.contains(&gid) {
            protected += 1;
            continue;
        }
        // Pure, host-tested formatters (er-logic name_override) so the exact GoodsName + caption a
        // lock/foreign slot shows is pinned by unit test, not inlined here.
        let lbl = if is_lock {
            // A lock ware that is a real good was protected above; only synthetic/non-grantable lock
            // wares reach here, so renaming them is safe (nothing else shares the row).
            locks += 1;
            er_logic::name_override::shop_lock_label(&s.name)
        } else {
            overridden += 1;
            er_logic::name_override::shop_label(&s.name, &s.owner, &s.game, s.kind)
        };
        // 🛑 COLLECT, DO NOT INSERT. `nmap.insert(gid, ..)` OVERWRITES, and gid is the goods ROW --
        // so when several slots share a row (the spare pool is 65 against up to 501 shop checks) the
        // last write silently spoke for all of them. Funnyfail, 2026-08-16: "all AP items from Kale
        // look like they are 'Arrows(10)'". That name was REAL and correctly scouted; it just
        // belonged to one of the three. Gather per row, decide after.
        PER_LOC.lock().unwrap().push((*loc, gid, lbl.clone()));
        by_row.entry(gid).or_default().push(lbl);
    }

    // ⭐ ONE ROW, ONE TRUTH. A row claimed by a single slot keeps that slot's real name. A row several
    // slots share says so, in the player's words, instead of picking one of them and sounding sure.
    // Labels are compared, not counted: two slots holding the SAME item name on one row is not a
    // collision and must keep the specific name.
    let mut shared_rows = 0u32;
    for (gid, mut lbls) in by_row {
        let distinct = {
            let mut names: Vec<&str> = lbls.iter().map(|l| l.name.as_str()).collect();
            names.sort_unstable();
            names.dedup();
            names.len()
        };
        let lbl = if distinct > 1 {
            shared_rows += 1;
            er_logic::name_override::shop_shared_label(distinct)
        } else {
            lbls.pop()
                .expect("entry() only exists because something was pushed")
        };
        // PADDED to fixed capacity (er_logic::shop_repaint): every override gets a private slot
        // in the rebuilt block, and the trailing NULs are the storage the #937 per-shop-open
        // repaint rewrites in place -- no per-open block rebuild, no leak.
        nmap.insert(
            gid,
            er_logic::shop_repaint::pad_units(&lbl.name, er_logic::shop_repaint::PAD_NAME),
        );
        let u: Vec<u16> =
            er_logic::shop_repaint::pad_units(&lbl.caption, er_logic::shop_repaint::PAD_CAPTION);
        imap.insert(gid, u.clone());
        cmap.insert(gid, u);
    }
    let names: Vec<(u32, Vec<u16>)> = nmap.into_iter().collect();
    let infos: Vec<(u32, Vec<u16>)> = imap.into_iter().collect();
    let caps: Vec<(u32, Vec<u16>)> = cmap.into_iter().collect();
    let (n, nblk) = crate::fmg_inject::extend_swap_overrides_tracked(GOODS_NAME_CAT, &names);
    // RETRY, do not latch, when the NAME write did not land. `extend_swap_overrides` returns 0 when
    // the MSG repo or the category is not up yet and explicitly asks the caller to retry next tick --
    // `check_lots::dress_placeholder` does exactly that, and this pass did not, so it latched DONE on
    // a write of nothing and never tried again. shop_preview arms as soon as the scout cache and
    // apIdsToItemIds are ready, which is comfortably before the MSG repo, so losing the whole
    // override this way is the common case rather than the rare one. The icon (a PARAM write, repo up
    // much earlier) landed regardless, which is why a slot could show the AP flower with a vanilla
    // name. (Alaric, playtest 2026-07-25.)
    if n == 0 && !names.is_empty() {
        let tries = NAME_RETRIES.fetch_add(1, Ordering::Relaxed) + 1;
        if tries < NAME_RETRY_LIMIT {
            if tries == 1 {
                log::info!(
                    "shop-preview: name override wrote 0 of {} -- MSG repo/category not up yet, \
                     retrying (NOT latching)",
                    names.len()
                );
            }
            return false;
        }
        log::warn!(
            "shop-preview: name override still wrote 0 of {} after {NAME_RETRY_LIMIT} ticks -- \
             giving up and latching. Those slots keep their VANILLA name (or render `?GoodsName?` \
             if the row has no FMG entry at all); the flower icon and the reward routing are \
             unaffected. See the FMG extend-swap warning above for the ids.",
            names.len()
        );
    }
    let (i, iblk) = crate::fmg_inject::extend_swap_overrides_tracked(GOODS_INFO_CAT, &infos);
    let (c, cblk) = crate::fmg_inject::extend_swap_overrides_tracked(GOODS_CAPTION_CAT, &caps);
    *MY_BLOCKS.lock().unwrap() = [nblk, iblk, cblk];
    *PAINTED.lock().unwrap() = None; // fresh blocks carry baseline labels; any open shelf repaints
    log::info!(
        "shop-preview: {overridden} foreign/gem slot(s) + {locks} region-lock slot(s) marked ({} distinct, \
         {native} own-world via shop_sell, {protected} left vanilla to protect a real good's shared FMG entry, \
         {shared_rows} row(s) SHARED by slots holding different items -> labelled 'Archipelago Items' rather \
         than named after one of them, world#231) \
         -> extend-swap names={n} infos={i} captions={c}",
        names.len()
    );
    // A PARTIAL WRITE IS A FAILURE, NOT A CLEAN RUN. All three maps are built from ONE key set, so
    // anything short of `want` in any category means some shop row got a NAME and no DESCRIPTION.
    // That is exactly what boblerrr's `0.3.1` log carried for 100 minutes -- `names=53 infos=25
    // captions=25` -- while this pass reported success, because the counts were printed and nothing
    // ever COMPARED them. Compare them, name the shortfall per category, and say what the player
    // sees.
    //
    // WARN, not retry: a short count means the block WAS built and swapped, and
    // `extend_swap_overrides` leaks each block by design (the game may still hold the old pointer).
    // Re-running it for the whole retry budget would leak ~600 GoodsCaption blocks -- that category
    // is lore text, megabytes apiece. The retryable case is the `n == 0` one handled above, where
    // nothing was built because the MSG repo was not up.
    let want = names.len();
    if n < want || i < want || c < want {
        log::warn!(
            "shop-preview: INCOMPLETE -- of {want} distinct preview good(s): names={n} infos={i} \
             captions={c}. {} row(s) are short an info line and {} short a caption; those render \
             `?GoodsInfo?` / `?GoodsCaption?` in the item panel. The FMG extend-swap line above \
             names the ids and the reason. Root cause when the shortfall is in {GOODS_INFO_CAT}/\
             {GOODS_CAPTION_CAT} only: those goods rows carry NO entry in those categories, so the \
             client must CREATE one -- if entry insertion reported itself unavailable, the apworld \
             has to spend a spare row with full FMG coverage instead (issue #300).",
            want.saturating_sub(i),
            want.saturating_sub(c)
        );
    }
    DONE.store(true, Ordering::Relaxed);
    true
}

// ---------------------------------------------------------------------------------------------
// #937: per-shop-open repaint. The world colors the spare pool so no two slots visible in ONE
// menu share a goods row, reusing rows across regular-shop menus -- legal precisely because the
// two functions below rewrite each opened shelf's rows to that shelf's own claimants. The pure
// halves (claim fold, padding) are er_logic::shop_repaint; the write arm is
// fmg_inject::rewrite_in_place, gated on block identity.

/// Called from esd_probe's command-22 detour, same frame as the open. MARK-ONLY: the FMG walk and
/// the param walk happen on the tick ([`repaint_tick`]), never inside the game's dispatch frame.
/// One deep -- menus do not stack, so a new open simply replaces an unpainted older one.
pub fn on_shop_open(begin: i32, end: i32) {
    if let Ok(mut g) = PENDING_RANGE.lock() {
        *g = Some((begin, end));
    }
}

/// The repaint arm -- runs every tick from core beside [`run`]. Idle unless a shop opened since
/// the last paint. The hover probe measured the buy menu re-looking names up EVERY frame, so a
/// rewrite that lands a tick after the open is what the player reads.
pub fn repaint_tick() {
    let Some((lo, hi)) = PENDING_RANGE.lock().ok().and_then(|g| *g) else {
        return;
    };
    if !DONE.load(Ordering::Relaxed) {
        return; // baseline (padded) pass not landed yet -- it runs first, we keep the range
    }
    if PAINTED
        .lock()
        .map(|g| *g == Some((lo, hi)))
        .unwrap_or(false)
    {
        PENDING_RANGE.lock().unwrap().take();
        return; // same shelf, same blocks: nothing to rewrite
    }
    let [Some(nb), Some(ib), Some(cb)] = *MY_BLOCKS.lock().unwrap() else {
        // The baseline latched without publishing (MSG repo never came up / gave up after the
        // retry budget). There are no padded slots to rewrite; dropping the range is honest.
        PENDING_RANGE.lock().unwrap().take();
        return;
    };
    let per_loc = PER_LOC.lock().unwrap().clone();
    if per_loc.is_empty() {
        PENDING_RANGE.lock().unwrap().take();
        return; // no overridden slots this seed -- nothing a shelf could show
    }
    // Walk the OPENED RANGE, exactly like shop_hints: each row's stock flag joins to an AP
    // location, and PER_LOC says which goods row + label the baseline gave that slot.
    // SAFETY: FD4 singleton, read-only, on the game thread -- the same sanctioned access
    // shop_flags/shop_hints use.
    let Ok(repo) = (unsafe { SoloParamRepository::instance() }) else {
        return; // repo not up (mid-load?) -- retry next tick, the range stays pending
    };
    if !crate::param_guard::is_available::<ShopLineupParam>(repo, "shop-open repaint") {
        return;
    }
    let hi = hi.min(lo.saturating_add(REPAINT_MAX_ROWS));
    let (lo_u, hi_u) = (lo.max(0) as u32, hi.max(0) as u32); // param ids are u32; a negative range is empty
    let mut claims: Vec<(u32, er_logic::name_override::ShopLabel)> = Vec::new();
    for id in lo_u..=hi_u {
        let Some(row) = crate::param_guard::get::<ShopLineupParam>(repo, id, "shop-open repaint")
        else {
            continue;
        };
        let flag = row.event_flag_for_stock();
        if flag == 0 {
            continue;
        }
        let Some(loc) = crate::shop_hints::loc_of_flag(flag) else {
            continue;
        };
        if let Some((_, gid, lbl)) = per_loc.iter().find(|(l, _, _)| *l == loc) {
            claims.push((*gid, lbl.clone()));
        }
    }
    if claims.is_empty() {
        PENDING_RANGE.lock().unwrap().take();
        *PAINTED.lock().unwrap() = Some((lo, hi));
        return; // a shelf with no overridden AP slots (all own-world/vanilla)
    }
    let overrides = er_logic::shop_repaint::per_shop_overrides(&claims);
    use crate::fmg_inject::{RewriteOutcome, rewrite_in_place};
    let (mut wrote, mut skipped) = (0usize, 0usize);
    for (gid, lbl) in &overrides {
        let name_u: Vec<u16> = lbl.name.encode_utf16().collect();
        let cap_u: Vec<u16> = lbl.caption.encode_utf16().collect();
        let writes: [(u32, usize, &[u16], usize); 3] = [
            (
                GOODS_NAME_CAT,
                nb,
                &name_u,
                er_logic::shop_repaint::PAD_NAME,
            ),
            (
                GOODS_INFO_CAT,
                ib,
                &cap_u,
                er_logic::shop_repaint::PAD_CAPTION,
            ),
            (
                GOODS_CAPTION_CAT,
                cb,
                &cap_u,
                er_logic::shop_repaint::PAD_CAPTION,
            ),
        ];
        for (cat, blk, txt, cap) in writes {
            match rewrite_in_place(cat, blk, *gid, txt, cap) {
                RewriteOutcome::Done => wrote += 1,
                RewriteOutcome::StaleBlock => {
                    // A sibling re-published (or a load reverted the pointer) between our baseline
                    // and this open: OUR padded slots are gone. Re-arm the baseline and keep the
                    // range pending -- next tick paints against the fresh blocks.
                    DONE.store(false, Ordering::Relaxed);
                    NAME_RETRIES.store(0, Ordering::Relaxed);
                    log::info!(
                        "shop-preview: repaint of rows {lo}..={hi} found a STALE cat-{cat} block; \
                         re-arming the padded baseline first (the shelf repaints right after)"
                    );
                    return;
                }
                RewriteOutcome::NoEntry | RewriteOutcome::NotPadded => skipped += 1,
            }
        }
    }
    PENDING_RANGE.lock().unwrap().take();
    *PAINTED.lock().unwrap() = Some((lo, hi));
    log::info!(
        "shop-preview: shelf rows {lo}..={hi} repainted -- {} AP slot(s) named for THIS shop \
         ({wrote} FMG rewrite(s) in place, {skipped} skipped)",
        overrides.len()
    );
}
