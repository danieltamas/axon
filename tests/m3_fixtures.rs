//! ccflare-family acceptance gates: a real SQLite `requests` table (built in a temp file)
//! parses → normalizes → costs, for BOTH family schemas:
//!
//! - better-ccflare (detailed token columns + `agent_used` + `project` + `cost_usd`)
//! - upstream ccflare (legacy `prompt_tokens`/`completion_tokens`, no agent/project)
//!
//! This is the integration counterpart to the pure `turn_from_row` unit tests in the module.

use std::path::PathBuf;

use axon::ingest;
use axon::model::Harness;
use axon::normalize;
use axon::pricing::Pricing;
use rusqlite::Connection;

/// A unique temp DB path (PID + caller-supplied tag — no rng needed for test isolation).
fn temp_db(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("axon_ccflare_{}_{}.db", std::process::id(), tag));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn better_ccflare_schema_attributes_agents_and_trusts_cost() {
    let db = temp_db("bcf");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE requests (
                id TEXT PRIMARY KEY,
                timestamp INTEGER NOT NULL,
                method TEXT, path TEXT,
                account_used TEXT,
                model TEXT,
                cost_usd REAL DEFAULT 0,
                output_tokens_per_second REAL,
                input_tokens INTEGER DEFAULT 0,
                cache_read_input_tokens INTEGER DEFAULT 0,
                cache_creation_input_tokens INTEGER DEFAULT 0,
                output_tokens INTEGER DEFAULT 0,
                response_time_ms INTEGER,
                agent_used TEXT,
                project TEXT
            );
            -- a main-thread completion, ccflare-priced
            INSERT INTO requests VALUES
              ('r1', 1781000000000, 'POST', '/v1/messages', 'acct-a',
               'claude-sonnet-4-5', 0.0123, 30.0, 700, 40, 20, 150, 5000, NULL, '/repo');
            -- a sub-agent completion, ccflare-priced
            INSERT INTO requests VALUES
              ('r2', 1781000100000, 'POST', '/v1/messages', 'acct-a',
               'claude-opus-4-5', 0.5, 12.0, 2000, 0, 0, 800, 9000, 'security-reviewer', '/repo');
            -- a non-completion row (token refresh) → must be skipped (model NULL)
            INSERT INTO requests VALUES
              ('r3', 1781000200000, 'POST', '/v1/oauth', 'acct-a',
               NULL, 0, NULL, 0, 0, 0, 0, NULL, NULL, NULL);",
        )
        .unwrap();
    }

    let turns = ingest::scan_ccflare_db(&db);
    assert_eq!(turns.len(), 2, "two completions; the oauth row is skipped");

    let p = Pricing::bundled();
    let main = turns.iter().find(|t| !t.is_subagent).unwrap();
    let e = normalize::to_event(main, &p).unwrap();
    assert_eq!(e.harness, Harness::Ccflare);
    assert_eq!(e.agent, "main");
    assert_eq!(e.project, "/repo");
    assert_eq!(e.tokens_in, 700);
    assert_eq!(e.tokens_out, 150);
    assert_eq!(e.cache_read, 40);
    assert_eq!(e.cache_write_5m, 20);
    assert!(e.ts > 0, "epoch-ms timestamp parsed");
    assert!(e.duration_ms.is_some(), "response_time_ms → duration");
    assert!(
        (e.cost_eur - 0.0123 * p.fx()).abs() < 1e-9,
        "trusts ccflare cost: 0.0123 USD × fx, got {}",
        e.cost_eur
    );

    let sub = turns.iter().find(|t| t.is_subagent).unwrap();
    let se = normalize::to_event(sub, &p).unwrap();
    assert_eq!(se.agent, "security-reviewer", "agent_used → attribution");
    assert!(se.is_subagent);
    assert_ne!(e.id, se.id, "distinct request ids → distinct events");

    let _ = std::fs::remove_file(&db);
}

#[test]
fn upstream_ccflare_legacy_schema_still_parses() {
    let db = temp_db("upstream");
    {
        let conn = Connection::open(&db).unwrap();
        // Lean legacy shape: no input_tokens/agent_used/project; only prompt/completion + cost.
        conn.execute_batch(
            "CREATE TABLE requests (
                id TEXT PRIMARY KEY,
                timestamp INTEGER NOT NULL,
                method TEXT, path TEXT,
                account_used TEXT,
                model TEXT,
                prompt_tokens INTEGER DEFAULT 0,
                completion_tokens INTEGER DEFAULT 0,
                total_tokens INTEGER DEFAULT 0,
                cost_usd REAL DEFAULT 0
            );
            INSERT INTO requests VALUES
              ('u1', 1781000000000, 'POST', '/v1/messages', 'acct-x',
               'claude-sonnet-4-5', 1000, 250, 1250, 0.02);",
        )
        .unwrap();
    }

    let turns = ingest::scan_ccflare_db(&db);
    assert_eq!(
        turns.len(),
        1,
        "legacy schema parsed via PRAGMA introspection"
    );
    let t = &turns[0];
    assert_eq!(t.agent, "main", "no agent_used column → main");
    assert_eq!(t.tokens_in, 1000, "prompt_tokens fallback");
    assert_eq!(t.tokens_out, 250, "completion_tokens fallback");
    assert_eq!(t.project, "", "no project column → empty");

    let e = normalize::to_event(t, &Pricing::bundled()).unwrap();
    assert_eq!(e.harness, Harness::Ccflare);
    assert!(e.ts > 0);

    let _ = std::fs::remove_file(&db);
}

#[test]
fn missing_or_foreign_db_degrades_to_empty() {
    // Non-existent file → empty, no panic.
    assert!(ingest::scan_ccflare_db(&temp_db("nope")).is_empty());

    // A DB without a usable requests table → empty.
    let db = temp_db("foreign");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE other (x INTEGER);")
            .unwrap();
    }
    assert!(ingest::scan_ccflare_db(&db).is_empty());
    let _ = std::fs::remove_file(&db);
}
