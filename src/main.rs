//! Axon — local, harness-agnostic observability for AI coding agents.
//!
//! Thin CLI over the `axon` library. M1 implements `--scan-only`: parse Claude Code logs
//! (main threads + sub-agents) → normalize → SQLite → print a JSON summary. The live server
//! and three.js dashboard arrive in M3/M4 (see DESIGN.md §14).

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;

use axon::ingest;
use axon::normalize;
use axon::pricing::Pricing;
use axon::store::Store;
use axon::summary::build_summary;

/// Axon — see DESIGN.md for the full build spec.
#[derive(Parser, Debug)]
#[command(name = "axon", version, about)]
struct Cli {
    /// Port for the local dashboard.
    #[arg(long, default_value_t = 7777)]
    port: u16,

    /// Do not open the browser on start.
    #[arg(long)]
    no_open: bool,

    /// Scan logs, print a JSON summary, then exit (no server).
    #[arg(long)]
    scan_only: bool,

    /// Optional OTLP/HTTP endpoint to ALSO export traces (requires `--features otel`).
    #[arg(long)]
    otel: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if cli.scan_only {
        return run_scan_only();
    }

    eprintln!(
        "axon {}: the live dashboard (server + 3D brain) lands in M3/M4.\n\
         Available now: `axon --scan-only` — Claude ingest -> SQLite -> JSON summary.\n\
         flags: port={} no_open={} otel={:?}",
        env!("CARGO_PKG_VERSION"),
        cli.port,
        cli.no_open,
        cli.otel
    );
    Ok(())
}

/// M1 entry point: scan `~/.claude/projects`, store events, print the JSON summary on stdout.
fn run_scan_only() -> anyhow::Result<()> {
    let pricing = load_pricing();
    let projects = claude_projects_dir();
    let turns = ingest::scan_claude_root(&projects);

    let mut events = Vec::with_capacity(turns.len());
    let mut skipped = 0usize;
    for t in &turns {
        match normalize::to_event(t, &pricing) {
            Ok(e) => events.push(e),
            Err(e) => {
                skipped += 1;
                eprintln!("axon: skipped turn {}: {e:#}", t.message_id);
            }
        }
    }
    if skipped > 0 {
        eprintln!("axon: skipped {skipped} turn(s) with unparseable timestamps");
    }

    let db = db_path();
    let mut store = Store::open(db.to_str().context("db path is not valid UTF-8")?)?;
    store.upsert_all(&events)?;

    let all = store.all_events()?;
    let summary = build_summary(&all);
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

/// Load `~/.config/axon/pricing.toml` if present, else the bundled defaults.
fn load_pricing() -> Pricing {
    let path = config_dir().join("axon").join("pricing.toml");
    if path.exists() {
        match Pricing::load(&path) {
            Ok(p) => return p,
            Err(e) => eprintln!(
                "axon: could not read {} ({e:#}); using bundled pricing defaults",
                path.display()
            ),
        }
    }
    Pricing::bundled()
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
}

fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local").join("share"))
}

fn claude_projects_dir() -> PathBuf {
    home().join(".claude").join("projects")
}

fn db_path() -> PathBuf {
    data_dir().join("axon").join("axon.db")
}
