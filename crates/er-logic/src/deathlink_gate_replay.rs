//! `deathlink_gate_replay` — headless timeline replay for the INCOMING-DeathLink opt-out gate.
//!
//! Replay-tier sibling of [`crate::start_grant_replay`] / [`crate::region_lock_replay`], for
//! SWEEP H2: the client advertises the DeathLink tag unconditionally (room visibility), so a slot
//! with `death_link: 0` still RECEIVES other players' death bounces — and the pre-sweep client
//! APPLIED them, killing a player who had explicitly opted out. The per-call decision fix already
//! lives in [`crate::deathlink`]: `latch_incoming(latch, enabled)` refuses to latch when the
//! option is off, and `drive_incoming_kill(hook, latch, enabled)` refuses to fire even a stale
//! latch (belt and braces). This module DRIVES those real fns over a session timeline, because
//! what had no host test is the sequencing around them:
//!
//!   (a) a full bounce -> latch -> drive pass with `death_link: 0` must leave the dedicated kill
//!       flag ([`crate::hook::DEATHLINK_KILL_FLAG`] = 76996) untouched, end to end; and
//!   (b) a RECONNECT that re-delivers the same DeathLink bounce must not kill the player a second
//!       time — the kill is once-per-death, not once-per-delivery.
//!
//! The only NEW production logic here is [`should_apply_incoming_deathlink`], the composed
//! delivery-site decision (option gate AND once-only). The option half restates the gate
//! `deathlink::latch_incoming` already enforces; the once-only half is the reconnect dedup the
//! Windows bounce handler has no pure home for today. The `enabled` bool itself comes off the
//! wire via `crate::options::parse_death_link` (int-or-bool `options.death_link`).

/// Delivery-site decision for an incoming DeathLink bounce: apply it only when this slot's
/// `death_link` option is ON **and** this particular death has not already been applied (a
/// reconnect re-delivers the last bounce; it must not re-kill). Pure; see the module docs.
pub fn should_apply_incoming_deathlink(death_link_enabled: bool, already_applied: bool) -> bool {
    death_link_enabled && !already_applied
}

#[cfg(test)]
mod replay {
    use super::*;
    use crate::deathlink::{drive_incoming_kill, latch_incoming, DeathLatch};
    use crate::hook::{GameHook, DEATHLINK_KILL_FLAG};
    use std::collections::HashMap;

    /// Flag-holder game model for the DeathLink surface: a flag map, holder readiness (a not-ready
    /// CSEventFlagMan fails `try_set` -> caller retries), and an ordered transcript of every flag
    /// write so the test can count how many times the player was actually killed.
    struct DeathGame {
        flags: HashMap<u32, bool>,
        holder_ready: bool,
        in_world: bool,
        /// Every flag write that LANDED: (flag, on).
        flags_set: Vec<(u32, bool)>,
    }

    impl DeathGame {
        fn new() -> Self {
            DeathGame {
                flags: HashMap::new(),
                holder_ready: true,
                in_world: true,
                flags_set: Vec::new(),
            }
        }
        fn flag(&self, flag: u32) -> bool {
            self.flags.get(&flag).copied().unwrap_or(false)
        }
        /// How many times the player was killed = landed `true` writes of the dedicated kill flag.
        fn kills(&self) -> usize {
            self.flags_set
                .iter()
                .filter(|&&(f, on)| f == DEATHLINK_KILL_FLAG && on)
                .count()
        }
    }

    impl GameHook for DeathGame {
        fn get_event_flag(&self, flag: u32) -> bool {
            self.flag(flag)
        }
        fn set_event_flag(&mut self, flag: u32, on: bool) {
            self.flags.insert(flag, on);
            self.flags_set.push((flag, on));
        }
        fn try_set_event_flag(&mut self, flag: u32, on: bool) -> bool {
            if !self.holder_ready {
                return false; // holder not ready -> caller must retry
            }
            self.flags.insert(flag, on);
            self.flags_set.push((flag, on));
            true
        }
        fn in_world(&self) -> bool {
            self.in_world
        }
        fn play_region_id(&self) -> Option<i32> {
            None
        }
        fn grant_full_id(&mut self, _full_id: i32, _qty: i32) -> bool {
            true
        }
        fn player_hp(&self) -> Option<i32> {
            None
        }
        fn weapon_track_and_cap(&self, _base: i32) -> Option<(i32, bool)> {
            None
        }
        fn highest_held_level(&self, _somber: bool) -> Option<i32> {
            None
        }
        fn scadutree_blessing(&self) -> Option<i32> {
            None
        }
        fn set_scadutree_blessing(&mut self, _level: i32) {}
    }

    /// One frame of the session timeline.
    #[derive(Clone, Copy)]
    enum Ev {
        /// A foreign DeathLink bounce arrives (another player died). A fresh bounce = a NEW death.
        IncomingDeath,
        /// Connection drops and re-establishes; the last DeathLink bounce (if any) is RE-DELIVERED.
        Reconnect,
        /// An update tick passes (the client drives any latched kill every tick).
        Tick,
    }

    /// Deliver one bounce to the latch. `honor_gate = true` is the real client: the composed
    /// [`should_apply_incoming_deathlink`] decision, with the REAL `deathlink::latch_incoming`
    /// enforcing the option half. `false` reproduces the pre-sweep H2 shape: every received bounce
    /// latches a kill, option and dedup ignored.
    fn deliver(enabled: bool, honor_gate: bool, latch: &mut DeathLatch, applied: &mut bool) {
        if !honor_gate {
            latch.kill_pending = true; // pre-fix: no gate, no dedup
            return;
        }
        if should_apply_incoming_deathlink(enabled, *applied) {
            if latch_incoming(latch, enabled) {
                *applied = true;
            }
        }
    }

