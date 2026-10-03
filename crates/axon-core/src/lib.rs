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

/// Epoch-ms lower bound of a dashboard range: `today` (local midnight), `7d`/`30d`
/// (rolling), anything else 0. Usage and tasks share it, so their totals agree.
pub fn range_start_ms(range: &str) -> i64 {
    use chrono::{Duration, Local};
    let now = Local::now();
    match range {
        "today" => now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .and_then(|d| d.and_local_timezone(Local).single())
            .map(|d| d.timestamp_millis())
            .unwrap_or(0),
        "7d" => (now - Duration::days(7)).timestamp_millis(),
        "30d" => (now - Duration::days(30)).timestamp_millis(),
        _ => 0,
    }
}
