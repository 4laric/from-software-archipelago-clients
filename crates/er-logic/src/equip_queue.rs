//! Equipment writes have their own budget: grant pacing cannot bound a ready equip backlog.

/// Conservative spacing for model-changing equips, independent of item delivery.
pub const INTERVAL_MS: u64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    Changed,
    /// Already equipped, or not an equipable item. Do not spend the budget.
    Done,
    /// Not in the bag yet, or held for a boss fight. Keep the original stream metadata.
    Retry,
}

#[derive(Debug, Default)]
pub struct Pacer {
    last_change_ms: Option<u64>,
}

impl Pacer {
    pub const fn new() -> Self {
        Self {
            last_change_ms: None,
        }
    }

    pub fn ready(&self, now_ms: u64) -> bool {
        self.last_change_ms
            .is_none_or(|last| now_ms.saturating_sub(last) >= INTERVAL_MS)
    }

    pub fn record_change(&mut self, now_ms: u64) {
        self.last_change_ms = Some(now_ms);
    }

    /// At most one successful write per interval; idle time never banks a catch-up burst.
    /// A retry does not block later ready items (notably armour behind a held weapon).
    pub fn drain<T>(
        &mut self,
        pending: Vec<T>,
        now_ms: u64,
        mut apply: impl FnMut(&T) -> Attempt,
    ) -> Vec<T> {
        let mut remaining = Vec::new();
        for item in pending {
            if !self.ready(now_ms) {
                remaining.push(item);
                continue;
            }
            match apply(&item) {
                Attempt::Changed => self.record_change(now_ms),
                Attempt::Done => {}
                Attempt::Retry => remaining.push(item),
            }
        }
        remaining
    }
}

/// Session override, separate from the seed so reconnect cannot re-enable it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Recovery {
    #[default]
    Seed,
    Off,
}

impl Recovery {
    pub fn parse(arg: &str) -> Option<Self> {
        match arg.trim() {
            "seed" => Some(Self::Seed),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    pub fn enabled(self, seed_enabled: bool) -> bool {
        seed_enabled && self == Self::Seed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_backlog_changes_one_piece_per_interval_without_losing_metadata() {
        let items: Vec<_> = (0..100).map(|n| (n, n as u64, 3u8)).collect();
        let mut pending = items.clone();
        let mut pacer = Pacer::new();
        let mut applied = Vec::new();
        for n in 0..100 {
            let now = n * INTERVAL_MS;
            pending = pacer.drain(pending, now, |item| {
                applied.push(*item);
                Attempt::Changed
            });
            assert_eq!(applied.len(), n as usize + 1);
            // Another tick, even just before the deadline, cannot mutate equipment.
            pending = pacer.drain(pending, now + INTERVAL_MS - 1, |_| panic!("burst"));
        }
        assert!(pending.is_empty());
        assert_eq!(applied, items);
    }

    #[test]
    fn held_and_missing_items_survive_while_noops_do_not_spend_budget() {
        let mut pacer = Pacer::new();
        let mut seen = Vec::new();
        let pending = pacer.drain(vec![1, 2, 3, 4, 5], 0, |item| {
            seen.push(*item);
            match item {
                1 | 2 => Attempt::Retry,
                3 => Attempt::Done,
                _ => Attempt::Changed,
            }
        });
        assert_eq!(seen, [1, 2, 3, 4]);
        assert_eq!(pending, [1, 2, 5]);
        let pending = pacer.drain(pending, INTERVAL_MS, |_| Attempt::Changed);
        assert_eq!(pending, [2, 5]);
    }

    #[test]
    fn long_idle_never_accumulates_credit() {
        let mut pacer = Pacer::new();
        assert!(pacer.drain(vec![1], 0, |_| Attempt::Changed).is_empty());
        assert_eq!(
            pacer.drain(vec![2, 3, 4], 60_000, |_| Attempt::Changed),
            [3, 4]
        );
        assert!(!pacer.ready(60_000));
        assert!(!pacer.ready(0));
    }

    #[test]
    fn recovery_survives_seed_changes_and_never_forces_a_disabled_seed_on() {
        let recovery = Recovery::parse("off").unwrap();
        for seed in [false, true, false, true] {
            assert!(!recovery.enabled(seed));
        }
        let recovery = Recovery::parse("seed").unwrap();
        assert!(!recovery.enabled(false));
        assert!(recovery.enabled(true));
        for invalid in ["on", "off now", "", "pause"] {
            assert_eq!(Recovery::parse(invalid), None);
        }
    }
}
