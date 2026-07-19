//! `er-logic` — pure + seam-based, host-compiled, unit-tested decision logic lifted out of the
//! Windows-gated `eldenring-ap` game modules. No `eldenring` / `windows` / `archipelago_rs` / socket
//! deps, so CI runs every test on any host (alongside `er-codec` / `er-semver`).
//!
//! PURE modules (no game at all): [`receive`], [`version`], [`save_state`], [`progressive`],
//! [`region_lock`], [`options`], [`tracker`], [`capital`].
//! SEAM modules (game side effects via the [`hook::GameHook`] trait + `FakeGame` mock): [`deathlink`],
//! [`grace`], [`grants`], [`upgrades`]. The real `EldenRingHook` impl lives in `eldenring-ap`
//! (`#[cfg(windows)]`).
//!
//! See SHARED-CONVERGENCE-PLAN.md.

pub mod attunement;
pub mod attunement_replay;
pub mod auto_equip;
pub mod boss_felled;
pub mod boss_key_replay;
pub mod capital;
pub mod capital_replay;
pub mod config_reload;
pub mod config_reload_replay;
pub mod deathlink;
pub mod deathlink_gate_replay;
pub mod fast_travel;
pub mod fast_travel_replay;
pub mod flagpoll_baseline_replay;
pub mod flask_grant_replay;
pub mod flask_reconcile;
pub mod grace;
pub mod grace_flush_replay;
pub mod grants;
pub mod hook;
pub mod map_reveal_replay;
pub mod name_override;
pub mod options;
pub mod progressive;
pub mod receive;
pub mod receive_watermark_replay;
pub mod reconcile;
pub mod reconciler_replay;
pub mod region_lock;
pub mod region_lock_replay;
pub mod region_locks;
pub mod save_state;
pub mod scadu_blessing_replay;
pub mod scaling;
pub mod seed_change;
pub mod start_backfill;
pub mod start_grant_replay;
pub mod static_lots;
pub mod sweep_gate;
pub mod torrent_start_replay;
pub mod tracker;
pub mod tracker_regions;
pub mod unique_grants;
pub mod upgrade_cost;
pub mod upgrade_cost_replay;
pub mod upgrades;
pub mod upgrades_replay;
pub mod vanilla_suppress;
pub mod vanilla_suppress_replay;
pub mod version;
