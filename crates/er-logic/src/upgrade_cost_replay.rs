//! `upgrade_cost_replay` -- headless timeline replay for the flatten_regular_upgrades RECONNECT
//! RE-APPLY. Sibling of the other `*_replay` modules.
//!
//! The clamp decision is already pure + unit-tested in [`crate::upgrade_cost`] (`clamp_count`,
//! `FlattenLatch`). Those fire one call. This module adds the dimension they miss and that the replay
//! tier exists for: the STATEFUL sequence around a reconnect.
//!
//! The hazard is unique to this feature: the flatten is applied by mutating the LIVE `EquipMtrlSetParam`
//! table, but the game reloads regulation.bin (params -> VANILLA) on every load. So on reconnect the
//! clamp is GONE and must be re-applied, or weapons silently revert to the vanilla 2/4/6 cost. The
//! re-arm latch (`FlattenLatch::set` on every connect) is what guarantees that; this replay proves a
//! reconnect actually re-clamps a freshly-reloaded (vanilla) table, that it stays idempotent within a
//! connection, that a cap change on reconnect re-applies at the new cap, and that off never touches it.

#[cfg(test)]
mod replay {
    use crate::upgrade_cost::{clamp_count, FlattenLatch};

    const REG: i32 = 10100; // Smithing Stone [1] (a REGULAR stone)
    const SOMBER: i32 = 10160; // a somber stone (must never be touched)

    /// A tiny model of the live param table: the three regular-stone steps of one weapon band at
    /// their vanilla 2/4/6 counts, plus one somber step. A reconnect RESETS it to vanilla, exactly as
    /// the game does when it reloads regulation on load.
    fn vanilla_table() -> Vec<(i32, i8)> {
        vec![(REG, 2), (REG, 4), (REG, 6), (SOMBER, 1)]
    }

    struct Sim {
        table: Vec<(i32, i8)>,
        latch: FlattenLatch,
    }

    impl Sim {
        fn new() -> Self {
            Sim { table: vanilla_table(), latch: FlattenLatch::new() }
        }
        /// Fresh connect: arm the latch at `cap`.
        fn connect(&mut self, cap: i32) {
            self.latch.set(cap);
        }
        /// Reconnect: the game reloaded regulation -> table back to VANILLA, and the latch re-arms.
        fn reconnect(&mut self, cap: i32) {
            self.table = vanilla_table();
            self.latch.set(cap);
        }
        /// One in-world tick: apply the clamp iff armed, exactly like `upgrade_cost::maybe_apply`.
        fn tick(&mut self) -> u32 {
            if !self.latch.should_apply() {
                return 0;
            }
            let cap = self.latch.cap;
            let mut changed = 0u32;
            for (id, cnt) in self.table.iter_mut() {
                if let Some(nv) = clamp_count(*id, *cnt, cap) {
                    *cnt = nv;
                    changed += 1;
                }
            }
            self.latch.mark_applied();
            changed
        }
        fn regular(&self) -> Vec<i8> {
            self.table.iter().filter(|(id, _)| *id == REG).map(|(_, c)| *c).collect()
        }
        fn somber(&self) -> Vec<i8> {
            self.table.iter().filter(|(id, _)| *id == SOMBER).map(|(_, c)| *c).collect()
        }
    }

    #[test]
    fn reconnect_reapplies_the_clamp() {
        let mut s = Sim::new();
        s.connect(3);
        assert_eq!(s.tick(), 2, "cap 3: 4->3 and 6->3 clamped, 2 untouched");
        assert_eq!(s.regular(), vec![2, 3, 3]);
        assert_eq!(s.somber(), vec![1], "somber never touched");
        // Idempotent within the connection: a second tick changes nothing.
        assert_eq!(s.tick(), 0, "one-shot within a connection");

        // RECONNECT: the game reloaded regulation -> params are VANILLA again.
        s.reconnect(3);
        assert_eq!(s.regular(), vec![2, 4, 6], "reload reverts the live params to vanilla");
        // THE SHIP RISK: the re-arm must re-clamp the freshly reloaded table on the next tick.
        assert_eq!(s.tick(), 2, "flatten MUST re-apply after reconnect (re-arm latch)");
        assert_eq!(s.regular(), vec![2, 3, 3]);
    }

    #[test]
    fn cap_change_on_reconnect_reapplies_at_new_cap() {
        let mut s = Sim::new();
        s.connect(3);
        s.tick();
        assert_eq!(s.regular(), vec![2, 3, 3]);
        // Player lowers flatten to 1 and reconnects.
        s.reconnect(1);
        assert_eq!(s.tick(), 3, "new cap 1 re-clamps every step above 1");
        assert_eq!(s.regular(), vec![1, 1, 1]);
    }

    #[test]
    fn off_never_touches_the_table() {
        let mut s = Sim::new();
        s.connect(0);
        assert_eq!(s.tick(), 0);
        assert_eq!(s.regular(), vec![2, 4, 6]);
        // And a reconnect while off is still a no-op.
        s.reconnect(0);
        assert_eq!(s.tick(), 0);
        assert_eq!(s.regular(), vec![2, 4, 6]);
    }

    #[test]
    fn lower_only_never_raises_across_reconnect() {
        // Cap 5: only the 6-step is clamped (to 5); 2 and 4 are left (never raised). Holds across a
        // reconnect too -- the reloaded vanilla table is re-clamped the same lower-only way.
        let mut s = Sim::new();
        s.connect(5);
        assert_eq!(s.tick(), 1);
        assert_eq!(s.regular(), vec![2, 4, 5]);
        s.reconnect(5);
        assert_eq!(s.tick(), 1);
        assert_eq!(s.regular(), vec![2, 4, 5]);
    }
}
