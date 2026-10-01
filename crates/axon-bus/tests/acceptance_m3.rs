//! Contract decisions
//! - `budget set ID 50Ktok [--usd N]` sets a tree ceiling; K/M mean 1,000/1,000,000.
//!   Token totals include input, output, cache reads and cache writes, once each.
//! - `budget show ID --json` returns {scope_id,tokens,state,cost_usd,unpriced}; cost_usd
//!   is null for any unpriced usage (never a zero pretending to be a complete price).
//! - Warning enqueues one sync to the root; a budget stop enqueues stop to every member.
//! - §6's "90% of its last known total" has no defined denominator. Here it means the
//!   last known token total >=90% of the configured ceiling: first stale observation
//!   warns, subsequent stale observations deny at that threshold. Stale means newest
//!   usage receipt >120 s old; below the threshold it warns once and continues.
//! - Stale warnings contain `stale` in the sync body. Fresh or wholly idle trees do not
//!   use the stale gate. Warnings alone allow tool calls without stdout decisions.
//! - Stop doorbells are $XDG_DATA_HOME/axon/doorbells/<agent-id>.json; send writes them
//!   before returning. Their format is private: tests generate them through real sends.
//! - cost_usd=NULL usage rows must be priced from axon-core's bundled USD rates, without
//!   applying its EUR display FX. Unknown rows fall back to the explicitly set token cap.
//! - agents.model preserves the raw resolvedModel from a spawn result; price lookup may
//!   canonicalize it, but must still report unknown models as unpriced.

mod common;
use common::*;
use serde_json::{json, Value};
use std::fs;

fn budgeted(bus: &Bus) {
    bus.init();
    bus.register("root", "claude", None);
    bus.register("child", "codex", Some("root"));
    bus.ok(&["budget", "set", "root", "50Ktok"]);
    assert_allowed(&bus.gate("claude", "root"));
}
fn status(bus: &Bus) -> Value {
    bus.json(&["budget", "show", "root", "--json"])
}
fn stale_warnings(bus: &Bus) -> i64 {
    bus.count("SELECT count(*) FROM messages WHERE kind='sync' AND to_id='root' AND lower(body) LIKE '%stale%'")
}
fn make_db_unreadable(bus: &Bus) {
    bus.db()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    fs::rename(bus.db_path(), bus.root.join("saved.db")).unwrap();
    // A directory reliably fails SQLite open even under root; chmod(000) would not.
    fs::create_dir(bus.db_path()).unwrap();
}

