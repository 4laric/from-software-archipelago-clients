//! The boss-fight OUTCOME instrument: when to sample, and what the line says (er-archipelago#553).
//!
//! # The gap this closes
//!
//! Every scaling complaint this project has had ends the same way: a player says a fight felt
//! wrong, and settling it costs a log dig, a datamine and an argument. The three most recent are
//! all the same shape --
//!
//! * boblerrr: *"2 bosses in one fight wildly different"* (Gideon vs Godfrey, #552)
//! * lizzymagala: *"one super squishy and weak, the other insanely tanky"*
//! * lavakoala6: Gideon *"1 shots from 50 vigor"* **and** a sponge
//!
//! -- and **not one of them is answerable from what the client logs today.** The scaling census
//! (`(re)scaled N`, `still NoTouch N`) describes OUR DECISION. It says nothing about what the fight
//! actually was. Two HP curves do, and a report is about the outcome.
//!
//! # What this module is, and is not
//!
//! It is the *decision* half only: a state machine over "is a boss healthbar up, and has enough
//! time passed", plus the line formatter. It reads no game memory and owns no `unsafe`, so the
//! whole thing is exercised on any host. `eldenring_archipelago::boss_fight_probe` is the I/O half.
//!
//! 🛑 IT DELIBERATELY DOES NOT KNOW VANILLA HP. #553 asks the sampler to record enough to NOTICE
//! that a modded stack edited an enemy's base stats -- boblerrr co-loads matt's rando, whose
//! `regulation.bin` edits base stats, and a residual of ~0.50 with wide spread has already been
//! measured there. The way to notice it is `npc_param_id` + observed `max_hp`, because
//! **`NpcParam.hp` is a column in the datamine bundle**, so the vanilla number is available offline
//! for any id. Baking an HP table into the client to compute a ratio it could not act on would be a
//! second copy of a datum that already has an owner. `scaling.rs`'s `ER_SCALING_SAMPLE` doc makes
//! exactly this argument for exactly this reason.
//!
//! # Why `play_region` and not "the arena id"
//!
//! #553 asks for "the arena/GameAreaParam id, so a session can be joined against `NATIVE_TIERS` /
//! the area index". The area index -- [`crate::area_tiers::AREA_TIERS`] -- is keyed on the
//! **play_region bucket** (`11050`, `12010`, ...), not on `GameAreaParam`. So the bucket is what
//! makes the join work, and it is what this line carries. Calling it an arena id would be a
//! confident wrong answer of exactly the kind #552 is about.
//!
//! # 🛑 `Some(0)` is an answer; `None` is not
//!
//! `GameDataMan.boss_health_bar_npc_param_id` reads `0` when no bar is up -- a real observation.
//! `None` means the holder was not reachable this tick (main menu, mid-load), which is
//! **don't-know**, and [`crate::boss_grants`] property 3 is that don't-know is never "no". Treating
//! an unreachable tick as "the fight ended" would end and restart a fight on every load stutter,
//! and the elapsed clock -- half the signal, since "too tanky" and "one-shots me" are opposite
//! complaints that both read as "hard" without it -- would reset with it. So `None` HOLDS.

/// Sample period. ~2 Hz, the rate #553 asks for.
///
/// Not per-frame: the tick is single-threaded and shared with every reconciler, so a per-frame log
/// would be both expensive and unreadable. Not slower, either -- at 1 Hz a burst that takes a
/// player from full to dead can fall between two samples, and "how many hits did that take" is one
/// of the questions the curve exists to answer.
pub const DEFAULT_INTERVAL_MS: u64 = 500;

/// Samples emitted per fight before the sampler goes quiet.
///
/// 1200 at [`DEFAULT_INTERVAL_MS`] is **ten minutes** of continuous healthbar. A census, not a dump
/// -- the same discipline as `scaling.rs`'s `SAMPLE_CAP`. The cap silences SAMPLES only:
/// [`Step::End`] still reports the fight's true elapsed time, so a capped fight is still measured,
/// just not traced. [`Step::Capped`] fires once so the gap in the log is never a mystery.
pub const MAX_SAMPLES_PER_FIGHT: u32 = 1200;

/// What the sampler wants the caller to do this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Nothing to do. Also what an unreachable tick (`None`) produces -- state is held.
    Idle,
    /// A boss healthbar just came up. The caller should emit a full reading at `t=0`.
    ///
    /// The `t=0` reading is the most valuable single line in the whole trace: it is the boss's
    /// `max_hp` before any of the fight has happened, which is the readback the scaler has never
    /// had -- proof of whether the applied tier moved `max_hp` at all.
    Start { npc_param_id: i32 },
    /// Emit a sample.
    Sample { npc_param_id: i32, elapsed_ms: u64 },
    /// `MAX_SAMPLES_PER_FIGHT` reached. Emitted exactly once per fight; sampling then stops but the
    /// fight is still being timed.
    Capped { npc_param_id: i32, elapsed_ms: u64 },
    /// The bar this sampler was following went away (killed, fled, reloaded, or replaced by a
    /// different boss).
    ///
    /// 🛑 `elapsed_ms` IS MEASURED TO THE LAST TICK THE BAR WAS ACTUALLY SEEN, not to the tick we
    /// noticed it was gone. Those are the same number in a normal end and wildly different when the
    /// player dies -- see [`FightSampler::last_seen_ms`] for the log that forced this.
    ///
    /// `unseen_ms` is the gap between the two: how long the sampler was blind before it found the
    /// bar cleared. The fight really ended somewhere inside that window, so `elapsed_ms` is a lower
    /// bound and `elapsed_ms + unseen_ms` is the upper one. A reader is told both rather than being
    /// handed a point estimate that is silently wrong on exactly the fights that were lost.
    End {
        npc_param_id: i32,
        elapsed_ms: u64,
        unseen_ms: u64,
    },
}

