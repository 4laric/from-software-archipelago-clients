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
                start_items: vec![StartItem { full_id: 3000, qty: 1 }],
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
                    assert!(out.skipped_unstable, "a tick during a load screen must skip");
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
            assert_eq!(run(&evs), want, "permutation {perm:?} diverged from the canonical fixpoint");
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
        assert_eq!(run(&evs), want, "injected load screens changed the fixpoint");
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
        assert_eq!(fp.set_flags, want_flags, "all region/map/rune/key/goal + bulk flags set exactly once");
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
            ProgTier { goods: vec![8101], flags: vec![70001] },
            ProgTier { goods: vec![8102], flags: vec![70002] },
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
                    assert!(out.skipped_unstable, "a tick during a load screen must skip");
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
        assert_eq!(want.goods, [8101i32, 8102].into_iter().collect::<BTreeSet<_>>());
        assert_eq!(want.set_flags, [70001u32, 70002].into_iter().collect::<BTreeSet<_>>());
        assert_eq!(want.ledger, vec![(2919, 1)], "exactly one overflow, never duplicated");

        let mut order: Vec<i64> = (0..3).collect();
        permute(&mut order, 3, &mut |perm| {
            let mut evs = vec![Ev::Load, Ev::Connect];
            for &k in perm {
                evs.push(Ev::Receive(k));
                evs.push(Ev::Receive(k)); // duplicate delivery
                evs.push(Ev::Load); // interleaved load screen
            }
            assert_eq!(run_prog(&evs), want, "progressive perm+dup+load {perm:?} diverged");
        });
    }
}
