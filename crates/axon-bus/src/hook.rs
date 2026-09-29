//! `axon-bus hook <harness> <event>`: payload → registry change → verdict (BUS-PLAN §2).
//!
//! A hook never hurts the host session: it always exits 0, stays silent when the hub is
//! absent, prints nothing on stdout when it allows, and allows on any error.

use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::registry::{self, Agent, Status};
use crate::{gate, msg, store};

/// What a hook event means for the registry, independent of the harness that sent it.
#[derive(Debug, PartialEq)]
enum Change<'a> {
    /// A session began; nothing runs until its first turn.
    SessionStart(Node<'a>),
    /// A turn or a tool is running in this agent; registers it on first sight.
    Active(Node<'a>),
    /// A subagent started under `parent`.
    ChildStart(Node<'a>),
    Idle(&'a str),
    Closed(&'a str),
    Ignored,
}

#[derive(Debug, PartialEq)]
struct Node<'a> {
    id: &'a str,
    session_id: &'a str,
    parent_id: Option<&'a str>,
    cwd: Option<&'a str>,
    model: Option<&'a str>,
}

pub fn run(db: &Path, harness: &str, event: &str) {
    let mut stdin = Vec::new();
    // Drained even when the hub is absent, so the harness never writes into a closed pipe.
    let read = std::io::stdin().read_to_end(&mut stdin);
    if !db.exists() {
        return;
    }
    let explicit_parent = std::env::var("AXON_BUS_PARENT").ok();
    let result = read
        .context("read hook payload")
        .and_then(|_| serde_json::from_slice::<Value>(&stdin).context("hook payload is not JSON"))
        .and_then(|payload| {
            let change = normalize(harness, event, &payload, explicit_parent.as_deref())?;
            apply(db, harness, event, change, &payload)
        });
    match result {
        Ok(Some(reply)) => println!("{reply}"),
        Ok(None) => {}
        Err(err) => eprintln!("axon-bus: hook {harness} {event} allowed after an error: {err:#}"),
    }
}

fn str_at<'a>(payload: &'a Value, pointer: &str) -> Option<&'a str> {
    payload.pointer(pointer).and_then(Value::as_str)
}

fn required<'a>(payload: &'a Value, pointer: &str) -> anyhow::Result<&'a str> {
    str_at(payload, pointer).with_context(|| format!("payload has no {pointer}"))
}

fn normalize<'a>(
    harness: &str,
    event: &str,
    payload: &'a Value,
    explicit_parent: Option<&'a str>,
) -> anyhow::Result<Change<'a>> {
    match harness {
        "claude" | "codex" => claude_or_codex(event, payload, explicit_parent),
        "opencode" => opencode(event, payload),
        "hermes" => hermes(event, payload, explicit_parent),
        other => bail!("unknown harness {other}"),
    }
}

fn claude_or_codex<'a>(
    event: &str,
    payload: &'a Value,
    explicit_parent: Option<&'a str>,
) -> anyhow::Result<Change<'a>> {
    let session_id = required(payload, "/session_id")?;
    // Inside a subagent every payload carries the child's agent_id next to the root session.
    let child = str_at(payload, "/agent_id");
    let node = |id, parent_id| Node {
        id,
        session_id,
        parent_id,
        cwd: str_at(payload, "/cwd"),
        model: str_at(payload, "/model"),
    };
    let actor = node(
        child.unwrap_or(session_id),
        child.map(|_| explicit_parent.unwrap_or(session_id)),
    );
    Ok(match event {
        "SessionStart" => Change::SessionStart(node(session_id, None)),
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Change::Active(actor),
        "SubagentStart" => Change::ChildStart(node(
            required(payload, "/agent_id")?,
            Some(explicit_parent.unwrap_or(session_id)),
        )),
        "SubagentStop" => Change::Closed(required(payload, "/agent_id")?),
        "Stop" => Change::Idle(actor.id),
        "SessionEnd" => Change::Closed(session_id),
        _ => Change::Ignored,
    })
}