/// Follows one boss healthbar at a time and decides when to sample.
///
/// ⚠️ ONE BAR AT A TIME IS A REAL LIMIT, NOT AN OVERSIGHT. `boss_health_bar_npc_param_id` is a
/// single field, and some fights are two bars (PCR is two healthbars in one fight). When the id
/// changes, this reports [`Step::End`] for the old subject and picks the new one up on the next
/// tick, so a two-bar fight reads as two adjacent fights that share a wall clock. That is honest --
/// the two phases really do have different `max_hp` -- and joining them back up is an offline
/// question, not one to guess at here.
#[derive(Debug, Default)]
pub struct FightSampler {
    /// `npc_param_id` of the bar being followed. `None` = no fight in progress.
    subject: Option<i32>,
    /// Wall clock at which the current subject's bar came up.
    started_ms: u64,
    /// Wall clock of the last emitted sample.
    last_sample_ms: u64,
    /// Wall clock of the last tick on which this bar was OBSERVED up.
    ///
    /// 🛑 THIS EXISTS BECAUSE THE FIGHT LENGTH WAS WRONG ON EVERY DEATH (client#184). The probe's
    /// death guard returns before [`FightSampler::step`] is ever called once the player's HP hits
    /// 0, and `read_player` returns `None` through the reload -- so the sampler simply stops being
    /// ticked for the whole death cam. The next tick it does get is after the respawn, with the bar
    /// already down, and measuring to *that* tick charges the load screen to the boss.
    ///
    /// Measured, from bobler's `archipelago-2026-08-12 (3).log`: last sample `t=34.5s`, `END ...
    /// after 60.1s`. 25.6 seconds of that was a death cam and a reload -- a 74% overstatement, and
    /// it lands only on the fights the player LOST, which are the ones a difficulty complaint is
    /// about. The three fights he won were all within 0.5s of their last sample, so it reads as
    /// plausible unless you go looking at a death on purpose.
    last_seen_ms: u64,
    /// Samples emitted for the current subject.
    samples: u32,
    /// Whether [`Step::Capped`] has already fired for the current subject.
    capped: bool,
}

impl FightSampler {
    /// `const` so the probe can hold one in a `static Mutex` without a lazy init. Field-for-field
    /// identical to the derived `Default`; the two must not be allowed to disagree.
    pub const fn new() -> Self {
        Self {
            subject: None,
            started_ms: 0,
            last_sample_ms: 0,
            last_seen_ms: 0,
            samples: 0,
            capped: false,
        }
    }

    /// The boss currently being followed, if any.
    pub fn subject(&self) -> Option<i32> {
        self.subject
    }

    /// Advance one tick.
    ///
    /// `healthbar` is `GameDataMan.boss_health_bar_npc_param_id`: `None` = unreachable (HOLD),
    /// `Some(0)` = no bar up, `Some(id)` = that boss's bar is up.
    pub fn step(&mut self, now_ms: u64, healthbar: Option<i32>) -> Step {
        self.step_every(now_ms, healthbar, DEFAULT_INTERVAL_MS)
    }

    /// [`Self::step`] with an explicit period, so the cadence is testable without sleeping.
    pub fn step_every(&mut self, now_ms: u64, healthbar: Option<i32>, interval_ms: u64) -> Step {
        // 🛑 DON'T-KNOW HOLDS. See the module doc: this is `boss_grants` property 3, and reading it
        // as "no bar" would end the fight on every load stutter.
        let Some(bar) = healthbar else {
            return Step::Idle;
        };

        // A clock that went BACKWARDS is a new process or a reset `Instant`, not a fight. Restart
        // cleanly rather than emitting a saturated `elapsed_ms` forever.
        if now_ms < self.started_ms {
            self.clear();
        }

        match (bar, self.subject) {
            // No bar, and we were not following one.
            (0, None) => Step::Idle,
            // The bar we were following went away. This is the fight-length line.
            (0, Some(prev)) => self.close(prev, now_ms),
            // A bar came up and we were following nothing.
            (id, None) => {
                self.subject = Some(id);
                self.started_ms = now_ms;
                self.last_sample_ms = now_ms;
                self.last_seen_ms = now_ms;
                self.samples = 1; // the Start line IS the t=0 reading
                self.capped = false;
                Step::Start { npc_param_id: id }
            }
            // A DIFFERENT bar replaced ours. Close the old fight; the next tick opens the new one.
            // Deliberately not collapsed into one step: two events in one tick would mean either
            // dropping the End line or returning a pair, and the End line carries the fight length.
            (id, Some(prev)) if id != prev => self.close(prev, now_ms),
            // Same bar, still up: sample on the cadence.
            (id, Some(_)) => {
                // This tick SAW the bar. Recorded before the cadence check, because an observation
                // is not the same event as a sample -- ticks arrive at frame rate and samples at
                // 2 Hz, and the fight length must be measured against the former.
                self.last_seen_ms = now_ms;
                let due = self.last_sample_ms.saturating_add(interval_ms);
                if now_ms < due {
                    return Step::Idle;
                }
                let elapsed = now_ms.saturating_sub(self.started_ms);
                if self.samples >= MAX_SAMPLES_PER_FIGHT {
                    if self.capped {
                        return Step::Idle;
                    }
                    self.capped = true;
                    return Step::Capped {
                        npc_param_id: id,
                        elapsed_ms: elapsed,
                    };
                }
                // Advance the schedule by a whole period rather than to `now_ms`, so one late tick
                // does not push every later sample later. Observed in the 08-12 log as `t=47.5 ->
                // 48.1 -> 48.6`: the cadence had walked 100ms off the grid and kept it. Resync
                // outright when more than a full period behind, which is a hitch, not jitter.
                self.last_sample_ms = if now_ms.saturating_sub(due) >= interval_ms {
                    now_ms
                } else {
                    due
                };
                self.samples = self.samples.saturating_add(1);
                Step::Sample {
                    npc_param_id: id,
                    elapsed_ms: elapsed,
                }
            }
        }
    }

