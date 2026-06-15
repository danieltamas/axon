//! Axon — local, harness-agnostic observability for AI coding agents.
//!
//! Thin CLI over the `axon` library.
//! - `axon --scan-only` — parse Claude logs → SQLite → print a JSON summary, then exit.
//! - `axon` — same scan, then print a short CLI summary, open the browser, and serve the
//!   local dashboard (M3-lite: analytics only; the live 3D brain lands in M4).

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;

use axon::ingest;
use axon::normalize;
use axon::pricing::Pricing;
use axon::server;
use axon::store::Store;
use axon::summary::{build_summary, Summary};

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if cli.scan_only {
        return run_scan_only();
    }
    run_server(&cli).await
}

/// `--scan-only`: print the JSON summary on stdout and exit.
fn run_scan_only() -> anyhow::Result<()> {
    let summary = scan_to_summary()?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

/// Bare `axon`: scan, print a CLI summary, open the browser, and serve the dashboard.
async fn run_server(cli: &Cli) -> anyhow::Result<()> {
    let summary = scan_to_summary()?;
    print_cli_summary(&summary, cli.port);

    if let Some(otel) = &cli.otel {
        eprintln!("axon: --otel {otel} ignored (OTEL export is M6; needs --features otel)");
    }

    let url = format!("http://127.0.0.1:{}", cli.port);
    if !cli.no_open {
        if let Err(e) = webbrowser::open(&url) {
            eprintln!("axon: couldn't open a browser ({e}); open {url} yourself");
        }
    }
    println!("  serving {url} — press Ctrl-C to stop\n");

    let addr: SocketAddr = ([127, 0, 0, 1], cli.port).into();
    server::serve(addr, Arc::new(server::AppState { summary })).await
}

/// Scan `~/.claude/projects`, normalize, persist to SQLite, and aggregate.
fn scan_to_summary() -> anyhow::Result<Summary> {
    let pricing = load_pricing();
    let turns = ingest::scan_claude_root(&claude_projects_dir());

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
    Ok(build_summary(&store.all_events()?))
}

fn print_cli_summary(s: &Summary, port: u16) {
    let top_agents: Vec<&str> = s
        .by_agent
        .iter()
        .take(4)
        .map(|a| a.agent.as_str())
        .collect();
    println!("\n  Axon — local AI-agent observability\n");
    println!(
        "  Events        {} across {} sessions",
        commafy(s.events),
        commafy(s.sessions)
    );
    println!(
        "  Tokens out    {}  (in {}, cache-read {})",
        commafy(s.tokens_out),
        commafy(s.tokens_in),
        commafy(s.cache_read)
    );
    println!(
        "  Lines edited  {} added / {} removed",
        commafy(s.loc_added),
        commafy(s.loc_removed)
    );
    println!(
        "  Cost          €{:.2}  (all logged history, EUR)",
        s.cost_eur
    );
    if !top_agents.is_empty() {
        println!("  Top agents    {}", top_agents.join(", "));
    }
    if !s.unpriced_models.is_empty() {
        println!(
            "  Unpriced      {}  (cost is a floor, not exact)",
            s.unpriced_models.join(", ")
        );
    }
    println!("  Dashboard →   http://127.0.0.1:{port}");
}

/// Group a number into thousands with commas (35929 → "35,929").
fn commafy(n: u64) -> String {
    let s = n.to_string();
    let len = s.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
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

fn home() -> std::path::PathBuf {
    std::env::var_os("HOME").map(Into::into).unwrap_or_default()
}

fn config_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(Into::into)
        .unwrap_or_else(|| home().join(".config"))
}

fn data_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(Into::into)
        .unwrap_or_else(|| home().join(".local").join("share"))
}

fn claude_projects_dir() -> std::path::PathBuf {
    home().join(".claude").join("projects")
}

fn db_path() -> std::path::PathBuf {
    data_dir().join("axon").join("axon.db")
}
