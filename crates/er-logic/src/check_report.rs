//! Durable AP check reporting (world#720).
//!
//! Detection and transport are separate facts: observing a boss flag earns the check, while a
//! socket call only attempts to report it. Keep observed ids as debt until the AP client accepts
//! them, so a death/load edge cannot make an unrelated later pickup the retry trigger.

use std::collections::HashSet;

/// Retire checks already present in the client's checked set and return the remaining retry batch
/// in stable order (stable diagnostics and tests; transport does not care about order).
pub fn retry_batch(pending: &mut HashSet<i64>, is_checked: impl Fn(i64) -> bool) -> Vec<i64> {
    pending.retain(|&loc| !is_checked(loc));
    let mut batch: Vec<i64> = pending.iter().copied().collect();
    batch.sort_unstable();
    batch
}

/// Retire exactly a batch accepted by `mark_checked`.
pub fn retire_accepted(pending: &mut HashSet<i64>, accepted: &[i64]) {
    for loc in accepted {
        pending.remove(loc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn death_between_detection_and_transport_does_not_lose_the_check() {
        let mut pending = HashSet::from([720]);

        // The death-frame transport attempt fails. Nothing is retired.
        assert_eq!(retry_batch(&mut pending, |_| false), vec![720]);
        assert!(pending.contains(&720));

        // Respawn: no pickup and no second flag edge, just the standing debt retrying.
        let retry = retry_batch(&mut pending, |_| false);
        assert_eq!(retry, vec![720]);
        retire_accepted(&mut pending, &retry);
        assert!(pending.is_empty());
    }

    #[test]
    fn reconnect_retires_server_accepted_debt_without_resending() {
        let mut pending = HashSet::from([10, 20]);
        assert_eq!(retry_batch(&mut pending, |loc| loc == 10), vec![20]);
        assert_eq!(pending, HashSet::from([20]));
    }

    #[test]
    fn repeated_detection_is_idempotent() {
        let mut pending = HashSet::new();
        pending.insert(5);
        pending.insert(5);
        assert_eq!(retry_batch(&mut pending, |_| false), vec![5]);
    }
}