    fn clear(&mut self) {
        self.subject = None;
        self.started_ms = 0;
        self.last_sample_ms = 0;
        self.last_seen_ms = 0;
        self.samples = 0;
        self.capped = false;
    }

    /// Close the current fight, reporting the length we can actually vouch for.
    ///
    /// Both callers (`bar dropped` and `a different bar replaced ours`) want identical accounting;
    /// having one of them measure to `now_ms` and the other to `last_seen_ms` is precisely the
    /// class of drift this module keeps getting bitten by.
    fn close(&mut self, prev: i32, now_ms: u64) -> Step {
        let elapsed_ms = self.last_seen_ms.saturating_sub(self.started_ms);
        let unseen_ms = now_ms.saturating_sub(self.last_seen_ms);
        self.clear();
        Step::End {
            npc_param_id: prev,
            elapsed_ms,
            unseen_ms,
        }
    }
}

/// One side's HP reading. `max` of 0 means the game had not populated it yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hp {
    pub cur: i32,
    pub max: i32,
}

impl Hp {
    pub fn new(cur: i32, max: i32) -> Self {
        Self { cur, max }
    }

    /// Percent of max, floored. `0` when `max` is not positive -- never a divide by zero, and never
    /// a fabricated 100%.
    pub fn pct(&self) -> i32 {
        if self.max <= 0 {
            return 0;
        }
        // i64 so a large max cannot overflow the multiply.
        ((self.cur.max(0) as i64 * 100) / self.max as i64) as i32
    }
}

/// Everything one sample line carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// The row the boss's stats come from. 🛑 `npc_param_id`, NEVER `npc_id`: the two id spaces
    /// overlap, and keying a join on the wrong one resolves to confident wrong answers (#553).
    pub npc_param_id: i32,
    /// The 4-digit chr/model id. Carried because an enemy whose model does not belong to the
    /// `npc_param_id` it stands on is a SWAPPED enemy -- matt's rando signing its own work.
    pub npc_id: i32,
    /// The play_region bucket, i.e. the key [`crate::area_tiers::AREA_TIERS`] is keyed on. `None`
    /// when it could not be read this tick, which is stated rather than defaulted to 0.
    pub play_region: Option<i32>,
    pub boss: Hp,
    pub player: Hp,
    pub elapsed_ms: u64,
}

/// Format one line of the trace.
///
/// Deliberately one flat line per sample with every field named: these logs are read by grep months
/// later, by someone who does not have this file open. Seconds to one decimal because a 2 Hz series
/// printed in whole seconds pairs up two samples per tick value and reads like a duplicate.
pub fn format_sample(tag: &str, r: &Reading) -> String {
    format!(
        "boss-fight {tag}: t={}.{}s npc_param {} npc_id {} region {} boss {}/{} ({}%) player \
         {}/{} ({}%)",
        r.elapsed_ms / 1000,
        (r.elapsed_ms % 1000) / 100,
        r.npc_param_id,
        r.npc_id,
        r.play_region
            .map(|b| b.to_string())
            .unwrap_or_else(|| "?".into()),
        r.boss.cur,
        r.boss.max,
        r.boss.pct(),
        r.player.cur,
        r.player.max,
        r.player.pct(),
    )
}

/// How a fight ended, as far as the instrument can honestly tell.
///
/// 🛑 THE OLD `END` LINE SAID `bar down after Xs` FOR ALL FOUR CASES, which is true and useless.
/// In bobler's 08-12 log three fights were kills and one was a death, and nothing in the line that
/// summarises a fight distinguished them -- so any aggregate built on those durations silently
/// mixed wins and losses, which are the two populations a difficulty question is trying to
/// separate. The outcome was recoverable by hand from the preceding SAMPLE. That is not the same
/// as being recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The boss was OBSERVED at 0 HP. The strongest of the three: we saw it, we did not infer it.
    BossDown,
    /// The player's HP hit 0 during this fight. Inferred from the death guard tripping, which is
    /// the same signal that stops the sampler -- so it costs nothing and cannot disagree with it.
    PlayerDown,
    /// Neither. The bar went away for some other reason: fled, despawned, a cutscene, a reload, a
    /// phase change that swapped the bar, or a fight still going when the log ended.
    ///
    /// 🛑 THIS IS A REAL ANSWER AND MUST NOT BE COLLAPSED INTO `PlayerDown`. "The bar vanished and
    /// the player was on low HP" is the shape of a death AND the shape of running away, and this
    /// module does not get to guess between them. The final HP pair rides on the line so the reader
    /// can.
    Unresolved,
}

impl Outcome {
    /// The word that goes in the log line.
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::BossDown => "BOSS DOWN",
            Outcome::PlayerDown => "PLAYER DOWN",
            Outcome::Unresolved => "unresolved",
        }
    }
}

/// Classify a finished fight from the last reading we managed to take plus the death-guard latch.
///
/// `BossDown` wins a tie on purpose: an observed 0 beats an inferred one. If the boss was already
/// at 0 in the last reading, the bar came down because the boss died, and a death afterwards -- in
/// the death cam of a fight you actually won, or to a lingering hitbox -- does not change that.
pub fn classify(last_boss: Option<Hp>, player_went_down: bool) -> Outcome {
    if last_boss.is_some_and(|b| b.cur <= 0) {
        return Outcome::BossDown;
    }
    if player_went_down {
        return Outcome::PlayerDown;
    }
    Outcome::Unresolved
}

