//! `receive_watermark_replay` — headless multi-connect replay for the RECEIVE two-watermark split.
//!
//! Twin of [`crate::start_grant_replay`] / [`crate::region_lock_replay`], for the reconnect-safety
//! invariant that lives in [`crate::receive`]. The single-connect decision core (name-dispatch vs
//! grant-enqueue, the `AlreadyPushed` short-circuit) is already host-tested inside `receive.rs`; what
//! had no test is the state that only shows up ACROSS CONNECTS: the two watermarks
//! ([`crate::receive::process_received_item`], receive.rs:65) advance *independently* and are
//! *persisted differently*, and that asymmetry is exactly where a reconnect double-grant hides.
//!
//! THE INVARIANT (receive.rs:56-64). `process_received_item` runs two gates per item:
//!   - **NAME-dispatch** (`idx >= dispatched_through`): `dispatched_through` RESETS to 0 each connect,
//!     so the FULL `items_received` stream re-dispatches on every reconnect. This is safe *only*
//!     because the name effects (grace flags, received-name set, progressive's own index dedup) are
//!     IDEMPOTENT — re-running them is a no-op.
//!   - **GRANT-enqueue** (`idx >= pushed_through`): `pushed_through` RESUMES at the persisted
//!     `last_received_index`, so already-granted items are NOT re-enqueued on reconnect.
//!
//! THE FAILURE MODE this tier locks down: if the persisted index is lost / not resumed, a reconnect
//! resets `pushed_through` to 0 and RE-GRANTS the entire stream (double items — the flask/consumable
//! double-grant class). This module models the persistence explicitly — a good reconnect resumes
//! `pushed_through = last_received_index`; the buggy one resets it to 0 — and replays the same
//! items_received stream across a reconnect through the REAL `process_received_item`, tallying grants.
//! It reuses the production decision core; the only new code is the test-side `NetHook` mock.

#[cfg(test)]
mod replay {
    use crate::receive::{process_received_item, GrantAction, NetHook, RecvItem};
    use std::collections::{HashMap, HashSet};

    /// Test double for [`NetHook`] (receive.rs:48). Records the NAME-keyed side effects so the replay
    /// can assert they re-fire every connect (idempotence is the safety contract), and decides
    /// progressiveness from a preset name set — mirroring `receive.rs`'s own `MockHook`.
    #[derive(Default)]
    struct RecordingNetHook {
        /// Every `on_item_received` name, in dispatch order (grows once per connect's replay).
        dispatched: Vec<String>,
        /// Every `progressive_on_item_received(name, ap_index)` call.
        progressed: Vec<(String, i64)>,
        /// Names the mock treats as progressive (so the grant is skipped for them).
        progressive_names: HashSet<String>,
    }

    impl NetHook for RecordingNetHook {
        fn on_item_received(&mut self, name: &str) {
            self.dispatched.push(name.to_string());
        }
        fn progressive_on_item_received(&mut self, name: &str, ap_index: i64) -> bool {
            self.progressed.push((name.to_string(), ap_index));
            self.progressive_names.contains(name)
        }
    }

    /// Illustrative FullIDs / AP ids. The live client resolves the real ids from slot_data; the
    /// harness only needs stable tokens to tally grants through the connect timeline.
    const AP_WEAPON: i64 = 7777;
    const AP_RUNE: i64 = 8888;
    const AP_PROGRESSIVE: i64 = 6666;

    fn maps() -> (HashMap<i64, i64>, HashMap<i64, i64>) {
        // apIdsToItemIds: AP item id -> ER FullID; itemCounts: AP item id -> qty.
        let mut item_map = HashMap::new();
        item_map.insert(AP_WEAPON, 0x4000_2710i64);
        item_map.insert(AP_RUNE, 0x4000_0001i64);
        item_map.insert(AP_PROGRESSIVE, 0x4000_0002i64);
        let mut counts = HashMap::new();
        counts.insert(AP_RUNE, 3);
        (item_map, counts)
    }

    fn item(index: i64, ap_item_id: i64, name: &str) -> RecvItem {
        RecvItem {
            index,
            ap_item_id,
            name: name.to_string(),
            echo_skip: false,
        }
    }

