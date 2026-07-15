//! Capital-version reconciler replay: burning the Erdtree must never permanently strand the
//! Royal Capital (SPEC-capital-reconciler.md).
//!
//! THE STRAND THIS MODELS (ground truth, Alaric in-game + EMEVD 2026-07-14). Leyndell is two
//! mutually exclusive map versions on one save-persisted flag, 9116: OFF = Royal m11_00
//! (Morgott + ~152 checks), ON = Ashen m11_05 + Elden Throne m19 (the finale). The swap lives
//! in the ENGINE/play-region layer — invisible to event-script grep — and vanilla only ever
//! SETS the flag (Maliketh's death). In region-lock play the Farum Azula Lock lets the player
//! kill Maliketh before clearing Royal; the finale goal sits PAST the burn, so EVERY finale
//! seed burns, and a grace warp cannot reach m11_00 while 9116 is set: the ~152 Royal checks
//! are gone for good.
//!
//! The reconciler (pure decisions in [`crate::capital`]) is two kick-watch-shaped mechanisms:
//! a warp-target intercept (decide from the TARGET before the load resolves) and a per-tick
//! latch scoped to the capital buckets, both armed only once the vanilla burn's own completion
//! latch (118, monotonic) is set.
//!
//! BREAK-IT: flip `POLICY` to `Policy::VanillaOneWay` — `royal_capital_is_stranded_by_the_one_
//! way_burn_flag` goes red with `left: 11050, right: 11000` (the Royal-grace warp landing in
//! the Ashen version — the strand), and the round-trip fails at "latch re-asserts ON".

#![cfg(test)]

use crate::capital::{
    capital_flag_state, capital_flag_state_for_warp_target, reconcile_write, warp_target_bucket,
    CapitalSets,
};

/// The policy under test for the reconciler assertions. Flip to `VanillaOneWay` to reproduce
/// the strand (see module docs).
const POLICY: Policy = Policy::Reconciler;

#[derive(Clone, Copy, PartialEq)]
enum Policy {
    /// PRE-RECONCILER: the client never writes 9116; the burn is one-way (the strand).
    VanillaOneWay,
    /// SHIPPED: warp-target intercept + per-tick latch, armed on the burn-done flag.
    Reconciler,
}

// The capital geometry, straight from the SPEC's derived data (BonfireWarpParam, 2026-07-14).
const ROYAL: i32 = 11_000; // m11_00 bucket
const ASHEN: i32 = 11_050; // m11_05 bucket (also where the burn warp lands: region 11052010)
const ROUNDTABLE: i32 = 11_100;
const LIMGRAVE: i32 = 60_000;
const ROYAL_GRACE: u32 = 11_001_950; // BonfireWarpParam row 110000
const ASHEN_GRACE: u32 = 11_051_950; // row 110500
const ROUNDTABLE_GRACE: u32 = 11_102_950; // Table of Lost Grace

/// The game as the reconciler observes it: two flags + where the player is standing.
struct Sim {
    sets: CapitalSets,
    flag_9116: bool,
    flag_118: bool,
    /// Player position, KICK bucket space (what `play_region_id / 100` reduces to).
    bucket: i32,
    /// Every 9116 write the CLIENT made (the burn's own set is not recorded here).
    client_writes: Vec<bool>,
}

impl Sim {
    fn new() -> Self {
        Sim {
            sets: CapitalSets {
                ashen: vec![11_050, 19_000],
                royal: vec![11_000],
            },
            flag_9116: false,
            flag_118: false,
            bucket: LIMGRAVE,
            client_writes: Vec::new(),
        }
    }

    fn apply(&mut self, write: Option<bool>) {
        if let Some(w) = write {
            self.flag_9116 = w;
            self.client_writes.push(w);
        }
    }

    /// Maliketh dies and the vanilla burn sequence runs end to end: 9116 ON (m13's setter),
    /// the cutscene warps the player into Ashen (region 11052010), the Royal grace warp flags
    /// are cleared, and 118 latches ON as the LAST step. 100% the game's own sequence — the
    /// arming gate keeps the client out of it.
    fn burn(&mut self) {
        self.flag_9116 = true;
        self.bucket = ASHEN;
        self.flag_118 = true;
    }

    /// A grace warp. Under the reconciler policy the warp-target intercept writes 9116 from
    /// the TARGET before the load resolves. THE ENGINE RULE (ground truth): the map version
    /// that loads is selected by 9116 at load time — a warp aimed at a Royal grace while 9116
    /// is ON cannot reach m11_00; the player comes down in the Ashen version instead.
    fn warp(&mut self, target: u32, policy: Policy) {
        if policy == Policy::Reconciler {
            let desired = capital_flag_state_for_warp_target(&self.sets, target);
            let w = reconcile_write(self.flag_118, desired, self.flag_9116);
            self.apply(w);
        }
        // The load resolves against 9116 as it now stands.
        let b = warp_target_bucket(target).unwrap_or(LIMGRAVE);
        self.bucket = if b == ROYAL && self.flag_9116 {
            ASHEN
        } else {
            b
        };
    }

    /// One settled in-world tick: the per-tick latch (scoped to the capital buckets).
    /// Exercises the 7-digit -> bucket normalization by reporting an interior play_region id.
    fn tick(&mut self, policy: Policy) {
        if policy == Policy::Reconciler {
            let pr = self.bucket * 100 + 1; // what WorldChrMan actually reports inside a map
            let desired = capital_flag_state(&self.sets, pr);
            let w = reconcile_write(self.flag_118, desired, self.flag_9116);
            self.apply(w);
        }
    }
}

