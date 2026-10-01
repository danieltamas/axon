//! The command line (clap definitions); `main` dispatches it.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::{budget, replay, route, virtual_agent};

#[derive(Parser)]
#[command(
    name = "axon-bus",
    version,
    about = "Axon's control plane for coding agents"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum Harness {
    Claude,
    Codex,
    Opencode,
    Hermes,
}

impl Harness {
    pub const ALL: [Harness; 4] = [
        Harness::Claude,
        Harness::Codex,
        Harness::Opencode,
        Harness::Hermes,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Opencode => "opencode",
            Harness::Hermes => "hermes",
        }
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// Create or migrate the shared database.
    Init,
    /// Register an idle agent.
    Register {
        #[arg(long)]
        id: String,
        #[arg(long)]
        harness: Harness,
        #[arg(long)]
        session: String,
        #[arg(long)]
        cwd: String,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
    },
    /// Pick the harness, model and effort for a new agent from routes.toml.
    Route {
        task: String,
        #[arg(long)]
        role: Option<String>,
        /// Remaining budget, e.g. `0.5usd`; a lane it cannot cover steps down one lane.
        #[arg(long, value_parser = route::parse_usd)]
        budget: Option<f64>,
        /// A captured advisor proposal (JSON), logged as a shadow beside the rule.
        #[arg(long)]
        advisor_response: Option<PathBuf>,
    },
    /// Start an agent; for now only a virtual agent's registration (§5).
    Spawn {
        task: String,
        #[arg(long = "virtual", required = true)]
        is_virtual: bool,
        /// Register the node and its children without launching anything.
        #[arg(long, required = true)]
        register_only: bool,
        #[arg(long)]
        parent: String,
        /// Comma-separated harness:model members.
        #[arg(long, value_delimiter = ',', required = true, value_parser = virtual_agent::parse_member)]
        panel: Vec<virtual_agent::Member>,
        #[arg(long, value_parser = virtual_agent::parse_member)]
        judge: virtual_agent::Member,
    },
    /// Handle one harness hook; the payload arrives on stdin.
    Hook { harness: String, event: String },
    /// Send one message along an edge; prints {id, thread}.
    Send {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        body: String,
        #[arg(long)]
        thread: Option<String>,
        /// Content by reference, e.g. `src/main.rs:L10-40@abc123`.
        #[arg(long = "ref")]
        refs: Vec<String>,
    },
    /// Ask a question and wait for its answer; prints {body, timed_out, thread}.
    Ask {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        body: String,
        /// Seconds to wait before returning the default.
        #[arg(long)]
        wait: u64,
        #[arg(long)]
        default: String,
        #[arg(long)]
        thread: Option<String>,
    },
    /// Answer a question addressed to you.
    Reply {
        question: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        body: String,
    },
    /// Propose a root-to-root link.
    Link {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
    },
    /// The agent playbook: what Axon is and how to message, link, claim and stop.
    Guide,
    /// Whom an agent can message now, and which sessions in its repo it could link with.
    Peers {
        #[arg(long)]
        agent: String,
    },
    /// Accept the link another root proposed.
    Accept {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
    },
    /// Open a temporary direct edge for one thread.
    Grant {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        thread: String,
        /// Lifetime such as `90s`, `10m` or `1h`.
        #[arg(long, value_parser = parse_ttl)]
        ttl: std::time::Duration,
    },
    /// Claim paths in the current checkout; a directory claims its subtree.
    Claim {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        task: Option<String>,
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Release claimed paths in the current checkout, or all of the agent's claims.
    Release {
        #[arg(long)]
        agent: String,
        paths: Vec<String>,
    },
    /// List every claim as JSON lines.
    Claims,
    /// Check the audit log's hash chain.
    Audit {
        #[arg(long, required = true)]
        verify: bool,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Wire the hooks into each harness's config (the detected ones by default).
    Install {
        #[arg(long = "harness")]
        harnesses: Vec<Harness>,
    },
    /// Restore each harness's config from its pre-install backup.
    Uninstall {
        #[arg(long = "harness")]
        harnesses: Vec<Harness>,
    },
    /// Report which harnesses are wired and whether the hub exists.
    Doctor,
    /// Serve the dashboard on 127.0.0.1.
    Serve {
        /// 0 picks a free port.
        #[arg(long, default_value_t = 7433)]
        port: u16,
        /// Write {url} here once the server accepts connections.
        #[arg(long)]
        ready_file: Option<PathBuf>,
        /// Show what agents say and think (the default; redacted, kept 7 days).
        #[arg(long, conflicts_with = "no_content")]
        content: bool,
        /// Structure only: turns, tokens and tool names, never text.
        #[arg(long)]
        no_content: bool,
    },
    /// Print a fresh login link for the dashboard server that is running.
    Open(crate::session::OpenArgs),
    /// Replay a JSONL corpus of transcript records into the registry and narrative.
    Replay {
        file: PathBuf,
        #[arg(long, default_value = "1x", value_parser = replay::parse_speed)]
        speed: f64,
    },
    /// Token and USD ceilings on a tree (set on its root) or on one agent.
    #[command(subcommand)]
    Budget(BudgetCommand),
}

#[derive(Subcommand)]
pub enum BudgetCommand {
    /// Set or replace the ceiling, e.g. `50Ktok` or `2Mtok --usd 30`.
    Set {
        scope: String,
        #[arg(value_parser = budget::parse_tokens)]
        tokens: Option<i64>,
        #[arg(long, value_parser = budget::parse_usd_ceiling)]
        usd: Option<f64>,
    },
    /// Print the ceiling, spend and state.
    Show {
        scope: String,
        #[arg(long)]
        json: bool,
    },
}

pub fn parse_ttl(text: &str) -> Result<std::time::Duration, String> {
    let (digits, unit) = text.split_at(
        text.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len()),
    );
    let count: u64 = digits.parse().map_err(|_| format!("invalid ttl {text}"))?;
    let seconds = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => return Err(format!("invalid ttl unit in {text}; use s, m or h")),
    };
    let seconds = count
        .checked_mul(seconds)
        .filter(|&s| s <= MAX_TTL_SECS)
        .ok_or_else(|| format!("ttl {text} is over the 24h limit"))?;
    Ok(std::time::Duration::from_secs(seconds))
}

/// A grant is for a conversation, not a standing edge; that is what `link` is for.
const MAX_TTL_SECS: u64 = 24 * 3600;
