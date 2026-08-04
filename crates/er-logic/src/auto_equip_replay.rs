//! `auto_equip_replay` -- headless timeline replay for the TALISMAN slot policy across reconnects.
//! Sibling of the other `*_replay` modules. Issue #342.
//!
//! [`crate::auto_equip::slot_for_accessory`] is already pure and unit-tested. Those tests fire one
//! call. This module adds the dimension they miss and that the replay tier exists for: the
//! reconciler replays the WHOLE received set on every reconnect, so a slot policy is only correct
//! if it is a pure function of things that replay identically.
//!
//! Two bugs live in that timeline and neither is visible in a single call:
//!
//! 1. **The freeze.** `clobber the lowest` makes slot 1 the only slot that ever changes once the
//!    loadout is full, so slots 2/3/4 stick on the 2nd/3rd/4th talismans for the rest of the run.
//! 2. **The varying modulus.** The obvious fix -- port #48's `ordinal % n` from physick -- reads
//!    `n` from the LIVE `unlocked_talisman_slots`, which grows from 1 to 4 as Talisman Pouches are
//!    found. Live it is evaluated against a different modulus than on replay, so the loadout
//!    silently rearranges itself on reconnect. #342 read this as proof the mechanism cannot port.
//!
//! It ports. The Talisman Pouch is itself an AP item, so the slot count can be taken from the
//! player's POSITION IN THE STREAM instead of from live state, which replays identically by
//! construction. [`Policy`] toggles all three so the fix is provable by breaking it (CONTRIBUTING
//! rule 7): `LiveModulus` fails `a_pouch_midstream_does_not_rearrange_the_loadout_on_reconnect`,
//! `ClobberLowest` fails `a_full_loadout_churns_every_slot`, `StreamDerived` passes both.

#[cfg(test)]
mod replay {
    use crate::auto_equip::{
        slot_for_accessory, stream_accessory_slots, usable_accessory_slots, ACCESSORY_SLOTS,
    };

    /// The frames that matter. A reconnect is the whole point of the module.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Ev {
        /// A talisman arrives from AP, identified by its accessory param row.
        Talisman(i32),
        /// A Talisman Pouch arrives from AP and the grant LANDS -- the character earns a slot.
        Pouch,
        /// A Talisman Pouch arrives from AP and the grant does NOT land (the #308 capped-grant
        /// shape). The stream has seen it; the game's field has not moved.
        PouchThatNeverLanded,
        /// Drop the connection and reconnect: the client's counters reset and AP replays the entire
        /// received set, in order, onto whatever the character is currently wearing.
        Reconnect,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Policy {
        /// Shipped today. Rule 3 = the lowest unlocked slot, always.
        ClobberLowest,
        /// The naive port of #48: `ordinal % n`, with `n` read from the live game field.
        LiveModulus,
        /// The fix: `n` counted off the received stream, bounded by the live field.
        StreamDerived,
    }

    /// The character. `game_pouches` is save state -- it survives a reconnect, which is exactly why
    /// it cannot be used as a modulus.
    struct Sim {
        worn: [Option<i32>; 4],
        game_pouches: u8,
        policy: Policy,
        /// Per-CONNECTION counters. Both reset at connect, which is what makes them replayable.
        ordinal: u64,
        pouches_seen: u32,
    }

    impl Sim {
        fn new(policy: Policy) -> Self {
            Sim {
                worn: [None; 4],
                game_pouches: 0,
                policy,
                ordinal: 0,
                pouches_seen: 0,
            }
        }

        /// A fresh connection: the client zeroes its counters before AP replays the received set.
        fn connect(&mut self) {
            self.ordinal = 0;
            self.pouches_seen = 0;
        }

        /// ONE received item. Live this runs once per arrival; on a reconnect it runs again for
        /// every item in the set, in the same order, from ordinal zero.
        fn receive(&mut self, ev: Ev) {
            let param_id = match ev {
                Ev::Pouch | Ev::PouchThatNeverLanded => {
                    self.pouches_seen += 1;
                    return;
                }
                Ev::Talisman(id) => id,
                Ev::Reconnect => unreachable!("a reconnect is not part of a received set"),
            };
            let slot = match self.policy {
                Policy::ClobberLowest => {
                    legacy_clobber_lowest(self.game_pouches, self.worn, param_id)
                }
                // The naive port: modulus from the LIVE field, which is why it does not replay.
                Policy::LiveModulus => slot_for_accessory(
                    self.game_pouches,
                    usable_accessory_slots(self.game_pouches),
                    self.worn,
                    param_id,
                    self.ordinal,
                ),
                // The fix: modulus from the STREAM, bounded by the live field.
                Policy::StreamDerived => slot_for_accessory(
                    self.game_pouches,
                    stream_accessory_slots(self.pouches_seen),
                    self.worn,
                    param_id,
                    self.ordinal,
                ),
            };
            if let Some(slot) = slot {
                let i = ACCESSORY_SLOTS.iter().position(|&x| x == slot).unwrap();
                self.worn[i] = Some(param_id);
            }
            self.ordinal += 1;
        }
    }

