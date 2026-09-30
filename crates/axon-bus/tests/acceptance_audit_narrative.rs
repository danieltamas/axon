//! Independent TEST-7, TEST-8 and TEST-12 regressions for BUS-PLAN §7.
//! Expiry is exercised through replay/serve, not a call into transcript internals.

mod common;
use common::*;
use rusqlite::params;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

fn setup() -> Bus {
    let bus = Bus::with_limit(Duration::from_secs(8));
    bus.init();
    bus.register("root", "claude", None);
    bus
}

fn record(bus: &Bus, marker: &str, child: Option<&str>) -> Value {
    let mut value: Value = serde_json::from_str(
        include_str!("../../../tests/fixtures/claude_subagent.jsonl")
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    value["timestamp"] = json!(chrono::Utc::now().to_rfc3339());
    value["sessionId"] = json!("root");
    value["cwd"] = json!(bus.root.join("work"));
    value["uuid"] = json!(marker);
    value["message"]["id"] = json!(marker);
    value["message"]["content"] = json!([{"type":"text", "text":marker}]);
    if let Some(child) = child {
        value["agentId"] = json!(child);
    } else {
        for key in [
            "agentId",
            "isSidechain",
            "attributionAgent",
            "sourceToolAssistantUUID",
        ] {
            value.as_object_mut().unwrap().remove(key);
        }
    }
    value
}

fn node<'a>(value: &'a Value, id: &str) -> Option<&'a Value> {
    match value {
        Value::Object(object) => {
            if object.get("id").and_then(Value::as_str) == Some(id) {
                Some(value)
            } else {
                object.values().find_map(|v| node(v, id))
            }
        }
        Value::Array(array) => array.iter().find_map(|v| node(v, id)),
        _ => None,
    }
}

fn narrative(snapshot: &Value, id: &str) -> Vec<Value> {
    node(snapshot, id).unwrap_or_else(|| panic!("missing node {id}: {snapshot}"))["narrative"]
        .as_array()
        .unwrap()
        .clone()
}

// serve publishes snapshots asynchronously. Wait for the expected state, bounded by
// the existing M5 contract that a committed update reaches the page within one second.
fn snapshot_when(bus: &Bus, server: &Server, ready: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let snapshot = server.snapshot(bus);
        if ready(&snapshot) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "snapshot condition unmet: {snapshot}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn stop(bus: &Bus, path: &Path) {
    let mut payload = bus.fixture("claude", "Stop");
    payload["session_id"] = json!("root");
    payload["transcript_path"] = json!(path);
    bus.hook("claude", "Stop", &payload);
}

fn replay(bus: &Bus, records: &[Value]) {
    let path = bus.root.join("narrative-replay.jsonl");
    let envelope: Vec<Value> = records
        .iter()
        .map(|record| json!({"harness":"claude", "session_id":"root", "record":record}))
        .collect();
    write_jsonl(&path, &envelope);
    bus.ok(&["replay", path.to_str().unwrap(), "--speed", "50x"]);
}

#[test]
fn test_7_stop_hook_captures_last_assistant_line_in_snapshot() {
    let bus = setup();
    let server = Server::start(&bus, true);
    let path = bus.root.join("stop-transcript.jsonl");
    write_jsonl(&path, &[record(&bus, "last assistant at Stop", None)]);
    stop(&bus, &path);
    let snapshot = snapshot_when(&bus, &server, |s| {
        s.to_string().contains("last assistant at Stop")
    });
    assert!(narrative(&snapshot, "root")
        .iter()
        .any(|r| r["kind"] == "assistant"
            && r["source"] == "claude"
            && r["text"] == "last assistant at Stop"));
    assert_eq!(bus.count("SELECT count(*) FROM narrative WHERE agent_id='root' AND text='last assistant at Stop'"), 1);
}