    /// One frame of a multi-connect session timeline.
    #[derive(Clone)]
    enum Ev {
        /// An item arrives on the wire this connect (server re-streams the whole history on connect,
        /// so `Receive`s are re-emitted after every `Connect`).
        Receive {
            index: i64,
            ap_item_id: i64,
            name: String,
        },
        /// A (re)connect. `dispatched_through` ALWAYS resets to 0 (name-dispatch replays the full
        /// stream — the idempotent side). `pushed_through` is set from PERSISTENCE:
        ///   - `persist_index = true`  -> resume at the persisted `last_received_index` (correct).
        ///   - `persist_index = false` -> the persisted index was lost -> reset to 0 (the BUG).
        Connect { persist_index: bool },
    }

    fn receive(index: i64, ap_item_id: i64, name: &str) -> Ev {
        Ev::Receive {
            index,
            ap_item_id,
            name: name.to_string(),
        }
    }

    /// A grant the replay observed (an `Enqueue`). Tallying these across the timeline is how we detect
    /// a reconnect DOUBLE-GRANT: the same `ap_index` appearing in `Enqueue` twice.
    #[derive(Clone, Debug, PartialEq)]
    struct Granted {
        ap_index: i64,
        full_id: i32,
        qty: i32,
    }

    /// Outcome of a full multi-connect replay.
    struct Outcome {
        /// Every `Enqueue` in timeline order (the grant tally — re-grants show as duplicate ap_index).
        grants: Vec<Granted>,
        /// The name-dispatch log (from the mock) — proves names re-dispatch every connect.
        dispatched: Vec<String>,
        /// The progressive-dispatch log (name, ap_index) — proves progressives re-dispatch too.
        progressed: Vec<(String, i64)>,
        /// Every non-`AlreadyPushed`, non-`Enqueue` action, for edge assertions.
        other_actions: Vec<GrantAction>,
        /// Final persisted watermark (what would be written to disk after the run).
        final_pushed: i64,
    }

    impl Outcome {
        /// How many times a given ap_index was ENQUEUED across the whole session. 1 = correct;
        /// >1 = the reconnect double-grant this tier exists to catch.
        fn grant_count(&self, ap_index: i64) -> usize {
            self.grants
                .iter()
                .filter(|g| g.ap_index == ap_index)
                .count()
        }
    }

    /// Replay a multi-connect timeline through the REAL [`process_received_item`], modelling the two
    /// watermarks and their persistence exactly as `net.rs` does:
    ///   - `dispatched_through` is per-connect session state — reset to 0 by every `Connect`.
    ///   - `pushed_through` is backed by `persisted_index` (the on-disk `last_received_index`) — a
    ///     `Connect{persist_index:true}` resumes it, `false` loses it (resets to 0).
    /// The persisted index advances whenever an item is genuinely granted (an `Enqueue`, and — as in
    /// `net.rs` — also on the skip-but-delivered outcomes, since the watermark tracks delivery). We
    /// mirror that by reading `pushed_through` back out of `process_received_item` after each item.
    fn replay(events: &[Ev], hook: &mut RecordingNetHook) -> Outcome {
        let (im, ic) = maps();
        let mut dispatched_through = 0i64;
        let mut pushed_through = 0i64;
        // The persisted `last_received_index` (what survives a disconnect on disk).
        let mut persisted_index = 0i64;

        let mut grants: Vec<Granted> = Vec::new();
        let mut other_actions: Vec<GrantAction> = Vec::new();

        for ev in events {
            match ev {
                Ev::Connect { persist_index } => {
                    // Name-dispatch always replays the full stream this connect.
                    dispatched_through = 0;
                    // Grant watermark is restored from persistence — or lost.
                    pushed_through = if *persist_index { persisted_index } else { 0 };
                }
                Ev::Receive {
                    index,
                    ap_item_id,
                    name,
                } => {
                    let ri = item(*index, *ap_item_id, name);
                    let action = process_received_item(
                        &ri,
                        &mut dispatched_through,
                        &mut pushed_through,
                        &im,
                        &ic,
                        hook,
                    );
                    match &action {
                        GrantAction::Enqueue {
                            full_id,
                            qty,
                            ap_index,
                            ..
                        } => {
                            grants.push(Granted {
                                ap_index: *ap_index,
                                full_id: *full_id,
                                qty: *qty,
                            });
                        }
                        GrantAction::AlreadyPushed => {}
                        other => other_actions.push(other.clone()),
                    }
                    // net.rs persists the advanced watermark after each processed item; anything the
                    // grant path advanced past `pushed_through` is now durable on disk.
                    persisted_index = persisted_index.max(pushed_through);
                }
            }
        }

        Outcome {
            grants,
            dispatched: std::mem::take(&mut hook.dispatched),
            progressed: std::mem::take(&mut hook.progressed),
            other_actions,
            final_pushed: persisted_index,
        }
    }

