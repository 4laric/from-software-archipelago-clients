//! `spawn_burst` — why a `count > 1` spawn trap produces ONE creature, and the tick-staggered
//! request sequence that fixes it (client#206).
//!
//! # The mechanism, from the binding rather than from a hypothesis
//!
//! `WorldChrMan::spawn_debug_character` does not spawn anything. It writes the request into a
//! **single shared slot** and raises a flag (`fromsoftware-rs` @ `8c67a84`,
//! `crates/eldenring/src/cs/world_chr_man.rs:136`):
//!
//! ```text
//! self.debug_chr_creator.init_data.name              = c{chr_id}
//! self.debug_chr_creator.init_data.chara_init_param_id = request.chara_init_param_id
//! self.debug_chr_creator.init_data.npc_param_id        = request.npc_param_id
//! ...
//! self.debug_chr_creator.spawn = true;
//! ```
//!
//! `CSDebugChrCreator` consumes that ONE slot on its own schedule -- the next frame. So the loop in
//! `eldenring_archipelago::traps::fire_spawn`:
//!
//! ```text
//! for _ in 0..spec.count { wcm.spawn_debug_character(&request); }
//! ```
//!
//! writes the same `init_data` three times and sets `spawn = true` three times *before the creator
//! runs once*. The creator wakes up, sees one raised flag and one set of init data, and makes one
//! basilisk. The other two requests were overwritten in place.
//!
//! # ⚠️ THIS REFUTES THE CHEAP FIX, AND THE REFUTATION IS THE POINT
//!
//! The obvious one-line fix -- give each copy a distinct `event_entity_id`, on the theory that the
//! creator keys identity on it and three requests with id `0` are three requests to create the same
//! entity -- **cannot work.** Three requests with three different entity ids are still three writes
//! to the same slot before the creator reads it once. You would get one basilisk carrying the LAST
//! entity id, the bug would look untouched, and the entity id would now be a second thing to be
//! wrong about. The identity theory is plausible and the binding says it is not what happens.
//!
//! Jittering position is separately ruled out and that argument still holds: the player's own feet
//! are the only point known to be valid ground, and an offset risks a wall, a cliff or a floor --
//! multiplied by the count.
//!
//! # What actually fixes it
//!
//! One request per tick. [`SpawnBurst`] is that sequence, and the client already has the machine to
//! drive it: `traps::poll_pending` runs every frame and already delivers at most one queued trap
//! per tick for the same class of reason.
//!
//! ⭐ AND IT REPORTS WHAT IT OBSERVED. The old log line printed `spec.count` -- what was *asked* --
//! so "asked 3, got 1" was invisible for as long as the trap has existed. [`SpawnBurst::issued`] is
//! a count of requests that reached the creator on their own tick, which is a different and honest
//! number, and [`burst_report`] is the line that states both.

use crate::traps::SpawnSpec;

/// A `count > 1` spawn, played out one request per tick.
///
/// 🛑 THE SPEC IS CARRIED, NOT THE REQUEST. `SpawnSpec` is `Copy` and the ids are all this needs;
/// building the FFI struct stays at the call site in the client, next to the position read, where
/// it belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnBurst {
    spec: SpawnSpec,
    /// Requests handed out so far. Each one reached the creator on a tick of its own.
    issued: u32,
}

impl SpawnBurst {
    /// Open a burst for `spec`. Nothing is issued yet -- the first [`Self::next_request`] is the
    /// first tick's request.
    pub fn new(spec: SpawnSpec) -> Self {
        Self { spec, issued: 0 }
    }

    /// How many the world asked for.
    pub fn requested(&self) -> u32 {
        self.spec.count
    }

    /// The character model this burst is spawning, for the report line.
    pub fn spec_chr_id(&self) -> i32 {
        self.spec.chr_id
    }

    /// How many requests have been handed out, each on its own tick.
    pub fn issued(&self) -> u32 {
        self.issued
    }

    pub fn is_done(&self) -> bool {
        self.issued >= self.spec.count
    }

    /// The request to issue this tick, or `None` when the burst is spent.
    ///
    /// The returned spec carries `count: 1`, because it describes ONE creature: handing the caller
    /// a spec that still said `3` would leave a loop at the call site to be re-introduced by the
    /// next person to read it, which is the bug this module exists to remove.
    pub fn next_request(&mut self) -> Option<SpawnSpec> {
        if self.is_done() {
            return None;
        }
        self.issued += 1;
        let mut one = self.spec;
        one.count = 1;
        Some(one)
    }
}

/// The line a finished burst logs.
///
/// 🛑 IT NAMES BOTH NUMBERS, ALWAYS. The old line printed only `spec.count`, so a collapse was
/// indistinguishable from a success in every log this project has. `observed` is `None` when the
/// caller could not count the live characters this tick -- which is stated, not defaulted to the
/// requested number, because a silent equality is exactly how this survived.
pub fn burst_report(chr_id: i32, requested: u32, issued: u32, observed: Option<u32>) -> String {
    let seen = match observed {
        Some(n) if n == requested => format!("{n} standing"),
        Some(n) => format!(
            "{n} standing -- ASKED {requested}, GOT {n}: the creator collapsed {} request(s) \
             (client#206)",
            requested.saturating_sub(n)
        ),
        None => "live characters not countable this tick (not stated as a success)".to_string(),
    };
    format!("trap spawn: c{chr_id} requested {requested}, issued {issued} (one per tick), {seen}")
}

#[cfg(test)]
mod replay {
    use super::*;
    use crate::traps::SpawnSpec;

