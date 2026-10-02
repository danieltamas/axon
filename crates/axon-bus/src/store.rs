//! The bus tables inside Axon's `axon.db` (BUS-PLAN §2): schema, the append-only
//! hash-chained `events` log, and its verification.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};

// Beside this file, so the tests that include `store.rs` by path still resolve it.
#[path = "store_migrate.rs"]
mod migrate;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS agents (
    id           TEXT PRIMARY KEY,
    harness      TEXT NOT NULL,
    session_id   TEXT NOT NULL,
    agent_ref    TEXT,
    pid          INTEGER,
    parent_id    TEXT REFERENCES agents(id),
    root_id      TEXT NOT NULL,
    role         TEXT,
    mission      TEXT,
    model        TEXT,
    effort       TEXT,
    repo         TEXT,
    cwd          TEXT,
    worktree     TEXT,
    status       TEXT NOT NULL CHECK (status IN ('active','idle','closed','orphaned')),
    started_at   INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    ended_at     INTEGER,
    introduced_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_agents_root ON agents(root_id);
CREATE INDEX IF NOT EXISTS idx_agents_session ON agents(session_id);

CREATE TABLE IF NOT EXISTS claims (
    agent_id   TEXT NOT NULL REFERENCES agents(id),
    checkout   TEXT NOT NULL,
    path       TEXT NOT NULL,
    task       TEXT,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (agent_id, checkout, path)
);

CREATE TABLE IF NOT EXISTS edges (
    from_id    TEXT NOT NULL REFERENCES agents(id),
    to_id      TEXT NOT NULL REFERENCES agents(id),
    kind       TEXT NOT NULL CHECK (kind IN ('tree','link','grant')),
    thread     TEXT,
    expires_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_edges_pair ON edges(from_id, to_id);

CREATE TABLE IF NOT EXISTS messages (
    id            TEXT PRIMARY KEY,
    thread        TEXT NOT NULL,
    seq           INTEGER NOT NULL,
    from_id       TEXT NOT NULL,
    to_id         TEXT NOT NULL REFERENCES agents(id),
    kind          TEXT NOT NULL,
    body          TEXT NOT NULL,
    refs_json     TEXT NOT NULL DEFAULT '[]',
    needs_reply   INTEGER NOT NULL DEFAULT 0,
    deadline      INTEGER,
    default_reply TEXT,
    sent_at       INTEGER,
    delivered_at  INTEGER,
    acked_at      INTEGER
);
CREATE INDEX IF NOT EXISTS idx_messages_inbox ON messages(to_id, delivered_at);
CREATE INDEX IF NOT EXISTS idx_messages_thread ON messages(thread, seq);
-- from_id has no foreign key: a remote sender, `peer:<label>/<session>`, has no agents row
-- (docs/P2P-SPEC.md §5). These triggers keep the key for every local sender.
CREATE TRIGGER IF NOT EXISTS messages_local_sender_insert BEFORE INSERT ON messages
WHEN NEW.from_id NOT LIKE 'peer:%' AND NOT EXISTS (SELECT 1 FROM agents WHERE id=NEW.from_id)
BEGIN SELECT RAISE(ABORT, 'FOREIGN KEY constraint failed'); END;
CREATE TRIGGER IF NOT EXISTS messages_local_sender_update BEFORE UPDATE OF from_id ON messages
WHEN NEW.from_id NOT LIKE 'peer:%' AND NOT EXISTS (SELECT 1 FROM agents WHERE id=NEW.from_id)
BEGIN SELECT RAISE(ABORT, 'FOREIGN KEY constraint failed'); END;

-- cache_write_tokens are 5-minute writes; 1-hour writes are priced differently.
-- source_key dedupes transcript turns (one row per message id); source_offset is the
-- transcript byte offset ingest resumes from.
CREATE TABLE IF NOT EXISTS usage (
    agent_id              TEXT NOT NULL REFERENCES agents(id),
    ts                    INTEGER NOT NULL,
    model                 TEXT NOT NULL,
    input_tokens          INTEGER NOT NULL DEFAULT 0,
    output_tokens         INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens     INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens    INTEGER NOT NULL DEFAULT 0,
    cache_write_1h_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd              REAL,
    source_offset         INTEGER,
    source_key            TEXT,
    -- When the bus ingested the row; staleness (§6) is about receipt, not turn time.
    received_at           INTEGER,
    UNIQUE (agent_id, source_key)
);
CREATE INDEX IF NOT EXISTS idx_usage_agent ON usage(agent_id, ts);

-- stale_warned_at: when the one stale-usage warning went out (reset by fresh usage).
CREATE TABLE IF NOT EXISTS budgets (
    scope_id        TEXT PRIMARY KEY REFERENCES agents(id),
    kind            TEXT NOT NULL CHECK (kind IN ('tree','agent')),
    tokens_max      INTEGER,
    usd_max         REAL,
    state           TEXT NOT NULL DEFAULT 'ok' CHECK (state IN ('ok','warned','stopped')),
    stale_warned_at INTEGER
);

-- One row per `route` answer: the rule's answer and, as a shadow, what an advisor proposed.
CREATE TABLE IF NOT EXISTS routing_decisions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           INTEGER NOT NULL,
    role         TEXT,
    task_hash    TEXT NOT NULL,
    rule_json    TEXT NOT NULL,
    advisor_json TEXT
);

-- What agents say and think (BUS-PLAN §7), one row per block. `text` and `tool_detail`
-- stay NULL unless content capture is on; rows expire after `narrative_days` (default 7, transcript::retention_ms).
CREATE TABLE IF NOT EXISTS narrative (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_id    TEXT NOT NULL REFERENCES agents(id),
    ts          INTEGER NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('assistant','reasoning','progress','tool')),
    source      TEXT NOT NULL,
    text        TEXT,
    recorded    INTEGER,
    tokens      INTEGER,
    tool_name   TEXT,
    tool_detail TEXT,
    failed      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_narrative_agent ON narrative(agent_id, id);

-- How far each agent's transcript has been read, whether or not that part held usage.
CREATE TABLE IF NOT EXISTS ingest_cursors (
    agent_id    TEXT NOT NULL REFERENCES agents(id),
    path        TEXT NOT NULL,
    byte_offset INTEGER NOT NULL,
    PRIMARY KEY (agent_id, path)
);

-- Machine-wide switches; `content_capture` ('1'/'0') is set by the last `serve` boot.
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Owner login (P2P-SPEC §1): single-use nonces and cookie sessions, both stored as SHA-256 hashes.
CREATE TABLE IF NOT EXISTS login_nonces (
    nonce_hash TEXT PRIMARY KEY,
    expires_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS dashboard_sessions (
    session_hash TEXT PRIMARY KEY,
    token_hash   TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           INTEGER NOT NULL,
    actor        TEXT NOT NULL,
    verb         TEXT NOT NULL,
    subject      TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    prev_hash    TEXT NOT NULL
);
CREATE TRIGGER IF NOT EXISTS events_no_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'events is append-only'); END;
CREATE TRIGGER IF NOT EXISTS events_no_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'events is append-only'); END;

