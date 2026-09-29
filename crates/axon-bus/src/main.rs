//! axon-bus — the Axon control plane (docs/BUS-PLAN.md).
//!
//! No async runtime here by design: `axon-bus hook` runs on every tool call and must
//! stay near the process-spawn floor (BUS-PLAN §00, spike Q4).

mod claims;
mod gate;
mod hook;
mod install;
mod msg;
mod registry;
mod route;
mod store;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "axon-bus",
    version,
    about = "Axon's control plane for coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
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
enum Command {
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
}

/// A request the CLI rejects as invalid (exit 2), as opposed to a failure (exit 1).
#[derive(Debug)]
struct Invalid(String);

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Invalid {}

/// Exit code of a refusal by policy, such as a conflicting claim or a non-edge send.
const REFUSED: u8 = 3;

fn parse_ttl(text: &str) -> Result<std::time::Duration, String> {
    let (digits, unit) = text.split_at(text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len()));
    let count: u64 = digits.parse().map_err(|_| format!("invalid ttl {text}"))?;
    let seconds = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => return Err(format!("invalid ttl unit in {text}; use s, m or h")),
    };
    Ok(std::time::Duration::from_secs(count * seconds))
}

fn hub(db: &std::path::Path) -> anyhow::Result<rusqlite::Connection> {
    store::open(db).context("no hub; run `axon-bus init`")
}

/// Print a refused send and turn it into its exit code.
fn refused(refused: msg::Refused) -> ExitCode {
    let (code, why) = msg::refused_error(refused);
    eprintln!("axon-bus: {why}");
    ExitCode::from(code)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let db = axon_core::store::default_path();
    match run(cli.command, db) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("axon-bus: {err:#}");
            ExitCode::from(if err.is::<Invalid>() { 2 } else { 1 })
        }
    }
}

fn run(command: Command, db: PathBuf) -> anyhow::Result<ExitCode> {
    match command {
        Command::Hook { harness, event } => hook::run(&db, &harness, &event),
        Command::Init => {
            store::init(&db)?;
            println!("{}", db.display());
        }
        Command::Register {
            id,
            harness,
            session,
            cwd,
            parent,
        } => {
            let mut conn = hub(&db)?;
            let tx = store::write_tx(&mut conn)?;
            if let Some(parent) = &parent {
                if registry::root_of(&tx, parent)?.is_none() {
                    return Err(Invalid(format!("parent {parent} is not registered")).into());
                }
            }
            let agent = registry::Agent {
                id: &id,
                harness: harness.as_str(),
                session_id: &session,
                parent_id: parent.as_deref(),
                cwd: Some(&cwd),
                model: None,
            };
            registry::register(&tx, &agent).map_err(|e| Invalid(e.to_string()))?;
            tx.commit()?;
        }
        Command::Send {
            from,
            to,
            kind,
            body,
            thread,
            refs,
        } => {
            let mut conn = hub(&db)?;
            let tx = store::write_tx(&mut conn)?;
            let outgoing = msg::Outgoing {
                from: &from,
                to: &to,
                kind: &kind,
                body: &body,
                thread: thread.as_deref(),
                refs: &refs,
                wait: None,
            };
            match msg::send(&tx, &outgoing)? {
                Ok((id, thread)) => {
                    tx.commit()?;
                    println!("{}", serde_json::json!({"id": id, "thread": thread}));
                }
                Err(why) => return Ok(refused(why)),
            }
        }
        Command::Ask {
            from,
            to,
            body,
            wait,
            default,
            thread,
        } => {
            let wait = std::time::Duration::from_secs(wait);
            let mut conn = hub(&db)?;
            let tx = store::write_tx(&mut conn)?;
            let question = msg::Outgoing {
                from: &from,
                to: &to,
                kind: "question",
                body: &body,
                thread: thread.as_deref(),
                refs: &[],
                wait: Some((wait, &default)),
            };
            let (id, thread) = match msg::send(&tx, &question)? {
                Ok(sent) => sent,
                Err(why) => return Ok(refused(why)),
            };
            // Committed before waiting, so the addressee can see and answer it.
            tx.commit()?;
            let answer = msg::await_answer(&conn, &id, &thread, wait, &default)?;
            println!("{answer}");
        }
        Command::Reply {
            question,
            from,
            body,
        } => {
            let mut conn = hub(&db)?;
            let tx = store::write_tx(&mut conn)?;
            match msg::reply(&tx, &question, &from, &body)? {
                Ok((id, thread)) => {
                    tx.commit()?;
                    println!("{}", serde_json::json!({"id": id, "thread": thread}));
                }
                Err(why) => return Ok(refused(why)),
            }
        }
        Command::Link { from, to } => {
            let mut conn = hub(&db)?;
            let tx = store::write_tx(&mut conn)?;
            route::link(&tx, &from, &to).map_err(|e| Invalid(e.to_string()))?;
            tx.commit()?;
        }
        Command::Accept { from, to } => {
            let mut conn = hub(&db)?;
            let tx = store::write_tx(&mut conn)?;
            route::accept(&tx, &from, &to).map_err(|e| Invalid(e.to_string()))?;
            tx.commit()?;
        }
        Command::Grant {
            from,
            to,
            thread,
            ttl,
        } => {
            let mut conn = hub(&db)?;
            let tx = store::write_tx(&mut conn)?;
            route::grant(&tx, &from, &to, &thread, ttl.as_millis() as i64)
                .map_err(|e| Invalid(e.to_string()))?;
            tx.commit()?;
        }
        Command::Claim { agent, task, paths } => {
            let mut conn = hub(&db)?;
            let tx = store::write_tx(&mut conn)?;
            let cwd = std::env::current_dir()?;
            let conflicts = claims::claim(&tx, &agent, &cwd, task.as_deref(), &paths)?;
            if !conflicts.is_empty() {
                for c in conflicts {
                    let task = c.task.map(|t| format!(" ({t})")).unwrap_or_default();
                    eprintln!("axon-bus: {} is claimed by {}{task}", c.path, c.owner);
                }
                return Ok(ExitCode::from(REFUSED));
            }
            tx.commit()?;
        }
        Command::Release { agent, paths } => {
            let mut conn = hub(&db)?;
            let tx = store::write_tx(&mut conn)?;
            claims::release(&tx, &agent, &std::env::current_dir()?, &paths)?;
            tx.commit()?;
        }
        Command::Claims => {
            let conn = hub(&db)?;
            for claim in claims::list(&conn)? {
                println!("{claim}");
            }
        }
        Command::Audit { db: other, .. } => {
            let path = other.unwrap_or(db);
            let conn = store::open(&path)?;
            let checked = store::verify(&conn)?;
            println!("audit verified: {checked} events in {}", path.display());
        }
        Command::Install { harnesses } => {
            let targets = if harnesses.is_empty() {
                install::detected()
            } else {
                harnesses
            };
            for harness in targets {
                install::install(harness)?;
            }
        }
        Command::Uninstall { harnesses } => {
            let targets = if harnesses.is_empty() {
                Harness::ALL.to_vec()
            } else {
                harnesses
            };
            for harness in targets {
                install::uninstall(harness)?;
            }
        }
        Command::Doctor => {
            if !install::doctor(&db)? {
                return Ok(ExitCode::FAILURE);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}