/// Format the one line that summarises a finished fight.
///
/// Same flat named-field shape as [`format_sample`], for the same reason: this is read by grep,
/// months later, by someone who does not have this file open.
pub fn format_end(
    npc_param_id: i32,
    outcome: Outcome,
    elapsed_ms: u64,
    unseen_ms: u64,
    last: Option<(Hp, Hp)>,
) -> String {
    let secs = |ms: u64| format!("{}.{}", ms / 1000, (ms % 1000) / 100);
    let tail = match last {
        Some((boss, player)) => format!(
            "last boss {}/{} ({}%) player {}/{} ({}%)",
            boss.cur,
            boss.max,
            boss.pct(),
            player.cur,
            player.max,
            player.pct(),
        ),
        // Not padding: a fight whose boss was never once readable is a different -- and worse --
        // observation than one that ended at a known HP, and the line has to be able to say so.
        None => "last unread (the boss was never found in the live sets)".to_string(),
    };
    // 🛑 THE LINE CHECKS ITSELF (client#201). The outcome and the HP pair come from two
    // independent signals -- the death-guard latch and the last remembered reading -- and until
    // this call nothing compared them, so `PLAYER DOWN ... player 414/414 (100%)` printed as an
    // ordinary result 113 samples running. The verdict is appended to the line rather than logged
    // beside it because the failure mode is a line that READS like a measurement: a reader who
    // greps one END line has to see the fault on the line they got.
    let fault = match crate::boss_fight_end_guard_replay::end_instrument_fault(outcome, last) {
        Some(reason) => format!(
            " -- {}: {reason}",
            crate::boss_fight_end_guard_replay::INSTRUMENT_FAULT
        ),
        None => String::new(),
    };
    format!(
        "boss-fight END: npc_param {npc_param_id} outcome={} t={}s unseen={}s {tail}{fault}",
        outcome.label(),
        secs(elapsed_ms),
        secs(unseen_ms),
    )
}

/// Heartbeat for [`SampleGate`]: emit an unchanged reading at least this often.
///
/// A stalemate has to stay visible. Without a heartbeat, "the player and the boss traded nothing
/// for forty seconds" and "the probe died" produce an identical gap in the log, and this repo has
/// already learned that a silent gap in a trace reads as a broken instrument.
pub const DEFAULT_HEARTBEAT_MS: u64 = 5_000;

/// Suppresses SAMPLE lines whose numbers have not moved.
///
/// # Why this exists
///
/// Boss HP only changes when the boss is hit, but the sampler runs at a fixed 2 Hz. Measured over
/// bobler's four fights on 2026-08-12: **412 SAMPLE lines carrying 36 distinct readings**
/// (115 samples / 9 values, 154 / 13, 143 / 14). Twelve identical lines for every one that says
/// something, and 412 of that session's 2715 lines.
///
/// ⭐ The cost was never really the bytes -- it was that the signal was unreadable. Fight 1
/// deduplicated is eight numbers: `3826 -> 3294 -> 3100 -> 2568 -> 2036 -> 1770 -> 1402 -> 603 ->
/// 0`, deltas `532, 194, 532, 532, 266, 368, 799, 603`, which is a visible ~266 per-hit quantum.
/// Nobody had noticed it under a hundred duplicate lines.
///
/// 🛑 THE POLL RATE IS NOT WHAT CHANGED. The sampler still steps at 2 Hz, so a burst that takes the
/// player from full to dead between two hits is still resolved -- that is why
/// [`DEFAULT_INTERVAL_MS`] is 500 and not slower, and this gate must not be mistaken for a reason
/// to raise it. Only the EMIT is conditional.
#[derive(Debug, Default)]
pub struct SampleGate {
    /// The last `(boss, player)` pair actually written to the log.
    last: Option<(Hp, Hp)>,
    /// Wall clock of that write.
    last_emit_ms: u64,
}

impl SampleGate {
    /// `const` for the same reason [`FightSampler::new`] is: it lives in a `static Mutex`.
    pub const fn new() -> Self {
        Self {
            last: None,
            last_emit_ms: 0,
        }
    }

    /// Forget everything. Called at [`Step::Start`], so a new fight always emits its `t=0` reading
    /// even when it opens on exactly the numbers the previous fight closed on.
    pub fn reset(&mut self) {
        self.last = None;
        self.last_emit_ms = 0;
    }

    /// Should this reading be written?
    pub fn admit(&mut self, now_ms: u64, boss: Hp, player: Hp) -> bool {
        self.admit_every(now_ms, boss, player, DEFAULT_HEARTBEAT_MS)
    }