#[test]
fn test_7_subagent_stop_does_not_attribute_main_thread_rows_to_child() {
    let bus = setup();
    bus.register("child", "claude", Some("root"));
    let server = Server::start(&bus, true);
    let path = bus.root.join("mixed-transcript.jsonl");
    write_jsonl(
        &path,
        &[
            record(&bus, "main thread only marker", None),
            record(&bus, "child thread only marker", Some("child")),
        ],
    );
    let mut payload = bus.fixture("claude", "SubagentStop");
    payload["session_id"] = json!("root");
    payload["agent_id"] = json!("child");
    payload["transcript_path"] = json!(path);
    // Exercise the shared-transcript fallback with no separate sidechain file.
    payload["agent_transcript_path"] = json!(bus.root.join("absent-child.jsonl"));
    bus.hook("claude", "SubagentStop", &payload);
    let snapshot = snapshot_when(&bus, &server, |s| {
        s.to_string().contains("child thread only marker")
    });
    let rows = narrative(&snapshot, "child");
    assert!(rows.iter().any(|r| r["text"] == "child thread only marker"));
    assert!(!serde_json::to_string(&rows)
        .unwrap()
        .contains("main thread only marker"));
    assert_eq!(bus.count("SELECT count(*) FROM narrative WHERE agent_id='child' AND text='main thread only marker'"), 0);
    stop(&bus, &path);
    let snapshot = snapshot_when(&bus, &server, |s| {
        narrative(s, "root")
            .iter()
            .any(|r| r["text"] == "main thread only marker")
    });
    assert!(!serde_json::to_string(&narrative(&snapshot, "root"))
        .unwrap()
        .contains("child thread only marker"));
}

#[test]
fn test_7_bad_transcript_path_keeps_stop_hook_successful() {
    let bus = setup();
    for path in [bus.root.join("missing.jsonl"), bus.root.join("work")] {
        stop(&bus, &path);
        assert_eq!(bus.agent("root")["status"], "idle");
    }
    assert_eq!(bus.count("SELECT count(*) FROM narrative"), 0);
}

#[test]
fn test_8_restart_without_content_hides_prior_text_but_keeps_structure() {
    let bus = setup();
    let first = Server::start(&bus, true);
    // --content belongs to serve; replay uses its live capture lease (M5 contract).
    let mut turn = record(&bus, "prior assistant marker", None);
    turn["message"]["content"] = json!([
        {"type":"text","text":"prior assistant marker"},
        {"type":"thinking","thinking":"prior reasoning marker"},
        {"type":"thinking","thinking":"prior progress marker","display":"updates"},
        {"type":"tool_use","id":"read-1","name":"Read","input":{"file_path":"prior private path"}}
    ]);
    replay(&bus, &[turn]);
    let before = snapshot_when(&bus, &first, |s| {
        s.to_string().contains("prior assistant marker")
    });
    let kinds: Vec<Value> = narrative(&before, "root")
        .iter()
        .map(|r| r["kind"].clone())
        .collect();
    assert_eq!(
        kinds,
        vec![
            json!("assistant"),
            json!("reasoning"),
            json!("progress"),
            json!("tool_run")
        ]
    );
    drop(first);
    let second = Server::start(&bus, false);
    let after = second.snapshot(&bus);
    for marker in [
        "prior assistant marker",
        "prior reasoning marker",
        "prior progress marker",
        "prior private path",
    ] {
        assert!(
            !after.to_string().contains(marker),
            "capture-off leaked {marker}: {after}"
        );
    }
    let after_rows = narrative(&after, "root");
    assert_eq!(
        after_rows
            .iter()
            .map(|r| r["kind"].clone())
            .collect::<Vec<_>>(),
        kinds
    );
    assert_eq!(
        bus.count("SELECT count(*) FROM narrative WHERE agent_id='root'"),
        4
    );
}