-- Federation (docs/P2P-SPEC.md §5); the owner-session tables of the section live in session.rs.
CREATE TABLE IF NOT EXISTS peers (
  peer_id TEXT PRIMARY KEY,            -- random, local
  node_id TEXT NOT NULL, label TEXT NOT NULL,   -- node_id: remote iroh public key
  generation INTEGER NOT NULL,         -- new on every pairing of this node_id
  state TEXT NOT NULL CHECK (state IN ('pending_confirm','active','paused','removed')),
  local_confirmed_at INTEGER, remote_confirmed_at INTEGER,
  paired_at INTEGER NOT NULL, paused_at INTEGER, removed_at INTEGER, removed_reason TEXT,
  last_error TEXT, remote_paused INTEGER NOT NULL DEFAULT 0, lifecycle_seq INTEGER NOT NULL DEFAULT 0, remote_lifecycle_seq INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX IF NOT EXISTS peers_live_node ON peers(node_id) WHERE state <> 'removed';
CREATE UNIQUE INDEX IF NOT EXISTS peers_live_label ON peers(label) WHERE state <> 'removed';
-- Direct addresses last seen on an authenticated connection from the peer (never a claim the
-- peer makes); fed to the endpoint on start. Removing the peer deletes the row.
CREATE TABLE IF NOT EXISTS peer_addrs (peer_id TEXT PRIMARY KEY, addrs_json TEXT NOT NULL, seen_at INTEGER NOT NULL);
CREATE TRIGGER IF NOT EXISTS peer_addrs_forget AFTER UPDATE OF state ON peers WHEN NEW.state='removed'
BEGIN DELETE FROM peer_addrs WHERE peer_id=NEW.peer_id; END;
CREATE TABLE IF NOT EXISTS peer_invites (invite_id TEXT PRIMARY KEY, secret_hash TEXT NOT NULL, expires_at INTEGER NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, consumed_by TEXT, cancelled_at INTEGER);
CREATE TABLE IF NOT EXISTS peer_shares (
  share_id TEXT PRIMARY KEY, peer_id TEXT NOT NULL, label TEXT NOT NULL,
  local_repo TEXT,                     -- canonical repo_of path; NULL until this owner maps it
  inbound INTEGER NOT NULL, outbound INTEGER NOT NULL,
  remote_inbound INTEGER NOT NULL DEFAULT 0, remote_outbound INTEGER NOT NULL DEFAULT 0,
  revision INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('offered_out','offered_in','active','removed')),
  root_commit TEXT                     -- matching hint only
);
CREATE TABLE IF NOT EXISTS fed_sessions (session TEXT PRIMARY KEY, agent_id TEXT NOT NULL, share_id TEXT NOT NULL, UNIQUE(agent_id, share_id));
CREATE TABLE IF NOT EXISTS fed_outbox (
  message_id TEXT PRIMARY KEY, peer_id TEXT NOT NULL, generation INTEGER NOT NULL,
  share_id TEXT NOT NULL, revision INTEGER NOT NULL, from_agent TEXT NOT NULL,
  envelope_json TEXT NOT NULL, bytes INTEGER NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('queued','accepted','expired','cancelled','rejected')),
  attempts INTEGER NOT NULL DEFAULT 0, next_attempt_at INTEGER, last_error TEXT
);
CREATE TABLE IF NOT EXISTS fed_inbox (
  peer_id TEXT NOT NULL, generation INTEGER NOT NULL, message_id TEXT NOT NULL,
  content_hash TEXT NOT NULL, local_message_id TEXT NOT NULL, accepted_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
  PRIMARY KEY (peer_id, generation, message_id)
);
CREATE TABLE IF NOT EXISTS fed_remote_sessions (peer_id TEXT NOT NULL, share_id TEXT NOT NULL, session TEXT NOT NULL, label TEXT NOT NULL, availability TEXT NOT NULL, seen_at INTEGER NOT NULL, PRIMARY KEY (peer_id, session));
CREATE TABLE IF NOT EXISTS fed_audit (seq INTEGER PRIMARY KEY, ts INTEGER NOT NULL, peer_fingerprint TEXT, generation INTEGER, share_id TEXT, message_id TEXT, direction TEXT, decision TEXT NOT NULL, reason TEXT);
";

