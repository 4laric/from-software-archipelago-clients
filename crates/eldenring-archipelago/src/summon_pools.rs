//! `summon_pools` — activate every summoning pool, without touching a flag that is also a check.
//!
//! Elden Ring gates each summoning pool (the "Activate?" effigies) on an event flag whose id IS the
//! `SignPuddleParam` ROW id. Turning them all on is therefore a param walk plus a flag write, and the
//! Hexinton CE table does exactly that. We cannot: pool row ids live in the `670xxx` band, which
//! ALSO holds shop `eventFlag_forStock` values, and several collide with live AP shop checks
//! (`670100` is both the Witchbane Ruins pool and the Blue Cloth Vest purchase flag). Writing those
//! blind releases checks the player never made and sells out slots holding placed AP items.
//!
//! So the partition — which ids are safe — is pure and lives in [`er_logic::summon_pools`], with the
//! collision as its motivating test. This module only supplies the two inputs (the row ids the game
//! actually has; the flag universe the check poll watches) and does the writing.
//!
//! Typed all the way down: `SoloParamRepository::rows::<SignPuddleParam>()` gives the ids, so there
//! is no AOB scan here and nothing to go stale on a game patch.
//!
//! Applied ONCE per connected session, gated on a loaded world — the same reason
//! `startgrants::apply_start_flags` waits for `has_inventory()`: flags set during the load screen get
//! clobbered by the save-data load. Idempotent and save-persisted, so a replay would be harmless
//! anyway. Kill switch: `ER_NO_SUMMON_POOLS=0`.

use std::collections::BTreeSet;

use eldenring::cs::{SignPuddleParam, SoloParamRepository};

use crate::flags;

/// Kill switch. Default ON when the feature is enabled by config; `ER_NO_SUMMON_POOLS=0` forces off.
fn env_allows() -> bool {
    !matches!(
        std::env::var("ER_NO_SUMMON_POOLS").ok().as_deref(),
        Some("0") | Some("false") | Some("off")
    )
}

/// Read every `SignPuddleParam` row id. `None` if the param repository is not up yet (retry later) —
/// distinct from `Some(vec![])`, which would mean the table really is empty.
fn pool_row_ids() -> Option<Vec<u32>> {
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    Some(repo.rows::<SignPuddleParam>().map(|(id, _row)| id).collect())
}

/// Turn on every pool flag that is not also a check flag.
///
/// `protected` is the flag universe the check poll watches — assembled by the caller from the MERGED
/// location-flag values and sweep-flag keys, never from a literal list. Returns `false` if the game
/// was not ready (caller retries next tick); `true` once applied.
pub fn apply(protected: &BTreeSet<u32>) -> bool {
    if !env_allows() {
        log::info!("summoning pools: disabled via ER_NO_SUMMON_POOLS");
        return true;
    }
    let Some(row_ids) = pool_row_ids() else {
        return false;
    };
    if row_ids.is_empty() {
        log::warn!("summoning pools: SignPuddleParam read back EMPTY — not applying");
        return false;
    }
    let plan = er_logic::summon_pools::plan(row_ids, protected);
    let mut set = 0usize;
    for &flag in &plan.to_set {
        if !flags::try_set_event_flag(flag, true) {
            // Flag holder went away mid-walk: report nothing applied and retry whole.
            log::warn!("summoning pools: flag holder not ready at {flag} — retrying next tick");
            return false;
        }
        set += 1;
    }
    // The skipped list is the interesting half — it is the evidence the guard is doing work, and a
    // SILENT zero would be indistinguishable from the guard having been dropped. Log both counts and
    // name the withheld ids.
    if plan.skipped.is_empty() {
        log::info!("summoning pools: {set} activated, 0 withheld (no check-flag collisions)");
    } else {
        log::info!(
            "summoning pools: {set} activated, {} WITHHELD because the check poll watches them: {:?}",
            plan.skipped.len(),
            plan.skipped
        );
    }
    true
}
