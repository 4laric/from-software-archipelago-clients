//! Standalone Bloodborne Archipelago client components.

/// Exact cross-repository runtime contract. The apworld, native client, and
/// Cheat Engine harness bump this together whenever their shared shape changes.
pub const RUNTIME_BUILD: &str = "bb-0.1.0-r5";

pub mod backend;
pub mod bridge;
pub mod client_loop;
pub mod config;
pub mod event_flags;
pub mod feed;
pub mod ledger;
pub mod native;
pub mod upgrades;
