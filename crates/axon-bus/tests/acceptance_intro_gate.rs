//! Independent contract: BUS-PLAN §3, introduction gate (2026-10-03).
//! Real CLI/hooks and captured payloads; only the greeting is conditional.

mod common;
use common::fed::{db, git};
use common::*;
use serde_json::{json, Value};
use std::process::Output;
use std::time::Duration;

fn hub(in_repo: bool) -> Bus {
    let bus = Bus::with_limit(Duration::from_secs(15));
    bus.init();
    if in_repo {
        git(&bus, &bus.root.join("work"), &["init"]);
    }
    bus
}

fn payload(bus: &Bus, harness: &str, event: &str, id: &str) -> Value {
    let mut value = bus.fixture(harness, event);
    let cwd = bus.root.join("work");
    if harness == "opencode" {
        if event == "session.created" {
            value["properties"]["sessionID"] = json!(id);
            value["properties"]["info"]["id"] = json!(id);
            value["properties"]["info"]["directory"] = json!(cwd);
            value["properties"]["info"]["path"] = json!(cwd);
        } else {
            value["input"]["sessionID"] = json!(id);
        }
    } else {
        value["session_id"] = json!(id);
        value["cwd"] = json!(cwd);
    }
    if event == "PostToolUse" {
        // The captured Claude completion spawns an Agent; this one is an ordinary tool.
        value["tool_name"] = json!("Bash");
        value["tool_input"] = json!({"command":"echo fixture"});
        value["tool_response"] = json!({"stdout":"fixture"});
    }
    value
}

fn hook(bus: &Bus, harness: &str, event: &str, id: &str) -> Output {
    bus.hook(harness, event, &payload(bus, harness, event, id))
}

