//! Axon — local, harness-agnostic observability for AI coding agents.
//!
//! Thin CLI over the `axon` library.
//! - `axon --scan-only` — parse Claude logs → SQLite → print a JSON summary, then exit.
//! - `axon` — same scan, then print a short CLI summary, open the browser, and serve the
//!   one dashboard: projects and their agents, and usage (docs/BUS-PLAN.md §0b).
//! - `axon bus <cmd>` — the control plane (install, send, budget, hook…).

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use chrono::Datelike;
use clap::Parser;

use axon::config::Config;
use axon::ingest::{self, Source, SourceKind};
use axon::model::Event;
use axon::normalize;
use axon::pricing::Pricing;
use axon::rtk;
use axon::server;
use axon::store::Store;
use axon::summary::{build_summary, windowed_cost, Summary};

mod cli_summary;
mod stamp;
use cli_summary::print_cli_summary;
use stamp::stamp;

/// Axon — see DESIGN.md for the full build spec.
#[derive(Parser, Debug)]
#[command(
    name = "axon",
    version,
    about,
    after_help = "Control plane: `axon bus --help` (install, send, budget, hook…)."
)]
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

    /// Structure only in the agents' narrative: turns, tokens and tool names, never text.
    #[arg(long)]
    no_content: bool,

    /// Leave harness configs alone: do not wire Claude Code, Codex, OpenCode or Hermes
    /// hooks (messages, stops and budgets need them; `axon bus uninstall` removes them).
    #[arg(long)]
    no_hooks: bool,

    /// Optional OTLP/HTTP endpoint to ALSO export traces (requires `--features otel`).
    #[arg(long)]
    otel: Option<String>,
}

fn main() -> ExitCode {
    // Harness hooks run `axon bus hook …` on every tool call, so the control plane is
    // dispatched before any runtime starts or any log is scanned.
    if std::env::args_os().nth(1).is_some_and(|arg| arg == "bus") {
        let args = std::iter::once("axon bus".into()).chain(std::env::args_os().skip(2));
        return axon_bus::cli_main(args);
    }
    if std::env::args_os().nth(1).is_some_and(|arg| arg == "open") {
        return axon_bus::session::open_main(
            &db_path(),
            std::env::args_os().skip(1),
            open_in_browser,
        );
    }
    let cli = Cli::parse();
    let outcome = if cli.scan_only {
        run_scan_only()
    } else {
        tokio::runtime::Runtime::new()
            .context("start the async runtime")
            .and_then(|runtime| runtime.block_on(run_server(&cli)))
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err:?}");
            ExitCode::FAILURE
        }
    }
}

/// `--scan-only`: print the JSON summary on stdout and exit.
fn run_scan_only() -> anyhow::Result<()> {
    let (summary, _) = scan()?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn open_in_browser(link: &str) {
    // A minimal Linux session (no desktop env, no xdg-settings) has xdg-open but the
    // webbrowser crate never reaches it, so it is asked first.
    #[cfg(all(unix, not(target_os = "macos")))]
    if std::process::Command::new("xdg-open")
        .arg(link)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
    {
        return;
    }
    if let Err(e) = webbrowser::open(link) {
        eprintln!("axon: couldn't open a browser ({e}); open the link yourself");
    }
}

/// Bare `axon`: serve the live dashboard at once and open the browser; the first scan
/// runs in the background and prints the CLI summary when it lands.
async fn run_server(cli: &Cli) -> anyhow::Result<()> {
    if let Some(otel) = &cli.otel {
        eprintln!("axon: --otel {otel} ignored (OTEL export is M6; needs --features otel)");
    }

    let state = Arc::new(server::AppState {
        summary: std::sync::RwLock::new(build_summary(&[])),
        events: std::sync::RwLock::new(Vec::new()),
    });

    let addr: SocketAddr = ([127, 0, 0, 1], cli.port).into();
    let db = db_path();
    axon_bus::init(&db)?;
    // Everything slower than this runs after the port is open; `open --print` already resolves it.
    axon_bus::session::remember_port(&db, cli.port)?;
    let listener = server::bind(addr).await?;
    // Installing Axon is the whole setup: each harness is wired to this binary once.
    if !cli.no_hooks {
        axon_bus::ensure_hooks();
    }
    let (dashboard, federation) = axon_bus::serve::router(&db, cli.port, !cli.no_content)?;
    let link = axon_bus::session::login_link(&db, cli.port)?;
    println!("Dashboard: {link}");
    spawn_refresher(state.clone());
    if !cli.no_open {
        if axon_bus::session::app_takes_over(&db) {
            println!("  the installed Axon app reconnects on its own");
        } else {
            // An opener that blocks must not hold the dashboard back.
            let link = link.clone();
            std::thread::spawn(move || open_in_browser(&link));
        }
    }
    println!("  live (file-watch) — press Ctrl-C to stop\n");
    federation.restart().await;
    let served = server::serve(listener, state, &db, dashboard).await;
    federation.shutdown().await;
    served
}

/// Keep the dashboard live by re-scanning whenever a log file changes (via `notify`
/// file-watch), debounced, with a 15s periodic fallback. The scan is blocking (fs + SQLite)
/// so it runs on the blocking pool — it never stalls the server, and the browser (a separate
/// process) keeps animating at 60fps regardless. A min-gap caps re-scan frequency under load.
/// The first pass runs at once and prints the CLI summary.
fn spawn_refresher(state: Arc<server::AppState>) {
    use std::time::Duration;
    let trigger = Arc::new(tokio::sync::Notify::new());
    let poke = trigger.clone();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            poke.notify_one();
        }
    });

    tokio::spawn(async move {
        // Hold the watcher for the task's lifetime; register the log roots.
        let mut watcher = watcher.ok();
        if let Some(w) = watcher.as_mut() {
            use notify::{RecursiveMode, Watcher};
            let mut roots = vec![claude_projects_dir(), codex_sessions_dir(), opencode_dir()];
            roots.extend(ccflare_watch_dirs());
            for path in roots {
                let _ = w.watch(&path, RecursiveMode::Recursive);
            }
        }
        let mut first = true;
        loop {
            if !first {
                tokio::select! {
                    _ = trigger.notified() => {}                              // a log changed
                    _ = tokio::time::sleep(Duration::from_secs(15)) => {}      // periodic fallback
                }
                tokio::time::sleep(Duration::from_millis(600)).await; // debounce a write burst
            }
            match tokio::task::spawn_blocking(scan).await {
                Ok(Ok((s, ev))) => {
                    if std::mem::take(&mut first) {
                        print_cli_summary(&s);
                    }
                    *state.summary.write().unwrap_or_else(|p| p.into_inner()) = s;
                    *state.events.write().unwrap_or_else(|p| p.into_inner()) = ev;
                }
                Ok(Err(e)) => eprintln!("axon: re-scan failed: {e:#}"),
                Err(e) => eprintln!("axon: re-scan task error: {e}"),
            }
            tokio::time::sleep(Duration::from_secs(2)).await; // min gap between re-scans
        }
    });
}

