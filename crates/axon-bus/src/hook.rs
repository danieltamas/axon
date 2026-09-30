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
use crate::{doorbell, gate, msg, store};

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
    let hub_exists = db.exists();
    if !hub_exists && !doorbell::dir(db).exists() {
        return;
    }
    let explicit_parent = std::env::var("AXON_BUS_PARENT").ok();
    let result = read
        .context("read hook payload")
        .and_then(|_| serde_json::from_slice::<Value>(&stdin).context("hook payload is not JSON"))
        .and_then(|payload| {
            let change = normalize(harness, event, &payload, explicit_parent.as_deref())?;
            let actor = actor_of(&change);
            let reply = if hub_exists {
                apply(db, harness, event, change, &payload)
            } else {
                Err(anyhow::anyhow!("the hub at {} was removed", db.display()))
            };
            reply.or_else(|err| rung_doorbell(db, harness, event, actor).ok_or(err))
        });
    match result {
        Ok(Some(reply)) => println!("{reply}"),
        Ok(None) => {}
        Err(err) if hub_exists => {
            eprintln!("axon-bus: hook {harness} {event} allowed after an error: {err:#}")
        }
        Err(_) => {}
    }
}

/// When the hub cannot answer, a stop already persisted as a doorbell still denies the
/// actor's next tool call (fail closed); everything else is allowed.
fn rung_doorbell(db: &Path, harness: &str, event: &str, actor: Option<&str>) -> Option<Option<Value>> {
    if !gate::is_pre_tool(harness, event) {
        return None;
    }
    let reason = doorbell::reason(db, actor?)?;
    Some(Some(gate::deny(harness, reason)))
}

fn actor_of<'a>(change: &Change<'a>) -> Option<&'a str> {
    match change {
        Change::Ignored => None,
        Change::SessionStart(node) | Change::Active(node) | Change::ChildStart(node) => Some(node.id),
        Change::Idle(id) | Change::Closed(id) => Some(id),
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
    if change == Change::Ignored {
        return Ok(None);
    }
    let mut conn = store::open(db)?;
    let tx = store::write_tx(&mut conn)?;
    let resolve = |id: &str| -> rusqlite::Result<String> {
        Ok(registered_as(&tx, harness, id)?.unwrap_or_else(|| id.to_owned()))
    };
    let closing = matches!(change, Change::Closed(_));
    let (actor, acked) = match change {
        Change::Ignored => return Ok(None),
        Change::Idle(id) | Change::Closed(id) => {
            let id = resolve(id)?;
            let status = if closing { Status::Closed } else { Status::Idle };
            registry::set_status(&tx, &id, status)?;
            msg::ack_stops(&tx, &id)?;
            (id, true)
        }
        Change::SessionStart(ref node) | Change::Active(ref node) | Change::ChildStart(ref node) => {
            let id = resolve(node.id)?;
            let parent = node.parent_id.map(resolve).transpose()?;
            let node = Node {
                id: &id,
                parent_id: parent.as_deref(),
                ..*node
            };
            match change {
                Change::SessionStart(_) => upsert(&tx, harness, &node, Status::Idle)?,
                // A late tool event from a finished subagent must not reopen it.
                Change::Active(_) if status_of(&tx, &id)?.as_deref() == Some("closed") => {
                    registry::touch(&tx, &id)?
                }
                _ => upsert(&tx, harness, &node, Status::Active)?,
            }
            (id, false)
        }
    };
    let reply = gate::verdict(&tx, harness, event, &actor, payload)?;
    if let Err(err) = tx.commit() {
        // A denial stands even when the transaction that computed it cannot commit.
        if reply.is_some() && gate::is_pre_tool(harness, event) {
            eprintln!("axon-bus: hook {harness} {event} denied; its changes did not commit: {err}");
            return Ok(reply);
        }
        return Err(err.into());
    }
    if acked {
        if let Err(err) = msg::silence_doorbell(&conn, &actor) {
            eprintln!("axon-bus: doorbell for {actor} not cleared: {err:#}");
        }
    }
    Ok(reply)
}

/// The id a root was registered under (`register --id orch --session S`), for hooks that
/// name only its session. None when `id` is itself an agent or no root has that session.
fn registered_as(conn: &Connection, harness: &str, id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT id FROM agents WHERE session_id=?1 AND harness=?2 AND parent_id IS NULL
         AND NOT EXISTS (SELECT 1 FROM agents WHERE id=?1) ORDER BY started_at LIMIT 1",
        [id, harness],
        |r| r.get(0),
    )
    .optional()
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
        ..Default::default()
    };
    registry::upsert(conn, &agent, status)
}

fn status_of(conn: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT status FROM agents WHERE id=?1", [id], |r| r.get(0))
        .optional()
}
