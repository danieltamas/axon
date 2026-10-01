//! Axon — local, harness-agnostic observability for AI coding agents.
//!
//! Library crate. The binary (`src/main.rs`) is a thin CLI over these modules so that
//! the M1 acceptance gates in `tests/` can exercise the real parsing/normalization code.
//!
//! Pipeline (M1): raw logs -> [`ingest`] (collapse by `message.id`, attribute sub-agents)
//! -> [`normalize`] (ISO->ms, id hash, cost) -> [`model::Event`] -> [`store`] (SQLite)
//! -> [`summary`] (the `--scan-only` JSON).
//!
//! See `DESIGN.md` for the full build spec.

// Moved to `axon-core` (shared with `axon-bus`); re-exported so `axon::<module>` paths are unchanged.
pub use axon_core::{ingest, model, normalize, pricing, store};

pub mod config;
pub mod rtk;
pub mod server;
pub mod summary;