    /// The full items_received history the server re-streams on every connect (indices 0..=1).
    fn base_stream() -> Vec<Ev> {
        vec![
            receive(0, AP_WEAPON, "Lordsworn's Greatsword"),
            receive(1, AP_RUNE, "Golden Rune [1]"),
        ]
    }

    #[test]
    fn reconnect_redispatches_names_but_does_not_regrant() {
        // THE watermark-split test. Connect, receive two items (both granted, watermark persists to
        // 2), then RECONNECT resuming the persisted index and re-stream the whole history. Names must
        // re-dispatch (idempotent side) while each item is granted exactly ONCE (pushed_through
        // resumed at 2 -> the replay is all AlreadyPushed).
        let mut hook = RecordingNetHook::default();
        let mut timeline = vec![Ev::Connect {
            persist_index: true,
        }];
        timeline.extend(base_stream());
        // Reconnect — resume the persisted watermark, then the server re-streams from index 0.
        timeline.push(Ev::Connect {
            persist_index: true,
        });
        timeline.extend(base_stream());

        let out = replay(&timeline, &mut hook);

        // Each item granted exactly once despite being streamed across two connects.
        assert_eq!(
            out.grant_count(0),
            1,
            "weapon must grant exactly once across the reconnect"
        );
        assert_eq!(
            out.grant_count(1),
            1,
            "rune must grant exactly once across the reconnect"
        );
        assert_eq!(
            out.grants.len(),
            2,
            "no re-grants — the resumed watermark suppresses the replay"
        );
        // ...but the NAME dispatch re-ran the full stream on BOTH connects (idempotent side effects).
        assert_eq!(
            out.dispatched,
            vec![
                "Lordsworn's Greatsword",
                "Golden Rune [1]",
                "Lordsworn's Greatsword",
                "Golden Rune [1]",
            ],
            "names must re-dispatch every connect (dispatched_through resets to 0)"
        );
        assert_eq!(out.final_pushed, 2);
    }

    #[test]
    fn lost_watermark_regrants_on_reconnect() {
        // FAILURE MODE (documents the bug). Same timeline, but the reconnect LOSES the persisted
        // index (persist_index:false) -> pushed_through resets to 0 -> the re-streamed history is
        // enqueued a SECOND time. This is the flask/consumable reconnect double-grant class.
        let mut hook = RecordingNetHook::default();
        let mut timeline = vec![Ev::Connect {
            persist_index: true,
        }];
        timeline.extend(base_stream());
        timeline.push(Ev::Connect {
            persist_index: false,
        }); // watermark lost
        timeline.extend(base_stream());

        let out = replay(&timeline, &mut hook);

        assert_eq!(
            out.grant_count(0),
            2,
            "regression guard: a lost watermark re-grants the weapon on reconnect (documents the bug)"
        );
        assert_eq!(out.grant_count(1), 2, "the rune is double-granted too");
        assert_eq!(
            out.grants.len(),
            4,
            "the whole two-item stream was granted twice"
        );
    }