// BUS-PLAN §6 and §9 M3: crossing 50K stops each member's next call, but not other trees.
#[test]
fn crossing_50k_tokens_stops_the_entire_tree_on_the_next_tool_call() {
    let bus = Bus::new();
    budgeted(&bus);
    bus.register("grandchild", "hermes", Some("child"));
    bus.register("unrelated", "opencode", None);
    bus.usage("root", "claude-opus-4.8", [30_000, 0, 0, 0], now_ms(), 1);
    bus.usage("child", "gpt-5.5", [10_000, 0, 0, 0], now_ms(), 2);
    bus.usage(
        "grandchild",
        "claude-sonnet-4.6",
        [9_999, 0, 0, 0],
        now_ms(),
        3,
    );
    assert_allowed(&bus.gate("codex", "child"));
    bus.usage("grandchild", "claude-sonnet-4.6", [2, 0, 0, 0], now_ms(), 4);
    for (harness, id) in [
        ("hermes", "grandchild"),
        ("claude", "root"),
        ("codex", "child"),
    ] {
        let reason = deny_reason(harness, &bus.gate(harness, id));
        assert!(reason.to_lowercase().contains("budget"), "{reason}");
        let count: i64 = bus
            .db()
            .query_row(
                "SELECT count(*) FROM messages WHERE kind='stop' AND to_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(count >= 1, "budget stop must be delivered to {id}");
    }
    let total = status(&bus);
    assert_eq!(total["tokens"], 50_001);
    assert_eq!(total["state"], "stopped");
    assert_allowed(&bus.gate("opencode", "unrelated"));
}

// BUS-PLAN §6: the exact 80% boundary warns once and remains below the stop ceiling.
#[test]
fn eighty_percent_warns_the_root_once_without_denying() {
    let bus = Bus::new();
    budgeted(&bus);
    bus.usage("child", "gpt-5.5", [39_999, 0, 0, 0], now_ms(), 1);
    assert_allowed(&bus.gate("codex", "child"));
    assert_eq!(status(&bus)["state"], "ok");
    assert_eq!(
        bus.count("SELECT count(*) FROM messages WHERE kind='sync' AND to_id='root'"),
        0
    );
    bus.usage("root", "claude-opus-4.8", [1, 0, 0, 0], now_ms(), 2);
    assert_allowed(&bus.gate("codex", "child"));
    assert_eq!(status(&bus)["state"], "warned");
    assert_allowed(&bus.gate("codex", "child"));
    assert_eq!(
        bus.count("SELECT count(*) FROM messages WHERE kind='sync' AND to_id='root'"),
        1
    );
    assert_eq!(
        bus.count("SELECT count(*) FROM messages WHERE kind='stop'"),
        0
    );
}

// BUS-PLAN §6: stale usage on an active budgeted tree warns once below the 90% stop gate.
#[test]
fn stale_usage_below_ninety_percent_warns_once_and_allows() {
    let bus = Bus::new();
    budgeted(&bus);
    bus.usage("child", "gpt-5.5", [44_999, 0, 0, 0], now_ms() - 121_000, 1);
    assert_allowed(&bus.gate("codex", "child"));
    assert_eq!(stale_warnings(&bus), 1);
    assert_allowed(&bus.gate("codex", "child"));
    assert_eq!(stale_warnings(&bus), 1);
    assert_eq!(
        bus.count("SELECT count(*) FROM messages WHERE kind='stop'"),
        0
    );
}

// BUS-PLAN §6: after its one stale warning, 90% of the ceiling denies without fresh data.
#[test]
fn stale_usage_at_ninety_percent_warns_then_denies_the_next_call() {
    let bus = Bus::new();
    budgeted(&bus);
    bus.usage("child", "gpt-5.5", [45_000, 0, 0, 0], now_ms() - 121_000, 1);
    assert_allowed(&bus.gate("codex", "child"));
    assert_eq!(stale_warnings(&bus), 1);
    let reason = deny_reason("codex", &bus.gate("codex", "child"));
    assert!(reason.to_lowercase().contains("stale"), "{reason}");
    assert_eq!(stale_warnings(&bus), 1);
}

// BUS-PLAN §6: fresh usage must not accidentally take the stale 90% denial branch.
#[test]
fn fresh_usage_at_ninety_percent_keeps_allowing() {
    let bus = Bus::new();
    budgeted(&bus);
    bus.usage("child", "gpt-5.5", [45_000, 0, 0, 0], now_ms(), 1);
    assert_allowed(&bus.gate("codex", "child"));
    assert_allowed(&bus.gate("codex", "child"));
    assert_eq!(stale_warnings(&bus), 0);
}

// BUS-PLAN §6: the stale gate applies only to trees with a budget.
#[test]
fn stale_usage_without_a_budget_does_not_warn_or_deny() {
    let bus = Bus::new();
    bus.init();
    bus.register("root", "claude", None);
    bus.usage(
        "root",
        "claude-opus-4.8",
        [900_000, 0, 0, 0],
        now_ms() - 121_000,
        1,
    );
    assert_allowed(&bus.gate("claude", "root"));
    assert_allowed(&bus.gate("claude", "root"));
    assert_eq!(stale_warnings(&bus), 0);
}

// BUS-PLAN §6: unreadable database plus a persisted stop doorbell must fail closed.
#[test]
fn unreadable_database_with_a_stop_doorbell_denies() {
    let bus = Bus::new();
    bus.init();
    bus.register("root", "claude", None);
    bus.register("child", "claude", Some("root"));
    bus.send("root", "child", "stop", "persist this stop");
    assert!(bus.root.join("data/axon/doorbells/child.json").is_file());
    make_db_unreadable(&bus);
    assert!(!deny_reason("claude", &bus.gate("claude", "child")).is_empty());
}

// BUS-PLAN §6: unreadable database without a stop doorbell must not brick a session.
#[test]
fn unreadable_database_without_a_doorbell_allows() {
    let bus = Bus::new();
    bus.init();
    bus.register("root", "claude", None);
    assert!(!bus.root.join("data/axon/doorbells/root.json").exists());
    make_db_unreadable(&bus);
    assert_allowed(&bus.gate("claude", "root"));
}

// BUS-PLAN §00 and §6: removing a hub mid-session does not erase an already pending stop.
#[test]
fn removed_database_with_a_stop_doorbell_still_denies() {
    let bus = Bus::new();
    bus.init();
    bus.register("root", "claude", None);
    bus.register("child", "hermes", Some("root"));
    bus.send("root", "child", "stop", "stop survives hub removal");
    bus.db()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    fs::rename(bus.db_path(), bus.root.join("saved.db")).unwrap();
    assert!(!bus.db_path().exists());
    deny_reason("hermes", &bus.gate("hermes", "child"));
    assert!(
        !bus.db_path().exists(),
        "a hook must not recreate the removed hub"
    );
}

// BUS-PLAN §6: tree cost sums disjoint descendants' token buckets using axon-core's prices.
#[test]
fn tree_cost_sums_priced_usage_once_and_excludes_other_roots() {
    let bus = Bus::new();
    budgeted(&bus);
    bus.register("grandchild", "claude", Some("child"));
    bus.register("other", "claude", None);
    bus.usage(
        "root",
        "claude-opus-4.8",
        [1_000, 200, 300, 400],
        now_ms(),
        1,
    );
    bus.usage("child", "gpt-5.5", [2_000, 100, 500, 0], now_ms(), 2);
    bus.usage(
        "grandchild",
        "claude-sonnet-4.6",
        [300, 200, 100, 50],
        now_ms(),
        3,
    );
    bus.usage(
        "other",
        "claude-opus-4.8",
        [1_000_000, 0, 0, 0],
        now_ms(),
        4,
    );
    // USD/MTok from crates/axon-core/assets/pricing.toml; independent of pricing code.
    let expected = (1_000.0 * 5.0
        + 200.0 * 25.0
        + 300.0 * 0.5
        + 400.0 * 6.25
        + 2_000.0 * 5.0
        + 100.0 * 30.0
        + 500.0 * 0.5
        + 300.0 * 3.0
        + 200.0 * 15.0
        + 100.0 * 0.3
        + 50.0 * 3.75)
        / 1_000_000.0;
    for _ in 0..2 {
        let total = status(&bus);
        assert_eq!(total["scope_id"], "root");
        assert_eq!(total["tokens"], 5_150);
        assert_eq!(total["unpriced"], false);
        let actual = total["cost_usd"].as_f64().unwrap();
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }
}

// BUS-PLAN §9b and §9 M0: unknown models are unpriced and use the token ceiling.
#[test]
fn captured_unknown_model_is_unpriced_and_falls_back_to_token_budget() {
    let bus = Bus::new();
    bus.init();
    bus.replay_hook("claude", "UserPromptSubmit");
    bus.replay_hook("claude", "SubagentStart");
    let post = bus.fixture("claude", "PostToolUse");
    bus.hook("claude", "PostToolUse", &post);
    let root = post["session_id"].as_str().unwrap();
    let child = post["tool_response"]["agentId"].as_str().unwrap();
    assert_eq!(
        bus.agent(child)["model"],
        post["tool_response"]["resolvedModel"]
    );
    // Use a synthetic id so new rates for real models cannot invalidate the unpriced premise.
    let model = "claude-unreleased-test-model";
    bus.ok(&["budget", "set", root, "50Ktok", "--usd", "3"]);
    bus.usage(child, model, [49_999, 0, 0, 0], now_ms(), 1);
    let total = bus.json(&["budget", "show", root, "--json"]);
    assert_eq!(total["unpriced"], true);
    assert!(
        total.get("cost_usd").unwrap().is_null(),
        "unknown is not free: {total}"
    );
    assert_allowed(&bus.replay_hook("claude", "PreToolUse.child"));
    bus.usage(child, model, [2, 0, 0, 0], now_ms(), 2);
    deny_reason("claude", &bus.replay_hook("claude", "PreToolUse.child"));
    assert_eq!(
        bus.json(&["budget", "show", root, "--json"])["state"],
        "stopped"
    );
}

// BUS-PLAN §9 M3: transcript usage joins the registered session/child without charging the root twice.
#[test]
fn child_transcript_usage_is_attributed_once_to_the_registered_child() {
    let bus = Bus::new();
    bus.init();
    bus.replay_hook("claude", "UserPromptSubmit");
    bus.replay_hook("claude", "SubagentStart");
    let mut payload = bus.fixture("claude", "PostToolUse.child");
    let root = payload["session_id"].as_str().unwrap().to_owned();
    let child = payload["agent_id"].as_str().unwrap().to_owned();
    let transcript = bus.root.join("child.jsonl");
    let records: Vec<Value> = include_str!("../../../tests/fixtures/claude_subagent.jsonl")
        .lines()
        .map(|line| {
            let mut record: Value = serde_json::from_str(line).unwrap();
            record["sessionId"] = json!(root);
            record["agentId"] = json!(child);
            record["cwd"] = json!(bus.root.join("work"));
            record
        })
        .collect();
    write_jsonl(&transcript, &records);
    payload["transcript_path"] = json!(transcript);
    bus.hook("claude", "PostToolUse", &payload);
    bus.hook("claude", "PostToolUse", &payload);
    let (input, output, count): (i64, i64, i64) = bus
        .db()
        .query_row(
            "SELECT sum(input_tokens),sum(output_tokens),count(*) FROM usage WHERE agent_id=?1",
            [&child],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((input, output, count), (5_000, 850, 1));
    assert_eq!(
        bus.db()
            .query_row(
                "SELECT count(*) FROM usage WHERE agent_id=?1",
                [&root],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
