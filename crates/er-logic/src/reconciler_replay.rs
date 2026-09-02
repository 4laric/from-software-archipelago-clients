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
                start_item_flags: Vec::new(),
                start_items: vec![StartItem {
                    full_id: 3000,
                    qty: 1,
                }],
                goal_flag: Some(9700),
                goal_met: true,
                // A receive-stream prerequisite pair (the Leyndell seal shape): desired-SET,
                // never owned, proven invariant alongside everything above.
                prereq_set_flags: vec![105, 182],
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
            if k.is_multiple_of(2) {
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
            // receive-stream prerequisite pair (the Leyndell seal shape): desired-SET, never
            // owned -- these must land exactly once and never be re-cleared by the seal fold
            105, 182,
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

    // ---- the POLL itself: issue #237, the gate that must never come back --------------------

    /// Drive an OBSERVED-ONLY divergence under one of the two tick policies.
    ///
    /// `convergence_gated == true` reproduces the PRE-#237 architecture faithfully: the `DIRTY`
    /// gate lived in `reconcile_io::tick`, OUTSIDE the pure reconciler, and decided whether `tick`
    /// was called at all. So the policy belongs in this driver -- at the call site -- not in
    /// `Reconciler`. It is the shape you get by "finishing the event layer": once a tick reports
    /// converged, stop polling until something nudges you.
    ///
    /// Nothing nudges. That is the entire point. Elden Ring emits no event when it diverges from
    /// what we wrote, so the gated arm sleeps through the divergence forever.
    ///
    /// Returns `(flag_healed, good_healed)` after the divergence + 30 frames of ordinary play.
    fn drive_observed_only_divergence(convergence_gated: bool) -> (bool, bool) {
        const RUNE_GOODS: GoodsId = 195;
        const RESTORED_FLAG: FlagId = 6905;

        let mut g = MockGame::stable();
        let mut r = Reconciler::new(rune_inputs());

        // 1. Converge normally: the rune is granted and its restored-flag set.
        r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
        assert!(
            g.goods.contains(&RUNE_GOODS) && g.get_flag(RESTORED_FLAG),
            "precondition: the run must converge before the divergence is interesting"
        );

        // 2. THE DIVERGENCE -- game-side ONLY. Desired does not move, nothing is received,
        //    `set_inputs` is never called, there is no world edge and no nudge of any kind.
        //    This is the shared shape of all four no-event classes: the 2026-07-30 rune going
        //    blind at a live player action, a CONTESTED flag vanilla re-clears (the 9116 shape),
        //    a co-op accessor switch, and a save-scum rollback.
        g.goods.remove(&RUNE_GOODS);
        g.flags.insert(RESTORED_FLAG, false);

        // 3. Ordinary play. The gated arm believes it converged, so it never looks again.
        let mut believed_converged = true;
        for _ in 0..30 {
            if convergence_gated && believed_converged {
                continue; // the dead `DIRTY` early-out, alive
            }
            let out = r.tick(&mut g, TickBudget::default());
            believed_converged = out.converged;
        }

        (g.get_flag(RESTORED_FLAG), g.goods.contains(&RUNE_GOODS))
    }

    /// An observed-only divergence heals with NO nudge -- and a convergence gate loses it.
    ///
    /// This is issue #237 encoded. `reconcile_io` shipped a documented event-nudge architecture
    /// that was never built: a `static DIRTY` that `set_inputs` re-stored `true` every frame, so
    /// the early-out at the top of `tick()` could never once observe `false`. Dead code -- and an
    /// armed footgun, because the obvious tidy-up ("finish the event layer", or "add the missing
    /// equality guard so the flag stops being re-dirtied") turns the gate ON, and the gate loses
    /// every divergence class the game does not announce.
    ///
    /// The deletion shipped in client `0e12450`. This pair is what stops it coming back: the
    /// mutation is no longer a paragraph in a commit message, it is the `true` arm below.
    ///
    /// 🛑 Do not "optimise" `tick` to skip work after convergence. Converging is not a reason to
    /// stop looking; it is only a statement about the last frame's DESIRED-vs-OBSERVED diff.
    #[test]
    fn observed_only_divergence_is_lost_under_a_convergence_gate_but_heals_when_polling_replay() {
        // PRE-FIX / "finished event layer": the gate holds, nothing re-observes, nothing heals.
        let (gated_flag, gated_good) = drive_observed_only_divergence(true);
        assert!(
            !gated_flag && !gated_good,
            "PRE-FIX: a convergence gate sleeps through an unannounced divergence -- the flag \
             stays clear and the good stays missing for the rest of the session. That is the \
             regression this test exists to keep dead."
        );

        // SHIPPED: an unconditional per-frame poll sees it and re-plans against it.
        let (polled_flag, polled_good) = drive_observed_only_divergence(false);
        assert!(
            polled_flag,
            "the flag vanilla cleared must be re-asserted with no nudge"
        );
        assert!(
            polled_good,
            "the good that went blind must be re-granted with no nudge"
        );
    }

    /// Drive `ticks` polling ticks against a game that ACCEPTS every grant and lands none.
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
        }
        (g.unique_grant_calls.len(), stalled)
    }

    /// A unique good the game keeps refusing must stop being re-granted, say so exactly once, and
    /// get a fresh allowance on the next world edge.
    ///
    /// THE BUG: `diff` re-emits `GrantUnique` for any desired good a snapshot cannot see;
    /// `tick` polls every frame regardless of convergence; and `grant_good` reports success for
    /// anything it dispatched, because `grant_item`'s result is captured but not yet INTERPRETED
    /// (`add_item_probe` records it for the stall log; `grant_full_id_outcome` still says
    /// `Placed`). When the
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

    /// clients#575: the AP cursor can advance beyond an accepted-but-unobservable unique grant.
    /// A reconnect may then expose no historical ReceivedItems entry at all. Recovered durable
    /// debt must restore the desired good independently, and may retire only after possession is
    /// positively observed.
    #[test]
    fn durable_unique_debt_survives_cursor_advance_and_retires_on_observation() {
        let mut g = MockGame::stable();
        g.refuse_unique_adds = true;
        let mut first = Reconciler::new(rune_inputs());
        let mut debt = Vec::new();
        for _ in 0..MAX_GRANT_ATTEMPTS + 2 {
            debt.extend(first.tick(&mut g, TickBudget::default()).newly_stalled);
        }
        assert_eq!(debt, vec![195]);

        // Reconnect after the save cursor advanced: the historical packet is absent, so only the
        // sidecar debt can make the rune desired in this new reconciler instance.
        let empty = DesiredInputs {
            seed: SEED.into(),
            save: SaveIdentity("slot0".into()),
            received: vec![],
            slot_data: SlotData::default(),
        };
        let mut resumed = Reconciler::from_persisted(empty.clone(), 99);
        resumed.restore_unique_debt(debt, []);
        let mut reconnect_inputs = empty.clone();
        reconnect_inputs.slot_data.reveal_all_maps = true; // force a same-seed desired rebuild
        resumed.set_inputs(reconnect_inputs);
        g.refuse_unique_adds = false;
        let grant = resumed.tick(&mut g, TickBudget::default());
        assert!(
            grant
                .applied
                .iter()
                .any(|action| matches!(action, Action::GrantUnique(195, _))),
            "the cursor-independent debt must issue one safe retry"
        );
        let observed = resumed.tick(&mut g, TickBudget::default());
        assert_eq!(observed.observed_unique_goods, vec![195]);

        // Persistence removes the debt on that witness. A later reconnect with the same advanced
        // cursor and no restored debt has no desired grant and therefore emits nothing.
        let mut after_retirement = Reconciler::from_persisted(empty, 99);
        g.drop_good(195);
        let out = after_retirement.tick(&mut g, TickBudget::default());
        assert!(out.applied.is_empty());
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
            r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
            assert!(g.has_good(195), "heal {i} must land");
            assert!(
                r.stalled_goods().is_empty(),
                "a LANDING re-grant must never consume the refusal allowance"
            );
        }
    }

    /// A stalled good must not keep the plan non-empty forever: once it is parked, a tick with no
    /// other work reports converged, so the log stops lying. (The poll itself never stops — parking
    /// suppresses the ACTION, not the observation.)
    #[test]
    fn a_stalled_good_lets_the_tick_converge_replay() {
        let mut g = MockGame::stable();
        g.refuse_unique_adds = true;
        let mut r = Reconciler::new(rune_inputs());
        for _ in 0..16 {
            r.tick(&mut g, TickBudget::default());
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

    /// The 2026-08-26 WILD timeline (TechnoForge, clients#439, player log `archipelago-2026-08-26.log`,
    /// build `379756368cbb`): Glintstone Whetblade (goods `0x4000230d`) went readback-blind LIVE and
    /// the guard parked it **14 times in 15 minutes = 42 refusal popups**, 13:58:01 -> 14:13:01.
    ///
    /// The burst size was never wrong. Every one of the 14 park lines reads `accepted 3 grant(s)`,
    /// and the dispatch counter steps by exactly 3 between consecutive parks (seq 65, 68, 71 ...
    /// 98). `MAX_GRANT_ATTEMPTS` did its whole job. What was wrong is the RE-ARM RATE: the guard
    /// re-armed on every UNSTABLE tick, and `stable()` goes false for things that are not world
    /// edges -- the post-death dwell reset (the seed runs `death_link=true`; three local deaths at
    /// 14:01:36 / 14:02:33 / 14:04:19) and the talk-script inventory window at a merchant. So the
    /// log shows FIVE parks inside world epoch 4 with no world edge whatsoever between them
    /// (13:59:11, 13:59:26, 13:59:34, 14:00:19, 14:00:35 -- 15 popups in 92 s, ~9.8/min), and the
    /// same shape in epochs 8 and 10 (three parks, one edge each).
    ///
    /// The negative control is in the same log: between epochs the parking DOES hold. 14:04:52 ->
    /// 14:11:38 is 6m46s of silence inside one epoch. And session B, same blind good but no deaths,
    /// cost 3 popups total.
    ///
    /// Encoded below as the measured per-epoch park counts for epochs 4..12, driven by
    /// death/talk-shaped unstable stretches that do NOT bump the world epoch. Pre-fix (re-arm on
    /// every unstable tick, spelled out here as the explicit `rearm_grant_stalls()` the old code
    /// path performed) reproduces the wild 42; post-fix each epoch costs exactly one bounded burst.
    #[test]
    fn wild_20260826_technoforge_deathlink_rearm_cadence_replay() {
        // Parks the log actually recorded, per world epoch, over 13:58:01 -> 14:13:01.
        // epoch 4 is the peak stretch (five parks, no intervening edge); epochs 8 and 10 are the
        // same shape at three; the rest are a single burst each.
        //
        // The three multi-park stretches are the ones the forensics names outright: epoch 4 with
        // five, epochs 8 and 10 with three each. The remaining three parks are single bursts in the
        // epochs around them (the first, 13:58:01, precedes the 13:59:03 edge into epoch 4). The
        // sum is the measured 14; the parked-epoch COUNT, six, is what the fix reduces the flood to.
        const WILD_PARKS_PER_EPOCH: [(u64, usize); 6] =
            [(3, 1), (4, 5), (6, 1), (8, 3), (10, 3), (11, 1)];

        // One epoch's worth of play: `unstable_stretches` death/merchant windows, each a few ticks
        // of `stable() == false` with `flags_ready()` still true and the world epoch UNMOVED,
        // separated by stable stretches long enough to spend a whole allowance if one is armed.
        fn drive_epoch(
            r: &mut Reconciler,
            g: &mut MockGame,
            unstable_stretches: usize,
            legacy_rearm_on_unstable_tick: bool,
        ) -> usize {
            let mut parks = 0usize;
            for _ in 0..unstable_stretches {
                // The death / talk-script window. NOT a world edge: `set_settled(false)` is the
                // dwell reset a respawn causes and `set_inventory_safe(false)` is the merchant talk
                // window; neither rebuilds the world, so neither moves the epoch.
                g.set_settled(false);
                g.set_inventory_safe(false);
                for _ in 0..4 {
                    let out = r.tick(g, TickBudget::default());
                    // PRE-FIX BEHAVIOUR, verbatim: `tick` cleared the stall set on any tick that
                    // was not fully stable.
                    if legacy_rearm_on_unstable_tick {
                        r.rearm_grant_stalls();
                    }
                    parks += out.newly_stalled.len();
                }
                g.set_inventory_safe(true);
                g.set_settled(true);
                for _ in 0..40 {
                    let out = r.tick(g, TickBudget::default());
                    parks += out.newly_stalled.len();
                }
            }
            parks
        }

        // ---- PRE-FIX: the wild 42 popups, reproduced ----
        let mut g = MockGame::stable();
        g.refuse_unique_adds = true; // accepted, never observable -- the whetblade's failure mode
        let mut r = Reconciler::new(rune_inputs());
        let mut wild_parks = 0usize;
        for (epoch, stretches) in WILD_PARKS_PER_EPOCH {
            // The world edge into this epoch (a death reload / warp arrival).
            g.set_stable(false);
            r.tick(&mut g, TickBudget::default());
            g.set_stable(true);
            let _ = epoch;
            wild_parks += drive_epoch(&mut r, &mut g, stretches, true);
        }
        let wild_total: usize = WILD_PARKS_PER_EPOCH.iter().map(|(_, n)| n).sum();
        assert_eq!(
            wild_total, 14,
            "the log records 14 parks across epochs 4..12"
        );
        assert_eq!(
            wild_parks, wild_total,
            "PRE-FIX: one park per unstable stretch -- the measured 14, not one per epoch"
        );
        assert_eq!(
            wild_parks * (MAX_GRANT_ATTEMPTS as usize),
            42,
            "PRE-FIX: 14 bounded bursts of 3 is the 42 popups TechnoForge counted in 15 minutes"
        );

        // ---- POST-FIX: one bounded burst per epoch ----
        let mut g = MockGame::stable();
        g.refuse_unique_adds = true;
        let mut r = Reconciler::new(rune_inputs());
        let mut per_epoch: Vec<usize> = Vec::new();
        let mut fixed_grants_before = 0usize;
        for (epoch, stretches) in WILD_PARKS_PER_EPOCH {
            g.set_stable(false);
            r.tick(&mut g, TickBudget::default());
            g.set_stable(true);
            let parks = drive_epoch(&mut r, &mut g, stretches, false);
            per_epoch.push(parks);
            let spent = g.unique_grant_calls.len() - fixed_grants_before;
            fixed_grants_before = g.unique_grant_calls.len();
            assert_eq!(
                spent, MAX_GRANT_ATTEMPTS as usize,
                "epoch {epoch}: exactly one allowance per world edge, not {spent}"
            );
        }
        assert_eq!(
            per_epoch,
            vec![1; WILD_PARKS_PER_EPOCH.len()],
            "FIX: every epoch parks ONCE -- epoch 4's five parks collapse to one"
        );
        assert_eq!(
            per_epoch.iter().sum::<usize>() * (MAX_GRANT_ATTEMPTS as usize),
            18,
            "FIX: 6 parked epochs x 3 popups = 18 over the same 15 minutes, down from 42 -- and \
             the within-epoch rate falls from 15 popups in 92 s to 3, which is the documented \
             worst case of one bounded burst per world edge"
        );

        // The negative control the same log supplies: 6m46s of silence inside ONE epoch. A long
        // stable stretch with no world edge must cost NOTHING once the good is parked.
        let quiet_before = g.unique_grant_calls.len();
        for _ in 0..2000 {
            let out = r.tick(&mut g, TickBudget::default());
            assert!(
                out.newly_stalled.is_empty(),
                "no re-park may happen inside an epoch"
            );
        }
        assert_eq!(
            g.unique_grant_calls.len(),
            quiet_before,
            "parking must HOLD for the rest of the epoch (14:04:52 -> 14:11:38 was silent)"
        );

        // And the guard must not have gone permanent: the next world edge still re-arms, which is
        // what let the whetblade heal at the epoch-11/12 reload instead of being abandoned.
        g.set_stable(false);
        r.tick(&mut g, TickBudget::default());
        g.set_stable(true);
        assert!(
            r.stalled_goods().is_empty(),
            "a real world edge must still hand back a full allowance"
        );
    }

    // ---- possession = bag lists UNION the great-rune EQUIP SLOT UNION the STORAGE BOX ----

    /// Which OUT-OF-BAG stores [`PossessionGame`]'s readback consults. Flipping one to `false` is
    /// how the two acceptance tests below VERIFY BY BREAKING: it drops exactly that term from the
    /// predicate and changes nothing else about the timeline.
    #[derive(Clone, Copy)]
    struct PossessionReads {
        equip_slot: bool,
        storage: bool,
    }

    /// A [`GameIo`] that models the live possession readback as the THREE STORES it really is: the
    /// bag lists (the inner [`MockGame`]'s goods set, which is all
    /// `reconcile_io::inventory_has_goods` walked before 2026-08-02), the GREAT-RUNE EQUIP SLOT,
    /// and the STORAGE BOX.
    ///
    /// [`Self::equip_great_rune`] and [`Self::store_good`] are each the whole hypothesis in three
    /// lines: the row moves OUT of every bag list, and the game keeps counting the good possessed,
    /// so a re-grant is REFUSED -- accepted by `AddItemFunc`, landing nothing, indistinguishable
    /// from success to the client. Nothing else in the mock has to know about it.
    struct PossessionGame {
        inner: MockGame,
        /// The goods row in the great-rune equip slot. `None` == nothing equipped, which must read
        /// as ABSENT rather than matching anything.
        equipped_great_rune: Option<GoodsId>,
        /// The goods rows sitting in the storage box.
        stored_goods: BTreeSet<GoodsId>,
        reads: PossessionReads,
    }

    impl PossessionGame {
        fn new(reads: PossessionReads) -> Self {
            PossessionGame {
                inner: MockGame::stable(),
                equipped_great_rune: None,
                stored_goods: BTreeSet::new(),
                reads,
            }
        }

        /// The player action at the Roundtable grace, 2026-07-29 ~01:36:10: equip the rune.
        fn equip_great_rune(&mut self, goods: GoodsId) {
            self.inner.drop_good(goods);
            self.equipped_great_rune = crate::great_runes::canonical_restored_row(goods);
            self.inner.refuse_unique_adds = true;
        }

        /// The player puts the good in the STORAGE BOX. Modelled exactly like equipping, and for
        /// the same reason: the row leaves every bag list while the game still counts the good
        /// owned, so a re-add is refused ("the maximum allowed in inventory" -- the message
        /// `keyitems.rs` already notes for a double-granted rune) and the client cannot tell that
        /// refusal from a success.
        fn store_good(&mut self, goods: GoodsId) {
            self.inner.drop_good(goods);
            self.stored_goods.insert(goods);
            self.inner.refuse_unique_adds = true;
        }
    }

    impl GameIo for PossessionGame {
        fn stability(&self) -> WorldStability {
            self.inner.stability()
        }
        fn get_flag(&self, f: FlagId) -> bool {
            self.inner.get_flag(f)
        }
        fn set_flag(&mut self, f: FlagId, on: bool) -> bool {
            self.inner.set_flag(f, on)
        }
        fn has_good(&self, g: GoodsId) -> bool {
            // THE PREDICATE UNDER TEST, mirroring `reconcile_io::inventory_has_goods`: bag lists
            // UNION the great-rune equip slot UNION the storage box. Drop either out-of-bag term
            // (that is what `PossessionReads` is for) and the matching acceptance test below reds.
            self.inner
                .goods
                .iter()
                .any(|&row| crate::great_runes::possession_row_satisfies(g, row))
                || (self.reads.equip_slot
                    && self
                        .equipped_great_rune
                        .is_some_and(|row| crate::great_runes::possession_row_satisfies(g, row)))
                || (self.reads.storage
                    && self
                        .stored_goods
                        .iter()
                        .any(|&row| crate::great_runes::possession_row_satisfies(g, row)))
        }
        fn grant_good(&mut self, g: GoodsId, comp: &[FlagId]) -> bool {
            self.inner.grant_good(g, comp)
        }
        fn grant_ledgered(&mut self, full_id: GoodsId, qty: i32) -> bool {
            self.inner.grant_ledgered(full_id, qty)
        }
    }

    /// Morgott's Great Rune exactly as the live mapper builds it (`core.rs` -> item map
    /// -> `KeyItem` via `keyitems::acquire_flags`): boss-drop goods row 8150 AS THE SEED SENDS IT,
    /// plus restored flag 193. Delivery deliberately does NOT rewrite to the restored row
    /// (clients#392 -- the restored row cannot be AddItem'd; the rewrite was the INERT-grant bug).
    fn morgott_rune_inputs() -> DesiredInputs {
        great_rune_inputs("Morgott's Great Rune", 8150, 193)
    }

    fn great_rune_inputs(name: &str, goods: GoodsId, restored_flag: FlagId) -> DesiredInputs {
        DesiredInputs {
            seed: SEED.into(),
            save: SaveIdentity("slot0".into()),
            received: vec![ReceivedItem {
                index: 0,
                name: name.into(),
                semantics: ItemSemantics::KeyItem {
                    goods,
                    obtained_flags: vec![restored_flag],
                },
            }],
            slot_data: SlotData::default(),
        }
    }

    /// THE ACCEPTANCE TEST for `possession = bag lists UNION great-rune equip slot`
    /// (`reconcile_io::inventory_has_goods`, 2026-08-02).
    ///
    /// The motivating case, in the order the 2026-07-29 player log records it: the rune is RECEIVED
    /// at 01:32:53 and granted -- it reads back present at 01:32:54, 01:33:33 and 01:34:37, so the
    /// grant path is fine and nine earlier Roundtable warps were clean. The player then EQUIPS it at
    /// a Roundtable grace around 01:36:10: live, no load screen, no world edge, no input to the
    /// reconciler. From that instant the readback says absent and the client re-grants at ~6.6/s for
    /// the rest of the evening -- 4525 grants, six sessions, six CTDs, two lost saves.
    ///
    /// Both arms drive the SAME reconciler over the SAME timeline and differ ONLY in the readback:
    ///
    /// * bag-only (PRE-FIX): the good goes blind, the re-grant is refused, and it is only
    ///   `MAX_GRANT_ATTEMPTS` that stops a flood -- at the cost of PARKING a rune the player is
    ///   visibly wearing;
    /// * bag UNION equip slot (FIX): the equipped rune reads as possessed, the diff is empty, and
    ///   the grant count stays at the ONE grant that actually landed.
    ///
    /// 🛑 Scope, stated so nobody over-reads a green: the mechanism (equipping detaches the row) is
    /// UNCONFIRMED IN GAME, and the real predicate lives in the Windows-only `eldenring-archipelago`
    /// crate, which cannot be built or tested on a host. What this proves is the half that IS
    /// testable and that the flood actually turned on: GIVEN a readback that includes the equip
    /// slot, the reconciler emits zero re-grants for an equipped rune.
    #[test]
    fn an_equipped_great_rune_reads_as_possessed_and_never_re_grants_replay() {
        // Delivery desires the boss-drop row (clients#392); equipping moves it to the slot, whose
        // canonical identity is the restored row (193) -- satisfied via the symmetric family rule.
        const MORGOTT: GoodsId = 8150;
        const TICKS_AFTER_EQUIP: usize = 400;

        for reads_equip_slot in [false, true] {
            let mut g = PossessionGame::new(PossessionReads {
                equip_slot: reads_equip_slot,
                storage: true,
            });
            let mut r = Reconciler::new(morgott_rune_inputs());

            // 01:32:53 -- received, granted, observable in the bag.
            r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
            assert_eq!(
                g.inner.unique_grant_calls.len(),
                1,
                "the rune is granted exactly once while it is still in the bag"
            );
            assert!(g.has_good(MORGOTT), "and reads back present, either way");

            // ~01:36:10 -- the player equips it. The ONLY thing that changes is where the row lives.
            g.equip_great_rune(MORGOTT);
            assert!(
                !g.inner.has_good(MORGOTT),
                "equipping detaches the row from every bag list -- the modelled mechanism"
            );
            assert!(
                g.stored_goods.is_empty(),
                "the equip slot is the ONLY out-of-bag store in play here"
            );

            let mut last = TickOutcome::default();
            for _ in 0..TICKS_AFTER_EQUIP {
                last = r.tick(&mut g, TickBudget::default());
            }
            let regrants = g.inner.unique_grant_calls.len() - 1;

            if reads_equip_slot {
                assert_eq!(
                    regrants, 0,
                    "FIX: an equipped rune is POSSESSED -- zero re-grants over {TICKS_AFTER_EQUIP} \
                     ticks, not {regrants}"
                );
                assert!(
                    r.stalled_goods().is_empty(),
                    "FIX: nothing is parked, so the client never warns about a rune the player is \
                     wearing"
                );
                assert!(
                    last.converged && last.applied.is_empty(),
                    "FIX: the tick sits converged with nothing to do"
                );
            } else {
                assert_eq!(
                    regrants, MAX_GRANT_ATTEMPTS as usize,
                    "PRE-FIX: a bag-only readback MUST go blind and re-grant -- the wild bug is \
                     reproduced, bounded only by the stall guard"
                );
                assert!(
                    r.stalled_goods().contains(&MORGOTT),
                    "PRE-FIX: the backstop parks a rune the player is holding -- a bound, not a fix"
                );
            }
        }
    }

    /// Regression for client#313, re-based on #392: delivery now desires the BOSS-DROP row, and a
    /// save already carrying the RESTORED row (a vanilla Divine-Tower visit on a hybrid save, or a
    /// pre-AP save) must suppress the grant whether that row is in the bag or storage -- the two
    /// families satisfy each other symmetrically.
    #[test]
    fn restored_great_rune_in_bag_or_storage_satisfies_boss_row_delivery() {
        const MOHG_BOSS_ROW: GoodsId = 8152;
        const MOHG_RESTORED_ROW: GoodsId = 195;
        const MOHG_RESTORED_FLAG: FlagId = 195;

        for in_storage in [false, true] {
            let mut g = PossessionGame::new(PossessionReads {
                equip_slot: true,
                storage: true,
            });
            if in_storage {
                g.stored_goods.insert(MOHG_RESTORED_ROW);
            } else {
                g.inner.goods.insert(MOHG_RESTORED_ROW);
            }
            g.inner.flags.insert(MOHG_RESTORED_FLAG, true);
            let mut r = Reconciler::new(great_rune_inputs(
                "Mohg's Great Rune",
                MOHG_BOSS_ROW,
                MOHG_RESTORED_FLAG,
            ));

            let out = r.tick(&mut g, TickBudget::default());

            assert!(
                g.has_good(MOHG_BOSS_ROW),
                "the restored row satisfies the boss-row desire"
            );
            assert!(g.inner.unique_grant_calls.is_empty());
            assert!(r.stalled_goods().is_empty());
            assert!(out.converged && out.applied.is_empty());
        }
    }

    /// Exact regression for the Rykard trace attached to client#313. Client 0.5.2 emitted one
    /// three-attempt burst on the initial load and another after each of 18 warps because row 194
    /// did not satisfy the seed's row 8151 desire. Rebuilding the reconciler models the per-load
    /// stall-guard reset; every load must instead converge without making even one grant call.
    #[test]
    fn rykard_restored_row_survives_repeated_loads_without_regranting() {
        const RYKARD_BOSS_ROW: GoodsId = 8151;
        const RYKARD_RESTORED_ROW: GoodsId = 194;
        const RYKARD_RESTORED_FLAG: FlagId = 194;

        let mut g = PossessionGame::new(PossessionReads {
            equip_slot: true,
            storage: true,
        });
        g.inner.goods.insert(RYKARD_RESTORED_ROW);
        g.inner.flags.insert(RYKARD_RESTORED_FLAG, true);

        for load in 0..19 {
            let mut r = Reconciler::new(great_rune_inputs(
                "Rykard's Great Rune",
                RYKARD_BOSS_ROW,
                RYKARD_RESTORED_FLAG,
            ));
            let out = r.tick(&mut g, TickBudget::default());

            assert!(
                out.converged && out.applied.is_empty(),
                "load {load}: restored Rykard row must satisfy the boss-row desire"
            );
            assert!(
                g.inner.unique_grant_calls.is_empty(),
                "load {load}: no grant burst may be armed"
            );
            assert!(
                r.stalled_goods().is_empty(),
                "load {load}: a held rune must never reach the stall guard"
            );
        }
    }

    /// The #316 backfill is DEAD (clients#392): it existed to migrate a legacy boss-row-only save
    /// to the restored row delivery then desired. Delivery desires the boss row again, so a legacy
    /// boss-row save satisfies it outright -- zero grants, nothing parked. (Granting the restored
    /// row was never even effective: AddItem swallows it, which was Corni's INERT loop.)
    #[test]
    fn a_legacy_boss_row_satisfies_delivery_with_no_backfill() {
        const MOHG_BOSS_ROW: GoodsId = 8152;
        const MOHG_RESTORED_FLAG: FlagId = 195;

        for boss_in_storage in [false, true] {
            let mut g = PossessionGame::new(PossessionReads {
                equip_slot: true,
                storage: true,
            });
            if boss_in_storage {
                g.stored_goods.insert(MOHG_BOSS_ROW);
            } else {
                g.inner.goods.insert(MOHG_BOSS_ROW);
            }
            g.inner.flags.insert(MOHG_RESTORED_FLAG, true);
            let mut r = Reconciler::new(great_rune_inputs(
                "Mohg's Great Rune",
                MOHG_BOSS_ROW,
                MOHG_RESTORED_FLAG,
            ));

            let out = r.tick(&mut g, TickBudget::default());
            assert!(
                g.inner.unique_grant_calls.is_empty(),
                "the legacy boss row IS the desired row now -- no backfill, no grant at all"
            );
            assert!(r.stalled_goods().is_empty());
            assert!(out.converged && out.applied.is_empty());
        }
    }
    /// The Rold Medallion exactly as the live mapper builds it (`core.rs` -> `KeyItem` via
    /// `keyitems::acquire_flags`): goods row 9000 with obtained flag 400001, the same shape the
    /// invariance corpus above uses. Deliberately NOT a Great Rune -- the storage term guards the
    /// whole presence-diffed class, not one item.
    fn rold_medallion_inputs() -> DesiredInputs {
        DesiredInputs {
            seed: SEED.into(),
            save: SaveIdentity("slot0".into()),
            received: vec![ReceivedItem {
                index: 0,
                name: "Rold Medallion".into(),
                semantics: ItemSemantics::KeyItem {
                    goods: 9000,
                    obtained_flags: vec![400001],
                },
            }],
            slot_data: SlotData::default(),
        }
    }

    /// THE ACCEPTANCE TEST for the STORAGE half of `possession = bag lists UNION great-rune equip
    /// slot UNION storage box` (`reconcile_io::inventory_has_goods`, 2026-08-02, on Alaric's
    /// instruction -- it REVERSES the exclusion the first commit on this branch argued for).
    ///
    /// A good visible ONLY in the storage box must read as POSSESSED and produce ZERO re-grants.
    /// Same shape as the equipped-rune case above and for the same reason: the row leaves every bag
    /// list while the game still counts the good owned, so a readback that skips the box goes blind
    /// and the diff re-emits `GrantUnique` forever. Both arms drive the SAME reconciler over the
    /// SAME timeline and differ ONLY in whether the readback consults the box.
    ///
    /// 🛑 Three things a green here does NOT prove, said plainly so nobody over-reads it:
    ///
    /// * the real predicate lives in the Windows-only `eldenring-archipelago` crate, which cannot
    ///   be built or tested on a host. This pins the LOGIC given such a readback, nothing more;
    /// * this state may not be reachable in game at all. Everything in
    ///   `DesiredInputs::unique_goods` today -- key items, Great Runes, OWNED progressive rungs --
    ///   is a KEY item, and the recorded finding is that the storage box does not take key items.
    ///   So the storage term is a guard for the class, not a fix for the 2026-07-29 flood;
    /// * the trade it buys is real and silent: a good the player deliberately STASHED reads as
    ///   possessed, so it is never re-delivered while it sits in the box. What that replaces is the
    ///   `PRE-FIX` arm below -- an unbounded re-grant loop, bounded only by `MAX_GRANT_ATTEMPTS`
    ///   parking a good the player owns.
    #[test]
    fn a_good_only_in_the_storage_box_reads_as_possessed_and_never_re_grants_replay() {
        const ROLD: GoodsId = 9000;
        const TICKS_AFTER_STORING: usize = 400;

        for reads_storage in [false, true] {
            let mut g = PossessionGame::new(PossessionReads {
                equip_slot: true,
                storage: reads_storage,
            });
            let mut r = Reconciler::new(rold_medallion_inputs());

            // Received, granted, observable in the bag.
            r.run_to_fixpoint(&mut g, TickBudget::default(), 8);
            assert_eq!(
                g.inner.unique_grant_calls.len(),
                1,
                "the key item is granted exactly once while it is still in the bag"
            );
            assert!(g.has_good(ROLD), "and reads back present, either way");

            // The player boxes it. The ONLY thing that changes is where the row lives.
            g.store_good(ROLD);
            assert!(
                !g.inner.has_good(ROLD),
                "storing detaches the row from every bag list -- the modelled mechanism"
            );
            assert!(
                g.equipped_great_rune.is_none(),
                "the storage box is the ONLY out-of-bag store in play here"
            );

            let mut last = TickOutcome::default();
            for _ in 0..TICKS_AFTER_STORING {
                last = r.tick(&mut g, TickBudget::default());
            }
            let regrants = g.inner.unique_grant_calls.len() - 1;

            if reads_storage {
                assert_eq!(
                    regrants, 0,
                    "FIX: a good in the storage box is POSSESSED -- zero re-grants over \
                     {TICKS_AFTER_STORING} ticks, not {regrants}"
                );
                assert!(
                    r.stalled_goods().is_empty(),
                    "FIX: nothing is parked, so the client never warns about a good the player is \
                     keeping in the box"
                );
                assert!(
                    last.converged && last.applied.is_empty(),
                    "FIX: the tick sits converged with nothing to do"
                );
            } else {
                assert_eq!(
                    regrants, MAX_GRANT_ATTEMPTS as usize,
                    "PRE-FIX: a readback that skips the storage box MUST go blind and re-grant -- \
                     bounded only by the stall guard"
                );
                assert!(
                    r.stalled_goods().contains(&ROLD),
                    "PRE-FIX: the backstop parks a good the player owns -- a bound, not a fix"
                );
            }
        }
    }
}
