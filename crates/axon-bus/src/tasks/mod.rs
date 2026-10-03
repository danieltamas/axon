//! Tasks (BUS-PLAN §3c B–E): every usage turn belongs to exactly one task, assigned once at
//! scan time and never moved. A declared task is a ledger take (`take` … `done`); a request
//! runs from one operator prompt to the next in its session.
//!
//! Assignment order, by timestamps: the agent's own open take (the latest wins), then the
//! nearest ancestor's, then a take whose holder handed this agent a handoff or question that
//! is not yet answered in that thread, else the session's latest prompt.

pub mod api;
pub mod cli;
mod list;

use std::collections::HashMap;
use std::path::Path;

use axon_core::ingest::Prompt;
use rusqlite::{params, Connection, OptionalExtension};

pub use list::list;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tasks (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL CHECK (kind IN ('declared','request')),
    key        TEXT,
    name       TEXT NOT NULL,
    -- A request's session; a declared task's taker's session.
    session_id TEXT,
    agent_id   TEXT,
    opened_at  INTEGER NOT NULL,
    closed_at  INTEGER
);
CREATE INDEX IF NOT EXISTS idx_tasks_session ON tasks(session_id, opened_at);
-- Written once per turn (INSERT OR IGNORE): a re-scan never moves a turn to another task.
CREATE TABLE IF NOT EXISTS task_turns (
    event_id TEXT PRIMARY KEY,
    task_id  TEXT NOT NULL,
    agent_id TEXT
);
CREATE INDEX IF NOT EXISTS idx_task_turns_task ON task_turns(task_id);
";

/// The longest request name kept from a prompt.
const NAME_MAX: usize = 80;
/// Parent links followed before giving up on a malformed tree.
const DEPTH_MAX: usize = 32;

pub fn ensure(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)
}

fn hash(parts: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(b"\x1f");
    }
    hasher.finalize().to_hex().to_string()
}

/// `request at HH:MM` (UTC), the name of a request whose prompt text is not kept.
pub(crate) fn time_name(ts: i64) -> String {
    let at = chrono::DateTime::from_timestamp_millis(ts).unwrap_or_default();
    format!("request at {}", at.format("%H:%M"))
}

/// With capture on, the prompt's first 80 characters, credentials masked; else the time.
fn request_name(prompt: &Prompt, capture: bool) -> String {
    let text = crate::redact::redact(&prompt.text);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !capture || text.is_empty() {
        return time_name(prompt.ts);
    }
    text.chars().take(NAME_MAX).collect()
}

