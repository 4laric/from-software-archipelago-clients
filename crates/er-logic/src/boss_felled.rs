//! Boss "Felled" trophy tracking — mode A (SPEC-boss-lock-tracker.md, BOSS_FELLED).
//!
//! Pure decision seam, same discipline as [`crate::sweep_gate`]: no I/O, no game deps, every
//! function deterministic over its arguments. The apworld now emits a `bossLockItems` map
//! (`{ str(boss_flag): {"name":"Felled: <Boss>", "region":<str>, "boss_ap_id":<int>} }`,
//! base-game bosses only); the client parses it into a static table and feeds it here as
//! [`BossDef`] rows plus two membership closures (boss-defeat flag set? / item name received?).
//!
//! MODE A ("Felled"): a boss earns a synthetic *Felled* trophy the moment its defeat event flag
//! flips set. There is no gating item; the trophy is a pure reflection of the kill flag, so the
//! tracker can render a "Bosses" group without any new AP item existing in the seed's pool.
//!
//! MODE B ("Released"): forward-compat with [`crate::sweep_gate`]. When a future seed maps a boss
//! to a "Boss Key: <Boss>" gate name (via `sweepLockGates`, already parsed for the sweep loop), a
//! felled boss additionally becomes *Released* once that key is in the cumulative received set.
//! This composes with — never fights — the sweep gate: the sweep loop still decides whether member
//! locations fire; this module only decides how the boss RENDERS (Locked / Felled / Released).
//!
//! Wiring (done separately — do NOT edit those files here):
//!  - `pub mod boss_felled;` in `crate` `lib.rs` (added serially alongside the other `pub mod`s).
//!  - CALL SITE: `Core::update_live` in `crates/eldenring-archipelago/src/core.rs`, section
//!    "5b. Flag-poll" (fn starts line ~283; boss/dungeon-sweep block ~944), where boss-defeat
//!    event flags are already read via `crate::flags::get_event_flag(flag)` right beside the
//!    existing `er_logic::sweep_gate::gate_open(...)` call. That poll should (a) call
//!    [`newly_felled`] with the per-boss previous/current flag state to emit the one-shot
//!    "Felled: <name>" overlay banner, and (b) call [`build_boss_group`] for the tracker window
//!    (`Core::render_tracker_window`, core.rs line ~1207) to draw the Bosses group.

use std::collections::BTreeMap;

/// Render state of a single boss in the tracker's "Bosses" group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BossState {
    /// Defeat flag unset — boss still alive.
    Locked,
    /// Defeat flag set (mode A trophy). No gate, or gate present but its key not yet received.
    Felled,
    /// Defeat flag set AND the boss's gate key received (mode B). Strictly beyond [`Self::Felled`].
    Released,
}

/// Decide a boss's render state from its defeat flag and (optional) mode-B gate.
///
///  - `flag_set` — is the boss's defeat event flag set this tick.
///  - `gate` — the boss's mode-B "Boss Key: <Boss>" item name, or `None` for a pure mode-A boss
///    (the common case today; the apworld emits no gate for `bossLockItems`).
///  - `received` — membership test over ALL received item names (cumulative, reconnect-replayed),
///    the same closure shape [`crate::sweep_gate::gate_open`] takes so one call site can share it.
///
/// Rules: `!flag_set => Locked`; `flag_set && gate.is_none() => Felled`;
/// `flag_set && gate == Some(key) => if received(key) { Released } else { Felled }`.
pub fn boss_state<F: Fn(&str) -> bool>(
    flag_set: bool,
    gate: Option<&str>,
    received: F,
) -> BossState {
    if !flag_set {
        return BossState::Locked;
    }
    match gate {
        None => BossState::Felled,
        Some(key) => {
            if received(key) {
                BossState::Released
            } else {
                BossState::Felled
            }
        }
    }
}

