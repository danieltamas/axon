//! Axon — local, harness-agnostic observability for AI coding agents.
//!
//! Thin CLI over the `axon` library.
//! - `axon --scan-only` — parse Claude logs → SQLite → print a JSON summary, then exit.
//! - `axon` — same scan, then print a short CLI summary, open the browser, and serve the
//!   local dashboard (M3-lite: analytics only; the live 3D brain lands in M4).

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use chrono::Datelike;
use clap::Parser;

use axon::config::Config;
use axon::ingest;
use axon::normalize;
use axon::pricing::Pricing;
use axon::rtk;
use axon::server;
use axon::store::Store;
use axon::summary::{build_summary, windowed_cost, Summary};

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

/// How often the background task re-scans logs to keep the dashboard live.
const REFRESH_SECS: u64 = 20;

/// Bare `axon`: scan, print a CLI summary, open the browser, and serve the live dashboard.
async fn run_server(cli: &Cli) -> anyhow::Result<()> {
    let summary = scan_to_summary()?;
    print_cli_summary(&summary, cli.port);

    if let Some(otel) = &cli.otel {
        eprintln!("axon: --otel {otel} ignored (OTEL export is M6; needs --features otel)");
    }

    let state = Arc::new(server::AppState {
        summary: std::sync::RwLock::new(summary),
    });
    spawn_refresher(state.clone());

    let url = format!("http://127.0.0.1:{}", cli.port);
    if !cli.no_open {
        if let Err(e) = webbrowser::open(&url) {
            eprintln!("axon: couldn't open a browser ({e}); open {url} yourself");
        }
    }
    println!("  serving {url} — live (re-scans every {REFRESH_SECS}s) — press Ctrl-C to stop\n");

    let addr: SocketAddr = ([127, 0, 0, 1], cli.port).into();
    server::serve(addr, state).await
}

/// Re-scan logs on an interval and swap the result into shared state, so the dashboard is
/// live. The scan is blocking (fs + SQLite), so it runs on the blocking pool. (The eventual
/// M4 ideal is an incremental file-watch → SSE pipeline; this is the pragmatic version.)
fn spawn_refresher(state: Arc<server::AppState>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(REFRESH_SECS)).await;
            match tokio::task::spawn_blocking(scan_to_summary).await {
                Ok(Ok(s)) => *state.summary.write().unwrap_or_else(|p| p.into_inner()) = s,
                Ok(Err(e)) => eprintln!("axon: background re-scan failed: {e:#}"),
                Err(e) => eprintln!("axon: background re-scan task error: {e}"),
            }
        }
    });
}

/// Scan `~/.claude/projects`, normalize, persist to SQLite, and aggregate.
fn scan_to_summary() -> anyhow::Result<Summary> {
    let pricing = load_pricing();
    let mut turns = ingest::scan_claude_root(&claude_projects_dir());
    turns.extend(ingest::scan_codex_root(&codex_sessions_dir()));
    turns.extend(ingest::scan_opencode_db(&opencode_db_path()));

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
    let mut summary = build_summary(&all);
    summary.rtk = rtk::savings(); // optional; None if rtk is not installed

    // Budget caps (config) + spend in the current local day / week.
    let cfg = Config::load(&config_dir().join("axon").join("config.toml"));
    summary.budget_day_eur = cfg.budget_eur_per_day;
    summary.budget_week_eur = cfg.budget_eur_per_week;
    summary.budget_month_eur = cfg.budget_eur_per_month;
    summary.today_cost_eur = windowed_cost(&all, local_day_start_ms());
    summary.week_cost_eur = windowed_cost(&all, local_week_start_ms());
    summary.month_cost_eur = windowed_cost(&all, local_month_start_ms());
    Ok(summary)
}

/// Epoch-ms of local midnight today.
fn local_day_start_ms() -> i64 {
    day_start_ms(chrono::Local::now().date_naive())
}

/// Epoch-ms of local midnight on the most recent Monday.
fn local_week_start_ms() -> i64 {
    let today = chrono::Local::now().date_naive();
    let back = today.weekday().num_days_from_monday() as u64;
    day_start_ms(today - chrono::Days::new(back))
}

/// Epoch-ms of local midnight on the 1st of the current month.
fn local_month_start_ms() -> i64 {
    let today = chrono::Local::now().date_naive();
    day_start_ms(today.with_day(1).unwrap_or(today))
}

fn day_start_ms(date: chrono::NaiveDate) -> i64 {
    date.and_hms_opt(0, 0, 0)
        .and_then(|ndt| ndt.and_local_timezone(chrono::Local).single())
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(0)
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
        "  Cost          {}  (all logged history, EUR)",
        eur(s.cost_eur)
    );
    {
        let span = |spent: f64, cap: Option<f64>| match cap {
            Some(c) => format!("{} / {} ({:.0}%)", eur(spent), eur(c), pct(spent, c)),
            None => eur(spent),
        };
        println!(
            "  Spend         today {} · week {}",
            span(s.today_cost_eur, s.budget_day_eur),
            span(s.week_cost_eur, s.budget_week_eur)
        );
    }
    if let Some(r) = &s.rtk {
        println!(
            "  RTK saved     {} tokens ({:.1}%) over {} commands",
            compact(r.tokens_saved),
            r.saved_pct,
            commafy(r.commands)
        );
    }
    if !s.by_harness.is_empty() {
        let parts: Vec<String> = s
            .by_harness
            .iter()
            .map(|h| format!("{} {}", h.harness, eur(h.cost_eur)))
            .collect();
        println!("  Harnesses     {}", parts.join(" · "));
    }
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

/// Percent of a budget cap consumed (0 if the cap is non-positive).
fn pct(spent: f64, cap: f64) -> f64 {
    if cap > 0.0 {
        100.0 * spent / cap
    } else {
        0.0
    }
}

/// Format euros with thousands separators: 5607.61 → "€5,607.61".
fn eur(n: f64) -> String {
    let cents = (n * 100.0).round() as u64;
    format!("€{}.{:02}", commafy(cents / 100), cents % 100)
}

/// Compact large counts: 134_504_579 → "134.5M".
fn compact(n: u64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.1}B", n as f64 / 1e9),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.1}K", n as f64 / 1e3),
        n => n.to_string(),
    }
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

fn codex_sessions_dir() -> std::path::PathBuf {
    home().join(".codex").join("sessions")
}

fn opencode_db_path() -> std::path::PathBuf {
    data_dir().join("opencode").join("opencode.db")
}

fn db_path() -> std::path::PathBuf {
    data_dir().join("axon").join("axon.db")
}
