//! `crash_tallies` — session counters the crash report appends, so ONE teardown crash answers the
//! client#301 correlation question instead of a human diffing the log around it.
//!
//! Cokeman5's two crashes fault inside me3's mimalloc free path on a world edge, and the strongest
//! client-side lead is that runtime scaling had just written rungs to many `UNLOADED` characters
//! (68 and 84 in the two sessions). #305's register capture names the pointer; these counters name
//! the SESSION: a report carrying a large unloaded-writes count keeps the lead alive, one carrying
//! zero exonerates it — either way the next crash moves the issue, with no behaviour changed on
//! correlation alone.
//!
//! 🛑 ATOMICS ONLY, like `seed_ids`. The crash handler runs in an exception context on the
//! faulting thread; a `Mutex` it happened to hold would deadlock the report itself. No strings, no
//! allocation at record time.

use std::sync::atomic::{AtomicU64, Ordering};

/// Rung writes this session (`apply_speffect` on the scaling path).
static SCALING_WRITES: AtomicU64 = AtomicU64::new(0);
/// …of which to characters whose load status was UNLOADED — the #301 correlation axis.
static SCALING_WRITES_UNLOADED: AtomicU64 = AtomicU64::new(0);
/// …of which later FAILED to recompute while loaded (the rescale watch's anomaly verdicts).
static SCALING_RECOMPUTE_FAILED_LOADED: AtomicU64 = AtomicU64::new(0);

/// Fold one sweep's tally into the session counters. Called from the ER scaling census at its
/// summary site, where the per-sweep numbers already exist.
pub fn record_scaling_sweep(scaled: u32, unloaded: u32, recompute_failed_loaded: u32) {
    SCALING_WRITES.fetch_add(scaled as u64, Ordering::Relaxed);
    SCALING_WRITES_UNLOADED.fetch_add(unloaded as u64, Ordering::Relaxed);
    SCALING_RECOMPUTE_FAILED_LOADED.fetch_add(recompute_failed_loaded as u64, Ordering::Relaxed);
}

/// The crash-report line. EMPTY when no scaling write happened all session: a scaling-off seed
/// (and every non-ER game) must not grow a line that says nothing.
pub fn annotate() -> String {
    let writes = SCALING_WRITES.load(Ordering::Relaxed);
    if writes == 0 {
        return String::new();
    }
    let unloaded = SCALING_WRITES_UNLOADED.load(Ordering::Relaxed);
    let failed = SCALING_RECOMPUTE_FAILED_LOADED.load(Ordering::Relaxed);
    format!(
        "session: enemy-scaling wrote {writes} rung(s) this session ({unloaded} to UNLOADED \
         chrs, {failed} recompute-failed-loaded) [client#301 correlation]\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ONE stateful test, deliberately: the counters are process-wide statics, so parallel tests
    /// would race (same constraint as `seed_ids`). Covers the zero case (silence) and the nonzero
    /// case (the line carries all three numbers and the issue ref).
    #[test]
    fn the_line_names_the_correlation_axes() {
        assert_eq!(annotate(), "", "no writes this session, no line");
        record_scaling_sweep(84, 68, 3);
        let line = annotate();
        assert!(line.contains("84 rung(s)"), "{line}");
        assert!(line.contains("68 to UNLOADED"), "{line}");
        assert!(line.contains("3 recompute-failed-loaded"), "{line}");
        assert!(line.contains("client#301"), "{line}");
        assert!(line.is_ascii(), "repo rule 10: {line}");
    }
}