/// Edge detector for the one-shot "Felled: <name>" banner: `true` only on the unset->set
/// transition of a boss's defeat flag. Idempotent-safe for reconnect replay — when `prev_set` is
/// already `true` (the flag was set in a prior session and re-seen on reconnect), this is `false`,
/// so the banner never re-fires. `true->false` (never expected from a monotonic kill flag) is also
/// `false`, so a spurious flag drop can't fire a banner either.
pub fn newly_felled(prev_set: bool, now_set: bool) -> bool {
    !prev_set && now_set
}

/// Static per-boss definition, injected by the caller from the parsed `bossLockItems` slot_data.
/// This module holds no table itself (same data-free stance as [`crate::tracker`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BossDef {
    /// The boss-defeat event flag (map key in `bossLockItems`).
    pub flag: u32,
    /// Display name, e.g. `"Felled: Godrick the Grafted"` (the apworld ships the full label).
    pub name: String,
    /// Region display name for grouping in the tracker.
    pub region: String,
    /// AP item id for the boss (mode-B forward-compat / cross-referencing). `0` when unused.
    pub boss_ap_id: i64,
    /// Mode-B gate item name (`"Boss Key: <Boss>"`), or `None` for a pure mode-A boss.
    pub gate: Option<String>,
    /// Legible vanilla lock name to SHOW in notifications instead of the synthetic `gate`
    /// (`"Academy Glintstone Key"` where `gate == Some("Boss Key: Rennala")`). Present only when a
    /// real vanilla key exists for this lock (`bossLockItems.display_key`, boss_keys ON). Naming
    /// only — fill and gating still key the synthetic `gate` name.
    pub display_key: Option<String>,
}

impl BossDef {
    /// The label to SHOW the player for this boss's mode-B gate key: the legible vanilla name
    /// (`display_key`) when the apworld supplied one, else the raw synthetic `gate`
    /// (`"Boss Key: <Boss>"`). `None` for a pure mode-A boss (no gate at all).
    pub fn gate_display(&self) -> Option<&str> {
        self.display_key.as_deref().or(self.gate.as_deref())
    }
}

/// One rendered row in the Bosses group — a [`BossDef`] resolved to a concrete [`BossState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BossRow {
    pub flag: u32,
    pub name: String,
    pub region: String,
    pub boss_ap_id: i64,
    /// Legible gate label for this boss (see [`BossDef::gate_display`]); `None` for a mode-A boss.
    pub display_key: Option<String>,
    pub state: BossState,
}

/// Aggregated snapshot the tracker's Bosses group renders from — counts for the header line plus
/// per-boss rows for the expanded node. Pure data, no game state (mirrors [`crate::tracker`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BossGroup {
    /// Bosses still alive (flag unset).
    pub locked: usize,
    /// Bosses felled but not (yet) released — includes every mode-A boss the moment it dies.
    pub felled: usize,
    /// Bosses felled AND released (mode B).
    pub released: usize,
    /// One row per boss, sorted by defeat flag for a stable render order.
    pub rows: Vec<BossRow>,
}

impl BossGroup {
    /// Total bosses tracked.
    pub fn total(&self) -> usize {
        self.rows.len()
    }

    /// Bosses whose defeat flag is set (felled or released) — the "defeated" progress numerator.
    pub fn defeated(&self) -> usize {
        self.felled + self.released
    }

    /// Every tracked boss is at least felled — the group's "complete" filter key.
    pub fn complete(&self) -> bool {
        self.locked == 0 && !self.rows.is_empty()
    }
}

