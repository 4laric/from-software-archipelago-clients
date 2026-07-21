//! `marker_replay` — headless timeline replay proving the [`crate::marker`] save-embedded reconcile
//! record behaves correctly across the transitions the live client hits: fresh init, same-identity
//! resume (no re-grant), a crash that commits the cursor BEHIND the grants, a restored save backup,
//! a brand-new character in a reused slot, a torn double-buffer write, legacy migration, and an
//! identity-mismatch REFUSE. It drives the REAL [`crate::reconcile::Reconciler`] against the in-memory
//! [`crate::reconcile::MockGame`], so the marker codec + the init wiring are proven on any host — only
//! the real flag-band audit and the Windows persist verify remain (they cannot be host-tested).
//!
//! Sibling of [`crate::reconciler_replay`] / [`crate::flask_grant_replay`]: the decision logic lives in
//! [`crate::marker`]; the pure init wiring is [`reconciler_for`]; the timelines are the tests.

use crate::marker::InitDecision;
use crate::reconcile::{DesiredInputs, ItemIndex, Reconciler};

/// The SESSION-INIT wiring the Windows glue will use: turn a [`marker::decide`] result into a
/// reconciler, or `None` on REFUSE.
///
/// * [`InitDecision::Resume`] → resume from the save's own watermark ([`Reconciler::from_persisted`]).
/// * [`InitDecision::Fresh`] → adopt a pre-minibake `reconcile.json` watermark ONCE if one exists
///   (the [`crate::reconcile::legacy_adopt`] migration), else a truly fresh [`Reconciler::new`]. Either
///   way the caller then [`marker::commit`]s a marker, and the legacy file is never consulted again.
/// * [`InitDecision::Refuse`] → `None`: the caller MUST gate the WHOLE pipeline (flag poll, check
///   detection, shop rewrites) and disconnect with a reason — NOT run a reconciler, NOT commit a
///   marker (never mutate a save we refused).
pub fn reconciler_for(
    decision: InitDecision,
    inputs: DesiredInputs,
    legacy_watermark: Option<ItemIndex>,
) -> Option<Reconciler> {
    match decision {
        InitDecision::Refuse { .. } => None,
        InitDecision::Resume { watermark } => Some(Reconciler::from_persisted(inputs, watermark)),
        InitDecision::Fresh => Some(match legacy_watermark {
            Some(wm) => Reconciler::from_persisted(inputs, wm),
            None => Reconciler::new(inputs),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marker::{self, identity_hash, FlagBand};
    use crate::reconcile::{
        DesiredInputs, GameIo, GoodsId, ItemSemantics, MockGame, ReceivedItem, Reconciler,
        SaveIdentity, SlotData, StartItem, TickBudget,
    };

    const BAND: FlagBand = FlagBand::PLACEHOLDER;
    const SEED: &str = "ROOMSEED-minibake";
    const SLOT: &str = "Alaric";

    fn id() -> u32 {
        identity_hash(SEED, SLOT)
    }

    fn consumable(index: ItemIndex, full_id: GoodsId) -> ReceivedItem {
        ReceivedItem {
            index,
            name: format!("consumable-{index}"),
            semantics: ItemSemantics::Consumable {
                full_id,
                qty: 1,
                echo_skip: false,
            },
        }
    }

    use crate::reconcile::FlagId;

    fn inputs(received: Vec<ReceivedItem>, start_items: Vec<StartItem>) -> DesiredInputs {
        DesiredInputs {
            seed: SEED.into(),
            save: SaveIdentity("slot0".into()),
            received,
            slot_data: SlotData {
                start_items,
                ..Default::default()
            },
        }
    }

    fn drive(r: &mut Reconciler, g: &mut MockGame) {
        r.run_to_fixpoint(g, TickBudget::default(), 64);
    }

    // (1) Fresh save: floor-seeded, start items + received granted ONCE, marker written.
    #[test]
    fn fresh_save_grants_once_and_writes_marker() {
        let inp = inputs(
            vec![consumable(0, 2001), consumable(1, 2002)],
            vec![StartItem {
                full_id: 1001,
                qty: 1,
            }],
        );
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inp);
        drive(&mut r, &mut g);
        assert_eq!(g.ledger_count(1001), 1, "start item granted once");
        assert_eq!(g.ledger_count(2001), 1);
        assert_eq!(g.ledger_count(2002), 1);
        assert_eq!(r.applied_watermark(), 2);

        assert!(marker::commit(&mut g, BAND, id(), r.applied_watermark()));
        assert_eq!(
            marker::read(&g, BAND),
            marker::MarkerRead::Present {
                identity: id(),
                watermark: 2
            }
        );
    }

    // (2) Same-identity resume: from the save's own marker, NOTHING re-grants.
    #[test]
    fn resume_same_identity_does_not_regrant() {
        let inp = inputs(
            vec![consumable(0, 2001), consumable(1, 2002)],
            vec![StartItem {
                full_id: 1001,
                qty: 1,
            }],
        );
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inp.clone());
        drive(&mut r, &mut g);
        assert!(marker::commit(&mut g, BAND, id(), r.applied_watermark()));

        // Reconnect: build the reconciler purely from what the SAVE says.
        let decision = marker::decide(marker::read(&g, BAND), id());
        assert_eq!(decision, InitDecision::Resume { watermark: 2 });
        let mut r2 = reconciler_for(decision, inp, None).expect("resume");
        drive(&mut r2, &mut g);

        assert_eq!(g.ledger_count(1001), 1, "no start-item re-grant");
        assert_eq!(g.ledger_count(2001), 1, "no consumable re-grant");
        assert_eq!(g.ledger_count(2002), 1);
    }

    // (3) Crash with the cursor committed BEHIND the grants: only the tail past the cursor replays.
    #[test]
    fn crash_cursor_behind_grants_replays_only_the_tail() {
        let inp = inputs(vec![consumable(0, 2001), consumable(1, 2002)], vec![]);
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inp.clone());
        drive(&mut r, &mut g);
        assert_eq!(r.applied_watermark(), 2);
        // Simulate a crash that flushed the cursor after idx0 but before idx1 (cursor = 1, grants = 2).
        assert!(marker::commit(&mut g, BAND, id(), 1));

        let decision = marker::decide(marker::read(&g, BAND), id());
        assert_eq!(decision, InitDecision::Resume { watermark: 1 });
        let mut r2 = reconciler_for(decision, inp, None).unwrap();
        drive(&mut r2, &mut g);

        assert_eq!(g.ledger_count(2001), 1, "idx0 (behind cursor) not replayed");
        assert_eq!(
            g.ledger_count(2002),
            2,
            "idx1 (the tail) replayed exactly once — bounded dup"
        );
    }

    // (4) Restored save backup: because the marker lives IN the save, the cursor rewinds WITH the
    // inventory, so the replayed tail lands exactly once — no strand, no dup. Contrast (3): there the
    // external-style stale cursor sat behind an inventory that STILL held idx1, so replay DOUBLED it;
    // here the backup rewound cursor AND inventory together, the coherence an external reconcile.json
    // could never give (its file cursor would sit stale-HIGH and strand idx1).
    #[test]
    fn restored_backup_rewinds_cursor_with_inventory() {
        let inp = inputs(vec![consumable(0, 2001), consumable(1, 2002)], vec![]);
        let mut g = MockGame::stable();
        let mut r = Reconciler::new(inp.clone());
        drive(&mut r, &mut g);
        assert_eq!(r.applied_watermark(), 2);
        assert!(marker::commit(&mut g, BAND, id(), 2));

        // The save as it was at cursor=1: idx1 had not landed. Rewind BOTH the inventory and the
        // in-save cursor to that point — a coherent backup.
        let mut restored = g.clone();
        restored.ledger_log.retain(|&(fid, _)| fid != 2002);
        assert!(marker::commit(&mut restored, BAND, id(), 1));

        let decision = marker::decide(marker::read(&restored, BAND), id());
        assert_eq!(decision, InitDecision::Resume { watermark: 1 });
        let mut r2 = reconciler_for(decision, inp, None).unwrap();
        drive(&mut r2, &mut restored);

        assert_eq!(
            restored.ledger_count(2001),
            1,
            "idx0 still present from the backup"
        );
        assert_eq!(
            restored.ledger_count(2002),
            1,
            "tail replayed exactly once — not stranded, not doubled"
        );
    }

    // (5) A brand-new character in a reused slot: an empty flag store reads Absent -> Fresh, so start
    // items are granted -- with NO play_time heuristic (the er-startitems-newchar-no-regrant case).
    #[test]
    fn new_character_same_slot_is_fresh() {
        let inp = inputs(
            vec![],
            vec![StartItem {
                full_id: 1001,
                qty: 1,
            }],
        );
        let mut g = MockGame::stable(); // brand-new character: no marker
        let decision = marker::decide(marker::read(&g, BAND), id());
        assert_eq!(decision, InitDecision::Fresh);
        let mut r = reconciler_for(decision, inp, None).unwrap();
        drive(&mut r, &mut g);
        assert_eq!(
            g.ledger_count(1001),
            1,
            "fresh char gets its own start items"
        );
        assert!(marker::commit(&mut g, BAND, id(), r.applied_watermark()));
    }

    // (6) A torn double-buffer write (crash after writing the inactive register, before the SEL flip)
    // resumes the last COMMITTED cursor, never a garbage one.
    #[test]
    fn torn_cursor_write_resumes_committed_value() {
        let mut g = MockGame::stable();
        assert!(marker::commit(&mut g, BAND, id(), 10)); // register A active
                                                         // Scramble the inactive register (B) without flipping SEL — the interrupted write.
        for bit in 0..32u32 {
            let flag: FlagId = BAND.base + 66 + bit; // CUR_B
            g.set_flag(flag, bit % 3 == 0);
        }
        let decision = marker::decide(marker::read(&g, BAND), id());
        assert_eq!(decision, InitDecision::Resume { watermark: 10 });
    }

    // (7) Legacy migration: no marker + a reconcile.json watermark -> adopt it ONCE, write a marker,
    // and thereafter the marker is authoritative (the file is never consulted again).
    #[test]
    fn legacy_watermark_is_adopted_once_then_marker_takes_over() {
        let inp = inputs(vec![consumable(0, 2001), consumable(1, 2002)], vec![]);
        let mut g = MockGame::stable(); // no marker yet
        let decision = marker::decide(marker::read(&g, BAND), id());
        assert_eq!(decision, InitDecision::Fresh);

        // The old path already granted through index 0; adopt that as the floor.
        let mut r = reconciler_for(decision, inp.clone(), Some(1)).unwrap();
        drive(&mut r, &mut g);
        assert_eq!(
            g.ledger_count(2001),
            0,
            "adopted watermark: idx0 not re-granted"
        );
        assert_eq!(g.ledger_count(2002), 1, "only the un-granted tail lands");
        assert!(marker::commit(&mut g, BAND, id(), r.applied_watermark()));

        // Next reconnect resumes from the marker alone — no legacy file needed.
        assert_eq!(
            marker::decide(marker::read(&g, BAND), id()),
            InitDecision::Resume { watermark: 2 }
        );
    }

    // (8) Identity mismatch REFUSES: no reconciler, and the save is left untouched (we never commit).
    #[test]
    fn identity_mismatch_refuses_and_does_not_touch_the_save() {
        let mut g = MockGame::stable();
        let stored = identity_hash("SOME-OTHER-SEED", SLOT);
        assert!(marker::commit(&mut g, BAND, stored, 7));
        let before = g.flags.clone();

        let decision = marker::decide(marker::read(&g, BAND), id());
        assert_eq!(
            decision,
            InitDecision::Refuse {
                stored,
                expected: id()
            }
        );
        let refused = reconciler_for(decision, inputs(vec![], vec![]), None);
        assert!(
            refused.is_none(),
            "refuse -> no reconciler, pipeline gated by caller"
        );
        assert_eq!(g.flags, before, "a refused save is not mutated");
    }

    // (9) Flag holder not ready during a commit: the write fails cleanly and the ACTIVE cursor never
    // regresses; when the holder is ready the cursor commits.
    #[test]
    fn not_ready_write_never_regresses_the_cursor() {
        let mut g = MockGame::stable();
        g.flag_ready = false;
        assert!(!marker::commit(&mut g, BAND, id(), 5));
        assert_eq!(
            marker::read(&g, BAND),
            marker::MarkerRead::Absent,
            "nothing committed"
        );

        g.flag_ready = true;
        assert!(marker::commit(&mut g, BAND, id(), 5));

        g.flag_ready = false;
        assert!(!marker::commit(&mut g, BAND, id(), 9), "update can't land");
        assert_eq!(
            marker::read(&g, BAND),
            marker::MarkerRead::Present {
                identity: id(),
                watermark: 5
            },
            "active cursor held at 5, not regressed or torn"
        );

        g.flag_ready = true;
        assert!(marker::commit(&mut g, BAND, id(), 9));
        assert_eq!(
            marker::read(&g, BAND),
            marker::MarkerRead::Present {
                identity: id(),
                watermark: 9
            }
        );
    }
}