/// Read the logs that changed since the last scan, normalize, persist to SQLite, and
/// aggregate. Returns the all-time summary plus the raw events (so the server can
/// re-aggregate for a selected time range).
fn scan() -> anyhow::Result<(Summary, Vec<Event>)> {
    let pricing = load_pricing();
    let mut sources = ingest::claude_sources(&claude_projects_dir());
    sources.extend(ingest::codex_sources(&codex_sessions_dir()));
    sources.push(Source {
        kind: SourceKind::OpenCode,
        path: opencode_db_path(),
    });
    sources.extend(ccflare_db_paths().into_iter().map(|path| Source {
        kind: SourceKind::Ccflare,
        path,
    }));

    let db = db_path();
    let mut store = Store::open(db.to_str().context("db path is not valid UTF-8")?)?;
    // A new parser or new prices mean every stored turn is read and priced again.
    let fingerprint = format!(
        "{}\n{}\n{}\n{}",
        env!("CARGO_PKG_VERSION"),
        ingest::PARSER_VERSION,
        axon::pricing::BUNDLED,
        std::fs::read_to_string(pricing_path()).unwrap_or_default()
    );
    let known = store.source_stamps(&fingerprint)?;

    let mut events = Vec::new();
    let mut stamps = Vec::new();
    let mut skipped = 0usize;
    for source in &sources {
        // Stamped before the read: a write that lands during it changes the stamp again.
        let Some(stamp) = stamp(source) else { continue };
        let key = source.path.to_string_lossy().into_owned();
        if known.get(&key) == Some(&stamp) {
            continue;
        }
        // An unreadable source keeps its old stamp, so the next scan tries it again.
        let Some(turns) = source.parse() else {
            continue;
        };
        for t in turns {
            match normalize::to_event(&t, &pricing) {
                Ok(e) => events.push(e),
                Err(e) => {
                    skipped += 1;
                    eprintln!("axon: skipped turn {}: {e:#}", t.message_id);
                }
            }
        }
        stamps.push((key, stamp));
    }
    if skipped > 0 {
        eprintln!("axon: skipped {skipped} turn(s) with unparseable timestamps");
    }
    store.record_scan(&events, &stamps)?;

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
    Ok((summary, all))
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

/// Load `~/.config/axon/pricing.toml` if present, else the bundled defaults.
fn pricing_path() -> std::path::PathBuf {
    config_dir().join("axon").join("pricing.toml")
}

fn load_pricing() -> Pricing {
    let path = pricing_path();
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

fn config_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(Into::into)
        .unwrap_or_else(|| axon_core::home().join(".config"))
}

fn data_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(Into::into)
        .unwrap_or_else(|| axon_core::home().join(".local").join("share"))
}