/// Store each prompt as a request, keyed by its session and timestamp so a re-scan or a
/// rebuilt database finds the same id.
pub fn record_prompts(db: &Path, prompts: &[Prompt]) -> anyhow::Result<()> {
    let mut conn = crate::store::init(db)?;
    ensure(&conn)?;
    let capture = crate::transcript::content_enabled(&conn)?;
    let tx = crate::store::write_tx(&mut conn)?;
    for prompt in prompts {
        tx.execute(
            "INSERT OR IGNORE INTO tasks (id,kind,name,session_id,opened_at)
             VALUES (?1,'request',?2,?3,?4)",
            params![
                hash(&[&prompt.session_id, &prompt.ts.to_string()]),
                request_name(prompt, capture),
                prompt.session_id,
                prompt.ts
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// A ledger take as a task: who took which key, from when until done, dropped or lapsed.
struct Take {
    id: String,
    agent: String,
    opened: i64,
    closed: Option<i64>,
}

impl Take {
    fn open_at(&self, ts: i64) -> bool {
        self.opened <= ts && self.closed.is_none_or(|closed| ts < closed)
    }
}

struct Agent {
    session: String,
    parent: Option<String>,
}

/// A handoff or question to an agent, and when the answer or ack in its thread ended it.
struct Delegation {
    to: String,
    from: String,
    sent: i64,
    ended: Option<i64>,
}

/// Upsert a declared task for every take in the audit log, closing it at the taker's next
/// ledger event on the key, or when the ledger shows the take lapsed. Only the taker's own
/// events count: the audit log does not record the repository, and another agent's event on
/// the same key may belong to another repository. A stored close is never reopened.
fn sync_declared(conn: &Connection, agents: &HashMap<String, Agent>) -> anyhow::Result<Vec<Take>> {
    let mut stmt = conn.prepare(
        "SELECT ts,actor,verb,subject FROM events WHERE verb IN ('take','done','drop') ORDER BY seq",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let now = crate::store::now_ms();
    let mut takes = Vec::new();
    for (i, (ts, actor, verb, key)) in rows.iter().enumerate() {
        if verb != "take" {
            continue;
        }
        let next = rows[i + 1..]
            .iter()
            .find(|row| &row.3 == key && &row.1 == actor)
            .map(|row| row.0);
        let closed = match next {
            Some(at) => Some(at),
            None => lapsed(conn, actor, key, *ts, now)?,
        };
        let id = hash(&["declared", actor, key, &ts.to_string()]);
        conn.execute(
            "INSERT INTO tasks (id,kind,key,name,session_id,agent_id,opened_at,closed_at)
             VALUES (?1,'declared',?2,?2,?3,?4,?5,?6)
             ON CONFLICT(id) DO UPDATE SET closed_at=coalesce(tasks.closed_at, excluded.closed_at)",
            params![
                id,
                key,
                agents.get(actor).map(|a| &a.session),
                actor,
                ts,
                closed
            ],
        )?;
        let closed = conn.query_row("SELECT closed_at FROM tasks WHERE id=?1", [&id], |r| {
            r.get(0)
        })?;
        takes.push(Take {
            id,
            agent: actor.clone(),
            opened: *ts,
            closed,
        });
    }
    Ok(takes)
}

/// When a take with no later ledger event ended anyway: expired, or freed with its taker. A
/// take whose ledger row is gone (retention deleted it) ended within its default lease.
fn lapsed(
    conn: &Connection,
    agent: &str,
    key: &str,
    taken: i64,
    now: i64,
) -> rusqlite::Result<Option<i64>> {
    let row = conn
        .query_row(
            "SELECT state,at,expires_at FROM handled WHERE key=?1 AND agent_id=?2
             ORDER BY changed_at DESC LIMIT 1",
            params![key, agent],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .optional()?;
    Ok(match row {
        Some((state, at, _)) if state == "free" => Some(at),
        Some((_, _, Some(expires))) if expires <= now => Some(expires),
        Some(_) => None,
        None => Some(taken + crate::handled::DEFAULT_TTL_MS).filter(|end| *end <= now),
    })
}

fn agents(
    conn: &Connection,
) -> rusqlite::Result<(HashMap<String, Agent>, HashMap<String, String>)> {
    let mut stmt =
        conn.prepare("SELECT id,session_id,parent_id,agent_ref FROM agents ORDER BY started_at")?;
    let mut agents = HashMap::new();
    let mut refs = HashMap::new();
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    for row in rows {
        let (id, session, parent, agent_ref) = row?;
        if let Some(agent_ref) = agent_ref {
            refs.insert(agent_ref, id.clone());
        }
        agents.insert(id, Agent { session, parent });
    }
    Ok((agents, refs))
}

fn delegations(conn: &Connection) -> rusqlite::Result<Vec<Delegation>> {
    let mut stmt = conn.prepare(
        "SELECT m.to_id, m.from_id, m.sent_at,
           (SELECT min(e.sent_at) FROM messages e WHERE e.thread=m.thread AND e.kind IN ('answer','ack')
              AND e.from_id=m.to_id AND e.to_id=m.from_id AND e.sent_at>=m.sent_at)
         FROM messages m WHERE m.kind IN ('handoff','question') AND m.sent_at IS NOT NULL
         ORDER BY m.sent_at DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Delegation {
            to: r.get(0)?,
            from: r.get(1)?,
            sent: r.get(2)?,
            ended: r.get(3)?,
        })
    })?;
    rows.collect()
}

struct Context {
    agents: HashMap<String, Agent>,
    /// Transcript sub-agent id → bus agent id, where they differ.
    refs: HashMap<String, String>,
    /// Session → the agent that owns it: its root, or a child with a session of its own.
    sessions: HashMap<String, String>,
    takes: Vec<Take>,
    delegations: Vec<Delegation>,
}

impl Context {
    /// The bus agent a turn ran as: a sub-agent by its transcript id, a main thread by its session.
    fn agent_of(&self, session: &str, sub_agent: Option<&str>) -> Option<String> {
        match sub_agent {
            Some(id) if self.agents.contains_key(id) => Some(id.to_owned()),
            Some(id) => self.refs.get(id).cloned(),
            None => self.sessions.get(session).cloned(),
        }
    }

    /// The agent's own open take at `ts`, else its nearest ancestor's.
    fn take_of(&self, agent: &str, ts: i64) -> Option<&Take> {
        let mut current = Some(agent);
        for _ in 0..DEPTH_MAX {
            let id = current?;
            let own = self
                .takes
                .iter()
                .filter(|t| t.agent == id && t.open_at(ts))
                .max_by_key(|t| t.opened);
            if own.is_some() {
                return own;
            }
            current = self.agents.get(id)?.parent.as_deref();
        }
        None
    }

    /// The take a handoff or question still binds `agent` to at `ts`, newest first: the
    /// sender's task when it was sent, while that task is still open.
    fn delegated(&self, agent: &str, ts: i64) -> Option<&Take> {
        self.delegations
            .iter()
            .filter(|d| d.to == agent && d.sent <= ts && d.ended.is_none_or(|end| ts < end))
            .filter_map(|d| self.take_of(&d.from, d.sent))
            .find(|take| take.open_at(ts))
    }
}

/// Give every turn not yet in a task its task. Existing assignments are never changed.
pub fn assign(db: &Path) -> anyhow::Result<()> {
    let mut conn = crate::store::init(db)?;
    ensure(&conn)?;
    let tx = crate::store::write_tx(&mut conn)?;
    let (agents, refs) = agents(&tx)?;
    let takes = sync_declared(&tx, &agents)?;
    tx.execute(
        "UPDATE tasks SET closed_at=(SELECT min(n.opened_at) FROM tasks n
           WHERE n.kind='request' AND n.session_id=tasks.session_id AND n.opened_at>tasks.opened_at)
         WHERE kind='request'",
        [],
    )?;
    let mut stmt = tx.prepare(
        "SELECT id,ts,session_id,is_subagent,agent_id FROM usage_events u
         WHERE NOT EXISTS (SELECT 1 FROM task_turns t WHERE t.event_id=u.id) ORDER BY ts",
    )?;
    let turns = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    if !turns.is_empty() {
        // A Claude subagent shares its root's session; the root owns it.
        let mut sessions = HashMap::new();
        for (id, a) in &agents {
            if a.parent.is_none() || !sessions.contains_key(&a.session) {
                sessions.insert(a.session.clone(), id.clone());
            }
        }
        let context = Context {
            agents,
            refs,
            sessions,
            takes,
            delegations: delegations(&tx)?,
        };
        for (event, ts, session, sub_agent, agent_id) in turns {
            let agent = context.agent_of(&session, agent_id.as_deref().filter(|_| sub_agent));
            let declared = agent
                .as_deref()
                .and_then(|a| context.take_of(a, ts).or_else(|| context.delegated(a, ts)));
            let task = match declared {
                Some(take) => take.id.clone(),
                None => request_of(&tx, &session, ts)?,
            };
            tx.execute(
                "INSERT OR IGNORE INTO task_turns (event_id,task_id,agent_id) VALUES (?1,?2,?3)",
                params![event, task, agent],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// The session's latest request opened at or before `ts`. A turn with no prompt before it
/// (a harness whose prompts are not read) opens one for its session, at its own time.
fn request_of(conn: &Connection, session: &str, ts: i64) -> rusqlite::Result<String> {
    let found = conn
        .query_row(
            "SELECT id FROM tasks WHERE kind='request' AND session_id=?1 AND opened_at<=?2
             ORDER BY opened_at DESC LIMIT 1",
            params![session, ts],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = found {
        return Ok(id);
    }
    let id = hash(&["session", session]);
    conn.execute(
        "INSERT INTO tasks (id,kind,name,session_id,opened_at) VALUES (?1,'request',?2,?3,?4)
         ON CONFLICT(id) DO UPDATE SET opened_at=min(opened_at, excluded.opened_at)",
        params![id, time_name(ts), session, ts],
    )?;
    Ok(id)
}
