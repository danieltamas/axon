//! The handled ledger (BUS-PLAN §3b): work items that are not files, taken or done once per
//! repository, so two agents never redo the same lead, issue or URL. Remote entries arrive
//! over a share (P2P-SPEC §7b) and are filed beside local ones with `peer_id` set.

pub mod api;
pub mod cli;

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::store::{append_event, now_ms};

pub const DEFAULT_TTL_MS: i64 = 2 * 3600 * 1000;
const KEY_MAX: usize = 200;
const NOTE_MAX: usize = 400;
const LIST_MAX: i64 = 500;
const DONE_KEPT_MS: i64 = 90 * 24 * 3600 * 1000;
const FREE_KEPT_MS: i64 = 7 * 24 * 3600 * 1000;

/// What a ledger command came to: done (exit 0) or refused with the line saying why (exit 1).
pub enum Outcome {
    Done(String),
    Refused(String),
}

#[derive(Clone)]
pub struct Entry {
    pub key: String,
    pub state: String,
    pub holder: String,
    pub agent_id: Option<String>,
    pub peer_id: Option<String>,
    pub note: Option<String>,
    pub at: i64,
    pub expires_at: Option<i64>,
}

impl Entry {
    fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            key: r.get(0)?,
            state: r.get(1)?,
            holder: r.get(2)?,
            agent_id: r.get(3)?,
            peer_id: r.get(4)?,
            note: r.get(5)?,
            at: r.get(6)?,
            expires_at: r.get(7)?,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({"key": self.key, "state": self.state, "holder": self.holder, "note": self.note,
               "at": self.at, "expires_at": self.expires_at})
    }
}

const COLUMNS: &str = "key,state,holder,agent_id,peer_id,note,at,expires_at";

/// `store::init` creates the table too; each ledger command runs this so a hub made by an
/// older version gains it without a restart.
pub fn ensure(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(crate::store::HANDLED_SCHEMA)
}

/// The ledger a working directory belongs to: its repository, worktrees resolving to the main one.
pub fn repo_of(cwd: &Path) -> Result<String, String> {
    crate::snapshot::repo_of(cwd)
        .and_then(|repo| repo.to_str().map(str::to_owned))
        .ok_or_else(|| "not in a repository".to_owned())
}

pub fn key(text: &str) -> Result<String, String> {
    let key = text.trim();
    if key.is_empty() || key.chars().count() > KEY_MAX || key.chars().any(char::is_control) {
        return Err(format!(
            "invalid key: 1 to {KEY_MAX} characters, no control characters"
        ));
    }
    Ok(key.to_owned())
}

pub fn note(text: Option<&str>) -> Result<Option<String>, String> {
    match text.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) if n.chars().count() > NOTE_MAX => {
            Err(format!("note is over {NOTE_MAX} characters"))
        }
        other => Ok(other.map(str::to_owned)),
    }
}

pub fn get(conn: &Connection, repo: &str, key: &str) -> rusqlite::Result<Option<Entry>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM handled WHERE repo=?1 AND key=?2"),
        params![repo, key],
        Entry::from_row,
    )
    .optional()
}

/// Whether an entry still holds its key: done, or taken, unexpired, by a taker still running.
pub fn holds(conn: &Connection, entry: &Entry, now: i64) -> rusqlite::Result<bool> {
    match entry.state.as_str() {
        "done" => Ok(true),
        "taken" if entry.expires_at.is_some_and(|at| at > now) => match &entry.agent_id {
            Some(agent) => Ok(conn
                .query_row("SELECT status FROM agents WHERE id=?1", [agent], |r| {
                    r.get::<_, String>(0)
                })
                .optional()?
                .is_some_and(|status| status == "active" || status == "idle")),
            None => Ok(true),
        },
        _ => Ok(false),
    }
}

