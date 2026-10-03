//! Frozen acceptance contract: 2026-10-03 same-repository auto-link spec change,
//! BUS-PLAN §3 and P2P-SPEC §2. Expectations are independent of src/.
//! All commands, hooks, Git repositories and owner sessions use common::Bus sandboxes.

mod common;
use common::fed::{api, count, db, git, repo};
use common::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

const DIRECT_INTRO: &str = "Sessions in this repository (you can message them directly)";

fn hub() -> Bus {
    let bus = Bus::with_limit(Duration::from_secs(45));
    bus.init();
    bus
}

fn hook_payload(bus: &Bus, harness: &str, event: &str, id: &str, cwd: &Path) -> Value {
    let mut payload = bus.fixture(harness, event);
    payload["session_id"] = json!(id);
    payload["cwd"] = json!(cwd);
    payload
}

fn start(bus: &Bus, harness: &str, id: &str, cwd: &Path) -> Output {
    let event = match harness {
        "opencode" => "session.created",
        "hermes" => "on_session_start",
        _ => "SessionStart",
    };
    let mut payload = hook_payload(bus, harness, event, id, cwd);
    if harness == "opencode" {
        payload["properties"]["sessionID"] = json!(id);
        payload["properties"]["info"]["id"] = json!(id);
        payload["properties"]["info"]["directory"] = json!(cwd);
        payload["properties"]["info"]["path"] = json!(cwd);
    }
    let output = bus.hook(harness, event, &payload);
    let agent = bus.agent(id);
    assert_eq!(agent["harness"], harness);
    assert_eq!(agent["root_id"], id);
    assert_eq!(agent["parent_id"], Value::Null);
    assert_eq!(agent["status"], "idle");
    output
}

fn pair() -> (Bus, PathBuf) {
    let bus = hub();
    let checkout = repo(&bus, "project");
    start(&bus, "claude", "root-a", &checkout);
    start(&bus, "codex", "root-b", &checkout);
    (bus, checkout)
}

