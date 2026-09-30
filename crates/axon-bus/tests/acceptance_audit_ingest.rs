//! Independent TEST-4 and TEST-6 regressions through captured Claude transcripts.

mod common;
use common::*;
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

fn bus_with_root() -> Bus {
    let bus = Bus::new();
    bus.init();
    bus.register("root", "claude", None);
    bus
}

fn turn(bus: &Bus, id: &str, child: Option<&str>) -> Value {
    let mut record: Value = serde_json::from_str(
        include_str!("../../../tests/fixtures/claude_subagent.jsonl")
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    record["sessionId"] = json!("root");
    record["uuid"] = json!(id);
    record["message"]["id"] = json!(id);
    record["cwd"] = json!(bus.root.join("work"));
    record["timestamp"] = json!(chrono::Utc::now().to_rfc3339());
    if let Some(child) = child {
        record["agentId"] = json!(child);
    } else {
        let object = record.as_object_mut().unwrap();
        for key in [
            "agentId",
            "isSidechain",
            "attributionAgent",
            "sourceToolAssistantUUID",
        ] {
            object.remove(key);
        }
    }
    record
}

fn ingest(bus: &Bus, path: &Path, child: Option<&str>) {
    let mut hook = bus.fixture("claude", "PostToolUse.child");
    hook["session_id"] = json!("root");
    hook["transcript_path"] = json!(path);
    if let Some(child) = child {
        hook["agent_id"] = json!(child);
    } else {
        hook.as_object_mut().unwrap().remove("agent_id");
    }
    bus.hook("claude", "PostToolUse", &hook);
}

fn assert_turns(bus: &Bus, agent: &str, count: i64) {
    let actual: (i64, i64, i64) = bus
        .db()
        .query_row(
            "SELECT count(*),coalesce(sum(input_tokens),0),coalesce(sum(output_tokens),0)
         FROM usage WHERE agent_id=?1",
            [agent],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    // The captured turn has 5,000 input and 850 output tokens.
    assert_eq!(
        actual,
        (count, count * 5_000, count * 850),
        "usage for {agent}"
    );
}

fn append(path: &Path, bytes: &[u8]) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

#[test]
fn test_4_usd_only_budget_denies_unpriced_captured_model() {
    let bus = bus_with_root();
    bus.ok(&["budget", "set", "root", "--usd", "3"]);
    let captured = bus.fixture("claude", "PostToolUse");
    let mut record = turn(&bus, "unpriced-turn", None);
    record["message"]["model"] = captured["tool_response"]["resolvedModel"].clone();
    let path = bus.root.join("unpriced.jsonl");
    write_jsonl(&path, &[record]);
    ingest(&bus, &path, None);
    assert_turns(&bus, "root", 1);
    let total = bus.json(&["budget", "show", "root", "--json"]);
    assert_eq!(total["unpriced"], true);
    assert!(total["cost_usd"].is_null());
    let reason = deny_reason("claude", &bus.gate("claude", "root"));
    assert!(reason.contains("USD-only"), "{reason}");
}

#[test]
fn test_4_usd_only_priced_under_ceiling_allows() {
    let bus = bus_with_root();
    bus.ok(&["budget", "set", "root", "--usd", "3"]);
    let path = bus.root.join("priced.jsonl");
    write_jsonl(&path, &[turn(&bus, "priced-turn", None)]);
    ingest(&bus, &path, None);
    assert_turns(&bus, "root", 1);
    let total = bus.json(&["budget", "show", "root", "--json"]);
    assert_eq!(total["unpriced"], false);
    let cost = total["cost_usd"].as_f64().unwrap();
    assert!(
        cost > 0.0 && cost < 3.0,
        "control must have real spend below ceiling: {total}"
    );
    assert_allowed(&bus.gate("claude", "root"));
}

#[test]
fn test_4_usd_only_zero_usage_allows() {
    let bus = bus_with_root();
    bus.ok(&["budget", "set", "root", "--usd", "3"]);
    assert_turns(&bus, "root", 0);
    assert_allowed(&bus.gate("claude", "root"));
}

#[test]
fn test_6_reingest_and_append_count_each_turn_once() {
    let bus = bus_with_root();
    let path = bus.root.join("incremental.jsonl");
    let first = turn(&bus, "first-turn", None);
    let mut duplicate = first.clone();
    duplicate["uuid"] = json!("same-message-later-record");
    write_jsonl(&path, &[first, duplicate]);
    ingest(&bus, &path, None);
    assert_turns(&bus, "root", 1);
    ingest(&bus, &path, None);
    assert_turns(&bus, "root", 1);
    append(
        &path,
        format!("{}\n", turn(&bus, "second-turn", None)).as_bytes(),
    );
    ingest(&bus, &path, None);
    assert_turns(&bus, "root", 2);
    ingest(&bus, &path, None);
    assert_turns(&bus, "root", 2);
}

#[test]
fn test_6_two_agents_sharing_a_path_keep_independent_cursors() {
    let bus = bus_with_root();
    for child in ["child-a", "child-b"] {
        bus.register(child, "claude", Some("root"));
    }
    let path = bus.root.join("shared.jsonl");
    write_jsonl(
        &path,
        &[
            turn(&bus, "turn-a", Some("child-a")),
            turn(&bus, "turn-b", Some("child-b")),
        ],
    );
    ingest(&bus, &path, Some("child-a"));
    assert_turns(&bus, "child-a", 1);
    assert_turns(&bus, "child-b", 0);
    ingest(&bus, &path, Some("child-b"));
    assert_turns(&bus, "child-b", 1);
    for child in ["child-a", "child-b"] {
        ingest(&bus, &path, Some(child));
        assert_turns(&bus, child, 1);
    }
    assert_turns(&bus, "root", 0);
}

#[test]
fn test_6_one_agent_keeps_independent_cursors_for_two_paths() {
    let bus = bus_with_root();
    let one = bus.root.join("one.jsonl");
    let two = bus.root.join("two.jsonl");
    write_jsonl(&one, &[turn(&bus, "file-one-turn", None)]);
    write_jsonl(&two, &[turn(&bus, "file-two-turn", None)]);
    ingest(&bus, &one, None);
    ingest(&bus, &two, None);
    assert_turns(&bus, "root", 2);
    append(
        &one,
        format!("{}\n", turn(&bus, "appended-to-one", None)).as_bytes(),
    );
    ingest(&bus, &one, None);
    ingest(&bus, &two, None);
    assert_turns(&bus, "root", 3);
}

#[test]
fn test_6_shrunk_file_is_reread_from_zero() {
    let bus = bus_with_root();
    let path = bus.root.join("rewritten.jsonl");
    write_jsonl(
        &path,
        &[turn(&bus, "old-one", None), turn(&bus, "old-two", None)],
    );
    ingest(&bus, &path, None);
    assert_turns(&bus, "root", 2);
    let previous_size = fs::metadata(&path).unwrap().len();
    write_jsonl(&path, &[turn(&bus, "new-turn", None)]);
    assert!(fs::metadata(&path).unwrap().len() < previous_size);
    ingest(&bus, &path, None);
    assert_turns(&bus, "root", 3);
    ingest(&bus, &path, None);
    assert_turns(&bus, "root", 3);
}

#[test]
fn test_6_partial_trailing_line_is_ingested_once_after_completion() {
    let bus = bus_with_root();
    let path = bus.root.join("partial.jsonl");
    write_jsonl(&path, &[turn(&bus, "complete-turn", None)]);
    let trailing = turn(&bus, "partial-turn", None).to_string();
    let split = trailing.len() / 2;
    append(&path, &trailing.as_bytes()[..split]);
    ingest(&bus, &path, None);
    assert_turns(&bus, "root", 1);
    ingest(&bus, &path, None);
    assert_turns(&bus, "root", 1);
    append(&path, &trailing.as_bytes()[split..]);
    // A valid JSON value without its newline is still an unfinished JSONL record.
    ingest(&bus, &path, None);
    assert_turns(&bus, "root", 1);
    append(&path, b"\n");
    ingest(&bus, &path, None);
    assert_turns(&bus, "root", 2);
    ingest(&bus, &path, None);
    assert_turns(&bus, "root", 2);
}
