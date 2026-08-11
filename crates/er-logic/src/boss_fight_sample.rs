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
    /// different boss). `elapsed_ms` is the fight length.
    End { npc_param_id: i32, elapsed_ms: u64 },
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
            (0, Some(prev)) => {
                let elapsed = now_ms.saturating_sub(self.started_ms);
                self.clear();
                Step::End {
                    npc_param_id: prev,
                    elapsed_ms: elapsed,
                }
            }
            // A bar came up and we were following nothing.
            (id, None) => {
                self.subject = Some(id);
                self.started_ms = now_ms;
                self.last_sample_ms = now_ms;
                self.samples = 1; // the Start line IS the t=0 reading
                self.capped = false;
                Step::Start { npc_param_id: id }
            }
            // A DIFFERENT bar replaced ours. Close the old fight; the next tick opens the new one.
            // Deliberately not collapsed into one step: two events in one tick would mean either
            // dropping the End line or returning a pair, and the End line carries the fight length.
            (id, Some(prev)) if id != prev => {
                let elapsed = now_ms.saturating_sub(self.started_ms);
                self.clear();
                Step::End {
                    npc_param_id: prev,
                    elapsed_ms: elapsed,
                }
            }
            // Same bar, still up: sample on the cadence.
            (id, Some(_)) => {
                if now_ms.saturating_sub(self.last_sample_ms) < interval_ms {
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
                self.last_sample_ms = now_ms;
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
        self.samples = 0;
        self.capped = false;
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
        // Bar drops -> the fight LENGTH, which is half the signal.
        assert_eq!(
            s.step(94_500, Some(0)),
            Step::End {
                npc_param_id: 11_050_800,
                elapsed_ms: 93_500
            }
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
        // And the elapsed clock kept running across it, rather than restarting.
        assert_eq!(
            s.step(9_500, Some(0)),
            Step::End {
                npc_param_id: 999,
                elapsed_ms: 9_500
            }
        );
    }

    #[test]
    fn some_zero_is_an_answer_and_does_end_the_fight() {
        let mut s = FightSampler::new();
        s.step(0, Some(7));
        assert_eq!(
            s.step(2_000, Some(0)),
            Step::End {
                npc_param_id: 7,
                elapsed_ms: 2_000
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
        assert_eq!(
            s.step(30_000, Some(200)),
            Step::End {
                npc_param_id: 100,
                elapsed_ms: 30_000
            }
        );
        assert_eq!(
            s.step(30_016, Some(200)),
            Step::Start { npc_param_id: 200 },
            "the replacing bar is picked up on the very next tick"
        );
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
        // Still timing: the fight length is true even though the trace stopped.
        let end_at = (MAX_SAMPLES_PER_FIGHT as u64 + 400) * DEFAULT_INTERVAL_MS;
        assert_eq!(
            s.step(end_at, Some(0)),
            Step::End {
                npc_param_id: 1,
                elapsed_ms: end_at
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
    }
}
