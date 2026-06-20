//! SQLite persistence (DESIGN.md §9, §16). Events are keyed on their idempotent `id`, so a
//! boot-scan, a re-scan, and (later) a live-tail delta all converge to the same row set.
//!
//! M1 uses a single connection. The mpsc single-writer / reader-pool contract and the
//! `files(path, inode, offset, size)` tail-resume table arrive with the live pipeline (M4).

use std::path::Path;

use anyhow::Context;
use rusqlite::{params, Connection};

use crate::model::{Event, Harness, PricingKind};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    id              TEXT PRIMARY KEY,
    ts              INTEGER NOT NULL,
    harness         TEXT NOT NULL,
    project         TEXT NOT NULL,
    agent           TEXT NOT NULL,
    is_subagent     INTEGER NOT NULL,
    model           TEXT NOT NULL,
    session_id      TEXT NOT NULL,
    tokens_in       INTEGER NOT NULL,
    tokens_out      INTEGER NOT NULL,
    cache_read      INTEGER NOT NULL,
    cache_write_5m  INTEGER NOT NULL,
    cache_write_1h  INTEGER NOT NULL,
    duration_ms     INTEGER,
    loc_added       INTEGER NOT NULL,
    loc_removed     INTEGER NOT NULL,
    loc_failed      INTEGER NOT NULL,
    skills          TEXT NOT NULL,
    cost_eur        REAL NOT NULL,
    cost_credits    REAL,
    pricing_kind    TEXT NOT NULL DEFAULT 'unknown',
    unpriced        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts);
CREATE INDEX IF NOT EXISTS idx_events_agent ON events(agent);
";

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (and create) the database. Pass `":memory:"` for tests. For a real file the
    /// parent dir is created `0700` and the db file set `0600` (it holds full cross-project
    /// history — PII-grade).
    pub fn open(path: &str) -> anyhow::Result<Self> {
        if path != ":memory:" {
            if let Some(parent) = Path::new(path).parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create data dir {}", parent.display()))?;
                set_mode(parent, 0o700);
            }
        }
        let conn = Connection::open(path).with_context(|| format!("open sqlite at {path}"))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;",
        )?;
        conn.execute_batch(SCHEMA)?;
        migrate_schema(&conn)?;
        if path != ":memory:" {
            set_mode(Path::new(path), 0o600);
        }
        Ok(Store { conn })
    }

    /// Insert or update one event, keyed on `id` (idempotent).
    pub fn upsert(&self, e: &Event) -> anyhow::Result<()> {
        upsert_with(&self.conn, e)
    }

    /// Upsert many events in a single transaction.
    pub fn upsert_all(&mut self, events: &[Event]) -> anyhow::Result<()> {
        let tx = self.conn.transaction()?;
        for e in events {
            upsert_with(&tx, e)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn count(&self) -> anyhow::Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?)
    }

    /// Read every event back (ordered by id for deterministic comparison).
    pub fn all_events(&self) -> anyhow::Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, harness, project, agent, is_subagent, model, session_id, \
             tokens_in, tokens_out, cache_read, cache_write_5m, cache_write_1h, duration_ms, \
             loc_added, loc_removed, loc_failed, skills, cost_eur, cost_credits, pricing_kind, unpriced \
             FROM events ORDER BY id",
        )?;
        let rows = stmt.query_map([], row_to_event)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

fn upsert_with(conn: &Connection, e: &Event) -> anyhow::Result<()> {
    let skills = serde_json::to_string(&e.skills)?;
    conn.execute(
        "INSERT INTO events (id, ts, harness, project, agent, is_subagent, model, session_id, \
            tokens_in, tokens_out, cache_read, cache_write_5m, cache_write_1h, duration_ms, \
            loc_added, loc_removed, loc_failed, skills, cost_eur, cost_credits, pricing_kind, unpriced) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22) \
         ON CONFLICT(id) DO UPDATE SET \
            ts=excluded.ts, harness=excluded.harness, project=excluded.project, \
            agent=excluded.agent, is_subagent=excluded.is_subagent, model=excluded.model, \
            session_id=excluded.session_id, tokens_in=excluded.tokens_in, \
            tokens_out=excluded.tokens_out, cache_read=excluded.cache_read, \
            cache_write_5m=excluded.cache_write_5m, cache_write_1h=excluded.cache_write_1h, \
            duration_ms=excluded.duration_ms, loc_added=excluded.loc_added, \
            loc_removed=excluded.loc_removed, loc_failed=excluded.loc_failed, \
            skills=excluded.skills, cost_eur=excluded.cost_eur, \
            cost_credits=excluded.cost_credits, pricing_kind=excluded.pricing_kind, \
            unpriced=excluded.unpriced",
        params![
            e.id,
            e.ts,
            e.harness.as_str(),
            e.project,
            e.agent,
            e.is_subagent as i64,
            e.model,
            e.session_id,
            e.tokens_in,
            e.tokens_out,
            e.cache_read,
            e.cache_write_5m,
            e.cache_write_1h,
            e.duration_ms,
            e.loc_added,
            e.loc_removed,
            e.loc_failed as i64,
            skills,
            e.cost_eur,
            e.cost_credits,
            e.pricing_kind.as_str(),
            e.unpriced as i64,
        ],
    )?;
    Ok(())
}

fn row_to_event(r: &rusqlite::Row) -> rusqlite::Result<Event> {
    let harness_str: String = r.get(2)?;
    let skills_json: String = r.get(17)?;
    let pricing_kind: String = r.get(20)?;
    Ok(Event {
        id: r.get(0)?,
        ts: r.get(1)?,
        harness: Harness::from_tag(&harness_str).unwrap_or(Harness::ClaudeCode),
        project: r.get(3)?,
        agent: r.get(4)?,
        is_subagent: r.get::<_, i64>(5)? != 0,
        model: r.get(6)?,
        session_id: r.get(7)?,
        tokens_in: r.get(8)?,
        tokens_out: r.get(9)?,
        cache_read: r.get(10)?,
        cache_write_5m: r.get(11)?,
        cache_write_1h: r.get(12)?,
        duration_ms: r.get(13)?,
        loc_added: r.get(14)?,
        loc_removed: r.get(15)?,
        loc_failed: r.get::<_, i64>(16)? != 0,
        skills: serde_json::from_str(&skills_json).unwrap_or_default(),
        cost_eur: r.get(18)?,
        cost_credits: r.get(19)?,
        pricing_kind: PricingKind::from_tag(&pricing_kind).unwrap_or(PricingKind::Unknown),
        unpriced: r.get::<_, i64>(21)? != 0,
    })
}

fn migrate_schema(conn: &Connection) -> anyhow::Result<()> {
    if !has_column(conn, "events", "cost_credits")? {
        conn.execute("ALTER TABLE events ADD COLUMN cost_credits REAL", [])?;
    }
    if !has_column(conn, "events", "pricing_kind")? {
        conn.execute(
            "ALTER TABLE events ADD COLUMN pricing_kind TEXT NOT NULL DEFAULT 'unknown'",
            [],
        )?;
    }
    conn.execute(
        "UPDATE events SET pricing_kind = 'api-money' \
         WHERE pricing_kind = 'unknown' AND unpriced = 0 AND cost_eur > 0",
        [],
    )?;
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&pragma)?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}
