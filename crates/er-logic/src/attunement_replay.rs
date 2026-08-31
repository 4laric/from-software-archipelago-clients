//! Headless timeline for the post-#1006 attunement contract.
//!
//! Boss payouts are sent immediately and independently of attunement. Attunement still derives
//! from the authoritative server checked set and controls only the once-per-region grace bloom.

#[cfg(test)]
mod replay {
    use crate::attunement::{attuned, newly_attuned};
    use std::collections::HashSet;

    struct Region {
        members: HashSet<i64>,
        threshold: u32,
        server_checked: HashSet<i64>,
        attuned_latched: bool,
        bloom_lit: bool,
        primed: bool,
        banners: Vec<String>,
    }

    impl Region {
        fn new(members: &[i64], threshold: u32) -> Self {
            Self {
                members: members.iter().copied().collect(),
                threshold,
                server_checked: HashSet::new(),
                attuned_latched: false,
                bloom_lit: false,
                primed: false,
                banners: Vec::new(),
            }
        }

        fn is_attuned(&self) -> bool {
            attuned(&self.members, self.threshold, |m| {
                self.server_checked.contains(&m)
            })
        }

        fn collect(&mut self, id: i64) {
            self.server_checked.insert(id);
        }

        /// Boss payout follows the ordinary LocationCheck path immediately. Attunement state does
        /// not participate in this operation.
        fn pay_boss(&mut self, payout: &[i64]) {
            self.server_checked.extend(payout.iter().copied());
        }

        fn poll(&mut self) {
            let attuned_now = self.is_attuned();
            if !self.primed {
                if attuned_now {
                    self.bloom_lit = true;
                    self.attuned_latched = true;
                }
                self.primed = true;
                return;
            }
            if newly_attuned(self.attuned_latched, attuned_now) {
                self.bloom_lit = true;
                self.attuned_latched = true;
                self.banners.push("attuned".to_string());
            }
        }

        fn reconnect(&mut self) {
            self.attuned_latched = false;
            self.bloom_lit = false;
            self.primed = false;
            self.banners.clear();
        }
    }

    #[test]
    fn boss_payout_is_immediate_below_attunement_threshold() {
        let mut r = Region::new(&[1, 2, 3, 4, 5], 3);
        r.collect(1);
        r.pay_boss(&[900, 901]);
        r.poll();
        assert!(!r.is_attuned());
        assert!(r.server_checked.contains(&900));
        assert!(r.server_checked.contains(&901));
        assert!(!r.bloom_lit, "payout must not fake region attunement");
    }

    #[test]
    fn threshold_crossing_still_blooms_once() {
        let mut r = Region::new(&[1, 2, 3], 3);
        r.collect(1);
        r.poll();
        r.collect(2);
        r.collect(3);
        r.poll();
        assert!(r.bloom_lit);
        assert_eq!(r.banners, ["attuned"]);
        r.poll();
        assert_eq!(r.banners, ["attuned"]);
    }

    #[test]
    fn reconnect_replays_payout_and_primes_bloom_silently() {
        let mut r = Region::new(&[1, 2, 3], 3);
        for id in [1, 2, 3] {
            r.collect(id);
        }
        r.pay_boss(&[900, 901]);
        r.poll();
        r.reconnect();
        r.poll();
        assert!(r.server_checked.contains(&900));
        assert!(r.bloom_lit);
        assert!(r.banners.is_empty());
    }
}
