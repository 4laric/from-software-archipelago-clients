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
//!
//! # The witness (world#689)
//!
//! `issued` is still not the same question as *did three basilisks appear*. #206 shipped with
//! `observed: None` because the client could not count what the creator made; this module now
//! carries the state machine that closes that, and it exists because **#206 was found by a human
//! standing in the arena counting basilisks.** An instrument that reports its own request is not a
//! witness to anything.
//!
//! Two things make the count honest, and both are the whole reason this is not a one-liner:
//!
//! * 🛑 **A BASELINE, NOT A RAW COUNT.** The trap's `npc_param_id` is a curated row, not a private
//!   one -- the world can already contain creatures on it, and Elden Ring is a game where you fight
//!   a basilisk near other basilisks. Reporting the live count would score a trap that spawned
//!   NOTHING as a full success in any room that already had three. The burst records the count
//!   BEFORE it issues anything and reports the delta.
//! * 🛑 **A SETTLE DELAY.** `CSDebugChrCreator` drains its slot on its own schedule and the
//!   character then has to load. Counting on the tick after the last request would read zero and
//!   report a failure that did not happen -- the mirror image of the bug, and a worse one, because
//!   a false alarm teaches a reader to stop believing the line.

use crate::traps::SpawnSpec;

/// How long after the LAST request to wait before counting what stands (world#689).
///
/// The creator drains one slot per frame and the character then loads, so the true settle is a
/// handful of frames. Two seconds is deliberately generous: this line is a witness, not a gate, and
/// nothing waits on it. Being late costs one log line's timestamp; being early costs a false
/// "ASKED 3, GOT 0" on a trap that worked, which is the failure that would make the whole
/// instrument ignorable.
pub const SETTLE_MS: u64 = 2_000;

/// Minimum wall-clock gap between successive spawn REQUESTS inside one burst.
///
/// One-request-per-tick (client#206) still drains at FRAME rate: eight copies land in ~130ms at
/// 60fps, and back-to-back traps -- "a couple of runebears and a couple of basilisks" -- arrive
/// effectively simultaneously and can overload `CSDebugChrCreator`. Frame pacing bounds the writes
/// per frame but not per second; this bounds them per second, the same shape the item-grant path
/// uses via `RECONCILE_GRANT_INTERVAL_MS`. `0` disables pacing (the RECONCILE convention: unpaced).
///
/// Env-overridable by the client through [`SpawnBurst::with_min_request_interval_ms`].
pub const DEFAULT_MIN_REQUEST_INTERVAL_MS: u64 = 250;

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
    /// Live characters on this `npc_param_id` BEFORE the burst issued anything (world#689).
    ///
    /// 🛑 THE TRAP'S ROW IS NOT PRIVATE. Without this, a trap that spawned nothing in a room that
    /// already held three basilisks reports a clean `3 standing`.
    baseline: u32,
    /// Wall clock of the last request, i.e. when the settle window starts, and the anchor the
    /// inter-request pacing gate measures from.
    last_issue_ms: u64,
    /// Minimum wall-clock gap between successive requests. `0` = unpaced (issue every tick).
    min_request_interval_ms: u64,
}

impl SpawnBurst {
    /// Open a burst for `spec`, having already counted `baseline` live characters on its row.
    ///
    /// Nothing is issued yet -- the first [`Self::next_request`] is the first tick's request.
    pub fn new(spec: SpawnSpec, baseline: u32, now_ms: u64) -> Self {
        Self {
            spec,
            issued: 0,
            baseline,
            last_issue_ms: now_ms,
            min_request_interval_ms: DEFAULT_MIN_REQUEST_INTERVAL_MS,
        }
    }

    /// Override the inter-request spacing, in ms. `0` issues every tick (unpaced). The client wires
    /// this from `ER_SPAWN_INTERVAL_MS` the same way `reconcile_io` wires the grant interval.
    #[must_use]
    pub fn with_min_request_interval_ms(mut self, ms: u64) -> Self {
        self.min_request_interval_ms = ms;
        self
    }

    /// The inter-request spacing this burst paces to, in ms (`0` = unpaced).
    pub fn min_request_interval_ms(&self) -> u64 {
        self.min_request_interval_ms
    }

    /// The row the witness counts on. Not the same id as [`Self::spec_chr_id`]: `chr_id` is the
    /// model (`c4150`) and several rows can share one.
    pub fn spec_npc_param_id(&self) -> i32 {
        self.spec.npc_param_id
    }

    /// What the world held on this row before the burst opened.
    pub fn baseline(&self) -> u32 {
        self.baseline
    }

