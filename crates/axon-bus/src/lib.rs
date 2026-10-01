//! The Axon control plane (docs/BUS-PLAN.md), run as `axon bus <cmd>` or `axon-bus <cmd>`.
//!
//! No async runtime here by design: `hook` runs on every tool call and must stay near the
//! process-spawn floor (BUS-PLAN §00, spike Q4). Only `serve` starts one.

mod assets;
mod budget;
mod chatter;
mod claims;
mod cli;
mod cli_guard;
mod doctor;
mod doorbell;
mod end;
mod gate;
mod hermes_hooks;
mod hook;
mod install;
mod memory;
mod msg;
mod observed;
mod opencode_plugin;
mod redact;
mod registry;
mod relay;
mod replay;
mod roster;
mod route;
pub mod serve;
mod setup;
mod snapshot;
mod store;
mod tail;
mod transcript;
mod uninstall;
mod usage;
mod virtual_agent;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::Parser;

pub use cli::Harness;
use cli::{BudgetCommand, Cli, Command};

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

fn hub(db: &std::path::Path) -> anyhow::Result<rusqlite::Connection> {
    store::open(db).context("no hub; run `axon-bus init`")
}

/// Print a refused send and turn it into its exit code.
fn refused(refused: msg::Refused) -> ExitCode {
    let (code, why) = msg::refused_error(refused);
    eprintln!("axon-bus: {why}");
    ExitCode::from(code)
}

/// Create the bus tables in `db` if they are missing: schema only, no harness config.
pub fn init(db: &std::path::Path) -> anyhow::Result<()> {
    store::init(db).map(drop)
}

pub use setup::ensure_hooks;

/// Parse `args` (program name first) and run the command.
pub fn cli_main<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = Cli::parse_from(args);
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
            role,
            model,
            effort,
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
                model: model.as_deref(),
                role: role.as_deref(),
                effort: effort.as_deref(),
                mission: None,
                pid: None,
            };
            registry::register(&tx, &agent).map_err(|e| Invalid(e.to_string()))?;
            tx.commit()?;
        }
        Command::Route {
            task,
            role,
            budget,
            advisor_response,
        } => {
            let advice = match advisor_response {
                Some(path) => {
                    let text = std::fs::read_to_string(&path)
                        .with_context(|| format!("read {}", path.display()))?;
                    let advice: serde_json::Value = serde_json::from_str(&text)
                        .map_err(|e| Invalid(format!("{} is not JSON: {e}", path.display())))?;
                    Some(advice)
                }
                None => None,
            };
            let mut conn = hub(&db)?;
            let tx = store::write_tx(&mut conn)?;
            let answer = route::route_task(&tx, &task, role.as_deref(), budget, advice.as_ref())?;
            tx.commit()?;
            println!("{answer}");
        }
        Command::Spawn {
            task,
            parent,
            panel,
            judge,
            ..
        } => {
            let mut conn = hub(&db)?;
            let tx = store::write_tx(&mut conn)?;
            if registry::root_of(&tx, &parent)?.is_none() {
                return Err(Invalid(format!("parent {parent} is not registered")).into());
            }
            let id = virtual_agent::register(&tx, &parent, &task, &panel, &judge)?;
            tx.commit()?;
            println!("{}", serde_json::json!({"id": id}));
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
            msg::link_notice(&tx, &from, &to)?;
            tx.commit()?;
        }
        Command::Peers { agent } => {
            let conn = hub(&db)?;
            let peers = roster::peers(&conn, &agent)?
                .ok_or_else(|| Invalid(format!("agent {agent} is not registered")))?;
            println!("{peers}");
        }
        Command::Accept { from, to } => {
            let mut conn = hub(&db)?;
            let tx = store::write_tx(&mut conn)?;
            route::accept(&tx, &from, &to).map_err(|e| Invalid(e.to_string()))?;
            msg::accepted_notice(&tx, &from, &to)?;
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
            let (checked, head) = store::verify(&conn)?;
            println!("audit verified: {checked} events in {}", path.display());
            println!("head {head}");
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
                uninstall::uninstall(harness)?;
            }
        }
        Command::Budget(BudgetCommand::Set { scope, tokens, usd }) => {
            let mut conn = hub(&db)?;
            let tx = store::write_tx(&mut conn)?;
            let members =
                budget::set(&tx, &scope, tokens, usd).map_err(|e| Invalid(format!("{e:#}")))?;
            tx.commit()?;
            for member in members {
                msg::silence_doorbell(&conn, &member)?;
            }
        }
        Command::Budget(BudgetCommand::Show { scope, json }) => {
            let status = budget::show(&hub(&db)?, &scope)?;
            if json {
                println!("{status}");
            } else {
                println!("{status:#}");
            }
        }
        Command::Serve {
            port,
            ready_file,
            content: _,
            no_content,
        } => {
            // The dashboard may be the first bus tool run on an Axon-only database.
            store::init(&db)?;
            serve::run(&db, port, ready_file.as_deref(), !no_content)?;
        }
        Command::Replay { file, speed } => replay::run(&db, &file, speed)?,
        Command::Doctor => {
            if !doctor::doctor(&db)? {
                return Ok(ExitCode::FAILURE);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}
