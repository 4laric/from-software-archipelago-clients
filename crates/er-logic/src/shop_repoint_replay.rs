//! `shop_repoint_replay` — headless timeline for the INVISIBLE SHOP PREVIEW.
//!
//! [`crate::shop_repoint::decide`] is pure and unit-tested one call at a time. This module adds the
//! dimension those miss and that the replay tier exists for: what the player actually SEES on the
//! shelf, across the load edges that revert runtime param writes.
//!
//! Two failures live in this timeline, and they are different bugs with the same symptom (the slot
//! reads as its vanilla ware), which is why both get a policy flag rather than one:
//!
//!  1. **No row write at all.** `shopPreviewGoods` repoints the preview at a spare goods row and the
//!     client rewrites that spare's FMG + icon — but nothing writes `ShopLineupParam.equipId`, so the
//!     menu keeps rendering the vanilla ware and the override is unobservable. Live on `main` from
//!     2026-07-20 (locks) / 2026-07-22 (foreign) until this fix; confirmed in-game 2026-07-25.
//!  2. **No re-arm on the load edge.** A map load streams `ShopLineupParam` back in and reverts the
//!     write while a `DONE` latch survives, so the repoint holds only until the first load. This is
//!     the identical shape as the 2026-07-24 `shop_sell` param-revert bug (`shop_echo`), and the
//!     reason `core.rs` must `reset()` this pass on the in-world edge beside `check_lots`.
//!
//! The model is deliberately its own — a single-tick mock cannot express a later load, which is the
//! whole point.

#[cfg(test)]
mod replay {
    use crate::shop_repoint::{decide, Repoint};

    const VANILLA_WARE: i32 = 9510; // a REAL grantable good (perfume bottle) -- the protected case
    const VANILLA_TYPE: u8 = 3;
    const SPARE: i32 = 9314; // dedicated spare from greenfield/spare_goods.tsv
    const GOODS_NIBBLE: i64 = 0x4000_0000;