    /// Has the settle window closed, so a count taken now means something?
    ///
    /// Only ever true once the burst is spent: counting mid-burst would be reading a number that is
    /// still being written.
    pub fn verify_due(&self, now_ms: u64) -> bool {
        self.is_done() && now_ms.saturating_sub(self.last_issue_ms) >= SETTLE_MS
    }

    /// How many of the creatures standing now are OURS, given the live count on this row.
    ///
    /// Saturating: a live count below the baseline means something died, despawned, or wandered
    /// off between the two reads. That is `0` of ours confirmed, never a negative, and never a
    /// wrap into a huge number that would read as wild success.
    pub fn observed_delta(&self, live_now: u32) -> u32 {
        live_now.saturating_sub(self.baseline)
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
    pub fn next_request(&mut self, now_ms: u64) -> Option<SpawnSpec> {
        if self.is_done() {
            return None;
        }
        // Wall-clock pacing (issue #2 onward): hold until the interval has elapsed since the last
        // request. Returning None here while NOT done is the "wait" signal the driver already
        // understands -- `drive_spawn_burst` keeps the burst and owns the tick, so nothing writes
        // the creator's slot underneath it, and `verify_due` stays false until the burst is spent.
        // The first request (issued == 0) always goes immediately; a backwards clock saturates to 0
        // and simply issues, never stalls.
        if self.min_request_interval_ms > 0
            && self.issued > 0
            && now_ms.saturating_sub(self.last_issue_ms) < self.min_request_interval_ms
        {
            return None;
        }
        self.issued += 1;
        // The settle window runs from the LAST request, not the first: a burst of eight staggered
        // across eight ticks must not start its clock before the eighth has been made.
        self.last_issue_ms = now_ms;
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
        let mut burst = SpawnBurst::new(spec, 0, 0).with_min_request_interval_ms(0); // pacing tested separately
                                                                                     // One request per tick, each with a frame in between for the creator to drain it.
        while let Some(one) = burst.next_request(0) {
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
        let mut burst = SpawnBurst::new(basilisk_x3(), 0, 0).with_min_request_interval_ms(0);
        let mut issued = 0;
        while burst.next_request(0).is_some() {
            issued += 1;
            assert!(issued <= 8, "MAX_SPAWN_COUNT is 8; a burst must terminate");
        }
        assert_eq!(issued, 3);
        assert_eq!(burst.next_request(0), None, "a spent burst issues nothing");
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

    /// ⭐ THE RED-FIRST ASSERTION FOR THE WITNESS (world#689). A trap that spawns NOTHING in a
    /// room that already holds three basilisks must report a failure, not `3 standing`.
    ///
    /// Driven with the raw live count instead of the delta, this fails -- which is exactly the
    /// shape a one-line "just count them" fix would have shipped.
    #[test]
    fn a_room_that_already_had_basilisks_cannot_pass_for_a_successful_trap() {
        let mut burst = SpawnBurst::new(basilisk_x3(), 3, 0).with_min_request_interval_ms(0);
        while burst.next_request(0).is_some() {}

        // The creator collapsed everything: the live count is still the three that were there.
        let observed = burst.observed_delta(3);
        assert_eq!(
            observed, 0,
            "none of the three standing are ours -- they were there before the trap fired"
        );
        let line = burst_report(4150, burst.requested(), burst.issued(), Some(observed));
        assert!(
            line.contains("ASKED 3, GOT 0"),
            "a trap that spawned nothing must say so even in a room full of basilisks: {line}"
        );
    }

    /// The happy path, in a room that already had some: baseline 2, five standing, three are ours.
    #[test]
    fn the_delta_is_what_the_trap_actually_added() {
        let mut burst = SpawnBurst::new(basilisk_x3(), 2, 0).with_min_request_interval_ms(0);
        while burst.next_request(0).is_some() {}
        assert_eq!(burst.baseline(), 2);
        assert_eq!(burst.observed_delta(5), 3);
        let line = burst_report(4150, burst.requested(), burst.issued(), Some(3));
        assert!(
            !line.contains("ASKED"),
            "three of ours is a full house: {line}"
        );
        assert!(line.contains("3 standing"), "{line}");
    }

    /// 🛑 A LIVE COUNT BELOW THE BASELINE IS NOT A NEGATIVE AND NOT A WRAP. Something died or
    /// despawned between the two reads; that is zero of ours confirmed. An unsigned wrap here
    /// would print a number in the billions and read as wild success.
    #[test]
    fn a_shrinking_world_reads_as_zero_not_as_a_wrap() {
        let burst = SpawnBurst::new(basilisk_x3(), 4, 0);
        assert_eq!(burst.observed_delta(1), 0);
        assert_eq!(burst.observed_delta(0), 0);
    }

    /// The settle window opens only once the burst is spent, and it runs from the LAST request --
    /// a burst staggered across eight ticks must not start its clock on the first.
    #[test]
    fn the_settle_window_runs_from_the_last_request() {
        let mut burst = SpawnBurst::new(basilisk_x3(), 0, 0);
        assert!(
            !burst.verify_due(SETTLE_MS * 10),
            "an unspent burst is never due: the number is still being written"
        );
        burst.next_request(0);
        burst.next_request(500);
        assert!(!burst.verify_due(SETTLE_MS), "still one request to go");
        burst.next_request(1_000); // the last one
        assert!(burst.is_done());
        assert!(
            !burst.verify_due(1_000 + SETTLE_MS - 1),
            "counting early reads a creature that has not loaded yet"
        );
        assert!(burst.verify_due(1_000 + SETTLE_MS));
    }

    /// A backwards clock must not stall the witness forever. `saturating_sub` yields 0, so the
    /// window simply re-opens on the next tick that reads forward -- the same degrade
    /// `SampleGate` documents for its heartbeat.
    #[test]
    fn a_backwards_clock_does_not_strand_the_witness() {
        let mut burst = SpawnBurst::new(basilisk_x3(), 0, 10_000).with_min_request_interval_ms(0);
        while burst.next_request(10_000).is_some() {}
        assert!(
            !burst.verify_due(5),
            "a clock that went backwards is not a due date"
        );
        assert!(burst.verify_due(10_000 + SETTLE_MS));
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

    /// ⭐ THE FIX FOR #947: successive requests are spaced by WALL CLOCK, not one-per-frame. The
    /// first goes immediately; each subsequent one is held until the interval elapses. This is what
    /// keeps "a couple of runebears and a couple of basilisks" from landing in the same ~130ms.
    #[test]
    fn requests_are_spaced_by_wall_clock() {
        let mut burst = SpawnBurst::new(basilisk_x3(), 0, 0).with_min_request_interval_ms(250);
        // First request is immediate (issued == 0).
        assert!(
            burst.next_request(0).is_some(),
            "first request must go at once"
        );
        assert_eq!(burst.issued(), 1);
        // Same tick / inside the interval: held, and holding does not spend the burst.
        assert!(burst.next_request(0).is_none(), "second request must wait");
        assert!(
            burst.next_request(249).is_none(),
            "still inside the interval"
        );
        assert!(!burst.is_done(), "a held burst is not a spent burst");
        // Interval elapsed: the second request goes, and the clock re-anchors from it.
        assert!(burst.next_request(250).is_some());
        assert_eq!(burst.issued(), 2);
        assert!(
            burst.next_request(499).is_none(),
            "measured from the last request, not the first"
        );
        assert!(burst.next_request(500).is_some());
        assert!(burst.is_done(), "three requests, spaced, then spent");
    }

    /// The witness clock still runs from the LAST request even when the burst is paced -- pacing
    /// only decides WHEN each request may go, never how the settle window is measured.
    #[test]
    fn a_paced_burst_settles_from_its_last_request() {
        let mut burst = SpawnBurst::new(basilisk_x3(), 0, 0).with_min_request_interval_ms(250);
        assert!(burst.next_request(0).is_some());
        assert!(burst.next_request(250).is_some());
        assert!(burst.next_request(500).is_some());
        assert!(burst.is_done());
        assert!(
            !burst.verify_due(500 + SETTLE_MS - 1),
            "settle runs from the third request"
        );
        assert!(burst.verify_due(500 + SETTLE_MS));
    }

    /// `0` is unpaced, matching the RECONCILE convention: every call issues, so the pre-#947
    /// frame-rate behaviour is still reachable (and is what the accounting tests above rely on).
    #[test]
    fn interval_zero_is_unpaced() {
        let mut burst = SpawnBurst::new(basilisk_x3(), 0, 0).with_min_request_interval_ms(0);
        assert_eq!(burst.min_request_interval_ms(), 0);
        let mut issued = 0;
        while burst.next_request(0).is_some() {
            issued += 1;
            assert!(issued <= 8, "a burst must terminate even unpaced");
        }
        assert_eq!(issued, 3, "unpaced: all three drain at the same now");
    }

    /// A burst built with the plain constructor is paced by default (the fix is live without any
    /// client wiring): a second same-tick request is held.
    #[test]
    fn the_default_burst_is_paced() {
        let mut burst = SpawnBurst::new(basilisk_x3(), 0, 0);
        assert_eq!(
            burst.min_request_interval_ms(),
            DEFAULT_MIN_REQUEST_INTERVAL_MS
        );
        assert!(burst.next_request(0).is_some());
        assert!(
            burst.next_request(0).is_none(),
            "default pacing holds the second request"
        );
    }
}
