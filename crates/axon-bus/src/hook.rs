//! `axon-bus hook <harness> <event>`: payload → registry change → verdict (BUS-PLAN §2).
//!
//! A hook never hurts the host session: it always exits 0, stays silent when the hub is
//! absent, prints nothing on stdout when it allows, and allows on any error.

use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::registry::{self, Agent, Status};
use crate::{doorbell, gate, msg, store, usage};

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
    let result = read
        .context("read hook payload")
        .and_then(|_| serde_json::from_slice::<Value>(&stdin).context("hook payload is not JSON"))
        .and_then(|payload| {
            let explicit_parent = trusted_parent(db, &payload);
            let change = normalize(harness, event, &payload, explicit_parent.as_deref())?;
            let actor = actor_of(&change);
            let reply = if hub_exists {
                apply(db, harness, event, change, &payload)
            } else {
                Err(anyhow::anyhow!("the hub at {} was removed", db.display()))
            };
            reply.or_else(|err| {
                let fallback = gate_only(db, harness, event, actor, &payload)
                    .or_else(|| rung_doorbell(db, harness, event, actor))
                    .or_else(|| guard_only(harness, event, actor, &payload));
                if fallback.is_some() {
                    eprintln!(
                        "axon-bus: hook {harness} {event} answered by the gate alone: {err:#}"
                    );
                }
                fallback.ok_or(err)
            })
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

/// `AXON_BUS_PARENT`, when it names an agent of this hook's own session tree. A parent
/// from another tree would move this session's spend onto that tree's budget, so it is
/// dropped and the child attaches to its session root instead.
fn trusted_parent(db: &Path, payload: &Value) -> Option<String> {
    let parent = std::env::var("AXON_BUS_PARENT").ok()?;
    let session = str_at(payload, "/session_id")?;
    let conn = store::open_for_hook(db).ok()?;
    let same_tree: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM agents a JOIN agents r ON r.id=a.root_id
             WHERE a.id=?1 AND (a.session_id=?2 OR r.session_id=?2))",
            [&parent, session],
            |r| r.get(0),
        )
        .ok()?;
    if !same_tree {
        eprintln!("axon-bus: AXON_BUS_PARENT={parent} is not in session {session}; ignored");
    }
    same_tree.then_some(parent)
}

/// The stop and budget gate alone, for a pre-tool hook whose registry write failed (most
/// often a hub busy past its timeout). A WAL reader is not blocked by the writer, so the
/// gate still answers; a stop or budget transition it must write fails, and so denies.
/// None when the hub cannot be read at all or the actor is not registered yet.
fn gate_only(
    db: &Path,
    harness: &str,
    event: &str,
    actor: Option<&str>,
    payload: &Value,
) -> Option<Option<Value>> {
    if !gate::is_pre_tool(harness, event) || !db.exists() {
        return None;
    }
    let answer = (|| -> anyhow::Result<Option<Option<Value>>> {
        let mut conn = store::open_for_hook(db)?;
        let tx = conn.transaction()?;
        let Some(actor) = actor else { return Ok(None) };
        let actor = registered_as(&tx, harness, actor)?.unwrap_or_else(|| actor.to_owned());
        if registry::root_of(&tx, &actor)?.is_none() {
            return Ok(None);
        }
        let reply = gate::verdict(&tx, harness, event, &actor, payload)?;
        // Transitions it recorded are kept when the lock is free by now; losing them is
        // harmless because the next hook recomputes them.
        let _ = tx.commit();
        Ok(Some(reply))
    })();
    answer.ok().flatten()
}

/// When the hub cannot answer, a stop already persisted as a doorbell still denies the
/// actor's next tool call (fail closed); everything else is allowed.
fn rung_doorbell(
    db: &Path,
    harness: &str,
    event: &str,
    actor: Option<&str>,
) -> Option<Option<Value>> {
    if !gate::is_pre_tool(harness, event) {
        return None;
    }
    let reason = doorbell::reason(db, actor?)?;
    Some(Some(gate::deny(harness, reason)))
}

/// The bus-command guard needs no hub: when nothing above could answer, the human-only
/// verbs and forged senders are still refused rather than let through by the error.
fn guard_only(
    harness: &str,
    event: &str,
    actor: Option<&str>,
    payload: &Value,
) -> Option<Option<Value>> {
    if !gate::is_pre_tool(harness, event) {
        return None;
    }
    let reason = crate::cli_guard::refusal(actor.unwrap_or_default(), payload)?;
    Some(Some(gate::deny(harness, reason)))
}