    /// What the shop menu renders for this slot. The menu reads the ROW, never slot_data — that is
    /// the entire bug, so the model resolves the name the same way the game does.
    fn displayed(row_equip_id: i32, row_equip_type: u8, fmg: &[(i32, &'static str)]) -> String {
        if row_equip_type != 3 {
            return format!("<non-goods ware {row_equip_id}>");
        }
        for (id, nm) in fmg {
            if *id == row_equip_id {
                return (*nm).to_string();
            }
        }
        format!("<vanilla goods {row_equip_id}>")
    }

    #[derive(Clone, Copy)]
    struct Policy {
        /// Write the preview good onto the row (the fix). False = pre-2026-07-25 behaviour.
        repoint_rows: bool,
        /// Re-arm the pass on the in-world edge, so a load-reverted row is repointed again.
        rearm_on_load: bool,
    }

    enum Ev {
        /// One in-world tick.
        Tick,
        /// A map load: params stream back in, reverting every runtime write.
        MapLoad,
    }

    struct Sim {
        equip_id: i32,
        equip_type: u8,
        /// The client's FMG overrides, keyed by goods row id. `shop_preview` writes these regardless
        /// of the row -- they are global per goods row and always "succeed".
        fmg: Vec<(i32, &'static str)>,
        done: bool,
        writes: u32,
    }

    impl Sim {
        fn new() -> Self {
            Sim {
                equip_id: VANILLA_WARE,
                equip_type: VANILLA_TYPE,
                // shop_preview's override on the SPARE. Present from the first tick in both policies:
                // the override was never the broken half.
                fmg: vec![(SPARE, "AP: Hookclaws / For: Bob (Dark Souls III)")],
                done: false,
                writes: 0,
            }
        }

        fn tick(&mut self, p: Policy) {
            if !p.repoint_rows || self.done {
                return;
            }
            match decide(
                Some(GOODS_NIBBLE | SPARE as i64),
                self.equip_id,
                self.equip_type,
                false,
            ) {
                Repoint::Write(eid, etype) => {
                    self.equip_id = eid;
                    self.equip_type = etype;
                    self.writes += 1;
                }
                Repoint::Skip(_) => {}
            }
            self.done = true;
        }

        fn map_load(&mut self, p: Policy) {
            self.equip_id = VANILLA_WARE; // params reverted
            self.equip_type = VANILLA_TYPE;
            if p.rearm_on_load {
                self.done = false;
            }
        }

        fn run(&mut self, events: &[Ev], p: Policy) {
            for e in events {
                match e {
                    Ev::Tick => self.tick(p),
                    Ev::MapLoad => self.map_load(p),
                }
            }
        }

        fn shelf(&self) -> String {
            displayed(self.equip_id, self.equip_type, &self.fmg)
        }
    }

    const FIXED: Policy = Policy {
        repoint_rows: true,
        rearm_on_load: true,
    };

    #[test]
    fn preview_override_is_invisible_while_the_row_still_sells_the_vanilla_ware() {
        // WITHOUT the fix: the FMG override for the spare is written and correct, and the player
        // still reads the vanilla ware off the shelf. Reproduces Alaric's 2026-07-25 report.
        let mut s = Sim::new();
        s.run(
            &[Ev::Tick, Ev::Tick],
            Policy {
                repoint_rows: false,
                rearm_on_load: true,
            },
        );
        assert_eq!(s.shelf(), "<vanilla goods 9510>");
        assert_eq!(s.writes, 0);

        // WITH the fix: the row sells the spare, so the override the client was already writing
        // finally renders.
        let mut s = Sim::new();
        s.run(&[Ev::Tick, Ev::Tick], FIXED);
        assert_eq!(s.shelf(), "AP: Hookclaws / For: Bob (Dark Souls III)");
        assert_eq!(
            s.writes, 1,
            "second tick must be idempotent, not a re-write"
        );
    }

    #[test]
    fn a_map_load_reverts_the_repoint_and_only_the_rearm_restores_it() {
        // The shop_sell param-revert shape. Without the in-world-edge reset the slot is correct until
        // the player's first load and vanilla for the rest of the run -- which is worse than never
        // working, because it looks confirmed in a short test.
        let mut s = Sim::new();
        s.run(
            &[Ev::Tick, Ev::MapLoad, Ev::Tick, Ev::Tick],
            Policy {
                repoint_rows: true,
                rearm_on_load: false,
            },
        );
        assert_eq!(s.shelf(), "<vanilla goods 9510>");

        let mut s = Sim::new();
        s.run(&[Ev::Tick, Ev::MapLoad, Ev::Tick, Ev::Tick], FIXED);
        assert_eq!(s.shelf(), "AP: Hookclaws / For: Bob (Dark Souls III)");
        assert_eq!(s.writes, 2, "one write per load edge, not per tick");
    }

    #[test]
    fn repeated_loads_keep_restoring_it() {
        let mut s = Sim::new();
        s.run(
            &[
                Ev::Tick,
                Ev::MapLoad,
                Ev::Tick,
                Ev::MapLoad,
                Ev::Tick,
                Ev::MapLoad,
                Ev::Tick,
            ],
            FIXED,
        );
        assert_eq!(s.shelf(), "AP: Hookclaws / For: Bob (Dark Souls III)");
        assert_eq!(s.writes, 4);
    }

    #[test]
    fn a_row_shop_sell_owns_is_never_dragged_back_to_its_vanilla_ware() {
        // Own-world sellable reward: shop_sell wrote the REWARD onto the row and the world left the
        // preview at the vanilla ware. The repoint must decline, or it undoes the native sale and
        // trips ECHO-DEDUP's param-revert guard (which reads the row back to prove delivery).
        let reward_id = 1030000; // a weapon the slot now natively sells
        assert_eq!(
            decide(Some(GOODS_NIBBLE | VANILLA_WARE as i64), reward_id, 0, true),
            Repoint::Skip(crate::shop_repoint::SkipReason::SoldNatively)
        );
    }
}
