//! Bounded follow-up cadence for ItemLotParam suppression after a world load.
//!
//! A false->true `in_world` edge can precede the end of param streaming. The live client still
//! performs its immediate rewrite on that edge, then uses this planner for two delayed verification
//! passes. This is deliberately a tiny finite schedule, not a per-tick rewrite loop.

/// Delays after the world edge at which suppression is re-armed once more.
pub const FOLLOW_UP_MS: [u64; 2] = [750, 2_500];

#[derive(Debug, Default, Clone)]
pub struct SuppressionRearm {
    armed_at_ms: u64,
    next: usize,
    armed: bool,
}

impl SuppressionRearm {
    /// Start a fresh grace window. A later load/warp replaces the old window.
    pub fn arm(&mut self, now_ms: u64) {
        self.armed_at_ms = now_ms;
        self.next = 0;
        self.armed = true;
    }

    /// Return the delay whose follow-up is due. At most two calls return `Some` per arm.
    pub fn poll(&mut self, now_ms: u64) -> Option<u64> {
        if !self.armed || self.next == FOLLOW_UP_MS.len() {
            return None;
        }
        let delay = FOLLOW_UP_MS[self.next];
        if now_ms.saturating_sub(self.armed_at_ms) < delay {
            return None;
        }
        self.next += 1;
        if self.next == FOLLOW_UP_MS.len() {
            self.armed = false;
        }
        Some(delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn early_clean_pass_then_reversion_gets_two_bounded_followups() {
        let mut gate = SuppressionRearm::default();
        gate.arm(10_000); // the live edge also performs the immediate clean pass
        assert_eq!(gate.poll(10_749), None);

        // Param streaming reverts that early pass; the first delayed pass repairs it.
        assert_eq!(gate.poll(10_750), Some(750));
        assert_eq!(gate.poll(11_000), None, "no per-tick rewrite churn");

        // A final verification pass covers a later table stream and then the gate goes quiet.
        assert_eq!(gate.poll(12_500), Some(2_500));
        assert_eq!(gate.poll(60_000), None);
    }

    #[test]
    fn grace_warp_starts_a_new_window_and_discards_the_old_deadlines() {
        let mut gate = SuppressionRearm::default();
        gate.arm(1_000);
        assert_eq!(gate.poll(1_750), Some(750));

        // Warp arrival is another world edge. Its immediate pass happens outside this planner;
        // delayed verification must now be relative to the new arrival, not the previous load.
        gate.arm(2_000);
        assert_eq!(gate.poll(2_500), None);
        assert_eq!(gate.poll(2_750), Some(750));
        assert_eq!(gate.poll(4_500), Some(2_500));
        assert_eq!(gate.poll(4_501), None);
    }
}
