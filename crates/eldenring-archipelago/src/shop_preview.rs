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
        nmap.insert(gid, lbl.name.encode_utf16().collect());
        let u: Vec<u16> = lbl.caption.encode_utf16().collect();
        imap.insert(gid, u.clone());
        cmap.insert(gid, u);
    }
    let names: Vec<(u32, Vec<u16>)> = nmap.into_iter().collect();
    let infos: Vec<(u32, Vec<u16>)> = imap.into_iter().collect();
    let caps: Vec<(u32, Vec<u16>)> = cmap.into_iter().collect();
    let n = crate::fmg_inject::extend_swap_overrides(GOODS_NAME_CAT, &names);
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
    let i = crate::fmg_inject::extend_swap_overrides(GOODS_INFO_CAT, &infos);
    let c = crate::fmg_inject::extend_swap_overrides(GOODS_CAPTION_CAT, &caps);
    log::info!(
        "shop-preview: {overridden} foreign/gem slot(s) + {locks} region-lock slot(s) marked ({} distinct, \
         {native} own-world via shop_sell, {protected} left vanilla to protect a real good's shared FMG entry) \
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