fn actor_of<'a>(change: &Change<'a>) -> Option<&'a str> {
    match change {
        Change::Ignored => None,
        Change::SessionStart(node) | Change::Active(node) | Change::ChildStart(node) => {
            Some(node.id)
        }
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
    let mut conn = store::open_for_hook(db)?;
    let transcript = read_claude_transcript(&conn, harness, event, &change, payload);
    let tx = store::write_tx(&mut conn)?;
    let resolve = |id: &str| -> rusqlite::Result<String> {
        Ok(registered_as(&tx, harness, id)?.unwrap_or_else(|| id.to_owned()))
    };
    let closing = matches!(change, Change::Closed(_));
    let (actor, acked) = match change {
        Change::Ignored => return Ok(None),
        Change::Idle(id) | Change::Closed(id) => {
            let id = resolve(id)?;
            let status = if closing {
                Status::Closed
            } else {
                Status::Idle
            };
            registry::set_status(&tx, &id, status)?;
            if closing {
                msg::ack_stops_on_close(&tx, &id)?;
            } else {
                msg::ack_stops(&tx, &id)?;
            }
            (id, true)
        }
        Change::SessionStart(ref node)
        | Change::Active(ref node)
        | Change::ChildStart(ref node) => {
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
    if harness == "claude" {
        record_claude_usage(&tx, &actor, payload, transcript.as_ref());
    }
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
    // After the commit: the command it runs takes the hub's write lock itself.
    if reply.is_none() && gate::is_pre_tool(harness, event) {
        return Ok(crate::relay::run(harness, &actor, payload));
    }
    Ok(reply)
}

/// After a Claude tool call or turn (a turn's last reply follows its last tool call), the
/// transcript lines the actor added, read before the write lock is taken.
fn read_claude_transcript(
    conn: &Connection,
    harness: &str,
    event: &str,
    change: &Change,
    payload: &Value,
) -> Option<usage::Pending> {
    if harness != "claude" || !matches!(event, "PostToolUse" | "Stop" | "SubagentStop") {
        return None;
    }
    let path = str_at(payload, "/transcript_path")?;
    let read = (|| {
        let id = actor_of(change).context("no actor")?;
        let actor = registered_as(conn, harness, id)?.unwrap_or_else(|| id.to_owned());
        usage::read_claude(conn, &actor, str_at(payload, "/agent_id"), Path::new(path))
    })();
    read.unwrap_or_else(|err| {
        eprintln!("axon-bus: transcript {path} not read: {err:#}");
        None
    })
}

/// Keep the model a spawn resolved for its child (raw, so an unknown model stays unpriced)
/// and store the transcript lines read earlier. Failures are reported, never allowed to
/// undo the hook's registry change or delivery.
fn record_claude_usage(
    conn: &Connection,
    actor: &str,
    payload: &Value,
    transcript: Option<&usage::Pending>,
) {
    let spawned = &payload["tool_response"];
    let asked = &payload["tool_input"];
    let recorded = (|| -> anyhow::Result<()> {
        if let (Some(child), Some(model)) = (
            spawned["agentId"].as_str(),
            spawned["resolvedModel"].as_str(),
        ) {
            conn.execute("UPDATE agents SET model=?2 WHERE id=?1", [child, model])?;
        }
        // The spawn names the child's type and task: the board's label and line for it.
        if let Some(child) = spawned["agentId"].as_str() {
            conn.execute(
                "UPDATE agents SET role=?2, mission=?3 WHERE id=?1",
                params![
                    child,
                    asked["subagent_type"].as_str(),
                    asked["description"].as_str()
                ],
            )?;
        }
        if let Some(pending) = transcript {
            usage::store(conn, pending)?;
        }
        Ok(())
    })();
    if let Err(err) = recorded {
        eprintln!("axon-bus: usage for {actor} not recorded: {err:#}");
    }
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
        pid: harness_pid(),
        ..Default::default()
    };
    registry::upsert(conn, &agent, status)
}

/// The hook's parent is the harness: a single-command hook line is exec'd by the shell,
/// so no wrapper process sits in between.
#[cfg(unix)]
fn harness_pid() -> Option<i64> {
    Some(i64::from(std::os::unix::process::parent_id()))
}

#[cfg(not(unix))]
fn harness_pid() -> Option<i64> {
    None
}

fn status_of(conn: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT status FROM agents WHERE id=?1", [id], |r| r.get(0))
        .optional()
}