    /// The policy as it ships on `main` today, kept here so the freeze stays reproducible after the
    /// production function stops implementing it. Rule 3 = `ACCESSORY_SLOTS[0]`, always.
    fn legacy_clobber_lowest(raw: u8, slots: [Option<i32>; 4], param_id: i32) -> Option<u32> {
        let n = usable_accessory_slots(raw);
        let visible = &slots[..n];
        if visible.contains(&Some(param_id)) {
            return None;
        }
        match visible.iter().position(Option::is_none) {
            Some(i) => Some(ACCESSORY_SLOTS[i]),
            None => Some(ACCESSORY_SLOTS[0]),
        }
    }

    /// Drive a timeline. A received item is applied ONCE as it arrives; a `Reconnect` re-applies
    /// everything AP has ever sent, from ordinal zero, onto the current loadout.
    fn drive(events: &[Ev], policy: Policy) -> [Option<i32>; 4] {
        let mut sim = Sim::new(policy);
        let mut received: Vec<Ev> = Vec::new();
        for &ev in events {
            match ev {
                Ev::Reconnect => {
                    sim.connect();
                    let set = received.clone();
                    for e in set {
                        sim.receive(e);
                    }
                }
                _ => {
                    if ev == Ev::Pouch {
                        sim.game_pouches += 1;
                    }
                    received.push(ev);
                    sim.receive(ev);
                }
            }
        }
        sim.worn
    }

    /// Receive `n` talismans with three pouches already in hand, no reconnect.
    fn eight_talismans() -> Vec<Ev> {
        let mut v = vec![Ev::Pouch, Ev::Pouch, Ev::Pouch];
        v.extend((0..8).map(|i| Ev::Talisman(1000 + i * 10)));
        v
    }

    /// 🛑 BUG 1, THE MOTIVATING CASE (CONTRIBUTING rule 11). Eight talismans into a fully unlocked
    /// character: every slot must have churned. `ClobberLowest` freezes three of them.
    #[test]
    fn a_full_loadout_churns_every_slot() {
        let worn = drive(&eight_talismans(), Policy::StreamDerived);
        assert_eq!(
            worn,
            [Some(1040), Some(1050), Some(1060), Some(1070)],
            "the four most recent talismans should be worn"
        );

        // ...and the same timeline on the shipped policy, so the bug is on the record, by value.
        let frozen = drive(&eight_talismans(), Policy::ClobberLowest);
        assert_eq!(
            frozen,
            [Some(1070), Some(1010), Some(1020), Some(1030)],
            "the shipped policy should freeze slots 2, 3 and 4 on the 2nd, 3rd and 4th arrivals"
        );
        assert_ne!(worn, frozen, "the fix must change this timeline");
    }

    /// Settle a timeline, then reconnect once and twice. A correct policy reaches a FIXED POINT:
    /// the first replay may move things (rule 2 is state-dependent -- #48 has the same property),
    /// but the second must not.
    fn is_fixed_point(stream: &[Ev], policy: Policy) -> bool {
        let mut once = stream.to_vec();
        once.push(Ev::Reconnect);
        let mut twice = once.clone();
        twice.push(Ev::Reconnect);
        drive(&once, policy) == drive(&twice, policy)
    }

    /// 🛑 BUG 2, THE ACCEPTANCE TEST for taking `n` from the stream rather than the live field.
    ///
    /// Three talismans arrive while the player has NO pouch, so all three fight over the single
    /// slot at `n = 1`. THEN a pouch lands. Every later reconnect evaluates that same stream at
    /// `n = 2`, so the live-modulus port keeps reshuffling a loadout the player already had:
    /// `[1020, 1010]` after one reconnect, `[1000, 1020]` after the next. That is the "loadout
    /// silently rearranged itself" behaviour #48's own commit message argues against.
    ///
    /// Found by the exhaustive sweep below, not by hand.
    #[test]
    fn a_pouch_after_the_slots_filled_rearranges_the_loadout_under_a_live_modulus() {
        let stream = [
            Ev::Talisman(1000),
            Ev::Talisman(1010),
            Ev::Talisman(1010),
            Ev::Talisman(1020),
            Ev::Pouch,
        ];
        assert!(
            !is_fixed_point(&stream, Policy::LiveModulus),
            "this is the timeline that motivated the fix -- if the live-modulus port no longer \
             diverges here the test has stopped testing anything (CONTRIBUTING rule 7)"
        );
        assert!(
            is_fixed_point(&stream, Policy::StreamDerived),
            "stream-derived n must settle on the very timeline that breaks the live modulus"
        );
    }

