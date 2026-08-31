//! The standalone Bloodborne Archipelago window.
//!
//! This crate replaces the single repainted `STATIC` control in `standalone-windows` with an
//! egui/eframe surface: a scrollable, selectable, per-line coloured activity feed under a fixed
//! header that says, in words, whether the client is delivering. The Win32 shell remains in the
//! tree behind `--legacy-window` until a live Windows session has accepted this one.
//!
//! **The seam is unchanged and must stay unchanged.** This crate reads
//! [`client_ui::HostEndpoint`] snapshots and sends [`client_ui::UiAction`] values back; it holds
//! no game process handle, performs no blocking call into the worker, and knows nothing about
//! Bloodborne. A renderer that stops responding must remain outside the delivery acknowledgement
//! path, which is what the bounded, coalescing bridge already guarantees -- so nothing here may
//! wait on the worker for anything.

pub mod view;

#[cfg(windows)]
mod app;

#[cfg(windows)]
pub mod hotkey;

#[cfg(windows)]
pub use app::{spawn, spawn_persisted};
