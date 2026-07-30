//! `reconciler_replay` — the THEOREM the reconciler design rests on, as an executable property test.
//!
//! The thesis of [`crate::reconcile`] is that grant/snapshot bugs vanish once state is driven by a
//! DIFF toward a fixpoint instead of by discrete events. The formal claim is:
//!
//! > For a fixed corpus of received items, the final converged game state is invariant under
//! > (a) any REORDERING of the events that drive the client, (b) DUPLICATION of any
//! > `ItemReceived` / `connect` event, and (c) INJECTION of load screens (unstable stretches)
//! > between any two events.
//!
//! If that holds, then every event-ordering bug the reconciler is meant to kill — flask double-grant
//! on reload, great-rune double-grant on reconnect, map-piece-on-connect, the flag-poll re-snapshot,
//! Torch clobber — is *impossible by construction*, because none of those perturbations can move the
//! fixpoint. This module drives the REAL [`crate::reconcile::Reconciler`] through a mock game for a
//! canonical in-order run, then asserts every permuted / duplicated / load-interleaved run reaches
//! the byte-identical fixpoint.

#[cfg(test)]
mod replay {
    use crate::reconcile::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// The observable fixpoint we compare across scrambles: the SET flags, the goods inventory, and
    /// the multiset of consumable grants that landed (sorted). Watermarks / event order are
    /// deliberately excluded — only the player-visible end state matters.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Fixpoint {
        set_flags: BTreeSet<FlagId>,
        goods: BTreeSet<GoodsId>,
        ledger: Vec<(GoodsId, i32)>,
    }

    fn snapshot(g: &MockGame) -> Fixpoint {
        let set_flags = g
            .flags
            .iter()
            .filter(|&(_, &on)| on)
            .map(|(&f, _)| f)
            .collect();
        let mut ledger = g.ledger_log.clone();
        ledger.sort();
        Fixpoint {
            set_flags,
            goods: g.goods.clone(),
            ledger,
        }
    }

    // ---- the corpus: one item of every observability class ------------------------------

    const SEED: &str = "SEED-A";
    const N: usize = 7;

    /// One item of EVERY observability class the client grants: a region lock (also a seal-override),
    /// a map piece (flags only), a key item (good + 4000xx obtained flag), a great rune (good +
    /// restored flag), a goal flag, and two consumables. This is the corpus the permutation /
    /// duplication / load-injection theorem is proven over. Progressive items are count-based (not a
    /// pure set) so they get their own dedicated invariance test below.
    fn corpus() -> Vec<ReceivedItem> {
        vec![
            ReceivedItem {
                index: 0,
                name: "Limgrave Lock".into(),
                semantics: ItemSemantics::RegionFlags(vec![76971, 76972]),
            },
            ReceivedItem {
                index: 1,
                name: "Underground Map".into(),
                semantics: ItemSemantics::MapReveal(vec![62060, 82001]),
            },
            ReceivedItem {
                index: 2,
                name: "Godrick's Great Rune".into(),
                semantics: ItemSemantics::GreatRune {
                    goods: 191,
                    restored_flag: 6901,
                },
            },
            ReceivedItem {
                index: 3,
                name: "Flask of Crimson Tears".into(),
                semantics: ItemSemantics::Consumable {
                    full_id: 1001,
                    qty: 3,
                    echo_skip: false,
                },
            },
            ReceivedItem {
                index: 4,
                name: "Flask of Cerulean Tears".into(),
                semantics: ItemSemantics::Consumable {
                    full_id: 1002,
                    qty: 1,
                    echo_skip: false,
                },
            },
            ReceivedItem {
                index: 5,
                name: "Rold Medallion".into(),
                semantics: ItemSemantics::KeyItem {
                    goods: 9000,
                    obtained_flags: vec![400001],
                },
            },
            ReceivedItem {
                index: 6,
                name: "Goal".into(),
                semantics: ItemSemantics::GoalFlag(9600),
            },
        ]
    }

    /// 76971 is also a SEAL flag: it starts desired-false, then item 0 opens it. This exercises the
    /// seal->open override inside the invariance corpus.
    fn make_inputs(prefix_hi: i64) -> DesiredInputs {
        let received: Vec<ReceivedItem> = corpus()
            .into_iter()
            .filter(|it| it.index <= prefix_hi)
            .collect();
        DesiredInputs {
            seed: SEED.into(),
            save: SaveIdentity("slot0".into()),
            received,
            slot_data: SlotData {
                seal_flags: vec![76971],
                // Bulk slot-data grants (constant across every scramble): start graces, an
                // unconditional + a reveal_all_maps map flag, one start item, and a met goal flag.
                // They are proven invariant alongside the permuted/duplicated/load-injected stream.
                start_graces: vec![76900],
                always_map_flags: vec![82005],
                reveal_all_maps: true,
                map_reveal_flags: vec![62010],
                start_items: vec![StartItem {
                    full_id: 3000,
                    qty: 1,
                }],
                goal_flag: Some(9700),
                goal_met: true,
            },
        }
    }

    /// One driver event. `Receive(k)` models the AP server delivering the received-item PREFIX up to
    /// index `k` (deliveries are always a growing prefix, so `high = max(high, k)`), then nudging the
    /// reconciler. `Connect` re-nudges. `Load` is an interleaved load screen: a stretch where the
    /// world is not stable, during which a tick must do NOTHING.
    #[derive(Clone, Copy, Debug)]
    enum Ev {
        Connect,
        Receive(i64),
        Load,
    }

    /// Run a driver-event sequence through the REAL reconciler + mock game, then force stability and
    /// drain to the fixpoint. Returns the observable end state.
    fn run(events: &[Ev]) -> Fixpoint {
        let budget = TickBudget::default();
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(make_inputs(-1)); // empty prefix to start
        let mut high: i64 = -1;

        for &ev in events {
            match ev {
                Ev::Connect => {
                    r.set_inputs(make_inputs(high));
                    // a few convergence ticks (stable)
                    g.set_stable(true);
                    r.run_to_fixpoint(&mut g, budget, 16);
                }
                Ev::Receive(k) => {
                    high = high.max(k);
                    r.set_inputs(make_inputs(high));
                    g.set_stable(true);
                    r.run_to_fixpoint(&mut g, budget, 16);
                }
                Ev::Load => {
                    // A load screen: the world goes unstable; a tick here must not mutate anything.
                    g.set_stable(false);
                    let out = r.tick(&mut g, budget);
                    assert!(
                        out.skipped_unstable,
                        "a tick during a load screen must skip"
                    );
                    g.set_stable(true);
                }
            }
        }

        // Whatever the scramble, end fully live and drained.
        g.set_stable(true);
        r.mark_dirty();
        r.run_to_fixpoint(&mut g, budget, 32);
        snapshot(&g)
    }

    /// The canonical in-order fixpoint: connect, then receive 0,1,2,3,4 in order.
    fn canonical() -> Fixpoint {
        let mut evs = vec![Ev::Connect];
        for k in 0..N as i64 {
            evs.push(Ev::Receive(k));
        }
        run(&evs)
    }

    /// Enumerate all permutations of `items` (Heap's algorithm), applying `f` to each. Bounded by the
    /// caller keeping the slice small (N=5 -> 120 permutations).
    fn permute<T: Clone, F: FnMut(&[T])>(items: &mut Vec<T>, k: usize, f: &mut F) {
        if k <= 1 {
            f(items);
            return;
        }
        for i in 0..k {
            permute(items, k - 1, f);
            if k % 2 == 0 {
                items.swap(i, k - 1);
            } else {
                items.swap(0, k - 1);
            }
        }
    }

    #[test]
    fn fixpoint_is_invariant_under_every_receive_permutation() {
        // (a) REORDERING: every order in which the five items are received must reach the same
        // fixpoint as the canonical in-order run.
        let want = canonical();
        let mut order: Vec<i64> = (0..N as i64).collect();
        let mut checked = 0;
        permute(&mut order, N, &mut |perm| {
            let mut evs = vec![Ev::Connect];
            for &k in perm {
                evs.push(Ev::Receive(k));
            }
            assert_eq!(
                run(&evs),
                want,
                "permutation {perm:?} diverged from the canonical fixpoint"
            );
            checked += 1;
        });
        assert_eq!(checked, 5040, "expected 7! = 5040 permutations");
    }

    #[test]
    fn fixpoint_is_invariant_under_duplicated_events() {
        // (b) DUPLICATION: duplicating ItemReceived and connect events (as a flaky socket / reconnect
        // would) must not change the fixpoint. Duplicate every receive and sprinkle extra connects.
        let want = canonical();
        let mut evs = vec![Ev::Connect, Ev::Connect];
        for k in 0..N as i64 {
            evs.push(Ev::Receive(k));
            evs.push(Ev::Receive(k)); // duplicate delivery
            if k % 2 == 0 {
                evs.push(Ev::Connect); // spurious reconnect
            }
        }
        // and a trailing duplicate of the whole prefix
        for k in 0..N as i64 {
            evs.push(Ev::Receive(k));
        }
        assert_eq!(run(&evs), want, "duplicated events changed the fixpoint");
    }

    #[test]
    fn fixpoint_is_invariant_under_injected_load_screens() {
        // (c) LOAD SCREEN INJECTION: a load screen (unstable stretch) between any two events must not
        // change the fixpoint — the gated tick simply does nothing until the world is live again.
        let want = canonical();
        let mut evs = vec![Ev::Load, Ev::Connect, Ev::Load];
        for k in 0..N as i64 {
            evs.push(Ev::Receive(k));
            evs.push(Ev::Load); // a reload between every delivery
        }
        assert_eq!(
            run(&evs),
            want,
            "injected load screens changed the fixpoint"
        );
    }

    #[test]
    fn fixpoint_is_invariant_under_permutation_plus_dup_plus_load() {
        // The full theorem: reorder AND duplicate AND interleave load screens simultaneously. A
        // representative sample of permutations (the full 120 each wrapped in dup+load) proves the
        // three perturbations compose without moving the fixpoint.
        let want = canonical();
        let mut order: Vec<i64> = (0..N as i64).collect();
        permute(&mut order, N, &mut |perm| {
            let mut evs = vec![Ev::Load, Ev::Connect];
            for &k in perm {
                evs.push(Ev::Receive(k));
                evs.push(Ev::Receive(k)); // dup
                evs.push(Ev::Load); // load screen
            }
            evs.push(Ev::Connect);
            assert_eq!(
                run(&evs),
                want,
                "perm+dup+load {perm:?} diverged from the canonical fixpoint"
            );
        });
    }

    #[test]
    fn canonical_fixpoint_is_the_expected_end_state() {
        // Pin the actual end state so a regression in the corpus semantics is caught, not just
        // self-consistency across scrambles.
        let fp = canonical();
        let want_flags: BTreeSet<FlagId> = [
            // received-stream flags
            76971u32, 76972, 62060, 82001, 6901, 400001, 9600,
            // slot-data bulk flags (start grace, always+reveal map flags, met goal)
            76900, 82005, 62010, 9700,
        ]
        .into_iter()
        .collect();
        assert_eq!(
            fp.set_flags, want_flags,
            "all region/map/rune/key/goal + bulk flags set exactly once"
        );
        assert_eq!(
            fp.goods,
            [191i32, 9000].into_iter().collect::<BTreeSet<_>>(),
            "the rune good AND the key-item good present, never a map piece"
        );
        assert_eq!(
            fp.ledger,
            vec![(1001, 3), (1002, 1), (3000, 1)],
            "each consumable + the start item granted exactly once (no double-grant)"
        );
    }

    #[test]
    fn no_map_piece_good_ever_lands_across_scrambles() {
        // The map-pieces-on-connect guard, phrased over the whole invariance run: no matter the event
        // order, the goods inventory is exactly {rune} — never a map-piece good. This is what the
        // MapReveal variant structurally guarantees.
        let mut order: Vec<i64> = (0..N as i64).collect();
        let mut buckets: BTreeMap<GoodsId, ()> = BTreeMap::new();
        permute(&mut order, N, &mut |perm| {
            let mut evs = vec![Ev::Connect];
            for &k in perm {
                evs.push(Ev::Receive(k));
            }
            for g in run(&evs).goods {
                buckets.insert(g, ());
            }
        });
        let goods: Vec<GoodsId> = buckets.into_keys().collect();
        assert_eq!(
            goods,
            vec![191, 9000],
            "only the great-rune + key-item goods ever land; never a map piece"
        );
    }

    // ---- progressive invariance (count-based, its own corpus) ---------------------------

    /// A 3-copy progressive stream over a 2-tier bell: tiers 0/1 land as unique goods 8101/8102 with
    /// flags 70001/70002, and the 3rd copy overflows to one Lord's Rune. Because it is COUNT-based,
    /// the corpus is a run of same-name copies; the theorem here is that its converged state is
    /// invariant under receiving those copies in any order (and under load-screen injection).
    fn prog_inputs(prefix_hi: i64) -> DesiredInputs {
        let tiers = vec![
            ProgTier {
                goods: vec![8101],
                flags: vec![70001],
                consumed: false,
            },
            ProgTier {
                goods: vec![8102],
                flags: vec![70002],
                consumed: false,
            },
        ];
        let received: Vec<ReceivedItem> = (0..3i64)
            .filter(|&k| k <= prefix_hi)
            .map(|k| ReceivedItem {
                index: k,
                name: "progressive_stone_bell".into(),
                semantics: ItemSemantics::Progressive {
                    tiers: tiers.clone(),
                    overflow_full_id: 2919,
                },
            })
            .collect();
        DesiredInputs {
            seed: SEED.into(),
            save: SaveIdentity("slot0".into()),
            received,
            slot_data: SlotData::default(),
        }
    }

    fn run_prog(events: &[Ev]) -> Fixpoint {
        let budget = TickBudget::default();
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(prog_inputs(-1));
        let mut high: i64 = -1;
        for &ev in events {
            match ev {
                Ev::Connect => {
                    r.set_inputs(prog_inputs(high));
                    g.set_stable(true);
                    r.run_to_fixpoint(&mut g, budget, 16);
                }
                Ev::Receive(k) => {
                    high = high.max(k);
                    r.set_inputs(prog_inputs(high));
                    g.set_stable(true);
                    r.run_to_fixpoint(&mut g, budget, 16);
                }
                Ev::Load => {
                    g.set_stable(false);
                    let out = r.tick(&mut g, budget);
                    assert!(
                        out.skipped_unstable,
                        "a tick during a load screen must skip"
                    );
                    g.set_stable(true);
                }
            }
        }
        g.set_stable(true);
        r.mark_dirty();
        r.run_to_fixpoint(&mut g, budget, 32);
        snapshot(&g)
    }

    #[test]
    fn progressive_fixpoint_is_invariant_under_permutation_and_load() {
        let want = {
            let mut evs = vec![Ev::Connect];
            for k in 0..3i64 {
                evs.push(Ev::Receive(k));
            }
            run_prog(&evs)
        };
        // Pin the expected end state: both tiers present, both flags set, one overflow Lord's Rune.
        assert_eq!(
            want.goods,
            [8101i32, 8102].into_iter().collect::<BTreeSet<_>>()
        );
        assert_eq!(
            want.set_flags,
            [70001u32, 70002].into_iter().collect::<BTreeSet<_>>()
        );
        assert_eq!(
            want.ledger,
            vec![(2919, 1)],
            "exactly one overflow, never duplicated"
        );

        let mut order: Vec<i64> = (0..3).collect();
        permute(&mut order, 3, &mut |perm| {
            let mut evs = vec![Ev::Load, Ev::Connect];
            for &k in perm {
                evs.push(Ev::Receive(k));
                evs.push(Ev::Receive(k)); // duplicate delivery
                evs.push(Ev::Load); // interleaved load screen
            }
            assert_eq!(
                run_prog(&evs),
                want,
                "progressive perm+dup+load {perm:?} diverged"
            );
        });
    }

    // ---- mass-grant CTD: bug reproduced unpaced, fixed by pacing -------------------------
    //
    // The crash Alaric hit: a burst of received checks arrives at once and the client tries to grant
    // them all in quick succession, overflowing the game's item-acquisition popup queue -> CTD. The
    // pure crate can't crash a real process, so we model the fragility EXACTLY where it lives: a
    // `GameIo` whose acquisition queue overflows if more than `SAFE_BURST` item grants land within any
    // trailing `ABSORB_WINDOW_MS` window. Then we drive the SAME delta through the SAME reconciler
    // twice, changing ONLY the budget:
    //   * `min_grant_interval_ms: 0` == the PRE-FIX unpaced path (grant up to `goods` EVERY frame) ->
    //     the fragile game CTDs mid-drain (bug reproduced).
    //   * the PACED budget (the live default) spaces grants so no window is ever exceeded -> no CTD,
    //     and the whole delta still drains, each item exactly once (fix demonstrated).

    /// Max item grants the game can absorb within any trailing [`ABSORB_WINDOW_MS`] window before its
    /// acquisition-notification queue overflows (the modelled CTD). Flag writes are cheap and excluded.
    const SAFE_BURST: usize = 4;
    const ABSORB_WINDOW_MS: u64 = 150;

    /// A fragile [`GameIo`]: real flag/goods/ledger state + the injected clock come from an inner
    /// [`MockGame`], so the reconciler drives its identical live loop — we only ADD the CTD fragility.
    struct CrashProneGame {
        inner: MockGame,
        /// `now_ms` of every ITEM grant (unique good or ledgered consumable) that landed.
        grant_times: Vec<u64>,
        /// Set once the acquisition queue overflows — a live game would be dead (CTD) from here.
        crashed: bool,
    }

    impl CrashProneGame {
        fn new() -> Self {
            CrashProneGame {
                inner: MockGame::stable(),
                grant_times: Vec::new(),
                crashed: false,
            }
        }
        /// Record a grant at the current clock; trip `crashed` if the trailing window now exceeds the
        /// queue's capacity (the overflow the real CTD came from).
        fn note_grant(&mut self) {
            let now = self.inner.stability.now_ms;
            self.grant_times.push(now);
            let window_lo = now.saturating_sub(ABSORB_WINDOW_MS);
            let in_window = self.grant_times.iter().filter(|&&t| t >= window_lo).count();
            if in_window > SAFE_BURST {
                self.crashed = true;
            }
        }
    }

    impl GameIo for CrashProneGame {
        fn stability(&self) -> WorldStability {
            self.inner.stability()
        }
        fn get_flag(&self, f: FlagId) -> bool {
            self.inner.get_flag(f)
        }
        fn set_flag(&mut self, f: FlagId, on: bool) -> bool {
            self.inner.set_flag(f, on) // flag writes never touch the acquisition queue
        }
        fn has_good(&self, g: GoodsId) -> bool {
            self.inner.has_good(g)
        }
        fn grant_good(&mut self, g: GoodsId, comp: &[FlagId]) -> bool {
            if !self.inner.grant_good(g, comp) {
                return false;
            }
            self.note_grant();
            true
        }
        fn grant_ledgered(&mut self, full_id: GoodsId, qty: i32) -> bool {
            if !self.inner.grant_ledgered(full_id, qty) {
                return false;
            }
            self.note_grant();
            true
        }
    }

    /// A "bunch of item checks at the same time": 40 consumables plus two unique goods, all owed at
    /// once — 42 item grants for the reconciler to place.
    fn mass_delta() -> DesiredInputs {
        let mut received: Vec<ReceivedItem> = (0..40i64)
            .map(|i| ReceivedItem {
                index: i,
                name: format!("Consumable {i}"),
                semantics: ItemSemantics::Consumable {
                    full_id: 6000 + i as i32,
                    qty: 1,
                    echo_skip: false,
                },
            })
            .collect();
        received.push(ReceivedItem {
            index: 40,
            name: "Rold Medallion".into(),
            semantics: ItemSemantics::KeyItem {
                goods: 9000,
                obtained_flags: vec![400001],
            },
        });
        received.push(ReceivedItem {
            index: 41,
            name: "Godrick's Great Rune".into(),
            semantics: ItemSemantics::GreatRune {
                goods: 191,
                restored_flag: 6901,
            },
        });
        DesiredInputs {
            seed: SEED.into(),
            save: SaveIdentity("slot0".into()),
            received,
            slot_data: SlotData::default(),
        }
    }

    /// Drive the whole delta through a 60fps poll loop with `budget`, advancing the injected clock a
    /// frame at a time exactly as the live poll thread advances real time. Stops on CTD, on a fully
    /// drained (converged) delta, or a generous frame cap. Returns (crashed, item grants landed).
    fn drive_mass_delta(budget: TickBudget) -> (bool, usize) {
        const FRAME_MS: u64 = 16; // ~60fps
        const MAX_FRAMES: usize = 6000; // ~96s of sim time — ample for the paced drain
        let mut game = CrashProneGame::new();
        let mut r = Reconciler::new(mass_delta());
        for _ in 0..MAX_FRAMES {
            let out = r.tick_with_classes(&mut game, budget, ApplyClasses::ALL);
            if game.crashed || out.converged {
                break;
            }
            game.inner.advance_ms(FRAME_MS); // next frame: real time passes
        }
        (game.crashed, game.grant_times.len())
    }

    #[test]
    fn mass_grant_delta_ctds_when_unpaced_but_survives_when_paced() {
        // PRE-FIX: `min_grant_interval_ms == 0` IS the old unpaced path (grant up to `goods` every
        // frame). Against the fragile game a burst of checks floods the acquisition queue -> CTD.
        let unpaced = TickBudget {
            goods: 4,
            flags: 32,
            min_grant_interval_ms: 0,
        };
        let (crashed_unpaced, granted_unpaced) = drive_mass_delta(unpaced);
        assert!(
            crashed_unpaced,
            "UNPACED (pre-fix): a large delta must overflow the acquisition queue — the CTD is reproduced"
        );
        assert!(
            granted_unpaced < 42,
            "the crash lands MID-drain (only {granted_unpaced}/42 granted), not after everything settled"
        );

        // POST-FIX: the paced budget (the live default) spaces grants so no absorb window is ever
        // exceeded — the identical delta now survives AND drains completely, each item exactly once.
        let paced = TickBudget {
            goods: 2,
            flags: 32,
            min_grant_interval_ms: 150,
        };
        let (crashed_paced, granted_paced) = drive_mass_delta(paced);
        assert!(
            !crashed_paced,
            "PACED (fix): the same delta must never overflow the queue — no CTD"
        );
        assert_eq!(
            granted_paced, 42,
            "PACED (fix): all 40 consumables + 2 unique goods land, none lost or double-granted"
        );
    }

    // ---- the grant-stall guard: the 2026-07-30 infinite-drop softlock ------------------------

    /// Mohg's Great Rune, the good both 2026-07-30 reporters watched loop.
    fn rune_inputs() -> DesiredInputs {
        DesiredInputs {
            seed: SEED.into(),
            save: SaveIdentity("slot0".into()),
            received: vec![ReceivedItem {
                index: 0,
                name: "Mohg's Great Rune".into(),
                semantics: ItemSemantics::GreatRune {
                    goods: 195,
                    restored_flag: 6905,
                },
            }],
            slot_data: SlotData::default(),
        }
    }

    /// Drive `ticks` dirty ticks against a game that ACCEPTS every grant and lands none.
    ///
    /// `guarded == false` reproduces the PRE-FIX reconciler exactly: re-arming on every tick means
    /// the attempt counter can never reach `MAX_GRANT_ATTEMPTS`, which is precisely the state the
    /// code was in before the stall set existed. That is the policy flag this pair toggles.
    fn drive_refused(guarded: bool, ticks: usize) -> (usize, Vec<GoodsId>) {
        let mut g = MockGame::stable();
        g.refuse_unique_adds = true;
        let mut r = Reconciler::new(rune_inputs());
        let mut stalled = Vec::new();
        for _ in 0..ticks {
            if !guarded {
                r.rearm_grant_stalls();
            }
            let out = r.tick(&mut g, TickBudget::default());
            stalled.extend(out.newly_stalled);
            r.mark_dirty(); // core.rs re-marks dirty EVERY frame; that is what defeats convergence
        }
        (g.unique_grant_calls.len(), stalled)
    }

    /// A unique good the game keeps refusing must stop being re-granted, say so exactly once, and
    /// get a fresh allowance on the next world edge.
    ///
    /// THE BUG: `diff` re-emits `GrantUnique` for any desired good a snapshot cannot see;
    /// `core.rs` re-marks the reconciler dirty every frame; and `grant_good` reports success for
    /// anything it dispatched, because `grant_item` throws away `AddItemFunc`'s result. When the
    /// game REFUSES the add — inventory at cap, "would exceed the maximum storage", item dropped on
    /// the floor — those three compose into an unbounded re-grant at ~6/second for the rest of the
    /// session, logged as `converged=true`. Two players lost saves to it on 2026-07-30; it is the
    /// third instance of the class (2026-07-12 flask ladder, 2026-07-19 co-op Morgott's rune), and
    /// the first two fixes each removed one CAUSE and left the loop.
    #[test]
    fn refused_unique_grant_stalls_after_max_attempts_and_rearms_on_reload_replay() {
        // PRE-FIX: unbounded. 200 frames of gameplay, 200 grants, no end in sight.
        let (unguarded_calls, unguarded_stalled) = drive_refused(false, 200);
        assert_eq!(
            unguarded_calls, 200,
            "PRE-FIX: the refused grant must re-fire on EVERY tick -- the softlock is reproduced"
        );
        assert!(
            unguarded_stalled.is_empty(),
            "PRE-FIX: nothing is ever parked, which is the whole defect"
        );

        // POST-FIX: bounded at the cap, and announced exactly once however long the player plays.
        let (guarded_calls, guarded_stalled) = drive_refused(true, 200);
        assert_eq!(
            guarded_calls, MAX_GRANT_ATTEMPTS as usize,
            "FIX: exactly MAX_GRANT_ATTEMPTS grants, then silence -- not {guarded_calls}"
        );
        assert_eq!(
            guarded_stalled,
            vec![195],
            "FIX: the good is announced ONCE (the client warns on this), not once per frame"
        );
    }

    /// The stall is per-load, not permanent: a load screen restores the full allowance, because the
    /// reason for a refusal (full bag, an accessor pointed at the multiplay key list) can stop being
    /// true across a world edge. Worst case is MAX_GRANT_ATTEMPTS popups per load, never a flood.
    #[test]
    fn grant_stall_rearms_across_a_load_screen_replay() {
        let mut g = MockGame::stable();
        g.refuse_unique_adds = true;
        let mut r = Reconciler::new(rune_inputs());

        for _ in 0..50 {
            r.tick(&mut g, TickBudget::default());
            r.mark_dirty();
        }
        assert_eq!(g.unique_grant_calls.len(), MAX_GRANT_ATTEMPTS as usize);
        assert!(r.stalled_goods().contains(&195), "parked before the load");

        // A load screen: one unstable tick.
        g.set_stable(false);
        r.tick(&mut g, TickBudget::default());
        assert!(
            r.stalled_goods().is_empty(),
            "a world edge must re-arm -- the refusal cause may be gone"
        );

        // Back in world, still refusing: exactly one more allowance, not an endless one.
        g.set_stable(true);
        for _ in 0..50 {
            r.tick(&mut g, TickBudget::default());
            r.mark_dirty();
        }
        assert_eq!(
            g.unique_grant_calls.len(),
            2 * MAX_GRANT_ATTEMPTS as usize,
            "exactly one fresh allowance per load"
        );
    }

    /// The guard must not cost us the save-scum self-heal. A good that is genuinely LOST (not
    /// refused) is re-granted, lands, is observed, and its counter is cleared -- so the allowance is
    /// never consumed and the good can be healed an unlimited number of times.
    #[test]
    fn stall_guard_leaves_the_save_scum_self_heal_intact_replay() {
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(rune_inputs());
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert!(g.has_good(195));

        // Lose and heal it far more times than the cap would allow if healing counted as an attempt.
        for i in 0..15 {
            g.drop_good(195);
            r.mark_dirty();
            r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
            assert!(g.has_good(195), "heal {i} must land");
            assert!(
                r.stalled_goods().is_empty(),
                "a LANDING re-grant must never consume the refusal allowance"
            );
        }
    }

    /// A stalled good must not hold the reconciler dirty forever: once it is parked, a tick with no
    /// other work reports converged, so the client stops re-planning and the log stops lying.
    #[test]
    fn a_stalled_good_lets_the_tick_converge_replay() {
        let mut g = MockGame::stable();
        g.refuse_unique_adds = true;
        let mut r = Reconciler::new(rune_inputs());
        for _ in 0..16 {
            r.tick(&mut g, TickBudget::default());
            r.mark_dirty();
        }
        let out = r.tick(&mut g, TickBudget::default());
        assert!(
            out.converged,
            "a parked good must not keep the reconciler permanently unconverged"
        );
        assert!(out.applied.is_empty(), "and it must not apply anything");
    }

    /// The 2026-07-29 WILD timeline (player log `archipelago-2026-07-29.log`: 6 process sessions,
    /// 4525 flood grants at ~6.6/s, every session ending in a native CTD). The risk under review:
    /// [`Reconciler::rearm_grant_stalls`] fires on EVERY unstable tick, so if world stability
    /// flapped mid-flood the guard would re-arm forever and never park. The log says stability did
    /// NOT flap: `inventory-ptr: retired at world edge` / `enemy-scaling: settle release` appear
    /// only at load screens -- one per session load-in, plus one mid-session warp in two sessions
    /// -- and each is followed by MINUTES of continuously stable flood (the last session floods
    /// stable for 8 minutes straight). Encoded here as [unstable load] -> [long stable stretch]
    /// per stretch, one stretch per world edge the log shows. The guard must spend exactly
    /// MAX_GRANT_ATTEMPTS grants per stretch and then hold converged, turning the wild 4525-grant
    /// flood into 24 grants for the whole evening.
    #[test]
    fn wild_20260729_six_session_flood_timeline_is_bounded_replay() {
        let mut g = MockGame::stable();
        g.refuse_unique_adds = true; // accepted, never observable -- the wild failure mode

        // Stable-stretch lengths (in ticks) between world edges, per session. Sessions 4 and 5 had
        // a second mid-session world edge (01:41:07+01:41:22, 01:42:27+01:42:41); the rest had only
        // the load-in. Lengths are scaled-down stand-ins for minutes of stable flood.
        let sessions: [&[usize]; 6] = [
            &[450],     // S1: edge 01:35:20, flood to CTD 01:37:19
            &[160],     // S2: 01:37:30 -> CTD 01:38:37
            &[290],     // S3: 01:38:46 -> CTD 01:40:18
            &[90, 140], // S4: two edges, CTD 01:41:43
            &[95, 260], // S5: two edges, CTD 01:43:21
            &[3200],    // S6: 01:45:21 edge, EIGHT stable minutes -> CTD 01:53:35
        ];

        let mut stretches = 0usize;
        for session in sessions {
            // Process restart = fresh reconciler state (the stall set is deliberately not
            // persisted), exactly like the player relaunching after each CTD.
            let mut r = Reconciler::new(rune_inputs());
            for &len in session {
                // The world edge: a few unstable ticks (load screen / dwell not yet settled).
                g.set_stable(false);
                for _ in 0..5 {
                    let out = r.tick(&mut g, TickBudget::default());
                    assert!(out.skipped_unstable);
                }
                g.set_stable(true);

                let before = g.unique_grant_calls.len();
                let mut last = TickOutcome::default();
                for _ in 0..len {
                    last = r.tick(&mut g, TickBudget::default());
                    r.mark_dirty(); // core.rs re-marks dirty every frame
                }
                assert_eq!(
                    g.unique_grant_calls.len() - before,
                    MAX_GRANT_ATTEMPTS as usize,
                    "each stable stretch must cost exactly the allowance, then park"
                );
                assert!(
                    last.converged && last.applied.is_empty(),
                    "after parking, the stretch must sit converged instead of flooding"
                );
                stretches += 1;
            }
        }
        assert_eq!(
            stretches, 8,
            "the log shows 8 world edges across the 6 flood sessions"
        );
        assert_eq!(
            g.unique_grant_calls.len(),
            stretches * MAX_GRANT_ATTEMPTS as usize,
            "the whole wild evening is bounded at 3 grants per world edge -- 24, not 4525"
        );
    }
}