/// "40s", "5m", "3h", "2d": how long ago, for a glance.
fn age(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// `taken by <holder> 5m ago: <note>`, the line other agents read.
pub fn line(entry: &Entry, now: i64) -> String {
    let note = entry
        .note
        .as_deref()
        .map(|n| format!(": {n}"))
        .unwrap_or_default();
    let age = age(now - entry.at);
    format!("{} by {} {age} ago{note}", entry.state, entry.holder)
}

/// Store `entry` for `repo` as a change peers will be sent.
pub fn put(conn: &Connection, repo: &str, entry: &Entry, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO handled (repo,key,state,holder,agent_id,peer_id,note,at,expires_at,changed_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(repo,key) DO UPDATE SET state=excluded.state, holder=excluded.holder,
           agent_id=excluded.agent_id, peer_id=excluded.peer_id, note=excluded.note,
           at=excluded.at, expires_at=excluded.expires_at, changed_at=excluded.changed_at",
        params![
            repo,
            entry.key,
            entry.state,
            entry.holder,
            entry.agent_id,
            entry.peer_id,
            entry.note,
            entry.at,
            entry.expires_at,
            now
        ],
    )?;
    Ok(())
}

fn local(
    agent: &str,
    key: &str,
    state: &str,
    note: Option<String>,
    at: i64,
    expires_at: Option<i64>,
) -> Entry {
    Entry {
        key: key.to_owned(),
        state: state.to_owned(),
        holder: agent.to_owned(),
        agent_id: Some(agent.to_owned()),
        peer_id: None,
        note,
        at,
        expires_at,
    }
}

fn record(conn: &Connection, agent: &str, verb: &str, repo: &str, key: &str) -> anyhow::Result<()> {
    append_event(
        conn,
        agent,
        verb,
        key,
        &json!({"repo": repo, "key": key}).to_string(),
    )
}

/// The held entry that stops `agent` from acting on `key`, if any. Call inside a write transaction.
fn blocker(
    conn: &Connection,
    repo: &str,
    agent: &str,
    key: &str,
    now: i64,
) -> rusqlite::Result<Option<Entry>> {
    let Some(entry) = get(conn, repo, key)? else {
        return Ok(None);
    };
    let mine = entry.state == "taken" && entry.agent_id.as_deref() == Some(agent);
    Ok((!mine && holds(conn, &entry, now)?).then_some(entry))
}

pub fn take(
    conn: &Connection,
    repo: &str,
    agent: &str,
    key: &str,
    note: Option<String>,
    ttl_ms: i64,
) -> anyhow::Result<Outcome> {
    let now = now_ms();
    if let Some(entry) = blocker(conn, repo, agent, key, now)? {
        return Ok(Outcome::Refused(line(&entry, now)));
    }
    put(
        conn,
        repo,
        &local(agent, key, "taken", note, now, Some(now + ttl_ms)),
        now,
    )?;
    record(conn, agent, "take", repo, key)?;
    Ok(Outcome::Done(format!("taken {key}")))
}

pub fn done(
    conn: &Connection,
    repo: &str,
    agent: &str,
    key: &str,
    note: Option<String>,
) -> anyhow::Result<Outcome> {
    let now = now_ms();
    if let Some(entry) = blocker(conn, repo, agent, key, now)? {
        return Ok(Outcome::Refused(line(&entry, now)));
    }
    put(conn, repo, &local(agent, key, "done", note, now, None), now)?;
    record(conn, agent, "done", repo, key)?;
    Ok(Outcome::Done(format!("done {key}")))
}

pub fn drop_take(conn: &Connection, repo: &str, agent: &str, key: &str) -> anyhow::Result<Outcome> {
    let now = now_ms();
    let ours = get(conn, repo, key)?
        .filter(|e| e.state == "taken" && e.agent_id.as_deref() == Some(agent));
    let Some(entry) = ours else {
        return Ok(Outcome::Refused(format!("{key} is not taken by you")));
    };
    put(
        conn,
        repo,
        &Entry {
            state: "free".into(),
            expires_at: None,
            at: now,
            ..entry
        },
        now,
    )?;
    record(conn, agent, "drop", repo, key)?;
    Ok(Outcome::Done(format!("dropped {key}")))
}

/// `handled --key`: handled (exit 0) when done or taken by someone else, else free (exit 1).
pub fn check(
    conn: &Connection,
    repo: &str,
    agent: Option<&str>,
    key: &str,
) -> anyhow::Result<Outcome> {
    let now = now_ms();
    let entry = get(conn, repo, key)?;
    Ok(match entry {
        Some(entry) if holds(conn, &entry, now)? => {
            let mine =
                entry.state == "taken" && agent.is_some() && entry.agent_id.as_deref() == agent;
            if mine {
                Outcome::Refused(format!("taken by you; {}", line(&entry, now)))
            } else {
                Outcome::Done(line(&entry, now))
            }
        }
        _ => Outcome::Refused(format!("{key} is free")),
    })
}

/// The live entries of `repo`, newest first, as the CLI and the dashboard list them.
pub fn list(conn: &Connection, repo: &str, prefix: Option<&str>) -> rusqlite::Result<Vec<Value>> {
    let now = now_ms();
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM handled WHERE repo=?1 AND state<>'free'
           AND (?2 IS NULL OR substr(key, 1, length(?2))=?2)
         ORDER BY at DESC LIMIT ?3"
    ))?;
    let entries = stmt
        .query_map(params![repo, prefix, LIST_MAX], Entry::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    for entry in entries {
        if holds(conn, &entry, now)? {
            out.push(entry.to_json());
        }
    }
    Ok(out)
}

/// Turn ended takes into `free` traces, and delete old done entries and traces.
pub fn expire(conn: &Connection, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE handled SET state='free', expires_at=NULL, at=?1, changed_at=?1
         WHERE state='taken' AND (expires_at<=?1 OR (agent_id IS NOT NULL AND NOT EXISTS
           (SELECT 1 FROM agents a WHERE a.id=handled.agent_id AND a.status IN ('active','idle'))))",
        [now],
    )?;
    conn.execute(
        "DELETE FROM handled WHERE state='done' AND at<?1",
        [now - DONE_KEPT_MS],
    )?;
    conn.execute(
        "DELETE FROM handled WHERE state='free' AND changed_at<?1",
        [now - FREE_KEPT_MS],
    )?;
    Ok(())
}