fn claude_projects_dir() -> std::path::PathBuf {
    axon_core::home().join(".claude").join("projects")
}

fn codex_sessions_dir() -> std::path::PathBuf {
    axon_core::home().join(".codex").join("sessions")
}

fn opencode_dir() -> std::path::PathBuf {
    data_dir().join("opencode")
}

fn opencode_db_path() -> std::path::PathBuf {
    opencode_dir().join("opencode.db")
}

/// Candidate ccflare-family proxy DBs, in priority order: explicit env overrides first, then
/// the better-ccflare and ccflare defaults. Each existing one is scanned; missing ones are
/// skipped. Duplicates (same canonical path) are de-duped so a request is never counted twice.
fn ccflare_db_paths() -> Vec<std::path::PathBuf> {
    let mut raw: Vec<std::path::PathBuf> = Vec::new();
    for var in ["BETTER_CCFLARE_DB_PATH", "CCFLARE_DB_PATH"] {
        if let Some(p) = std::env::var_os(var) {
            raw.push(p.into());
        }
    }
    raw.push(
        config_dir()
            .join("better-ccflare")
            .join("better-ccflare.db"),
    );
    raw.push(config_dir().join("ccflare").join("ccflare.db"));
    raw.push(data_dir().join("ccflare").join("ccflare.db"));

    let mut seen = std::collections::HashSet::new();
    raw.into_iter()
        .filter(|p| {
            let key = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            seen.insert(key)
        })
        .collect()
}

/// Parent dirs of the ccflare candidate DBs, for the live file-watch.
fn ccflare_watch_dirs() -> Vec<std::path::PathBuf> {
    ccflare_db_paths()
        .iter()
        .filter_map(|p| p.parent().map(|d| d.to_path_buf()))
        .collect()
}

fn db_path() -> std::path::PathBuf {
    axon_core::store::default_path()
}

#[cfg(test)]
mod acceptance_review2 {
    use super::*;

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "axon-review2-{}-{time}-{unique}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_at(path: &std::path::Path, body: &str, seconds: u64) {
        std::fs::write(path, body).unwrap();
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
            .unwrap();
    }

    #[test]
    fn r7_subagent_meta_change_invalidates_stamp() {
        let temp = Scratch::new();
        let source = Source {
            kind: SourceKind::ClaudeSubagent,
            path: temp.0.join("agent.jsonl"),
        };
        write_at(&source.path, "{}\n", 1_700_000_000);
        let meta = source.path.with_extension("meta.json");
        write_at(&meta, r#"{"agentType":"reviewer"}"#, 1_700_000_001);
        let before = stamp(&source).unwrap();
        write_at(&meta, r#"{"agentType":"engineer"}"#, 1_700_000_002);
        assert_ne!(stamp(&source).unwrap(), before);
    }

    #[test]
    fn r7_meta_change_older_than_transcript_invalidates_stamp() {
        let temp = Scratch::new();
        let source = Source {
            kind: SourceKind::ClaudeSubagent,
            path: temp.0.join("agent.jsonl"),
        };
        write_at(&source.path, "{}\n", 1_700_000_100);
        let meta = source.path.with_extension("meta.json");
        write_at(&meta, r#"{"agentType":"reviewer"}"#, 1_700_000_001);
        let before = stamp(&source).unwrap();
        write_at(&meta, r#"{"agentType":"engineer"}"#, 1_700_000_002);
        assert_ne!(
            stamp(&source).unwrap(),
            before,
            "sidecar changed although transcript mtime is greater"
        );
    }

    #[test]
    fn r7_metadata_preserving_rewrite_invalidates_stamp() {
        let temp = Scratch::new();
        let source = Source {
            kind: SourceKind::ClaudeMain,
            path: temp.0.join("session.jsonl"),
        };
        write_at(&source.path, "{\"tokens\":1}\n", 1_700_000_000);
        let before = stamp(&source).unwrap();
        write_at(&source.path, "{\"tokens\":2}\n", 1_700_000_000);
        assert_ne!(
            stamp(&source).unwrap(),
            before,
            "same size and mtime do not imply same content"
        );
    }

    #[test]
    fn r7_wal_and_db_size_changes_do_not_cancel() {
        let temp = Scratch::new();
        let source = Source {
            kind: SourceKind::Ccflare,
            path: temp.0.join("source.db"),
        };
        let wal = temp.0.join("source.db-wal");
        write_at(&source.path, "AAAA", 1_700_000_010);
        write_at(&wal, "BBBBBBBB", 1_700_000_009);
        let before = stamp(&source).unwrap();
        write_at(&source.path, "AAAAAAAA", 1_700_000_010);
        write_at(&wal, "BBBB", 1_700_000_010);
        assert_ne!(
            stamp(&source).unwrap(),
            before,
            "each file's identity must contribute independently"
        );
    }
}
