//! Agents and their status (BUS-PLAN §2). Every change is recorded in the audit log.

use anyhow::{bail, Context};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;

use crate::store::{append_event, now_ms};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Active,
    Idle,
    Closed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Active => "active",
            Status::Idle => "idle",
            Status::Closed => "closed",
        }
    }
}

#[derive(Default)]
pub struct Agent<'a> {
    pub id: &'a str,
    pub harness: &'a str,
    pub session_id: &'a str,
    pub parent_id: Option<&'a str>,
    pub cwd: Option<&'a str>,
    pub model: Option<&'a str>,
    pub role: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub mission: Option<&'a str>,
    /// The harness process the hook ran under, for its memory (§7.1).
    pub pid: Option<i64>,
}

/// Register `agent` with `status`, or, when it is already known, move it to `status`
/// and fill in fields the earlier hook could not see. Call inside a write transaction.
pub fn upsert(conn: &Connection, agent: &Agent, status: Status) -> anyhow::Result<()> {
    let now = now_ms();
    let known: Option<String> = conn
        .query_row("SELECT status FROM agents WHERE id=?1", [agent.id], |r| {
            r.get(0)
        })
        .optional()?;
    if let Some(previous) = known {
        conn.execute(
            "UPDATE agents SET status=?2, last_seen_at=?3, model=coalesce(?4, model),
             cwd=coalesce(cwd, ?5), pid=coalesce(?6, pid),
             ended_at=CASE WHEN ?2='closed' THEN ended_at END WHERE id=?1",
            params![
                agent.id,
                status.as_str(),
                now,
                agent.model,
                agent.cwd,
                agent.pid
            ],
        )?;
        if previous != status.as_str() {
            record_status(conn, agent.id, status)?;
        }
        return Ok(());
    }
    let root_id = match agent.parent_id {
        Some(parent) => {
            root_of(conn, parent)?.with_context(|| format!("parent {parent} is not registered"))?
        }
        None => agent.id.to_owned(),
    };
    conn.execute(
        "INSERT INTO agents (id,harness,session_id,parent_id,root_id,model,cwd,status,
                             started_at,last_seen_at,role,effort,mission,pid)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10,?11,?12,?13)",
        params![
            agent.id,
            agent.harness,
            agent.session_id,
            agent.parent_id,
            root_id,
            agent.model,
            agent.cwd,
            status.as_str(),
            now,
            agent.role,
            agent.effort,
            agent.mission,
            agent.pid
        ],
    )?;
    let payload = json!({"harness": agent.harness, "session_id": agent.session_id,
        "parent_id": agent.parent_id, "root_id": root_id, "status": status.as_str()});
    append_event(conn, agent.id, "register", agent.id, &payload.to_string())
}

/// Register a node from the CLI; unlike a hook, re-registering a known id is an error.
pub fn register(conn: &Connection, agent: &Agent) -> anyhow::Result<()> {
    if root_of(conn, agent.id)?.is_some() {
        bail!("agent {} is already registered", agent.id);
    }
    upsert(conn, agent, Status::Idle)
}

/// Move a known agent to `status`; unknown ids are ignored (nothing to close).
pub fn set_status(conn: &Connection, id: &str, status: Status) -> anyhow::Result<()> {
    let now = now_ms();
    let changed = conn.execute(
        "UPDATE agents SET status=?2, last_seen_at=?3,
         ended_at=CASE WHEN ?2='closed' THEN ?3 ELSE ended_at END
         WHERE id=?1 AND status<>?2",
        params![id, status.as_str(), now],
    )?;
    if changed == 0 {
        conn.execute(
            "UPDATE agents SET last_seen_at=?2 WHERE id=?1",
            params![id, now],
        )?;
        return Ok(());
    }
    if status == Status::Closed {
        crate::claims::release_all(conn, id)?;
    }
    record_status(conn, id, status)
}

pub fn touch(conn: &Connection, id: &str) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE agents SET last_seen_at=?2 WHERE id=?1",
        params![id, now_ms()],
    )?;
    Ok(())
}

/// Close the roots whose harness process exited without saying so (a crash, a killed
/// terminal), then every open agent under a closed root. `running` answers for a pid last
/// heard from at a time, or None when it cannot tell, which closes nothing.
pub fn reap(conn: &Connection, running: impl Fn(i64, i64) -> Option<bool>) -> anyhow::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT id,pid,last_seen_at FROM agents
         WHERE parent_id IS NULL AND pid IS NOT NULL AND status IN ('active','idle')",
    )?;
    let roots: Vec<(String, i64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, pid, last_seen) in roots {
        if running(pid, last_seen) == Some(false) {
            set_status(conn, &id, Status::Closed)?;
        }
    }
    let orphans: Vec<String> = conn
        .prepare(
            "SELECT a.id FROM agents a JOIN agents r ON r.id=a.root_id
             WHERE a.id<>a.root_id AND a.status IN ('active','idle') AND r.status='closed'",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for id in orphans {
        set_status(conn, &id, Status::Closed)?;
    }
    Ok(())
}

pub fn root_of(conn: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT root_id FROM agents WHERE id=?1", [id], |r| r.get(0))
        .optional()
}

fn record_status(conn: &Connection, id: &str, status: Status) -> anyhow::Result<()> {
    let payload = json!({"status": status.as_str()});
    append_event(conn, id, "status", id, &payload.to_string())
}