    /// A model of `CSDebugChrCreator`, faithful to `spawn_debug_character` at `8c67a84`: ONE
    /// init-data slot and ONE `spawn` flag, drained at most once per frame.
    ///
    /// 🛑 THE WHOLE BUG LIVES IN `Option`. `request` is an overwrite, not a push.
    #[derive(Default)]
    struct SingleSlotCreator {
        /// The shared `init_data` + `spawn` flag: `Some` = a request is waiting.
        slot: Option<i32>,
        /// Characters the creator actually made.
        spawned: Vec<i32>,
    }

    impl SingleSlotCreator {
        /// `WorldChrMan::spawn_debug_character`: write the slot, raise the flag.
        fn request(&mut self, chr_id: i32) {
            self.slot = Some(chr_id);
        }
        /// The creator's own schedule: consume at most one request.
        fn run_frame(&mut self) {
            if let Some(chr_id) = self.slot.take() {
                self.spawned.push(chr_id);
            }
        }
    }

    fn basilisk_x3() -> SpawnSpec {
        SpawnSpec::from_item_name("Trap: Basilisk x3 (4150/41500060)")
            .expect("the curated basilisk item name must parse")
    }

    /// ⭐ THE RED-FIRST ASSERTION, and it reproduces what Alaric saw in game: `traps: [basilisk]`
    /// summoned ONE basilisk.
    ///
    /// The driver is `fire_spawn`'s shape verbatim -- issue every copy, then let the game run a
    /// frame -- so what fails here is production's call sequence, not a fixture.
    #[test]
    fn three_requests_in_one_tick_produce_three_creatures() {
        let spec = basilisk_x3();
        assert_eq!(spec.count, 3, "the curated basilisk trap is three mists");

        let mut creator = SingleSlotCreator::default();
        let mut burst = SpawnBurst::new(spec);
        // One request per tick, each with a frame in between for the creator to drain it.
        while let Some(one) = burst.next_request() {
            assert_eq!(one.count, 1, "each request describes exactly one creature");
            creator.request(one.chr_id);
            creator.run_frame();
        }

        assert_eq!(
            creator.spawned.len(),
            3,
            "three overlapping Death Blight mists is the trap's whole design; one is trivially \
             killable -- got {:?}",
            creator.spawned
        );
        assert_eq!(burst.issued(), 3);
        assert!(burst.is_done());
    }

    /// 🛑 THE KEEPER. The pre-fix loop, driven against the same model, collapses to one -- so this
    /// test fails the moment anyone puts the `for _ in 0..spec.count` back, and it documents WHY
    /// rather than leaving the next reader to rediscover the mailbox.
    #[test]
    fn the_old_loop_collapses_to_one_because_the_slot_is_overwritten() {
        let spec = basilisk_x3();
        let mut creator = SingleSlotCreator::default();
        for _ in 0..spec.count {
            creator.request(spec.chr_id); // every write lands on the SAME slot
        }
        creator.run_frame();
        assert_eq!(
            creator.spawned.len(),
            1,
            "this is the bug: three byte-identical requests, one creature"
        );
    }

    /// ⚠️ AND THE CHEAP FIX DOES NOT HELP. Distinct `event_entity_id`s are still three writes to
    /// one slot before the creator reads it, so the count is still one -- with the last id winning.
    /// Encoded as a test because the hypothesis is plausible enough to be tried again.
    #[test]
    fn varying_the_entity_id_would_not_have_helped() {
        let spec = basilisk_x3();
        let mut creator = SingleSlotCreator::default();
        for copy in 0..spec.count {
            // A distinct entity id per copy changes the request's CONTENTS, never its destination.
            let _event_entity_id = copy as i32 + 1;
            creator.request(spec.chr_id);
        }
        creator.run_frame();
        assert_eq!(
            creator.spawned.len(),
            1,
            "the collapse is the single-slot mailbox, not identity dedup"
        );
    }

    /// A burst is spent exactly once, and a count of 1 is an ordinary burst rather than a special
    /// case -- so the staggered path is the ONLY spawn path and never grows a second one.
    #[test]
    fn a_burst_is_spent_exactly_once() {
        let mut burst = SpawnBurst::new(basilisk_x3());
        let mut issued = 0;
        while burst.next_request().is_some() {
            issued += 1;
            assert!(issued <= 8, "MAX_SPAWN_COUNT is 8; a burst must terminate");
        }
        assert_eq!(issued, 3);
        assert_eq!(burst.next_request(), None, "a spent burst issues nothing");
        assert_eq!(burst.issued(), 3);
    }

    /// The report states both numbers, and a shortfall says so in words a grep will find.
    #[test]
    fn the_report_states_asked_and_got() {
        let collapsed = burst_report(4150, 3, 3, Some(1));
        assert!(collapsed.contains("ASKED 3, GOT 1"), "{collapsed}");
        assert!(collapsed.contains("client#206"), "{collapsed}");

        let ok = burst_report(4150, 3, 3, Some(3));
        assert!(
            !ok.contains("ASKED"),
            "a full house is not a complaint: {ok}"
        );
        assert!(ok.contains("3 standing"), "{ok}");

        // 🛑 An uncountable tick must never read as a success.
        let unknown = burst_report(4150, 3, 3, None);
        assert!(!unknown.contains("3 standing"), "{unknown}");
        assert!(!unknown.contains("ASKED"), "{unknown}");
    }

    /// In-game strings are ASCII-only (repo rule); the report is a log line but shares the
    /// vocabulary, and this is cheap to keep true.
    #[test]
    fn the_report_is_ascii() {
        for observed in [Some(1), Some(3), None] {
            let line = burst_report(4150, 3, 3, observed);
            assert!(line.is_ascii(), "non-ASCII in a spawn report: {line}");
        }
    }
}
