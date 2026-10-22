//! RocksVision: read-only inspector for RocksDB 10.4.2 WAL, SST, CURRENT and MANIFEST files.
//!
//! Layers (dependencies point down only; see docs/module-map.md):
//! `ui` (eframe shell, panels, theme, layout, platform) → `app` (state, actions, prefs, jobs) →
//! `inspect` / `present` (views) → parsers `wal` / `sst` / `manifest` → `format` (shared log
//! framing, internal keys, codecs) → `io` (bounded range reads) / `model` / `util` / `limits`.
//! `dump` is the headless text mode; `branding` holds name/version/author/logo.

pub mod app;
pub mod branding;
pub mod discover;
pub mod dump;
pub mod format;
pub mod inspect;
pub mod io;
pub mod limits;
pub mod manifest;
pub mod model;
pub mod present;
pub mod sst;
pub mod ui;
pub mod util;
pub mod wal;

/// Shim: background jobs moved to `app::jobs`.
pub use app::jobs;

#[cfg(test)]
mod arch_tests;
#[cfg(test)]
mod diag_tests;
#[cfg(test)]
mod stall_tests;