    #[test]
    fn tail_only_grants_after_a_resumed_reconnect() {
        // A resumed reconnect that then receives NEW items must grant only the new tail — the resumed
        // watermark short-circuits the replayed prefix, the fresh index enqueues once.
        let mut hook = RecordingNetHook::default();
        let mut timeline = vec![Ev::Connect {
            persist_index: true,
        }];
        timeline.extend(base_stream());
        timeline.push(Ev::Connect {
            persist_index: true,
        });
        timeline.extend(base_stream()); // replayed prefix -> AlreadyPushed
        timeline.push(receive(2, AP_WEAPON, "Uchigatana")); // genuinely new tail

        let out = replay(&timeline, &mut hook);

        assert_eq!(out.grant_count(0), 1);
        assert_eq!(out.grant_count(1), 1);
        assert_eq!(
            out.grant_count(2),
            1,
            "the new tail item grants exactly once"
        );
        assert_eq!(out.grants.len(), 3);
        // Name dispatch ran the prefix twice + the tail once.
        assert_eq!(
            out.dispatched,
            vec![
                "Lordsworn's Greatsword",
                "Golden Rune [1]",
                "Lordsworn's Greatsword",
                "Golden Rune [1]",
                "Uchigatana",
            ]
        );
        assert_eq!(out.final_pushed, 3);
    }

    #[test]
    fn progressive_skip_does_not_grant_but_still_advances_across_reconnect() {
        // Edge: a progressive item never Enqueues (SkipProgressive), but its NAME still dispatches
        // every connect and its watermark still advances — so a resumed reconnect must not somehow
        // "recover" it into a grant, and the persisted index still moves past it.
        let mut hook = RecordingNetHook::default();
        hook.progressive_names
            .insert("Progressive Flask of Crimson Tears".to_string());

        let mut timeline = vec![Ev::Connect {
            persist_index: true,
        }];
        timeline.push(receive(
            0,
            AP_PROGRESSIVE,
            "Progressive Flask of Crimson Tears",
        ));
        timeline.push(receive(1, AP_RUNE, "Golden Rune [1]"));
        // Resume + re-stream: the progressive still dispatches, still never grants.
        timeline.push(Ev::Connect {
            persist_index: true,
        });
        timeline.push(receive(
            0,
            AP_PROGRESSIVE,
            "Progressive Flask of Crimson Tears",
        ));
        timeline.push(receive(1, AP_RUNE, "Golden Rune [1]"));

        let out = replay(&timeline, &mut hook);

        assert_eq!(
            out.grant_count(0),
            0,
            "a progressive item never enqueues (skipped both connects)"
        );
        assert_eq!(out.grant_count(1), 1, "the mapped rune grants exactly once");
        // SkipProgressive fires only on the FIRST connect: on the resumed reconnect the item sits
        // below `pushed_through`, so `process_received_item` returns AlreadyPushed before the
        // progressive branch (the NAME dispatch still re-runs — see `progressed` below).
        let skips = out
            .other_actions
            .iter()
            .filter(|a| matches!(a, GrantAction::SkipProgressive))
            .count();
        assert_eq!(
            skips, 1,
            "SkipProgressive fires once; the replay is AlreadyPushed, not a re-skip"
        );
        // The progressive's watermark still advanced, so the mapped item past it isn't stuck.
        assert_eq!(out.final_pushed, 2);
        // `progressed` logs EVERY dispatched item (progressive_on_item_received is called for all of
        // them, returning whether each is progressive), so filter to the progressive item: it
        // re-dispatches once per connect (dispatched_through resets), its own ap_index dedup keeping
        // it idempotent.
        let prog_dispatches = out
            .progressed
            .iter()
            .filter(|(name, _)| name == "Progressive Flask of Crimson Tears")
            .count();
        assert_eq!(
            prog_dispatches, 2,
            "progressive name re-dispatches once per connect"
        );
    }

    #[test]
    fn already_pushed_on_replay_is_the_no_op_that_prevents_the_double_grant() {
        // The mechanism, isolated: on a resumed reconnect, the replayed prefix returns AlreadyPushed
        // for every item below the watermark — the single fact that keeps the grant idempotent.
        let mut hook = RecordingNetHook::default();
        let mut timeline = vec![Ev::Connect {
            persist_index: true,
        }];
        timeline.extend(base_stream());
        timeline.push(Ev::Connect {
            persist_index: true,
        });
        timeline.extend(base_stream());

        // Instrument by replaying and confirming zero *new* grants after the reconnect: total grants
        // equals the number of distinct indices, not the number of times they were streamed.
        let out = replay(&timeline, &mut hook);
        let distinct: HashSet<i64> = out.grants.iter().map(|g| g.ap_index).collect();
        assert_eq!(
            out.grants.len(),
            distinct.len(),
            "every grant is for a distinct index — no AlreadyPushed item leaked into a re-grant"
        );
    }
}