/// `prev_hash` of the first event.
const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// A hook waits at most this long for a lock, inside its 300 ms budget (§2).
const HOOK_BUSY_TIMEOUT: Duration = Duration::from_millis(150);
/// Everything else (CLI verbs, the server) can wait out another process's write.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Create or migrate the shared database. Axon's own schema goes first, so a pre-bus
/// `events` usage table is moved aside before the audit log claims the name.
pub fn init(path: &Path) -> anyhow::Result<Connection> {
    let path_str = path.to_str().context("database path is not UTF-8")?;
    drop(axon_core::store::Store::open(path_str)?);
    let conn = open(path)?;
    migrate::drop_stale_login_tables(&conn)?;
    conn.execute_batch(SCHEMA)?;
    migrate::add_missing_columns(&conn)?;
    migrate::drop_sender_foreign_key(&conn)?;
    Ok(conn)
}

/// Open an existing database; never creates one (a hook must not create an absent hub).
pub fn open(path: &Path) -> anyhow::Result<Connection> {
    open_with(path, BUSY_TIMEOUT)
}

/// `open` for a hook, which gives up on a held lock quickly rather than stall the harness.
pub fn open_for_hook(path: &Path) -> anyhow::Result<Connection> {
    open_with(path, HOOK_BUSY_TIMEOUT)
}

fn open_with(path: &Path, busy: Duration) -> anyhow::Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .with_context(|| format!("open {}", path.display()))?;
    conn.busy_timeout(busy)?;
    // The hub is WAL (set by axon-core); NORMAL drops the fsync on every commit, as Axon does.
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(conn)
}

/// A write transaction that takes the lock up front, so reading the chain head and
/// appending to it cannot interleave with another process.
pub fn write_tx(conn: &mut Connection) -> rusqlite::Result<rusqlite::Transaction<'_>> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
}