    /// The sweep the witness above came out of: every interleaving of three talisman ids and a
    /// Talisman Pouch up to length 7. `StreamDerived` must reach a fixed point on ALL of them.
    ///
    /// `LiveModulus` is swept too, and is REQUIRED to fail somewhere. A sweep that finds nothing
    /// looks identical to a sweep that tests nothing (CONTRIBUTING rule 2), so the harness has to
    /// prove it can still catch the bug it was built for.
    #[test]
    fn no_timeline_rearranges_the_loadout_under_a_stream_derived_modulus() {
        const TOKENS: [Ev; 4] = [
            Ev::Talisman(1000),
            Ev::Talisman(1010),
            Ev::Talisman(1020),
            Ev::Pouch,
        ];
        let mut swept = 0u32;
        let mut live_failures = 0u32;
        let mut stream_failures: Vec<Vec<Ev>> = Vec::new();

        for len in 3..=7usize {
            for mut code in 0..4u32.pow(len as u32) {
                let mut stream = Vec::with_capacity(len);
                for _ in 0..len {
                    stream.push(TOKENS[(code % 4) as usize]);
                    code /= 4;
                }
                // Three pouches is the vanilla maximum, and a stream of one repeated talisman
                // never reaches rule 3 at all.
                if stream.iter().filter(|e| **e == Ev::Pouch).count() > 3 {
                    continue;
                }
                swept += 1;
                if !is_fixed_point(&stream, Policy::LiveModulus) {
                    live_failures += 1;
                }
                if !is_fixed_point(&stream, Policy::StreamDerived) {
                    stream_failures.push(stream);
                }
            }
        }

        assert!(
            swept > 10_000,
            "the sweep enumerated almost nothing: {swept}"
        );
        assert!(
            stream_failures.is_empty(),
            "{} of {swept} timelines failed to settle under a stream-derived modulus, e.g. {:?}",
            stream_failures.len(),
            stream_failures.first()
        );
        assert!(
            live_failures > 0,
            "the live-modulus port settled on all {swept} timelines -- this sweep can no longer \
             detect the bug it exists for (CONTRIBUTING rule 2: an empty result is a failure)"
        );
    }

    /// Reconnecting repeatedly must never keep moving things. This is #48's
    /// `replaying_the_received_set_converges` at four slots and with the unlock count in play.
    #[test]
    fn repeated_reconnects_settle() {
        let mut events = eight_talismans();
        events.insert(5, Ev::Pouch);
        let mut seen = Vec::new();
        for _ in 0..4 {
            events.push(Ev::Reconnect);
            seen.push(drive(&events, Policy::StreamDerived));
        }
        assert!(
            seen.windows(2).all(|w| w[0] == w[1]),
            "loadout kept moving across reconnects: {seen:?}"
        );
    }

    /// 🛑 THE DISAGREEMENT DIRECTION THAT MATTERS. The stream can claim a slot the character has
    /// not earned -- a pouch that was sent but whose grant capped or was refused. The game's field
    /// is the authority on what may be WRITTEN, so the extra slot must not be reachable.
    ///
    /// This is the case that justifies keeping two sources of truth at all (see the objection
    /// recorded on `usable_accessory_slots`): they are used for different things, and the one that
    /// can be wrong in the dangerous direction never bounds the write.
    #[test]
    fn a_pouch_that_never_landed_cannot_name_a_locked_slot() {
        let stream = vec![
            Ev::PouchThatNeverLanded,
            Ev::PouchThatNeverLanded,
            Ev::PouchThatNeverLanded,
            Ev::Talisman(1000),
            Ev::Talisman(1010),
            Ev::Talisman(1020),
            Ev::Talisman(1030),
            Ev::Talisman(1040),
        ];
        let worn = drive(&stream, Policy::StreamDerived);
        assert_eq!(
            worn,
            [Some(1040), None, None, None],
            "with no pouch actually granted only slot 1 exists, so every talisman lands there"
        );
    }

    /// The two counts are measurements of the same quantity from two sources, so they must agree
    /// wherever both are defined. If this ever fails, one of the two readings is wrong.
    #[test]
    fn the_stream_count_and_the_game_field_agree_when_both_are_honest() {
        for pouches in 0..=3u32 {
            assert_eq!(
                stream_accessory_slots(pouches),
                usable_accessory_slots(pouches as u8),
                "stream and game disagree at {pouches} pouches"
            );
        }
        // Both clamp rather than wrap above the vanilla maximum of three pouches.
        assert_eq!(stream_accessory_slots(9), 4);
        assert_eq!(usable_accessory_slots(9), 4);
    }
}