fn age_retention_rows(bus: &Bus) {
    replay(
        bus,
        &[
            record(bus, "expired marker", None),
            record(bus, "retained marker", None),
        ],
    );
    let cutoff = now_ms() - 7 * 24 * 60 * 60 * 1_000;
    let db = bus.db();
    assert_eq!(
        db.execute(
            "UPDATE narrative SET ts=?1 WHERE text=?2",
            params![cutoff - 60_000, "expired marker"]
        )
        .unwrap(),
        1
    );
    // A one-minute margin is just inside the seven-day window without a clock race.
    assert_eq!(
        db.execute(
            "UPDATE narrative SET ts=?1 WHERE text=?2",
            params![cutoff + 60_000, "retained marker"]
        )
        .unwrap(),
        1
    );
}

#[test]
fn test_8_snapshot_omits_rows_older_than_seven_days_and_keeps_inside_row() {
    let bus = setup();
    let first = Server::start(&bus, true);
    age_retention_rows(&bus);
    drop(first);
    let second = Server::start(&bus, true);
    let snapshot = snapshot_when(&bus, &second, |s| s.to_string().contains("retained marker"));
    assert!(
        !snapshot.to_string().contains("expired marker"),
        "{snapshot}"
    );
    assert!(narrative(&snapshot, "root")
        .iter()
        .any(|r| r["text"] == "retained marker"));
}

#[test]
fn test_8_replay_expiry_deletes_old_rows_and_keeps_inside_row() {
    let bus = setup();
    let server = Server::start(&bus, true);
    age_retention_rows(&bus);
    drop(server);
    // Empty replay invokes expiry without creating new narrative or requiring serve.
    replay(&bus, &[]);
    assert_eq!(
        bus.count("SELECT count(*) FROM narrative WHERE text='expired marker'"),
        0
    );
    assert_eq!(
        bus.count("SELECT count(*) FROM narrative WHERE text='retained marker'"),
        1
    );
}

#[test]
fn test_12_replay_redacts_secrets_in_snapshot_and_every_database_text_column() {
    let bus = setup();
    let server = Server::start(&bus, true);
    const SECRET: &str = "sk-auditfixture0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let sensitive = format!("KEY={SECRET}");
    let mut turn = record(&bus, "redaction-turn", None);
    turn["message"]["content"] = json!([
        {"type":"text","text":format!("assistant {sensitive}")},
        {"type":"thinking","thinking":format!("reasoning {sensitive}")},
        {"type":"thinking","thinking":format!("progress {sensitive}"),"display":"updates"},
        {"type":"tool_use","id":"secret-tool","name":"Bash","input":{"command":format!("echo {sensitive}")}}
    ]);
    replay(&bus, &[turn]);
    let snapshot = snapshot_when(&bus, &server, |s| s.to_string().contains("[redacted]"));
    assert!(
        !snapshot.to_string().contains(SECRET),
        "snapshot leaked the replay credential"
    );
    let rows = narrative(&snapshot, "root");
    for kind in ["assistant", "reasoning", "progress", "tool_run"] {
        let row = rows.iter().find(|r| r["kind"] == kind).unwrap();
        assert!(
            row.to_string().contains("[redacted]"),
            "missing redaction in {kind}: {row}"
        );
    }
    let db = bus.db();
    for kind in ["assistant", "reasoning", "progress", "tool"] {
        let text: String = db.query_row(
            "SELECT coalesce(text,tool_detail) FROM narrative WHERE kind=?1 AND agent_id='root'",
            [kind], |r| r.get(0),
        ).unwrap();
        assert!(text.contains("[redacted]"), "unredacted {kind}: {text}");
    }
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for table in tables {
        let mut query = db
            .prepare(&format!("SELECT * FROM \"{}\"", table.replace('"', "\"\"")))
            .unwrap();
        let columns = query.column_count();
        let mut rows = query.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for column in 0..columns {
                if let rusqlite::types::ValueRef::Text(bytes) = row.get_ref(column).unwrap() {
                    assert!(
                        !String::from_utf8_lossy(bytes).contains(SECRET),
                        "secret stored in {table}, column {column}"
                    );
                }
            }
        }
    }
    assert!(
        fs::read_to_string(bus.root.join("narrative-replay.jsonl"))
            .unwrap()
            .contains(SECRET),
        "the binary must redact a genuinely sensitive input"
    );
}