fn send(bus: &Bus, from: &str, to: &str, body: &str) {
    let sent = bus.send(from, to, "handoff", body);
    let stored: (String, String, String) = db(bus)
        .query_row(
            "SELECT from_id,to_id,body FROM messages WHERE id=?1",
            [sent["id"]
                .as_str()
                .expect("successful send returns its message id")],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(stored, (from.into(), to.into(), body.into()));
}

fn refused(bus: &Bus, from: &str, to: &str) -> String {
    let before = count(bus, "SELECT count(*) FROM messages");
    let output = bus
        .cmd()
        .args(["send", "--from", from, "--to", to])
        .args(["--kind", "handoff", "--body", "must refuse"])
        .assert()
        .code(3)
        .get_output()
        .clone();
    let reason = String::from_utf8(output.stderr).unwrap();
    assert!(!reason.is_empty(), "a refused send must explain why");
    assert_eq!(count(bus, "SELECT count(*) FROM messages"), before);
    reason
}

fn settings(bus: &Bus, server: &Server) -> Value {
    api(bus, server, "GET", "/api/settings", Value::Null)
}

fn set_auto_link(bus: &Bus, server: &Server, enabled: bool) -> Value {
    let written = api(
        bus,
        server,
        "PUT",
        "/api/settings/bus",
        json!({"auto_link":enabled}),
    );
    assert_eq!(written["bus"]["auto_link"], enabled);
    let read = settings(bus, server);
    for section in ["capture", "usage", "budgets", "hooks", "federation", "bus"] {
        assert!(
            written.get(section).is_some(),
            "missing full settings section: {section}"
        );
        assert_eq!(written[section], read[section], "{section}");
    }
    // Storage byte counts may change between requests.
    assert!(written["storage"]["db_bytes"].is_u64());
    assert!(written["storage"]["wal_bytes"].is_u64());
    assert!(written["storage"]["sessions"].is_u64());
    written
}

fn introduction(output: &Output) -> String {
    let reply = parse_json(&output.stdout);
    assert_eq!(reply["hookSpecificOutput"]["hookEventName"], "SessionStart");
    reply["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("SessionStart introduction")
        .to_owned()
}

#[test]
fn same_repo_roots_in_subdirectories_send_both_ways_while_idle_or_active() {
    let bus = hub();
    let checkout = repo(&bus, "project");
    let nested = checkout.join("nested/deeper");
    std::fs::create_dir_all(&nested).unwrap();
    start(&bus, "claude", "root-a", &checkout);
    start(&bus, "codex", "root-b", &nested);
    for active_a in [false, true] {
        for active_b in [false, true] {
            for (harness, id, cwd, active) in [
                ("claude", "root-a", &checkout, active_a),
                ("codex", "root-b", &nested, active_b),
            ] {
                let event = if active { "UserPromptSubmit" } else { "Stop" };
                bus.hook(harness, event, &hook_payload(&bus, harness, event, id, cwd));
                assert_eq!(
                    bus.agent(id)["status"],
                    if active { "active" } else { "idle" }
                );
            }
            send(&bus, "root-a", "root-b", "same repository");
            send(&bus, "root-b", "root-a", "reverse direction");
        }
    }
    assert_eq!(
        count(&bus, "SELECT count(*) FROM edges"),
        0,
        "auto-links are never stored"
    );
}

#[test]
fn worktree_and_main_checkout_roots_send_both_ways() {
    let bus = hub();
    let main = repo(&bus, "main");
    let worktree = bus.root.join("worktree");
    git(
        &bus,
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "acceptance-topic",
            worktree.to_str().unwrap(),
        ],
    );
    start(&bus, "claude", "root-a", &main);
    start(&bus, "codex", "root-b", &worktree);
    send(&bus, "root-a", "root-b", "worktree of main repository");
    send(&bus, "root-b", "root-a", "main checkout");
    assert_eq!(count(&bus, "SELECT count(*) FROM edges"), 0);
}

#[test]
fn opencode_and_hermes_hook_roots_also_qualify() {
    let bus = hub();
    let checkout = repo(&bus, "project");
    start(&bus, "opencode", "root-a", &checkout);
    start(&bus, "hermes", "root-b", &checkout);
    send(&bus, "root-a", "root-b", "supported harness");
    send(&bus, "root-b", "root-a", "supported harness");
}

#[test]
fn roots_in_different_repositories_are_refused() {
    let bus = hub();
    let one = repo(&bus, "one");
    let two = repo(&bus, "two");
    start(&bus, "claude", "root-a", &one);
    start(&bus, "codex", "root-b", &two);
    refused(&bus, "root-a", "root-b");
    refused(&bus, "root-b", "root-a");
}

#[test]
fn matching_non_repository_cwds_do_not_create_a_link() {
    let bus = hub();
    let outside = bus.root.join("work");
    start(&bus, "claude", "root-a", &outside);
    start(&bus, "codex", "root-b", &outside);
    refused(&bus, "root-a", "root-b");
    refused(&bus, "root-b", "root-a");
}

#[test]
fn closed_hook_root_is_not_auto_linked() {
    let (bus, checkout) = pair();
    bus.hook(
        "codex",
        "SessionEnd",
        &json!({"session_id":"root-b","cwd":checkout}),
    );
    assert_eq!(bus.agent("root-b")["status"], "closed");
    refused(&bus, "root-a", "root-b");
    refused(&bus, "root-b", "root-a");
}

#[test]
fn leaving_the_repository_removes_the_computed_link() {
    let (bus, checkout) = pair();
    let elsewhere = repo(&bus, "elsewhere");
    send(&bus, "root-a", "root-b", "before moving");
    start(&bus, "codex", "root-b", &elsewhere);
    refused(&bus, "root-a", "root-b");
    refused(&bus, "root-b", "root-a");
    start(&bus, "codex", "root-b", &checkout);
    send(&bus, "root-a", "root-b", "back in the repository");
    assert_eq!(count(&bus, "SELECT count(*) FROM edges"), 0);
}

#[test]
fn subagent_is_refused_with_the_route_through_auto_linked_roots() {
    let (bus, checkout) = pair();
    let mut payload = hook_payload(&bus, "claude", "SubagentStart", "root-a", &checkout);
    payload["agent_id"] = json!("child-a");
    bus.hook("claude", "SubagentStart", &payload);
    assert_eq!(bus.agent("child-a")["parent_id"], "root-a");
    assert_route(
        &refused(&bus, "child-a", "root-b"),
        &["child-a", "root-a", "root-b"],
    );
    assert_route(
        &refused(&bus, "root-b", "child-a"),
        &["root-b", "root-a", "child-a"],
    );
    send(&bus, "child-a", "root-a", "relay via parent");
    send(&bus, "root-a", "root-b", "root can forward");
}

#[test]
fn virtual_harness_root_is_not_auto_linked() {
    let (bus, checkout) = pair();
    non_hook_root(&bus, "virtual-root", "virtual", &checkout);
    assert_eq!(bus.agent("virtual-root")["status"], "idle");
    refused(&bus, "root-a", "virtual-root");
    refused(&bus, "virtual-root", "root-a");
}

#[test]
fn human_node_is_not_auto_linked() {
    let (bus, checkout) = pair();
    non_hook_root(&bus, "human", "human", &checkout);
    refused(&bus, "root-a", "human");
    refused(&bus, "human", "root-a");
}

fn non_hook_root(bus: &Bus, id: &str, harness: &str, cwd: &Path) {
    // The register CLI only accepts the four hook harnesses; seed this exclusion
    // fixture in its real schema without requiring a new registration API.
    bus.register_at(id, "claude", None, cwd);
    bus.db()
        .execute("UPDATE agents SET harness=?1 WHERE id=?2", [harness, id])
        .unwrap();
    assert_eq!(bus.agent(id)["harness"], harness);
    assert_eq!(bus.agent(id)["parent_id"], Value::Null);
}

#[test]
fn settings_default_to_auto_link_enabled() {
    let bus = hub();
    let server = Server::start(&bus, false);
    let value = settings(&bus, &server);
    assert_eq!(
        value["bus"]["auto_link"], true,
        "GET /api/settings: {value}"
    );
}

#[test]
fn owner_toggle_returns_full_settings_persists_and_controls_existing_roots() {
    let (bus, _) = pair();
    let server = Server::start(&bus, false);
    set_auto_link(&bus, &server, false);
    assert_eq!(
        count(&bus, "SELECT count(*) FROM settings WHERE key='auto_link'"),
        1
    );
    refused(&bus, "root-a", "root-b");
    refused(&bus, "root-b", "root-a");
    drop(server);
    let server = Server::start(&bus, false);
    assert_eq!(settings(&bus, &server)["bus"]["auto_link"], false);
    refused(&bus, "root-a", "root-b");
    set_auto_link(&bus, &server, true);
    send(&bus, "root-a", "root-b", "enabled again");
    send(&bus, "root-b", "root-a", "enabled both ways");
}

#[test]
fn proposed_link_still_needs_acceptance_and_works_when_auto_link_is_off() {
    let (bus, _) = pair();
    let server = Server::start(&bus, false);
    set_auto_link(&bus, &server, false);
    bus.ok(&["link", "--from", "root-a", "--to", "root-b"]);
    refused(&bus, "root-a", "root-b");
    refused(&bus, "root-b", "root-a");
    bus.ok(&["accept", "--from", "root-b", "--to", "root-a"]);
    send(&bus, "root-a", "root-b", "explicit accepted link");
    send(&bus, "root-b", "root-a", "explicit reverse link");
}

#[test]
fn invalid_bus_setting_bodies_return_the_exact_error_without_mutation() {
    let bus = hub();
    let server = Server::start(&bus, false);
    let before = settings(&bus, &server)["bus"].clone();
    let mut failures = Vec::new();
    for body in [
        "",
        "{",
        "null",
        "[]",
        "false",
        "{}",
        r#"{"enabled":false}"#,
        r#"{"auto_link":null}"#,
        r#"{"auto_link":"false"}"#,
        r#"{"auto_link":0}"#,
        r#"{"auto_link":1}"#,
        r#"{"auto_link":[]}"#,
        r#"{"auto_link":{}}"#,
        r#"{"auto_link":false,"unexpected":true}"#,
    ] {
        let response = server.request(
            &bus,
            "PUT",
            "/api/settings/bus",
            &[
                ("Origin", &server.url),
                ("Content-Type", "application/json"),
            ],
            body,
        );
        let expected = json!({"error":"invalid","field":"auto_link"});
        let actual = serde_json::from_slice::<Value>(&response.body).ok();
        if response.status != 400 || actual.as_ref() != Some(&expected) {
            failures.push(format!(
                "body={body:?}: expected 400 {expected}; got {} {}",
                response.status,
                String::from_utf8_lossy(&response.body)
            ));
        }
        assert_eq!(
            settings(&bus, &server)["bus"],
            before,
            "invalid body mutated settings: {body}"
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn session_start_lists_same_repo_root_under_direct_message_heading() {
    let bus = hub();
    let checkout = repo(&bus, "project");
    start(&bus, "codex", "root-a", &checkout);
    let text = introduction(&start(&bus, "claude", "root-b", &checkout));
    let roster = text
        .split_once(DIRECT_INTRO)
        .unwrap_or_else(|| panic!("missing {DIRECT_INTRO:?}: {text}"))
        .1;
    assert!(
        roster.contains("root-a"),
        "direct roster must list root-a: {text}"
    );
}

#[test]
fn session_start_keeps_link_instruction_when_auto_link_is_off() {
    let (bus, checkout) = pair();
    let server = Server::start(&bus, false);
    set_auto_link(&bus, &server, false);
    let text = introduction(&start(&bus, "claude", "root-a", &checkout));
    assert!(
        text.contains("root-b"),
        "other repository session missing: {text}"
    );
    assert!(
        text.contains("link --to"),
        "must explain how to propose a link: {text}"
    );
    assert!(
        !text.contains(DIRECT_INTRO),
        "auto-link is disabled: {text}"
    );
}

#[test]
fn auto_link_accepts_exactly_400_unicode_characters() {
    let (bus, _) = pair();
    for symbol in ["x", "🦀"] {
        send(&bus, "root-a", "root-b", &symbol.repeat(400));
    }
}

#[test]
fn auto_link_rejects_401_characters_without_storing_a_message() {
    let (bus, _) = pair();
    for symbol in ["x", "🦀"] {
        let before = count(&bus, "SELECT count(*) FROM messages");
        let output = bus
            .cmd()
            .args([
                "send",
                "--from",
                "root-a",
                "--to",
                "root-b",
                "--kind",
                "handoff",
                "--body",
                &symbol.repeat(401),
            ])
            .assert()
            .code(2)
            .get_output()
            .clone();
        assert!(
            !output.stderr.is_empty(),
            "body cap must explain the rejection"
        );
        assert_eq!(count(&bus, "SELECT count(*) FROM messages"), before);
    }
}

#[test]
fn auto_link_delivery_is_untrusted_peer_text() {
    let (bus, checkout) = pair();
    send(&bus, "root-a", "root-b", "auto-link-delivery-marker");
    let payload = hook_payload(&bus, "codex", "PostToolUse", "root-b", &checkout);
    let reply = parse_json(&bus.hook("codex", "PostToolUse", &payload).stdout);
    let text = reply["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(text.contains("auto-link-delivery-marker"), "{text}");
    assert!(text.to_lowercase().contains("untrusted peer"), "{text}");
}

#[test]
fn stop_received_over_auto_link_still_denies_the_next_tool_call() {
    let (bus, checkout) = pair();
    bus.send("root-a", "root-b", "stop", "auto-link-stop-marker");
    let payload = hook_payload(&bus, "codex", "PreToolUse", "root-b", &checkout);
    let reason = deny_reason("codex", &bus.hook("codex", "PreToolUse", &payload));
    assert!(reason.contains("auto-link-stop-marker"), "{reason}");
}

#[test]
fn auto_link_does_not_bypass_the_budget_gate() {
    let (bus, checkout) = pair();
    send(&bus, "root-a", "root-b", "before budget exhaustion");
    bus.ok(&["budget", "set", "root-a", "100tok"]);
    bus.usage("root-a", "claude-sonnet-4.6", [101, 0, 0, 0], now_ms(), 1);
    let mut payload = hook_payload(&bus, "claude", "PreToolUse", "root-a", &checkout);
    payload["tool_name"] = json!("Bash");
    payload["tool_input"] =
        json!({"command":"axon bus send --to root-b --kind handoff --body blocked"});
    let before = count(&bus, "SELECT count(*) FROM messages WHERE kind='handoff'");
    let reason = deny_reason("claude", &bus.hook("claude", "PreToolUse", &payload));
    assert!(reason.to_lowercase().contains("budget"), "{reason}");
    assert_eq!(
        count(&bus, "SELECT count(*) FROM messages WHERE kind='handoff'"),
        before
    );
}