/// Build the Bosses-group snapshot the tracker renders once per frame.
///
///  - `defs` — the injected static boss table (from parsed `bossLockItems`).
///  - `flag_set` — membership test: is this boss-defeat flag currently set (client reads the live
///    event flags each poll).
///  - `received` — membership test over the cumulative received item-name set (mode-B gate keys).
///
/// Rows come out sorted by defeat flag for a stable tree order; counts are folded in one pass.
pub fn build_boss_group<S, R>(defs: &[BossDef], flag_set: S, received: R) -> BossGroup
where
    S: Fn(u32) -> bool,
    R: Fn(&str) -> bool,
{
    // BTreeMap keyed by flag => rows come out flag-sorted for free (and dedups repeated flags,
    // keeping the last def — a safe under-enforcement matching tracker.rs's stance).
    let mut by_flag: BTreeMap<u32, BossRow> = BTreeMap::new();
    for def in defs {
        let state = boss_state(flag_set(def.flag), def.gate.as_deref(), &received);
        by_flag.insert(
            def.flag,
            BossRow {
                flag: def.flag,
                name: def.name.clone(),
                region: def.region.clone(),
                boss_ap_id: def.boss_ap_id,
                display_key: def.display_key.clone(),
                state,
            },
        );
    }

    let rows: Vec<BossRow> = by_flag.into_values().collect();
    let (mut locked, mut felled, mut released) = (0usize, 0usize, 0usize);
    for row in &rows {
        match row.state {
            BossState::Locked => locked += 1,
            BossState::Felled => felled += 1,
            BossState::Released => released += 1,
        }
    }

    BossGroup {
        locked,
        felled,
        released,
        rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn def(flag: u32, name: &str, region: &str, gate: Option<&str>) -> BossDef {
        BossDef {
            flag,
            name: name.to_string(),
            region: region.to_string(),
            boss_ap_id: flag as i64,
            gate: gate.map(|g| g.to_string()),
            display_key: None,
        }
    }

    // ---- gate_display (legible key label) --------------------------------------------------

    #[test]
    fn gate_display_prefers_legible_key_then_falls_back_then_none() {
        // display_key present -> show the vanilla key name, never the synthetic.
        let mut d = def(
            5000,
            "Felled: Rennala",
            "Liurnia of the Lakes",
            Some("Boss Key: Rennala"),
        );
        d.display_key = Some("Academy Glintstone Key".to_string());
        assert_eq!(d.gate_display(), Some("Academy Glintstone Key"));
        // gate but no display_key -> fall back to the synthetic "Boss Key: <Boss>".
        let d2 = def(4000, "Felled: Margit", "Limgrave", Some("Boss Key: Margit"));
        assert_eq!(d2.gate_display(), Some("Boss Key: Margit"));
        // pure mode-A boss (no gate) -> None.
        let d3 = def(6000, "Felled: Godrick", "Stormveil Castle", None);
        assert_eq!(d3.gate_display(), None);
    }

    // ---- boss_state ------------------------------------------------------------------------

    #[test]
    fn locked_when_flag_unset() {
        let r = set(&[]);
        // Even with a gate key already held, an unset flag is Locked — the kill gates everything.
        assert_eq!(
            boss_state(false, None, |n| r.contains(n)),
            BossState::Locked
        );
        assert_eq!(
            boss_state(false, Some("Boss Key: Godrick"), |n| r.contains(n)),
            BossState::Locked
        );
    }

    #[test]
    fn felled_on_set_without_gate() {
        let r = set(&[]);
        // Mode A: flag set, no gate => Felled, no item required.
        assert_eq!(boss_state(true, None, |n| r.contains(n)), BossState::Felled);
    }

    #[test]
    fn released_only_when_gate_key_received() {
        // Mode B: flag set but key not received => still Felled (not Released).
        let r = set(&["Boss Key: Rennala"]);
        assert_eq!(
            boss_state(true, Some("Boss Key: Godrick"), |n| r.contains(n)),
            BossState::Felled
        );
        // Key now in the received set => Released.
        let r2 = set(&["Boss Key: Godrick"]);
        assert_eq!(
            boss_state(true, Some("Boss Key: Godrick"), |n| r2.contains(n)),
            BossState::Released
        );
    }

    // ---- newly_felled ----------------------------------------------------------------------

    #[test]
    fn newly_felled_fires_on_rising_edge_only() {
        assert!(newly_felled(false, true)); // the kill this tick -> banner fires once
        assert!(!newly_felled(false, false)); // still alive -> no banner
        assert!(!newly_felled(true, true)); // reconnect replay: already set -> stays false
        assert!(!newly_felled(true, false)); // spurious drop -> no banner
    }

    // ---- build_boss_group ------------------------------------------------------------------

    #[test]
    fn group_counts_and_sorts_rows_by_flag() {
        let defs = vec![
            def(
                6000,
                "Felled: Godrick the Grafted",
                "Stormveil Castle",
                None,
            ),
            def(4000, "Felled: Margit", "Limgrave", None),
            def(
                5000,
                "Felled: Rennala",
                "Raya Lucaria Academy",
                Some("Boss Key: Rennala"),
            ),
        ];
        // Godrick (6000) and Margit (4000) are dead; Rennala (5000) is alive.
        let dead: HashSet<u32> = [6000u32, 4000u32].iter().copied().collect();
        // Rennala's key is held, but since she's not dead she stays Locked, not Released.
        let received = set(&["Boss Key: Rennala"]);

        let g = build_boss_group(&defs, |f| dead.contains(&f), |n| received.contains(n));

        assert_eq!(g.total(), 3);
        assert_eq!((g.locked, g.felled, g.released), (1, 2, 0));
        assert_eq!(g.defeated(), 2);
        assert!(!g.complete());
        // Rows are flag-sorted: 4000, 5000, 6000.
        let flags: Vec<u32> = g.rows.iter().map(|r| r.flag).collect();
        assert_eq!(flags, vec![4000, 5000, 6000]);
        // Margit felled, Rennala locked, Godrick felled.
        assert_eq!(g.rows[0].state, BossState::Felled);
        assert_eq!(g.rows[1].state, BossState::Locked);
        assert_eq!(g.rows[2].state, BossState::Felled);
    }

    #[test]
    fn group_marks_released_when_dead_and_key_received() {
        let defs = vec![def(
            5000,
            "Felled: Rennala",
            "Raya Lucaria Academy",
            Some("Boss Key: Rennala"),
        )];
        let dead: HashSet<u32> = [5000u32].iter().copied().collect();
        let received = set(&["Boss Key: Rennala"]);
        let g = build_boss_group(&defs, |f| dead.contains(&f), |n| received.contains(n));
        assert_eq!((g.locked, g.felled, g.released), (0, 0, 1));
        assert_eq!(g.rows[0].state, BossState::Released);
        // A dead, key-received boss is beyond felled but the group is still complete (no locked).
        assert!(g.complete());
    }

    #[test]
    fn empty_defs_is_empty_group_not_complete() {
        let g = build_boss_group(&[], |_| true, |_| true);
        assert_eq!(g.total(), 0);
        assert_eq!((g.locked, g.felled, g.released), (0, 0, 0));
        assert_eq!(g.defeated(), 0);
        // No bosses => nothing to complete (complete() is false on an empty group).
        assert!(!g.complete());
        assert!(g.rows.is_empty());
    }

    #[test]
    fn all_felled_group_is_complete() {
        let defs = vec![
            def(4000, "Felled: Margit", "Limgrave", None),
            def(
                6000,
                "Felled: Godrick the Grafted",
                "Stormveil Castle",
                None,
            ),
        ];
        let g = build_boss_group(&defs, |_| true, |_| false);
        assert_eq!((g.locked, g.felled, g.released), (0, 2, 0));
        assert!(g.complete());
    }

    #[test]
    fn duplicate_flag_keeps_last_def_and_counts_once() {
        // Two defs sharing a flag (table lag / dup) collapse to one row, not two.
        let defs = vec![
            def(4000, "Felled: Margit (old label)", "Limgrave", None),
            def(4000, "Felled: Margit, the Fell Omen", "Limgrave", None),
        ];
        let g = build_boss_group(&defs, |_| false, |_| false);
        assert_eq!(g.total(), 1);
        assert_eq!(g.locked, 1);
        assert_eq!(g.rows[0].name, "Felled: Margit, the Fell Omen");
    }
}
