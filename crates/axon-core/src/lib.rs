//! Axon core — shared by the `axon` dashboard and the `axon-bus` control plane.
//!
//! Pipeline: raw logs -> [`ingest`] (collapse by `message.id`, attribute sub-agents)
//! -> [`normalize`] (ISO->ms, id hash, cost) -> [`model::Event`] -> [`store`] (SQLite).

pub mod ingest;
pub mod model;
pub mod normalize;
pub mod pricing;
pub mod store;

use std::path::PathBuf;

/// The user's home: `HOME` when set, else the platform's own (USERPROFILE on Windows,
/// where PowerShell sets no `HOME`). An empty home would make every path under it
/// relative to whatever folder a process starts in, splitting one hub into several.
pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(std::env::home_dir)
        .unwrap_or_default()
}