/// THE STRAND, and its end. Kill Maliketh without clearing Royal (the Farum Azula Lock allows
/// it), burn, then try to go back for the ~152 Royal checks.
#[test]
fn royal_capital_is_stranded_by_the_one_way_burn_flag() {
    // Reproduction first: vanilla one-way, the warp to a Royal grace lands in the ASHEN
    // version — the checks are unreachable forever. This is the defect, pinned.
    let mut vanilla = Sim::new();
    vanilla.burn();
    vanilla.warp(ROYAL_GRACE, Policy::VanillaOneWay);
    assert_eq!(
        vanilla.bucket, ASHEN,
        "pre-reconciler reproduction: with 9116 stuck ON, a Royal-grace warp cannot reach m11_00"
    );
    assert!(
        vanilla.client_writes.is_empty(),
        "vanilla policy never writes"
    );

    // The reconciler: same timeline, the intercept writes 9116 OFF from the target before the
    // load, and the player comes down in the ROYAL capital. (BREAK-IT: POLICY = VanillaOneWay
    // fails here with left: 11050, right: 11000.)
    let mut s = Sim::new();
    s.burn();
    s.warp(ROYAL_GRACE, POLICY);
    assert_eq!(
        s.bucket, ROYAL,
        "the reconciler restores Royal: the strand is ended"
    );
    assert!(!s.flag_9116, "9116 reconciled OFF for the Royal load");
}

/// The full round trip of the SPEC's timeline: warp-to-Ashen (intercept ON) -> load ->
/// tick-in-Ashen (latch holds ON) -> warp-out (OFF) -> warp-to-Royal-grace (OFF, lands Royal)
/// -> tick-in-Royal (holds OFF).
#[test]
fn the_burn_round_trip_intercept_then_latch_then_royal_return() {
    let mut s = Sim::new();
    s.burn();
    // Warp home first (every warp anywhere but Ashen/Throne restores the Royal default)...
    s.warp(ROUNDTABLE_GRACE, POLICY);
    assert_eq!(s.bucket, ROUNDTABLE);
    assert!(!s.flag_9116, "warp-out writes OFF");
    // ...then back to the finale: the intercept writes ON before the load resolves, so the
    // player loads the correct (Ashen) version, not a replayed-burn surprise.
    s.warp(ASHEN_GRACE, POLICY);
    assert_eq!(s.bucket, ASHEN);
    assert!(
        s.flag_9116,
        "warp-to-Ashen intercept writes ON before the load"
    );
    // Standing in Ashen, something flips 9116 mid-session (the class of interference the
    // latch exists for): the next tick re-asserts it, or the map would swap on the next load.
    s.flag_9116 = false;
    s.tick(POLICY);
    assert!(s.flag_9116, "latch re-asserts ON");
    // Leave for Royal: OFF at the warp, and the latch HOLDS it off while standing there.
    s.warp(ROYAL_GRACE, POLICY);
    assert_eq!(s.bucket, ROYAL, "Royal is never permanently lost");
    s.tick(POLICY);
    assert!(!s.flag_9116, "latch holds OFF in the Royal capital");
    s.flag_9116 = true; // interference again, this time in Royal
    s.tick(POLICY);
    assert!(!s.flag_9116, "latch re-asserts OFF in Royal");
}

/// The arming gate: before the vanilla burn has completed once (118 unset), the reconciler
/// NEVER writes — pre-burn 9116-OFF is vanilla, and a write between Maliketh's death and 118
/// would fight the in-flight burn sequence.
#[test]
fn pre_burn_the_reconciler_never_writes() {
    let mut s = Sim::new();
    // Tour the whole map pre-burn, capitals included.
    for b in [LIMGRAVE, ROYAL, ROUNDTABLE] {
        s.bucket = b;
        s.tick(POLICY);
    }
    s.warp(ROYAL_GRACE, POLICY);
    s.warp(ROUNDTABLE_GRACE, POLICY);
    // Mid-burn window: Maliketh just died (9116 ON, 118 not yet) — the client must not fight
    // $Event(900) / m13's setter.
    s.flag_9116 = true;
    s.bucket = ROYAL;
    s.tick(POLICY);
    assert!(
        s.flag_9116,
        "mid-burn: the client did not fight the in-flight burn"
    );
    assert!(
        s.client_writes.is_empty(),
        "the first burn is 100% the game's own sequence: zero client writes before 118"
    );
}

/// The scoped latch: outside the capital buckets the flag is left ALONE (holding OFF globally
/// would fight m13's setter and re-trigger $Event(900)'s wait gratuitously). The next warp's
/// intercept — not the latch — is what restores the Royal default out there.
#[test]
fn outside_the_capitals_the_latch_leaves_the_flag_alone() {
    let mut s = Sim::new();
    s.burn();
    // Post-burn the player is warped into Ashen with 9116 ON. Suppose they walk/fall out into
    // a non-capital bucket without warping: the latch has no opinion there.
    s.bucket = LIMGRAVE;
    let writes_before = s.client_writes.len();
    s.tick(POLICY);
    s.tick(POLICY);
    assert!(
        s.flag_9116,
        "elsewhere -> leave alone: ON survives until a warp says otherwise"
    );
    assert_eq!(
        s.client_writes.len(),
        writes_before,
        "no gratuitous toggles"
    );
    // The next warp (anywhere non-Ashen) is what writes OFF.
    s.warp(ROUNDTABLE_GRACE, POLICY);
    assert!(
        !s.flag_9116,
        "the warp intercept, not the latch, restores the Royal default"
    );
}