    /// [`Self::admit`] with an explicit heartbeat, so the behaviour is testable without sleeping.
    pub fn admit_every(&mut self, now_ms: u64, boss: Hp, player: Hp, heartbeat_ms: u64) -> bool {
        let changed = self.last != Some((boss, player));
        // A backwards clock (process restart) must not stall the heartbeat until the clock catches
        // up again; `saturating_sub` yields 0, which is < heartbeat, so `changed` still governs.
        let stale = now_ms.saturating_sub(self.last_emit_ms) >= heartbeat_ms;
        if !changed && !stale {
            return false;
        }
        self.last = Some((boss, player));
        self.last_emit_ms = now_ms;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(elapsed_ms: u64) -> Reading {
        Reading {
            npc_param_id: 523_240_000,
            npc_id: 5240,
            play_region: Some(11050),
            boss: Hp::new(8342, 19200),
            player: Hp::new(412, 652),
            elapsed_ms,
        }
    }

    /// Tick the sampler at ~60 fps from `from_ms` to `to_ms` with the bar up, the way the client
    /// actually does, and hand back how many SAMPLE steps came out.
    ///
    /// Several tests below used to jump straight from `Start` to the closing tick. That was always
    /// unrealistic -- the probe is called every frame -- and now it is also misleading, because a
    /// fight with no observations between its ends has a real `unseen` gap and SHOULD report one.
    fn run_fight(s: &mut FightSampler, id: i32, from_ms: u64, to_ms: u64) -> u32 {
        let mut samples = 0;
        let mut t = from_ms;
        while t < to_ms {
            if let Step::Sample { .. } = s.step(t, Some(id)) {
                samples += 1;
            }
            t += 16;
        }
        // Always land the last observation exactly on `to_ms`, so a test can name the instant the
        // bar was last seen instead of whatever a 16ms stride happened to reach.
        if let Step::Sample { .. } = s.step(to_ms, Some(id)) {
            samples += 1;
        }
        samples
    }

    // -------------------------------------------------------------------------------------------
    // THE MOTIVATING CASE (rule 11). #553's acceptance is: one boss fight, probe on, produces a
    // series from which you can state the boss's max_hp, the fight length, and the player's HP
    // trace. This walks exactly that.
    // -------------------------------------------------------------------------------------------

    #[test]
    fn one_fight_produces_start_a_cadenced_series_and_a_length() {
        let mut s = FightSampler::new();
        // Idle before the fight.
        assert_eq!(s.step(0, Some(0)), Step::Idle);
        // Bar up -> Start, which IS the t=0 max_hp readback.
        assert_eq!(
            s.step(1_000, Some(11_050_800)),
            Step::Start {
                npc_param_id: 11_050_800
            }
        );
        // Frame-rate ticks between samples produce nothing.
        assert_eq!(s.step(1_016, Some(11_050_800)), Step::Idle);
        assert_eq!(s.step(1_400, Some(11_050_800)), Step::Idle);
        // ... until the period elapses.
        assert_eq!(
            s.step(1_500, Some(11_050_800)),
            Step::Sample {
                npc_param_id: 11_050_800,
                elapsed_ms: 500
            }
        );
        assert_eq!(
            s.step(2_000, Some(11_050_800)),
            Step::Sample {
                npc_param_id: 11_050_800,
                elapsed_ms: 1_000
            }
        );
        // The client keeps ticking for the rest of the fight.
        run_fight(&mut s, 11_050_800, 2_016, 94_488);
        // Bar drops -> the fight LENGTH, which is half the signal.
        assert_eq!(
            s.step(94_500, Some(0)),
            Step::End {
                npc_param_id: 11_050_800,
                elapsed_ms: 93_488,
                unseen_ms: 12,
            },
            "a fight the probe watched to the end has a length and essentially no unseen gap"
        );
        assert_eq!(s.subject(), None);
        assert_eq!(s.step(95_000, Some(0)), Step::Idle);
    }

    #[test]
    fn the_series_is_dense_enough_to_count_hits() {
        // 60 seconds of fight at the shipped cadence. #553 wants hits-to-kill and damage-per-hit
        // derivable; both need the series to resolve individual swings.
        let mut s = FightSampler::new();
        assert!(matches!(s.step(0, Some(4242)), Step::Start { .. }));
        let mut samples = 0;
        for f in 1..=3_600u64 {
            // ~60 fps
            if let Step::Sample { .. } = s.step(f * 1000 / 60, Some(4242)) {
                samples += 1;
            }
        }
        // 60_000 ms / 500 ms. The Start line at t=0 is separate from these, so the series has
        // one reading per half-second of fight and nothing is lost between them.
        assert_eq!(
            samples, 120,
            "60s at 2 Hz must yield 120 samples, got {samples}"
        );
    }

    // -------------------------------------------------------------------------------------------
    // 🛑 THE FIGHT LENGTH IS MEASURED TO THE LAST OBSERVATION -- client#184.
    // -------------------------------------------------------------------------------------------

    /// The motivating case for #184, replayed from bobler's log to the tenth of a second.
    ///
    /// Fight 2 of `archipelago-2026-08-12 (3).log`: last SAMPLE at `t=34.5s`, the player dies, and
    /// the probe is not ticked again until after the respawn -- the death guard returns before
    /// `step` is reached, and `read_player` is `None` through the reload. The old code measured to
    /// the tick that noticed the bar was gone and reported `bar down after 60.1s`.
    #[test]
    fn a_death_does_not_charge_the_load_screen_to_the_boss() {
        let mut s = FightSampler::new();
        assert!(matches!(s.step(0, Some(42_600_110)), Step::Start { .. }));
        run_fight(&mut s, 42_600_110, 16, 34_500);

        // The player dies at 34.5s. NOTHING is ticked for 25.6 seconds: death cam, YOU DIED,
        // reload, respawn. Then the first tick back finds the bar already down.
        let end = s.step(60_100, Some(0));

        assert_eq!(
            end,
            Step::End {
                npc_param_id: 42_600_110,
                elapsed_ms: 34_500,
                unseen_ms: 25_600,
            },
            "the fight is 34.5s of observed fighting plus a 25.6s blind gap -- NOT a 60.1s boss"
        );
        let Step::End {
            elapsed_ms,
            unseen_ms,
            ..
        } = end
        else {
            unreachable!()
        };
        assert_eq!(
            elapsed_ms + unseen_ms,
            60_100,
            "the two halves must still add up to the wall clock -- the old number is recoverable, \
             it is just no longer the headline"
        );
    }

    #[test]
    fn a_fight_watched_to_the_end_reports_no_meaningful_gap() {
        let mut s = FightSampler::new();
        s.step(0, Some(7));
        run_fight(&mut s, 7, 16, 57_888);
        assert_eq!(
            s.step(57_900, Some(0)),
            Step::End {
                npc_param_id: 7,
                elapsed_ms: 57_888,
                unseen_ms: 12,
            },
            "a kill is watched frame by frame, so unseen is one tick -- this is why the three \
             fights bobler WON looked correct and only the death was wrong"
        );
    }

    // -------------------------------------------------------------------------------------------
    // 🛑 DON'T-KNOW IS NOT "NO" -- boss_grants property 3.
    // -------------------------------------------------------------------------------------------

    #[test]
    fn an_unreachable_tick_holds_the_fight_rather_than_ending_it() {
        let mut s = FightSampler::new();
        s.step(0, Some(999));
        // A load stutter: GameDataMan unreachable for a stretch.
        for t in [100, 200, 300, 5_000, 9_000] {
            assert_eq!(
                s.step(t, None),
                Step::Idle,
                "None must never end a fight -- it is don't-know, not 'no bar'"
            );
        }
        assert_eq!(
            s.subject(),
            Some(999),
            "the subject must survive an unreachable stretch"
        );
        // And the elapsed clock kept running across it, rather than restarting: the bar is seen
        // again on the far side, and the fight length spans the whole thing.
        s.step(9_100, Some(999));
        assert_eq!(
            s.step(9_500, Some(0)),
            Step::End {
                npc_param_id: 999,
                elapsed_ms: 9_100,
                unseen_ms: 400,
            },
            "the clock spans the stutter (9.1s, not 0.4s) because started_ms was never reset"
        );
    }

    #[test]
    fn some_zero_is_an_answer_and_does_end_the_fight() {
        let mut s = FightSampler::new();
        s.step(0, Some(7));
        run_fight(&mut s, 7, 16, 1_984);
        assert_eq!(
            s.step(2_000, Some(0)),
            Step::End {
                npc_param_id: 7,
                elapsed_ms: 1_984,
                unseen_ms: 16,
            },
            "Some(0) IS the game saying no bar is up -- unlike None, it is a real observation"
        );
    }

    // -------------------------------------------------------------------------------------------
    // Two bars, one fight (PCR).
    // -------------------------------------------------------------------------------------------

    #[test]
    fn a_replacing_bar_closes_the_old_fight_and_opens_a_new_one() {
        let mut s = FightSampler::new();
        assert!(matches!(s.step(0, Some(100)), Step::Start { .. }));
        run_fight(&mut s, 100, 16, 29_984);
        assert_eq!(
            s.step(30_000, Some(200)),
            Step::End {
                npc_param_id: 100,
                elapsed_ms: 29_984,
                unseen_ms: 16,
            }
        );
        assert_eq!(
            s.step(30_016, Some(200)),
            Step::Start { npc_param_id: 200 },
            "the replacing bar is picked up on the very next tick"
        );
    }

    // -------------------------------------------------------------------------------------------
    // Cadence.
    // -------------------------------------------------------------------------------------------

    /// The 08-12 log shows `t=47.5 -> 48.1 -> 48.6`: the schedule had walked off the grid and kept
    /// the offset, because each sample re-based the next one on the tick that happened to deliver
    /// it. On a machine whose frame rate is what is being complained about, that loses samples.
    #[test]
    fn a_late_tick_does_not_push_every_later_sample_later() {
        let mut s = FightSampler::new();
        assert!(matches!(s.step(0, Some(1)), Step::Start { .. }));
        // Ticks arriving every 300ms -- slow, but comfortably faster than the 500ms period.
        let mut samples = 0;
        for t in (300..=2_100).step_by(300) {
            if let Step::Sample { .. } = s.step(t as u64, Some(1)) {
                samples += 1;
            }
        }
        // Grid is 500/1000/1500/2000, so four are due by t=2100. Re-basing on the delivering tick
        // yields only three (600, 1200, 1800) -- a 25% loss with no hitch in sight.
        assert_eq!(
            samples, 4,
            "the schedule must stay on the 500ms grid rather than following the ticks that \
             happened to deliver it, got {samples}"
        );
    }

    #[test]
    fn a_real_hitch_resyncs_rather_than_firing_a_burst() {
        let mut s = FightSampler::new();
        s.step(0, Some(1));
        // A 4-second freeze. If the schedule caught up by whole periods it would owe 8 samples and
        // fire them on consecutive ticks, which is a burst of duplicates, not a measurement.
        assert!(matches!(s.step(4_000, Some(1)), Step::Sample { .. }));
        assert_eq!(
            s.step(4_100, Some(1)),
            Step::Idle,
            "after resyncing, the next sample is one full period away -- no catch-up burst"
        );
        assert!(matches!(s.step(4_500, Some(1)), Step::Sample { .. }));
    }

    // -------------------------------------------------------------------------------------------
    // Bounds.
    // -------------------------------------------------------------------------------------------

    #[test]
    fn sampling_caps_once_and_then_goes_quiet_but_keeps_timing() {
        let mut s = FightSampler::new();
        s.step(0, Some(1));
        let mut samples = 0u32;
        let mut capped = 0u32;
        // Well past the cap.
        let last_tick = (MAX_SAMPLES_PER_FIGHT + 200) as u64 * DEFAULT_INTERVAL_MS;
        for i in 1..=(MAX_SAMPLES_PER_FIGHT + 200) as u64 {
            match s.step(i * DEFAULT_INTERVAL_MS, Some(1)) {
                Step::Sample { .. } => samples += 1,
                Step::Capped { .. } => capped += 1,
                _ => {}
            }
        }
        assert_eq!(
            samples,
            MAX_SAMPLES_PER_FIGHT - 1,
            "the Start line counts as the first sample, so the cap admits CAP-1 more"
        );
        assert_eq!(capped, 1, "Capped must fire exactly once, not every tick");
        // Still timing: the fight length is true even though the trace stopped. The bar was last
        // SEEN on the final tick of the loop above, so that -- not the closing tick -- is the
        // length, and the difference is stated as the gap.
        let end_at = (MAX_SAMPLES_PER_FIGHT as u64 + 400) * DEFAULT_INTERVAL_MS;
        assert_eq!(
            s.step(end_at, Some(0)),
            Step::End {
                npc_param_id: 1,
                elapsed_ms: last_tick,
                unseen_ms: end_at - last_tick,
            }
        );
    }

    #[test]
    fn a_backwards_clock_restarts_rather_than_saturating() {
        let mut s = FightSampler::new();
        s.step(50_000, Some(5));
        // Process restart / Instant reset: now_ms goes back below started_ms.
        assert_eq!(
            s.step(10, Some(5)),
            Step::Start { npc_param_id: 5 },
            "a backwards clock must reopen the fight, not report a saturated elapsed forever"
        );
        assert_eq!(
            s.step(510, Some(5)),
            Step::Sample {
                npc_param_id: 5,
                elapsed_ms: 500
            }
        );
    }

    #[test]
    fn a_fight_that_never_starts_never_samples() {
        let mut s = FightSampler::new();
        for t in 0..100u64 {
            assert_eq!(s.step(t * 1_000, Some(0)), Step::Idle);
        }
        assert_eq!(s.subject(), None);
    }

    // -------------------------------------------------------------------------------------------
    // The outcome -- client#184.
    // -------------------------------------------------------------------------------------------

    #[test]
    fn an_observed_zero_is_a_kill() {
        assert_eq!(
            classify(Some(Hp::new(0, 3826)), false),
            Outcome::BossDown,
            "bobler's fights 1, 3 and 4 all ended on an observed boss 0/N"
        );
    }

    #[test]
    fn the_death_guard_latch_is_what_names_a_death() {
        // Fight 2: the last reading the probe got was boss 626/1200, player 28/833. The player was
        // NOT observed at 0 -- they died between samples -- so the HP pair alone cannot say this.
        assert_eq!(
            classify(Some(Hp::new(626, 1200)), true),
            Outcome::PlayerDown,
            "the last sample showed the player at 28 HP, not 0; only the guard knows they died"
        );
    }

    #[test]
    fn an_observed_kill_beats_an_inferred_death() {
        assert_eq!(
            classify(Some(Hp::new(0, 1200)), true),
            Outcome::BossDown,
            "dying in the death cam of a fight you won does not un-kill the boss"
        );
    }

    #[test]
    fn a_vanished_bar_with_a_live_player_is_not_guessed_at() {
        assert_eq!(
            classify(Some(Hp::new(626, 1200)), false),
            Outcome::Unresolved,
            "fled, despawned, cutscene, reload -- all real, none of them a death"
        );
        assert_eq!(
            classify(None, false),
            Outcome::Unresolved,
            "a boss never found in the live sets tells us nothing about how it ended"
        );
    }

    #[test]
    fn a_never_read_boss_says_so_instead_of_printing_a_zero() {
        let line = format_end(4242, Outcome::Unresolved, 1_000, 0, None);
        assert!(
            line.contains("last unread"),
            "an unread boss must not be formatted as 0/0 -- that reads as a kill. Got: {line}"
        );
        assert!(
            !line.contains("0/0"),
            "and specifically must not print 0/0. Got: {line}"
        );
    }

    #[test]
    fn the_end_line_names_the_outcome_the_length_and_the_gap() {
        // Bobler's fight 2, formatted the way it should have been logged.
        let line = format_end(
            42_600_110,
            Outcome::PlayerDown,
            34_500,
            25_600,
            Some((Hp::new(626, 1200), Hp::new(28, 833))),
        );
        assert_eq!(
            line,
            "boss-fight END: npc_param 42600110 outcome=PLAYER DOWN t=34.5s unseen=25.6s \
             last boss 626/1200 (52%) player 28/833 (3%)"
        );
        assert!(
            !line.contains("60.1"),
            "the headline number must no longer be the one that included the load screen"
        );
    }

    // -------------------------------------------------------------------------------------------
    // The dedupe gate -- client#185.
    // -------------------------------------------------------------------------------------------

    /// The motivating case for #185, replayed from bobler's fight 1.
    ///
    /// 115 SAMPLE lines carried nine distinct boss HP values. Under the gate the same fight emits
    /// those nine (plus whatever the heartbeat adds), and the damage curve becomes something you
    /// can read off the log rather than something you have to write a script to recover.
    #[test]
    fn the_gate_turns_a_hundred_duplicate_lines_into_the_damage_curve() {
        const CURVE: [i32; 9] = [3826, 3294, 3100, 2568, 2036, 1770, 1402, 603, 0];
        let mut gate = SampleGate::new();
        let player = Hp::new(377, 414);
        let mut emitted = Vec::new();
        // 115 samples at 2 Hz, holding each value for a stretch the way a real fight does.
        for i in 0..115u64 {
            let boss = Hp::new(CURVE[(i as usize * CURVE.len()) / 115], 3826);
            // A heartbeat far longer than the fight, so this test measures ONLY the deduping.
            if gate.admit_every(i * 500, boss, player, u64::MAX) {
                emitted.push(boss.cur);
            }
        }
        assert_eq!(
            emitted,
            CURVE.to_vec(),
            "the gate must emit each distinct reading exactly once, in order"
        );
        assert_eq!(
            emitted.len(),
            9,
            "115 samples in, 9 lines out -- that is the whole point"
        );
    }

    #[test]
    fn the_gate_lets_every_change_through_including_the_players_side() {
        let mut gate = SampleGate::new();
        let boss = Hp::new(1200, 1200);
        assert!(gate.admit_every(0, boss, Hp::new(833, 833), u64::MAX));
        assert!(
            !gate.admit_every(500, boss, Hp::new(833, 833), u64::MAX),
            "nothing moved"
        );
        assert!(
            gate.admit_every(1_000, boss, Hp::new(284, 833), u64::MAX),
            "the PLAYER took damage -- a change on either side is a change"
        );
        assert!(
            gate.admit_every(1_500, boss, Hp::new(284, 900), u64::MAX),
            "max_hp moving is a change too: it is the readback the scaler has never had"
        );
    }

    #[test]
    fn a_stalemate_still_reports_on_the_heartbeat() {
        let mut gate = SampleGate::new();
        let boss = Hp::new(1200, 1200);
        let player = Hp::new(833, 833);
        assert!(gate.admit_every(0, boss, player, 5_000));
        assert!(!gate.admit_every(4_500, boss, player, 5_000));
        assert!(
            gate.admit_every(5_000, boss, player, 5_000),
            "an unchanged fight must not be indistinguishable from a dead probe"
        );
        assert!(
            !gate.admit_every(9_000, boss, player, 5_000),
            "and the heartbeat re-bases on the line it just wrote"
        );
        assert!(gate.admit_every(10_000, boss, player, 5_000));
    }

    #[test]
    fn a_new_fight_always_gets_its_t0_line() {
        let mut gate = SampleGate::new();
        let boss = Hp::new(1200, 1200);
        let player = Hp::new(833, 833);
        assert!(gate.admit_every(0, boss, player, u64::MAX));
        assert!(!gate.admit_every(500, boss, player, u64::MAX));
        // Same boss, re-fought at full HP after a death -- exactly bobler's fights 2 and 3. Without
        // the reset the START line of the second fight would be suppressed as a duplicate, and the
        // t=0 max_hp readback is the single most valuable line in the trace.
        gate.reset();
        assert!(
            gate.admit_every(1_000, boss, player, u64::MAX),
            "reset must let an identical opening reading through"
        );
    }

    #[test]
    fn a_backwards_clock_does_not_stall_the_gate() {
        let mut gate = SampleGate::new();
        let boss = Hp::new(1200, 1200);
        let player = Hp::new(833, 833);
        assert!(gate.admit_every(50_000, boss, player, 5_000));
        // Process restart: the clock is now far behind last_emit_ms.
        assert!(
            !gate.admit_every(10, boss, player, 5_000),
            "saturating_sub yields 0, so an unchanged reading is still suppressed"
        );
        assert!(
            gate.admit_every(20, boss, Hp::new(1100, 1200), 5_000),
            "but a real change is never suppressed, whatever the clock is doing"
        );
    }

    #[test]
    fn the_gate_const_ctor_and_default_cannot_drift() {
        let a = SampleGate::new();
        let b = SampleGate::default();
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }

    // -------------------------------------------------------------------------------------------
    // The line itself.
    // -------------------------------------------------------------------------------------------

    #[test]
    fn pct_never_divides_by_zero_and_never_fabricates_a_hundred() {
        assert_eq!(
            Hp::new(0, 0).pct(),
            0,
            "unpopulated max must read 0, not 100"
        );
        assert_eq!(Hp::new(500, 0).pct(), 0);
        assert_eq!(
            Hp::new(-40, 652).pct(),
            0,
            "overkill floors at 0, never negative"
        );
        assert_eq!(Hp::new(652, 652).pct(), 100);
        assert_eq!(Hp::new(412, 652).pct(), 63);
        // A boss max_hp far above i32/100 must not overflow the percent multiply.
        assert_eq!(Hp::new(i32::MAX, i32::MAX).pct(), 100);
    }

    #[test]
    fn the_line_names_every_field_it_carries() {
        let line = format_sample("SAMPLE", &reading(12_500));
        assert_eq!(
            line,
            "boss-fight SAMPLE: t=12.5s npc_param 523240000 npc_id 5240 region 11050 boss \
             8342/19200 (43%) player 412/652 (63%)"
        );
    }

    #[test]
    fn an_unreadable_region_says_so_rather_than_printing_zero() {
        let mut r = reading(0);
        r.play_region = None;
        let line = format_sample("START", &r);
        assert!(
            line.contains("region ?"),
            "an unread bucket must not print as 0 -- 0 is not a play_region, and a join would \
             silently miss. Got: {line}"
        );
    }

    #[test]
    fn the_const_ctor_and_default_cannot_drift() {
        // `new()` is hand-written so it can be `const`; `Default` is derived. A future field added
        // to one and not the other would be a silent divergence.
        let a = FightSampler::new();
        let b = FightSampler::default();
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }

    #[test]
    fn in_game_text_is_ascii_only() {
        // These lines go to the client log. Non-ASCII has bitten this repo before (er-archipelago:
        // toast strings are ASCII only).
        let line = format_sample("SAMPLE", &reading(1_234));
        assert!(line.is_ascii(), "log line must be ASCII: {line}");
        for outcome in [Outcome::BossDown, Outcome::PlayerDown, Outcome::Unresolved] {
            let end = format_end(1, outcome, 1_000, 0, Some((Hp::new(1, 2), Hp::new(3, 4))));
            assert!(end.is_ascii(), "END line must be ASCII: {end}");
        }
        assert!(format_end(1, Outcome::Unresolved, 0, 0, None).is_ascii());
    }
}
