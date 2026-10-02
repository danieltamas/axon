-- Frozen from main 988ee52f444bb3e77b645c5ccd6585c32a0a8ca8:crates/axon-bus/src/store.rs SCHEMA.
-- Retains messages.from_id REFERENCES agents(id); never generated from the code under test.
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
    from_id       TEXT NOT NULL REFERENCES agents(id),
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
-- stay NULL unless content capture is on; rows expire after 7 days (transcript::RETENTION_MS).
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
