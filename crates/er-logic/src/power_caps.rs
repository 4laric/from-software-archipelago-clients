//! Seed-length power ladders. Not an enforcement mechanism: no player option
//! advertises a hard cap until the native upgrade/level-up transaction is gated.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ladder {
    pub initial: u32,
    pub unlocks: Vec<u32>,
}

impl Ladder {
    /// At most one useful step per kept region; long seeds bound the number of
    /// new items at ten. Every step advances, and the last reaches the maximum.
    pub fn new(initial: u32, maximum: u32, kept_regions: usize) -> Option<Self> {
        let width = maximum.checked_sub(initial)?;
        if width == 0 || kept_regions == 0 {
            return None;
        }
        let steps = kept_regions.min(10).min(width as usize) as u32;
        let unlocks = (1..=steps)
            .map(|step| {
                initial + (u64::from(width) * u64::from(step)).div_ceil(u64::from(steps)) as u32
            })
            .collect();
        Some(Self { initial, unlocks })
    }

    /// Recomputed from the complete received stream, rather than persisted
    /// mutable counters: reconnects and replay cannot add extra unlocks.
    pub fn target(&self, received_copies: usize) -> u32 {
        if received_copies == 0 {
            self.initial
        } else {
            self.unlocks[received_copies.min(self.unlocks.len()) - 1]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_seed_length_and_track_reaches_the_end_monotonically() {
        for regions in 1..=60 {
            for (start, end) in [(3, 25), (1, 10), (30, 150)] {
                let ladder = Ladder::new(start, end, regions).unwrap();
                assert!(ladder.unlocks.len() <= regions.min(10));
                let mut previous = start;
                for next in &ladder.unlocks {
                    assert!(*next > previous);
                    previous = *next;
                }
                assert_eq!(previous, end);
                assert_eq!(ladder.target(0), start);
                assert_eq!(ladder.target(usize::MAX), end);
            }
        }
    }

    #[test]
    fn invalid_or_empty_ladders_are_rejected() {
        assert!(Ladder::new(10, 3, 4).is_none());
        assert!(Ladder::new(3, 3, 4).is_none());
        assert!(Ladder::new(3, 25, 0).is_none());
    }
}
