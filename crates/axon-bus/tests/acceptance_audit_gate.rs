//! Independent regressions for audit TEST-1, TEST-2, TEST-3 and TEST-5.
//! Expectations come from the audit and BUS-PLAN §§3/6, including subtree stops.

mod common;
use common::*;
use rusqlite::{Connection, ErrorCode};
use serde_json::json;
use std::fs;
use std::path::PathBuf;

fn tree() -> Bus {
    let bus = Bus::new();
    bus.init();
    bus.register("root", "claude", None);
    bus.register("child", "claude", Some("root"));
    bus
}

fn bell(bus: &Bus, id: &str) -> PathBuf {
    bus.root
        .join("data/axon/doorbells")
        .join(format!("{id}.json"))
}

fn stop_hook(bus: &Bus, session: &str) -> std::process::Output {
    let mut payload = bus.fixture("claude", "Stop");
    payload["session_id"] = json!(session);
    bus.hook("claude", "Stop", &payload)
}

fn acked(bus: &Bus, id: &str) -> bool {
    bus.db()
        .query_row(
            "SELECT acked_at IS NOT NULL FROM messages WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

fn cross_budget(bus: &Bus, scope: &str) {
    bus.ok(&["budget", "set", scope, "100tok"]);
    bus.usage(scope, "claude-sonnet-4.6", [101, 0, 0, 0], now_ms(), 1);
    deny_reason("claude", &bus.gate("claude", scope));
    let pending: i64 = bus
        .db()
        .query_row(
            "SELECT count(*) FROM messages WHERE thread=?1 AND kind='stop' AND acked_at IS NULL",
            [format!("budget:{scope}")],
            |r| r.get(0),
        )
        .unwrap();
    assert!(pending > 0, "the crossing must persist a stop for {scope}");
}

fn query_error(harness: &str, table: &str) {
    let bus = Bus::new();
    bus.init();
    bus.register("agent", harness, None);
    bus.ok(&["budget", "set", "agent", "100tok"]);
    assert_allowed(&bus.gate(harness, "agent"));
    // No stop or doorbell can rescue an accidentally fail-open gate query.
    assert!(!bell(&bus, "agent").exists());
    match table {
        "messages" => bus
            .db()
            .execute_batch("ALTER TABLE messages RENAME TO hidden_messages"),
        "budgets" => bus
            .db()
            .execute_batch("ALTER TABLE budgets RENAME TO hidden_budgets"),
        _ => unreachable!(),
    }
    .unwrap();
    assert_eq!(bus.count("SELECT count(*) FROM agents WHERE id='agent'"), 1);
    if table == "budgets" {
        assert_eq!(
            bus.count("SELECT count(*) FROM messages WHERE kind='stop'"),
            0
        );
    }
    let reason = deny_reason(harness, &bus.gate(harness, "agent"));
    assert!(
        reason.contains("could not check stops and budgets"),
        "{harness}: {reason}"
    );
}

macro_rules! query_cases {
    ($stops:ident, $budget:ident, $healthy:ident, $harness:literal) => {
        #[test]
        fn $stops() {
            query_error($harness, "messages");
        }
        #[test]
        fn $budget() {
            query_error($harness, "budgets");
        }
        #[test]
        fn $healthy() {
            let bus = Bus::new();
            bus.init();
            bus.register("agent", $harness, None);
            bus.ok(&["budget", "set", "agent", "100tok"]);
            assert_allowed(&bus.gate($harness, "agent"));
        }
    };
}

query_cases!(
    test_1_claude_stop_query_error,
    test_1_claude_budget_query_error,
    test_1_claude_healthy_control,
    "claude"
);
query_cases!(
    test_1_codex_stop_query_error,
    test_1_codex_budget_query_error,
    test_1_codex_healthy_control,
    "codex"
);
query_cases!(
    test_1_opencode_stop_query_error,
    test_1_opencode_budget_query_error,
    test_1_opencode_healthy_control,
    "opencode"
);
query_cases!(
    test_1_hermes_stop_query_error,
    test_1_hermes_budget_query_error,
    test_1_hermes_healthy_control,
    "hermes"
);

#[test]
fn test_2_raising_child_budget_preserves_root_stop_until_root_is_raised() {
    let bus = tree();
    cross_budget(&bus, "child");
    cross_budget(&bus, "root");
    bus.ok(&["budget", "set", "child", "1Ktok"]);
    assert_eq!(bus.count("SELECT count(*) FROM messages WHERE thread='budget:child' AND acked_at IS NULL AND kind='stop'"), 0);
    assert!(bus.count("SELECT count(*) FROM messages WHERE thread='budget:root' AND acked_at IS NULL AND kind='stop'") > 0);
    deny_reason("claude", &bus.gate("claude", "child"));
    bus.ok(&["budget", "set", "root", "1Ktok"]);
    assert_allowed(&bus.gate("claude", "child"));
    assert_allowed(&bus.gate("claude", "root"));
}

#[test]
fn test_2_peer_stop_survives_budget_raise() {
    let bus = tree();
    cross_budget(&bus, "child");
    let peer = bus.send("root", "child", "stop", "peer stop survives raise");
    bus.ok(&["budget", "set", "child", "1Ktok"]);
    assert!(!acked(&bus, peer["id"].as_str().unwrap()));
    assert!(
        deny_reason("claude", &bus.gate("claude", "child")).contains("peer stop survives raise")
    );
}

#[test]
fn test_2_stop_hook_acks_peer_but_budget_stop_still_denies() {
    let bus = tree();
    cross_budget(&bus, "child");
    let peer = bus.send("root", "child", "stop", "end this turn");
    stop_hook(&bus, "child");
    assert!(acked(&bus, peer["id"].as_str().unwrap()));
    assert!(bus.count("SELECT count(*) FROM messages WHERE to_id='child' AND thread='budget:child' AND kind='stop' AND acked_at IS NULL") > 0);
    deny_reason("claude", &bus.gate("claude", "child"));
}

fn aliased() -> Bus {
    let bus = Bus::new();
    bus.init();
    // S in TEST-3 is a session identifier; lowercase avoids assuming the private
    // filename encoding for uppercase IDs (audit SEC-9).
    bus.ok(&[
        "register",
        "--id",
        "orch",
        "--harness",
        "claude",
        "--session",
        "session-s",
        "--cwd",
        bus.root.join("work").to_str().unwrap(),
    ]);
    bus.register("child", "claude", Some("orch"));
    bus
}

#[test]
fn test_3_stop_resolves_registered_root_session_alias() {
    let bus = aliased();
    bus.send("child", "orch", "stop", "aliased root stop");
    assert!(deny_reason("claude", &bus.gate("claude", "session-s")).contains("aliased root stop"));
    assert_eq!(
        bus.count("SELECT count(*) FROM agents WHERE id='session-s'"),
        0
    );
}

#[test]
fn test_3_budget_crossing_resolves_registered_root_session_alias() {
    let bus = aliased();
    bus.ok(&["budget", "set", "orch", "100tok"]);
    bus.usage("child", "claude-sonnet-4.6", [101, 0, 0, 0], now_ms(), 1);
    deny_reason("claude", &bus.gate("claude", "session-s"));
    assert!(bell(&bus, "orch").is_file());
    assert!(bell(&bus, "session-s").is_file());
}

#[test]
fn test_3_session_alias_doorbell_denies_without_hub() {
    let bus = aliased();
    bus.send("child", "orch", "stop", "persist alias stop");
    assert!(bell(&bus, "session-s").is_file());
    bus.db()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    fs::rename(bus.db_path(), bus.root.join("saved.db")).unwrap();
    deny_reason("claude", &bus.gate("claude", "session-s"));
    assert!(!bus.db_path().exists());
}

#[test]
fn test_3_stop_hook_acks_alias_and_removes_both_doorbells() {
    let bus = aliased();
    let peer = bus.send("child", "orch", "stop", "finish aliased turn");
    assert!(bell(&bus, "orch").is_file());
    assert!(bell(&bus, "session-s").is_file());
    stop_hook(&bus, "session-s");
    assert!(acked(&bus, peer["id"].as_str().unwrap()));
    assert!(!bell(&bus, "orch").exists());
    assert!(!bell(&bus, "session-s").exists());
    assert_allowed(&bus.gate("claude", "session-s"));
}

// A rollback-journal reader allows BEGIN IMMEDIATE and writes but blocks COMMIT.
// Prove that distinction with SQLite itself before exercising the hook binary.
fn block_commit(bus: &Bus) -> Connection {
    let reader = bus.db();
    reader
        .execute_batch("PRAGMA journal_mode=DELETE; BEGIN; SELECT * FROM agents;")
        .unwrap();
    let writer = bus.db();
    writer
        .execute_batch("BEGIN IMMEDIATE; UPDATE agents SET status='active' WHERE id='child';")
        .unwrap();
    let error = writer.execute_batch("COMMIT").unwrap_err();
    assert_eq!(error.sqlite_error_code(), Some(ErrorCode::DatabaseBusy));
    writer.execute_batch("ROLLBACK").unwrap();
    reader
}

#[test]
fn test_5_stop_hook_removes_child_doorbell_after_commit() {
    let bus = tree();
    let peer = bus.send("root", "child", "stop", "finish child turn");
    assert!(bell(&bus, "child").is_file());
    stop_hook(&bus, "child");
    assert!(acked(&bus, peer["id"].as_str().unwrap()));
    assert!(!bell(&bus, "child").exists());
    assert_allowed(&bus.gate("claude", "child"));
}

#[test]
fn test_5_failed_stop_commit_keeps_unacked_stop_and_doorbell() {
    let bus = tree();
    assert_allowed(&bus.gate("claude", "child"));
    let peer = bus.send("root", "child", "stop", "rollback must preserve stop");
    let reader = block_commit(&bus);
    let output = stop_hook(&bus, "child");
    assert!(
        !output.stderr.is_empty(),
        "failed commit must be observable"
    );
    reader.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        bus.agent("child")["status"],
        "active",
        "Stop status must roll back"
    );
    assert!(!acked(&bus, peer["id"].as_str().unwrap()));
    assert!(bell(&bus, "child").is_file());
    deny_reason("claude", &bus.gate("claude", "child"));
    stop_hook(&bus, "child");
    assert!(acked(&bus, peer["id"].as_str().unwrap()));
    assert!(!bell(&bus, "child").exists());
}

#[test]
fn test_5_pending_budget_stop_keeps_child_doorbell() {
    let bus = tree();
    cross_budget(&bus, "child");
    let peer = bus.send("root", "child", "stop", "peer finishes first");
    stop_hook(&bus, "child");
    assert!(acked(&bus, peer["id"].as_str().unwrap()));
    assert!(bus.count("SELECT count(*) FROM messages WHERE to_id='child' AND kind='stop' AND acked_at IS NULL") > 0);
    assert!(bell(&bus, "child").is_file());
    deny_reason("claude", &bus.gate("claude", "child"));
}

#[test]
fn test_5_budget_denial_stands_when_first_crossing_cannot_commit() {
    let bus = tree();
    bus.ok(&["budget", "set", "child", "100tok"]);
    bus.usage("child", "claude-sonnet-4.6", [101, 0, 0, 0], now_ms(), 1);
    assert_eq!(
        bus.count("SELECT count(*) FROM messages WHERE kind='stop'"),
        0
    );
    assert!(!bell(&bus, "child").exists());
    let reader = block_commit(&bus);
    let output = bus.gate("claude", "child");
    deny_reason("claude", &output);
    assert!(
        !output.stderr.is_empty(),
        "failed commit must be observable"
    );
    reader.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        bus.agent("child")["status"],
        "idle",
        "PreToolUse status must roll back"
    );
    assert_eq!(
        bus.count("SELECT count(*) FROM messages WHERE kind='stop'"),
        0
    );
    assert!(
        bell(&bus, "child").is_file(),
        "uncommitted denial must retain its fallback"
    );
    deny_reason("claude", &bus.gate("claude", "child"));
}
