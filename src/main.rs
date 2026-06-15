//! Axon — local, harness-agnostic observability for AI coding agents.
//!
//! STATUS: scaffold only. This compiles and parses the intended CLI, but does nothing yet.
//! The build spec is in DESIGN.md. First task: milestone M1 (§14) —
//! Claude Code ingest (incl. sub-agents) -> SQLite -> `--scan-only`,
//! gated by the fixtures in `tests/fixtures/`.

use clap::Parser;

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

fn main() {
    let cli = Cli::parse();
    eprintln!(
        "axon {}: scaffold only — not yet implemented.\n\
         Build spec: DESIGN.md. Start at M1 (§14): Claude ingest -> SQLite -> --scan-only.\n\
         flags: port={} no_open={} scan_only={} otel={:?}",
        env!("CARGO_PKG_VERSION"),
        cli.port,
        cli.no_open,
        cli.scan_only,
        cli.otel
    );
}