fn opencode<'a>(event: &str, payload: &'a Value) -> anyhow::Result<Change<'a>> {
    let session = |id| Node {
        id,
        session_id: id,
        parent_id: None,
        cwd: None,
        model: None,
    };
    Ok(match event {
        "session.created" => {
            let id = required(payload, "/properties/info/id")?;
            let parent_id = str_at(payload, "/properties/info/parentID");
            let node = Node {
                parent_id,
                cwd: str_at(payload, "/properties/info/directory"),
                ..session(id)
            };
            match parent_id {
                Some(_) => Change::ChildStart(node),
                None => Change::SessionStart(node),
            }
        }
        "tool.execute.before" | "tool.execute.after" => {
            Change::Active(session(required(payload, "/input/sessionID")?))
        }
        "session.idle" => Change::Idle(required(payload, "/properties/sessionID")?),
        "session.deleted" => Change::Closed(required(payload, "/properties/info/id")?),
        _ => Change::Ignored,
    })
}

fn hermes<'a>(
    event: &str,
    payload: &'a Value,
    explicit_parent: Option<&'a str>,
) -> anyhow::Result<Change<'a>> {
    let session_id = required(payload, "/session_id")?;
    let root = Node {
        id: session_id,
        session_id,
        parent_id: None,
        cwd: str_at(payload, "/cwd"),
        model: str_at(payload, "/extra/model"),
    };
    Ok(match event {
        "on_session_start" => Change::SessionStart(root),
        "pre_llm_call" | "pre_tool_call" | "post_tool_call" => Change::Active(root),
        "post_llm_call" => Change::Idle(session_id),
        "subagent_start" => {
            let child = required(payload, "/extra/child_session_id")?;
            Change::ChildStart(Node {
                id: child,
                session_id: child,
                parent_id: Some(explicit_parent.unwrap_or(session_id)),
                model: None,
                ..root
            })
        }
        "subagent_stop" => Change::Closed(required(payload, "/extra/child_session_id")?),
        "on_session_end" => Change::Closed(session_id),
        _ => Change::Ignored,
    })
}

fn apply(
    db: &Path,
    harness: &str,
    event: &str,
    change: Change,
    payload: &Value,
) -> anyhow::Result<Option<Value>> {
    let actor = match &change {
        Change::Ignored => return Ok(None),
        Change::SessionStart(node) | Change::Active(node) | Change::ChildStart(node) => node.id,
        Change::Idle(id) | Change::Closed(id) => id,
    };
    let mut conn = store::open(db)?;
    let tx = store::write_tx(&mut conn)?;
    match change {
        Change::SessionStart(node) => upsert(&tx, harness, &node, Status::Idle)?,
        Change::Active(node) => {
            // A late tool event from a finished subagent must not reopen it.
            if status_of(&tx, node.id)?.as_deref() == Some("closed") {
                registry::touch(&tx, node.id)?;
            } else {
                upsert(&tx, harness, &node, Status::Active)?;
            }
        }
        Change::ChildStart(node) => upsert(&tx, harness, &node, Status::Active)?,
        Change::Idle(id) => {
            registry::set_status(&tx, id, Status::Idle)?;
            msg::ack_stops(&tx, id)?;
        }
        Change::Closed(id) => {
            registry::set_status(&tx, id, Status::Closed)?;
            msg::ack_stops(&tx, id)?;
        }
        Change::Ignored => {}
    }
    let reply = gate::verdict(&tx, harness, event, actor, payload)?;
    tx.commit()?;
    Ok(reply)
}

/// Register `node`, first registering its session root when this is the first hook seen
/// for the session (`claude -p` never fires SessionStart). An explicit parent that is not
/// registered falls back to the session root. A child that runs in its own session
/// (Hermes, OpenCode) with an unknown parent becomes a root.
fn upsert(conn: &Connection, harness: &str, node: &Node, status: Status) -> anyhow::Result<()> {
    let mut parent_id = node.parent_id;
    if let Some(parent) = parent_id {
        if registry::root_of(conn, parent)?.is_none() {
            parent_id = (node.session_id != node.id).then_some(node.session_id);
            if parent_id.is_some() && registry::root_of(conn, node.session_id)?.is_none() {
                let root = Node {
                    id: node.session_id,
                    parent_id: None,
                    model: None,
                    ..*node
                };
                upsert(conn, harness, &root, Status::Active)?;
            }
        }
    }
    let agent = Agent {
        id: node.id,
        harness,
        session_id: node.session_id,
        parent_id,
        cwd: node.cwd,
        model: node.model,
    };
    registry::upsert(conn, &agent, status)
}

fn status_of(conn: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT status FROM agents WHERE id=?1", [id], |r| r.get(0))
        .optional()
}
