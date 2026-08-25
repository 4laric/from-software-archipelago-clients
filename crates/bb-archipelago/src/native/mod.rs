//! In-process ("native") item delivery for Bloodborne, replacing the Cheat
//! Engine file bridge.
//!
//! This is stage 2 of the CE-free client (SPEC-client-without-cheat-engine.md
//! "client-side injection"). It reads and writes shadPS4 process memory
//! directly, verifies the running image against the vendored
//! `bb-native-grant-v5` contract, installs a static grant payload atomically,
//! and drives the grant state machine the Cheat Engine table paid for live.
//!
//! Everything here is **untested against a live game** and gated behind the
//! delivery flag; the Cheat Engine bridge remains the default delivery path.
//! The pure logic (contract consumption, descriptor encoding, image
//! verification, install atomicity, the delivery state machine, the inventory
//! walk) is host-tested against fakes; the live Windows attach/install/thread
//! seams are compiled and checked by CI only and must be owner-validated
//! against a running process before the native path is trusted.
//!
//! Behaviour is ported from `tools/bb_native_delivery/` in the
//! `4laric/bb-archipelago` repo; the semantics are not reinvented.

pub mod backend;
pub mod contract;
pub mod delivery;
pub mod descriptor;
pub mod engine;
pub mod guest;
pub mod install;
pub mod mem;
pub mod threads;