    /// Replay a session timeline. Delivery goes through [`deliver`]; every frame then runs the real
    /// `deathlink::drive_incoming_kill` (as the Windows update loop does). Pre-fix mode also drives
    /// ungated (`enabled = true`), since the buggy client consulted no option anywhere.
    fn replay_with(events: &[Ev], death_link_enabled: bool, honor_gate: bool) -> DeathGame {
        let mut g = DeathGame::new();
        let mut latch = DeathLatch::default();
        // Session-scope bounce memory: whether a death bounce exists for a reconnect to re-deliver,
        // and whether THAT death has already been applied locally (the once-only dedup).
        let mut have_prior_death = false;
        let mut applied = false;
        for &ev in events {
            match ev {
                Ev::IncomingDeath => {
                    have_prior_death = true;
                    applied = false; // fresh bounce = new death: eligible again
                    deliver(death_link_enabled, honor_gate, &mut latch, &mut applied);
                }
                Ev::Reconnect => {
                    if have_prior_death {
                        // Same death re-delivered; `applied` carries over, so the gate drops it.
                        deliver(death_link_enabled, honor_gate, &mut latch, &mut applied);
                    }
                }
                Ev::Tick => {}
            }
            let drive_enabled = if honor_gate { death_link_enabled } else { true };
            drive_incoming_kill(&mut g, &mut latch, drive_enabled);
        }
        g
    }

    /// The real (gated) client behavior.
    fn replay(events: &[Ev], death_link_enabled: bool) -> DeathGame {
        replay_with(events, death_link_enabled, true)
    }

    #[test]
    fn incoming_ignored_when_death_link_off() {
        // death_link: 0 slot receives a foreign death bounce. The gated client must leave the kill
        // flag untouched end to end.
        let timeline = [Ev::IncomingDeath, Ev::Tick, Ev::Tick];
        let g = replay(&timeline, false);
        assert_eq!(g.kills(), 0, "death_link:0 slot must never be killed by a foreign death");
        assert!(!g.flag(DEATHLINK_KILL_FLAG));

        // What SWEEP H2 looked like: the ungated pre-fix delivery kills the opted-out player.
        let buggy = replay_with(&timeline, false, false);
        assert_eq!(
            buggy.kills(),
            1,
            "regression guard: ungated delivery kills the opted-out slot (documents SWEEP H2)"
        );
    }

    #[test]
    fn incoming_kills_when_death_link_on() {
        // Opted-in slot: the bounce must land exactly one kill via the dedicated flag (76996).
        let g = replay(&[Ev::IncomingDeath, Ev::Tick], true);
        assert!(g.flag(DEATHLINK_KILL_FLAG), "opted-in kill must place DEATHLINK_KILL_FLAG");
        assert_eq!(g.kills(), 1);
    }

    #[test]
    fn reconnect_does_not_reapply_incoming_death() {
        // The kill lands, then the connection bounces and the same death is re-delivered: the
        // once-only dedup must hold the count at one.
        let timeline = [Ev::IncomingDeath, Ev::Tick, Ev::Reconnect, Ev::Tick, Ev::Tick];
        let g = replay(&timeline, true);
        assert_eq!(g.kills(), 1, "a reconnect re-delivery must not kill the player twice");

        // Without the dedup (pre-fix shape) the re-delivery kills again.
        let buggy = replay_with(&timeline, true, false);
        assert_eq!(buggy.kills(), 2, "documents the double-kill a reconnect used to cause");

        // A genuinely NEW death after the reconnect must still kill (dedup is per-death, not
        // a session-wide fuse)...
        let fresh = replay(
            &[Ev::IncomingDeath, Ev::Tick, Ev::Reconnect, Ev::Tick, Ev::IncomingDeath, Ev::Tick],
            true,
        );
        assert_eq!(fresh.kills(), 2, "a new death after reconnect must still be applied");

        // ...and a reconnect with no prior death delivers nothing.
        assert_eq!(replay(&[Ev::Reconnect, Ev::Tick], true).kills(), 0);
    }

    #[test]
    fn pure_gate_semantics() {
        assert!(should_apply_incoming_deathlink(true, false));
        assert!(!should_apply_incoming_deathlink(true, true), "already applied -> never re-kill");
        assert!(!should_apply_incoming_deathlink(false, false), "death_link off -> drop (SWEEP H2)");
        assert!(!should_apply_incoming_deathlink(false, true));
    }

    #[test]
    fn death_link_option_wire_feeds_the_gate() {
        // End to end from slot_data: the yaml's `death_link: 0/1` (int form) parses via the real
        // options helper and flips the whole replay outcome.
        use crate::options::parse_death_link;
        use serde_json::json;
        let off = parse_death_link(&json!({ "options": { "death_link": 0 } }));
        let on = parse_death_link(&json!({ "options": { "death_link": 1 } }));
        assert!(!off && on);
        assert_eq!(replay(&[Ev::IncomingDeath, Ev::Tick], off).kills(), 0);
        assert_eq!(replay(&[Ev::IncomingDeath, Ev::Tick], on).kills(), 1);
    }
}