/// A machine-wide switch from the `settings` table.
pub fn setting(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
        r.get(0)
    })
    .optional()
}

/// Store a machine-wide switch, or remove it with `None`.
pub fn put_setting(conn: &Connection, key: &str, value: Option<&str>) -> rusqlite::Result<()> {
    match value {
        Some(value) => conn.execute(
            "INSERT INTO settings (key,value) VALUES (?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [key, value],
        ),
        None => conn.execute("DELETE FROM settings WHERE key=?1", [key]),
    }
    .map(drop)
}

/// Append one audit event, chained to the previous one. Call inside `write_tx`.
pub fn append_event(
    conn: &Connection,
    actor: &str,
    verb: &str,
    subject: &str,
    payload: &str,
) -> anyhow::Result<()> {
    let head: Option<(i64, i64, String, String, String, String, String)> = conn
        .query_row(
            "SELECT seq,ts,actor,verb,subject,payload_hash,prev_hash FROM events
             ORDER BY seq DESC LIMIT 1",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .optional()?;
    let prev_hash = head.map_or_else(
        || GENESIS.to_owned(),
        |(seq, ts, actor, verb, subject, payload_hash, prev_hash)| {
            row_hash(seq, ts, &actor, &verb, &subject, &payload_hash, &prev_hash)
        },
    );
    conn.execute(
        "INSERT INTO events (ts,actor,verb,subject,payload_hash,prev_hash)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            now_ms(),
            actor,
            verb,
            subject,
            blake3::hash(payload.as_bytes()).to_hex().as_str(),
            prev_hash
        ],
    )?;
    Ok(())
}

fn row_hash(
    seq: i64,
    ts: i64,
    actor: &str,
    verb: &str,
    subject: &str,
    payload_hash: &str,
    prev_hash: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    // Length-prefixed fields, so moving bytes between adjacent fields changes the hash.
    for field in [
        seq.to_string().as_str(),
        ts.to_string().as_str(),
        actor,
        verb,
        subject,
        payload_hash,
        prev_hash,
    ] {
        hasher.update(&(field.len() as u64).to_le_bytes());
        hasher.update(field.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// Walk the chain; fail naming the first event whose content no longer matches the
/// hash its successor recorded. Returns the number of events checked.
/// The number of events checked and the chain head's hash. Record the head elsewhere to
/// detect a truncated tail, which a chain cannot reveal by itself.
pub fn verify(conn: &Connection) -> anyhow::Result<(u64, String)> {
    let mut stmt = conn.prepare(
        "SELECT seq,ts,actor,verb,subject,payload_hash,prev_hash FROM events ORDER BY seq",
    )?;
    let mut rows = stmt.query([])?;
    let mut expected = GENESIS.to_owned();
    let mut previous_seq: Option<i64> = None;
    let mut checked = 0;
    while let Some(r) = rows.next()? {
        let seq: i64 = r.get(0)?;
        let prev_hash: String = r.get(6)?;
        if prev_hash != expected {
            match previous_seq {
                Some(damaged) => bail!(
                    "audit verification failed: event {damaged} was altered \
                     (event {seq} links to a different hash)"
                ),
                None => bail!("audit verification failed: event {seq} is not the chain start"),
            }
        }
        expected = row_hash(
            seq,
            r.get(1)?,
            &r.get::<_, String>(2)?,
            &r.get::<_, String>(3)?,
            &r.get::<_, String>(4)?,
            &r.get::<_, String>(5)?,
            &prev_hash,
        );
        previous_seq = Some(seq);
        checked += 1;
    }
    Ok((checked, expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn
    }

    #[test]
    fn chain_verifies_and_rejects_mutation() {
        let conn = memory();
        for actor in ["a", "b", "c"] {
            append_event(&conn, actor, "register", actor, "{}").unwrap();
        }
        assert_eq!(verify(&conn).unwrap().0, 3);
        assert!(conn.execute("UPDATE events SET actor='x'", []).is_err());
        assert!(conn.execute("DELETE FROM events", []).is_err());
    }

    #[test]
    fn verify_names_the_edited_event() {
        let conn = memory();
        for actor in ["a", "b"] {
            append_event(&conn, actor, "register", actor, "{}").unwrap();
        }
        conn.execute_batch("DROP TRIGGER events_no_update; UPDATE events SET verb='x' WHERE seq=1")
            .unwrap();
        let err = verify(&conn).unwrap_err().to_string();
        assert!(err.contains("event 1 was altered"), "{err}");
    }
}
