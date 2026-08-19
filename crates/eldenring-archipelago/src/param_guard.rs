//! Fallible access to regulation params while the game is streaming or tearing a world down.
//!
//! `SoloParamRepository::instance()` can remain available after individual holders have already
//! lost res-cap slot 0. The upstream typed getters assume that can never happen and panic. That
//! assumption is valid for ordinary game code, but not for our detours: a callback can arrive in
//! the teardown window, and a panic there aborts because it would cross an `extern "C"` frame.

use eldenring::cs::{ParamResCap, SoloParam, SoloParamRepository};

fn res_cap<'a, P: SoloParam>(
    repo: &'a SoloParamRepository,
    caller: &'static str,
) -> Option<&'a ParamResCap> {
    let Some(holder) = repo.solo_param_holders.get(P::INDEX as usize) else {
        log::error!(
            "param-guard: {caller} deferred: {} holder index {} is outside the repository",
            P::NAME,
            P::INDEX,
        );
        return None;
    };
    let Some(res_cap) = holder.get_res_cap(0) else {
        log::warn!(
            "param-guard: {caller} deferred: {} holder {} has no res-cap 0 (reported count {})",
            P::NAME,
            P::INDEX,
            holder.res_cap_count,
        );
        return None;
    };
    Some(res_cap)
}

/// Whether the requested holder currently has a payload.
///
/// Useful before a bounded callback loop so one teardown event emits one diagnostic rather than
/// one per attempted row. Lookups still re-check independently, keeping them safe for other callers.
pub fn is_available<P: SoloParam>(repo: &SoloParamRepository, caller: &'static str) -> bool {
    res_cap::<P>(repo, caller).is_some()
}

/// Look up a row without calling upstream's panicking `SoloParamRepository::get`.
///
/// `caller` is deliberately required: an empty holder is a lifecycle signal, not an absent row,
/// and the next report needs to identify which callback ran late without relying on stripped
/// Windows backtraces.
pub fn get<'a, P: SoloParam>(
    repo: &'a SoloParamRepository,
    param_id: u32,
    caller: &'static str,
) -> Option<&'a P::StructType> {
    let res_cap = res_cap::<P>(repo, caller)?;

    // SAFETY: `SoloParam` fixes the row type for this holder index. This is the same typed lookup
    // upstream's `SoloParamRepository::get` performs after its panicking res-cap expectation; the
    // only difference is that the unavailable-holder state returned above is fallible here.
    unsafe {
        res_cap
            .param_res_cap
            .data
            .get_row_by_id::<P::StructType>(param_id)
    }
}

/// Iterate a parameter without calling upstream's panicking `SoloParamRepository::rows`.
pub fn rows<'a, P: SoloParam + 'a>(
    repo: &'a SoloParamRepository,
    caller: &'static str,
) -> Option<impl Iterator<Item = (u32, &'a P::StructType)> + 'a> {
    let res_cap = res_cap::<P>(repo, caller)?;

    // SAFETY: same invariant as `get`; the iterator remains bounded by the borrowed res-cap.
    Some(unsafe { res_cap.param_res_cap.data.rows::<P::StructType>() })
}