fn introduced_at(bus: &Bus, id: &str) -> Option<i64> {
    db(bus)
        .query_row("SELECT introduced_at FROM agents WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
}

fn context(output: &Output, harness: &str, event: &str) -> String {
    let reply = parse_json(&output.stdout);
    let text = if matches!(harness, "claude" | "codex") {
        assert_eq!(reply["hookSpecificOutput"]["hookEventName"], event);
        &reply["hookSpecificOutput"]["additionalContext"]
    } else {
        &reply["context"]
    };
    text.as_str().expect("hook context").to_owned()
}

fn greeting(output: &Output, harness: &str, event: &str, id: &str) -> String {
    let text = context(output, harness, event);
    for required in [id, " send ", " reply ", " peers"] {
        assert!(text.contains(required), "missing {required:?}: {text}");
    }
    text
}

fn silent(bus: &Bus, harness: &str, event: &str, id: &str) {
    assert_allowed(&hook(bus, harness, event, id));
}

#[test]
fn lone_claude_registers_but_stays_unintroduced_across_hooks() {
    let bus = hub(true);
    for event in [
        "SessionStart",
        "PostToolUse",
        "PostToolUse",
        "UserPromptSubmit",
    ] {
        silent(&bus, "claude", event, "alone");
        assert_eq!(bus.agent("alone")["harness"], "claude");
        assert_eq!(introduced_at(&bus, "alone"), None, "{event}");
        assert_eq!(bus.count("SELECT count(*) FROM agents"), 1);
    }
    assert_eq!(bus.agent("alone")["status"], "active");
}

fn late_claude_peer(first_event: &str) {
    // Bus isolation gives the specified default auto_link=true, with no stored links.
    let bus = hub(true);
    silent(&bus, "claude", "SessionStart", "first");
    silent(&bus, "claude", "PostToolUse", "first");
    assert_eq!(introduced_at(&bus, "first"), None);
    let second = hook(&bus, "claude", "SessionStart", "second");
    let text = greeting(&second, "claude", "SessionStart", "second");
    assert!(text.contains("Sessions in this repository (you can message them directly)"));
    assert!(text.contains("first"));
    assert_eq!(introduced_at(&bus, "first"), None);
    assert!(introduced_at(&bus, "second").is_some());
    let peers = bus.json(&["peers", "--agent", "first"]);
    assert_eq!(peers["same_repo"][0]["id"], "second");
    assert_eq!(bus.count("SELECT count(*) FROM edges"), 0);
    bus.send("second", "first", "sync", "late-peer-message");
    let first = hook(&bus, "claude", first_event, "first");
    let text = greeting(&first, "claude", first_event, "first");
    assert!(text.contains("second"));
    // §3 delivers Claude messages at the next prompt/tool call; SessionStart greets.
    let delivery = if first_event == "SessionStart" {
        let output = hook(&bus, "claude", "PostToolUse", "first");
        let text = context(&output, "claude", "PostToolUse");
        assert!(!text.contains(" peers"), "must not greet again: {text}");
        text
    } else {
        text
    };
    assert!(delivery.contains("late-peer-message") && delivery.contains("untrusted peer"));
    for id in ["first", "second"] {
        let introduced = introduced_at(&bus, id);
        assert!(introduced.is_some());
        for event in ["PostToolUse", "UserPromptSubmit", "PostToolUse"] {
            silent(&bus, "claude", event, id);
            assert_eq!(introduced_at(&bus, id), introduced);
        }
    }
    bus.send("first", "second", "sync", "after-greeting-message");
    let delivered = hook(&bus, "claude", "PostToolUse", "second");
    let text = context(&delivered, "claude", "PostToolUse");
    assert!(text.contains("after-greeting-message"));
    assert!(!text.contains(" peers"), "must not greet again: {text}");
    silent(&bus, "claude", "PostToolUse", "second");
}

#[test]
fn late_same_repo_peer_greets_claude_on_post_tool_use_once() {
    late_claude_peer("PostToolUse");
}

#[test]
fn late_same_repo_peer_greets_claude_on_user_prompt_submit_once() {
    late_claude_peer("UserPromptSubmit");
}

#[test]
fn late_same_repo_peer_greets_claude_on_session_start() {
    late_claude_peer("SessionStart");
}

#[test]
fn subagent_is_greeted_for_its_parent_and_parent_on_its_next_hook() {
    // No Git repository: the parent/child relationship alone must open the gate.
    let bus = hub(false);
    silent(&bus, "claude", "SessionStart", "parent");
    let mut spawn = payload(&bus, "claude", "SubagentStart", "parent");
    spawn["agent_id"] = json!("child");
    let output = bus.hook("claude", "SubagentStart", &spawn);
    let text = greeting(&output, "claude", "SubagentStart", "child");
    assert!(text.contains("parent"));
    assert_eq!(bus.agent("child")["parent_id"], "parent");
    assert!(introduced_at(&bus, "child").is_some());
    assert_eq!(introduced_at(&bus, "parent"), None);
    bus.send("child", "parent", "sync", "child-is-ready");
    let output = hook(&bus, "claude", "PostToolUse", "parent");
    let text = greeting(&output, "claude", "PostToolUse", "parent");
    assert!(text.contains("child-is-ready") && text.contains("child"));
    assert!(introduced_at(&bus, "parent").is_some());
    silent(&bus, "claude", "PostToolUse", "parent");
    silent(&bus, "claude", "UserPromptSubmit", "parent");
    let mut child = payload(&bus, "claude", "PostToolUse", "parent");
    child["agent_id"] = json!("child");
    assert_allowed(&bus.hook("claude", "PostToolUse", &child));
}

fn deferred_harness(harness: &str, start: &str, event: &str) {
    let bus = hub(true);
    silent(&bus, harness, start, "waiting");
    for _ in 0..2 {
        silent(&bus, harness, event, "waiting");
        assert_eq!(bus.agent("waiting")["harness"], harness);
        assert_eq!(introduced_at(&bus, "waiting"), None);
        assert_eq!(bus.count("SELECT count(*) FROM agents"), 1);
    }
    greeting(
        &hook(&bus, "claude", "SessionStart", "peer"),
        "claude",
        "SessionStart",
        "peer",
    );
    bus.send("peer", "waiting", "sync", "harness-peer-arrived");
    let output = hook(&bus, harness, event, "waiting");
    let text = greeting(&output, harness, event, "waiting");
    assert!(text.contains("harness-peer-arrived") && text.contains("untrusted peer"));
    let introduced = introduced_at(&bus, "waiting");
    assert!(introduced.is_some());
    for next in if harness == "codex" {
        vec!["PostToolUse", "UserPromptSubmit", "PostToolUse"]
    } else {
        vec![event, event]
    } {
        silent(&bus, harness, next, "waiting");
        assert_eq!(introduced_at(&bus, "waiting"), introduced);
    }
    bus.send("peer", "waiting", "sync", "later-harness-message");
    let text = context(&hook(&bus, harness, event, "waiting"), harness, event);
    assert!(text.contains("later-harness-message"));
    assert!(!text.contains(" peers"), "must not greet again: {text}");
    silent(&bus, harness, event, "waiting");
}

#[test]
fn lone_codex_then_peer_greets_on_post_tool_use_once() {
    deferred_harness("codex", "SessionStart", "PostToolUse");
}

#[test]
fn lone_codex_then_peer_greets_on_user_prompt_submit_once() {
    deferred_harness("codex", "SessionStart", "UserPromptSubmit");
}

#[test]
fn lone_hermes_then_peer_greets_on_pre_llm_call_once() {
    deferred_harness("hermes", "on_session_start", "pre_llm_call");
}

#[test]
fn lone_opencode_then_peer_greets_on_tool_execute_after_once() {
    deferred_harness("opencode", "session.created", "tool.execute.after");
}
