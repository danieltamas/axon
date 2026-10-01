//! Independent acceptance contract for 42beff2, from the review request's five clauses.
//! Exercise the real CLI and JSON hook boundary against common::Bus's isolated hub.
//! Deliberately retain failing expectations; no source changes belong in this review.

mod common;
use common::*;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Output;
use std::time::Duration;

fn hub() -> Bus {
    let bus = Bus::with_limit(Duration::from_secs(15));
    bus.init();
    bus
}

fn pair() -> Bus {
    let bus = hub();
    bus.register("S1", "claude", None);
    bus.register("C1", "claude", Some("S1"));
    bus
}

fn git(bus: &Bus, cwd: &Path, args: &[&str]) {
    let mut command = std::process::Command::new("git");
    bus.isolate(&mut command);
    command.current_dir(cwd).args(args);
    assert_cmd::Command::from(command)
        .timeout(bus.remaining())
        .assert()
        .success();
}

fn payload(bus: &Bus, harness: &str, event: &str, actor: &str) -> Value {
    let mut value = bus.fixture(harness, event);
    if harness == "opencode" {
        value["input"]["sessionID"] = json!(actor);
    } else {
        value["session_id"] = json!(actor);
        value["cwd"] = json!(bus.root.join("work"));
    }
    if matches!(event, "PreToolUse" | "PostToolUse") {
        value["tool_name"] = json!("Bash");
        value["tool_input"] = json!({"command": "echo fixture"});
        value["tool_response"] = json!({"stdout": "fixture"});
    }
    value
}

fn context(bus: &Bus, harness: &str, event: &str, value: &Value) -> String {
    let reply = parse_json(&bus.hook(harness, event, value).stdout);
    let text = if matches!(harness, "claude" | "codex") {
        assert_eq!(reply["hookSpecificOutput"]["hookEventName"], event);
        &reply["hookSpecificOutput"]["additionalContext"]
    } else {
        &reply["context"]
    };
    text.as_str()
        .expect("delivery must contain context")
        .to_owned()
}

fn contains_all(text: &str, expected: &[&str]) {
    for &expected in expected {
        assert!(text.contains(expected), "missing {expected:?}: {text}");
    }
}

fn intro(text: &str, actor: &str) {
    contains_all(text, &[actor, " send ", " reply ", " peers"]);
}

fn bash(bus: &Bus, actor: &str, command: &str) -> Output {
    let mut value = payload(bus, "claude", "PreToolUse", actor);
    value["tool_input"]["command"] = json!(command);
    bus.hook("claude", "PreToolUse", &value)
}

fn ran(output: &Output) -> String {
    let reason = deny_reason("claude", output);
    assert!(reason.contains("do not run it again"), "{reason}");
    assert!(!reason.to_lowercase().contains("failed"), "{reason}");
    reason
}

fn result_json(reason: &str) -> Value {
    reason
        .lines()
        .find_map(|line| serde_json::from_str(line).ok())
        .expect("relay must return the command's JSON result")
}

fn delivered(bus: &Bus, actor: &str) -> String {
    let value = payload(bus, "claude", "PostToolUse", actor);
    context(bus, "claude", "PostToolUse", &value)
}

fn ids(peers: &Value, field: &str) -> Vec<String> {
    let mut found: Vec<String> = peers[field]
        .as_array()
        .unwrap()
        .iter()
        .map(|peer| peer["id"].as_str().unwrap().to_owned())
        .collect();
    found.sort();
    found
}

fn roster() -> Bus {
    let bus = hub();
    git(&bus, &bus.root.join("work"), &["init"]);
    for id in ["S1", "S2", "S3", "S4", "closed-session", "orphaned-session"] {
        bus.register(id, "claude", None);
    }
    for id in ["live-child", "closed-child", "orphaned-child"] {
        bus.register(id, "claude", Some("S1"));
    }
    bus.register_at("foreign-session", "claude", None, &bus.root.join("home"));
    bus.db()
        .execute_batch(
            "UPDATE agents SET status='closed' WHERE id IN ('closed-child','closed-session');
         UPDATE agents SET status='orphaned' WHERE id IN ('orphaned-child','orphaned-session');",
        )
        .unwrap();
    bus.ok(&["link", "--from", "S1", "--to", "S2"]);
    bus.ok(&["accept", "--from", "S2", "--to", "S1"]);
    bus.ok(&["link", "--from", "S3", "--to", "S1"]);
    bus
}

#[test]
fn session_start_introduces_identity_relationships_and_actionable_commands() {
    let bus = roster();
    let value = payload(&bus, "claude", "SessionStart", "S1");
    let text = context(&bus, "claude", "SessionStart", &value);
    intro(&text, "S1");
    contains_all(&text, &["root", "live-child", "S2", "S3", "S4"]);
    contains_all(&text, &[" accept --to ", " link --to "]);
    for excluded in [
        "closed-child",
        "orphaned-child",
        "closed-session",
        "orphaned-session",
        "foreign-session",
    ] {
        assert!(!text.contains(excluded), "not a live peer: {text}");
    }
    // Claude receives context again on resume, not on every tool completion.
    let mut resumed = value;
    resumed["source"] = json!("resume");
    intro(&context(&bus, "claude", "SessionStart", &resumed), "S1");
}

#[test]
fn subagent_start_introduces_the_child_parent_and_repo_roots() {
    let bus = hub();
    git(&bus, &bus.root.join("work"), &["init"]);
    bus.register("S1", "claude", None);
    bus.register("S2", "claude", None);
    let mut value = payload(&bus, "claude", "SubagentStart", "S1");
    value["agent_id"] = json!("C1");
    let text = context(&bus, "claude", "SubagentStart", &value);
    intro(&text, "C1");
    contains_all(&text, &["S1", "S2", " link --to "]);
    assert!(text.to_lowercase().contains("parent"), "{text}");
}

fn first_delivery(harness: &str, first: &str, next: &str) {
    let bus = hub();
    bus.register("S1", "claude", None);
    bus.register("C1", harness, Some("S1"));
    bus.send("S1", "C1", "sync", "first-delivery-marker");
    let value = payload(&bus, harness, first, "C1");
    let text = context(&bus, harness, first, &value);
    intro(&text, "C1");
    assert!(text.contains("first-delivery-marker"), "{text}");
    let value = payload(&bus, harness, next, "C1");
    assert_allowed(&bus.hook(harness, next, &value));
    bus.send("S1", "C1", "sync", "later-delivery-marker");
    let text = context(&bus, harness, next, &value);
    assert!(text.contains("later-delivery-marker"), "{text}");
    assert!(
        !text.contains("first-delivery-marker") && !text.contains(" peers"),
        "{text}"
    );
}

#[test]
fn codex_post_tool_use_introduces_once() {
    first_delivery("codex", "PostToolUse", "UserPromptSubmit");
}
#[test]
fn codex_user_prompt_submit_introduces_once() {
    first_delivery("codex", "UserPromptSubmit", "PostToolUse");
}
#[test]
fn hermes_introduces_once() {
    first_delivery("hermes", "pre_llm_call", "pre_llm_call");
}
#[test]
fn opencode_introduces_once() {
    first_delivery("opencode", "tool.execute.after", "tool.execute.after");
}

#[test]
fn peers_returns_the_contract_and_only_live_children_and_repo_candidates() {
    let bus = roster();
    let peers = bus.json(&["peers", "--agent", "S1"]);
    assert_eq!(peers.as_object().unwrap().len(), 7);
    assert_eq!(peers["you"], "S1");
    assert_eq!(peers["harness"], "claude");
    assert!(peers["parent"].is_null());
    assert_eq!(ids(&peers, "children"), ["live-child"]);
    assert_eq!(ids(&peers, "linked"), ["S2"]);
    assert_eq!(ids(&peers, "link_proposals"), ["S3"]);
    assert_eq!(ids(&peers, "same_repo"), ["S4"]);
    let child = bus.json(&["peers", "--agent", "live-child"]);
    assert_eq!(child["parent"]["id"], "S1");
}

#[test]
fn peers_rejects_an_unregistered_agent() {
    let bus = hub();
    let output = bus
        .cmd()
        .args(["peers", "--agent", "missing"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing"));
    assert!(output.stdout.is_empty());
}

#[test]
fn send_uses_the_calling_child_and_preserves_quoted_unicode_text() {
    let bus = pair();
    for (binary, actor_flag) in [
        ("axon bus", ""),
        ("axon-bus", "--from C1"),
        ("axon-bus", "--from=C1"),
    ] {
        let mut value = payload(&bus, "claude", "PreToolUse", "S1");
        value["agent_id"] = json!("C1");
        value["tool_input"]["command"] = json!(format!(
            "{binary} send {actor_flag} --to S1 --kind sync --body \"hello 'peer' 🦀\""
        ));
        let sent = result_json(&ran(&bus.hook("claude", "PreToolUse", &value)));
        assert!(sent["thread"].as_str().is_some_and(|s| !s.is_empty()));
        let stored: (String, String, String) = bus
            .db()
            .query_row(
                "SELECT from_id,to_id,body FROM messages WHERE id=?1",
                [sent["id"].as_str().unwrap()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(stored, ("C1".into(), "S1".into(), "hello 'peer' 🦀".into()));
    }
    assert_eq!(bus.count("SELECT count(*) FROM messages"), 3);
}

#[test]
fn forged_from_and_claim_agent_are_refused_before_mutation() {
    let bus = pair();
    for command in [
        "axon bus send --from S1 --to C1 --kind sync --body forged",
        "axon-bus send --from=S1 --to C1 --kind sync --body forged",
        "axon-bus claim --agent S1 src",
        "axon-bus release --agent=S1",
    ] {
        let reason = deny_reason("claude", &bash(&bus, "C1", command));
        assert!(reason.contains("as yourself"), "{reason}");
    }
    assert_eq!(bus.count("SELECT count(*) FROM messages"), 0);
    assert_eq!(bus.count("SELECT count(*) FROM claims"), 0);
}

#[test]
fn peers_cannot_override_the_calling_agent() {
    let bus = hub();
    bus.register("S1", "claude", None);
    bus.register("S2", "claude", None);
    assert_eq!(
        result_json(&ran(&bash(&bus, "S1", "axon bus peers")))["you"],
        "S1"
    );
    assert_eq!(
        result_json(&ran(&bash(&bus, "S1", "axon-bus peers --agent S1")))["you"],
        "S1"
    );
    let reason = deny_reason("claude", &bash(&bus, "S1", "axon-bus peers --agent S2"));
    assert!(reason.contains("as yourself"), "{reason}");
}

#[test]
fn windows_executable_names_and_quoted_paths_are_relayed() {
    let bus = hub();
    bus.register("S1", "claude", None);
    for command in [
        "axon.exe bus peers",
        "axon-bus.exe peers",
        r#""C:\Program Files\Axon\axon.exe" bus peers"#,
        r#""C:\Program Files\Axon\axon-bus.exe" peers"#,
    ] {
        assert_eq!(result_json(&ran(&bash(&bus, "S1", command)))["you"], "S1");
    }
}

#[test]
fn failed_relay_includes_stderr_and_tells_the_agent_not_to_repeat_it() {
    let bus = hub();
    bus.register("S1", "claude", None);
    let direct = bus
        .cmd()
        .args([
            "send", "--from", "S1", "--to", "missing", "--kind", "sync", "--body", "hello",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8(direct.stderr).unwrap();
    assert!(!stderr.trim().is_empty());
    let command = "axon bus send --to missing --kind sync --body hello";
    let reason = deny_reason("claude", &bash(&bus, "S1", command));
    contains_all(&reason, &["failed", stderr.trim(), "do not run it again"]);
    assert_eq!(bus.count("SELECT count(*) FROM messages"), 0);
}

#[test]
fn ask_is_denied_with_nonblocking_question_guidance() {
    let bus = hub();
    bus.register("S1", "claude", None);
    let command = "axon bus ask --to S2 --body question --wait 3600 --default no";
    let reason = deny_reason("claude", &bash(&bus, "S1", command));
    assert!(reason.contains("--kind question"), "{reason}");
    assert_eq!(bus.count("SELECT count(*) FROM messages"), 0);
}

#[test]
fn commands_outside_the_bare_bus_contract_pass_through_without_side_effects() {
    let bus = pair();
    for command in [
        "echo ordinary",
        "echo axon bus peers",
        "axon-bus doctor",
        "env axon-bus peers",
        "cd . && axon bus peers",
        "axon bus peers; echo suffix",
        "axon bus peers && echo suffix",
        "axon bus peers | cat",
        "axon bus peers\necho suffix",
        "axon bus peers > output.txt",
        "axon bus peers < input.txt",
        "axon bus send --to C1 --kind sync --body $(echo substituted)",
        "axon bus send --to C1 --kind sync --body `echo substituted`",
        "axon bus send --to C1 --kind sync --body 'unterminated",
    ] {
        assert_allowed(&bash(&bus, "S1", command));
    }
    assert_eq!(bus.count("SELECT count(*) FROM messages"), 0);
}

#[test]
fn attached_redirections_are_not_relayed() {
    let bus = pair();
    for suffix in ["hello>output.txt", "hello<input.txt", "hello 2>errors.txt"] {
        assert_allowed(&bash(
            &bus,
            "S1",
            &format!("axon bus send --to C1 --kind sync --body {suffix}"),
        ));
        assert_eq!(bus.count("SELECT count(*) FROM messages"), 0);
    }
}

#[test]
fn non_bash_tool_with_a_command_field_is_not_executed_by_the_relay() {
    let bus = pair();
    let mut value = payload(&bus, "claude", "PreToolUse", "S1");
    value["tool_name"] = json!("mcp__example__preview_command");
    value["tool_input"]["command"] = json!("axon bus send --to C1 --kind sync --body preview-only");
    assert_allowed(&bus.hook("claude", "PreToolUse", &value));
    assert_eq!(bus.count("SELECT count(*) FROM messages"), 0);
}

#[test]
fn claim_release_claims_and_budget_relay_use_the_payload_checkout() {
    let bus = hub();
    bus.register("S1", "claude", None);
    let checkout = bus.root.join("checkout with spaces");
    let nested = checkout.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    git(&bus, &checkout, &["init"]);
    let mut value = payload(&bus, "claude", "PreToolUse", "S1");
    value["cwd"] = json!(nested);
    for verb in ["claim", "release"] {
        value["tool_input"]["command"] = json!(format!("axon-bus {verb} \"file with spaces.rs\""));
        ran(&bus.hook("claude", "PreToolUse", &value));
        let listed = ran(&bash(&bus, "S1", "axon bus claims"));
        if verb == "claim" {
            let claim = result_json(&listed);
            assert_eq!(claim["agent_id"], "S1");
            assert_eq!(claim["checkout"], json!(checkout));
            let expected = Path::new("nested").join("file with spaces.rs");
            assert_eq!(claim["path"], json!(expected));
        } else {
            assert_eq!(bus.count("SELECT count(*) FROM claims"), 0);
        }
    }
    bus.ok(&["budget", "set", "S1", "100tok"]);
    let expected = bus.json(&["budget", "show", "S1", "--json"]);
    assert_eq!(
        result_json(&ran(&bash(&bus, "S1", "axon bus budget show S1 --json"))),
        expected
    );
}

#[test]
fn relay_does_not_fall_back_to_an_unrelated_cwd() {
    let bus = hub();
    bus.register("S1", "claude", None);
    let mut value = payload(&bus, "claude", "PreToolUse", "S1");
    value["cwd"] = json!(bus.root.join("removed-checkout"));
    value["tool_input"]["command"] = json!("axon bus claim src");
    let reason = deny_reason("claude", &bus.hook("claude", "PreToolUse", &value));
    assert!(reason.contains("failed"), "{reason}");
    assert_eq!(bus.count("SELECT count(*) FROM claims"), 0);
}

#[test]
fn grant_relay_injects_the_actor_and_opens_the_requested_thread() {
    let bus = hub();
    bus.linked_roots();
    let sent = bus.send("sub1", "orch1", "question", "request direct review");
    let thread = sent["thread"].as_str().unwrap();
    ran(&bash(
        &bus,
        "sub1",
        &format!("axon bus grant --to sub2 --thread {thread} --ttl 60s"),
    ));
    bus.ok(&[
        "send", "--from", "sub1", "--to", "sub2", "--kind", "sync", "--body", "direct", "--thread",
        thread,
    ]);
    bus.cmd()
        .args(["send", "--from", "sub1", "--to", "sub2", "--kind", "sync"])
        .args(["--body", "wrong-thread", "--thread", "unrelated"])
        .assert()
        .code(3);
}

#[test]
fn two_sessions_link_accept_question_and_answer_through_hooks() {
    let bus = hub();
    git(&bus, &bus.root.join("work"), &["init"]);
    for actor in ["S1", "S2"] {
        let value = payload(&bus, "claude", "SessionStart", actor);
        intro(&context(&bus, "claude", "SessionStart", &value), actor);
    }
    assert_eq!(
        ids(&bus.json(&["peers", "--agent", "S1"]), "same_repo"),
        ["S2"]
    );
    ran(&bash(&bus, "S1", "axon bus link --to S2"));
    let notice = delivered(&bus, "S2");
    contains_all(&notice, &["S1", "accept --to S1"]);
    ran(&bash(&bus, "S2", "axon-bus accept --to S1"));
    let ack = delivered(&bus, "S1");
    contains_all(&ack, &["S2", "accepted", "ack"]);
    let command = "axon bus send --to S2 --kind question --body 'May I proceed?'";
    let sent = result_json(&ran(&bash(&bus, "S1", command)));
    let question = delivered(&bus, "S2");
    contains_all(&question, &["May I proceed?", "untrusted peer"]);
    let answer = question
        .lines()
        .find_map(|line| line.strip_prefix("Answer with: "))
        .expect("question delivery must carry an executable reply command");
    assert!(answer.contains(sent["id"].as_str().unwrap()), "{answer}");
    assert!(answer.contains("--body \"...\""), "{answer}");
    let answer = answer.replace("--body \"...\"", "--body \"Yes, proceed.\"");
    let replied = result_json(&ran(&bash(&bus, "S2", &answer)));
    assert_eq!(replied["thread"], sent["thread"]);
    let received = delivered(&bus, "S1");
    contains_all(&received, &["Yes, proceed.", "S2"]);
    for actor in ["S1", "S2"] {
        let value = payload(&bus, "claude", "PostToolUse", actor);
        assert_allowed(&bus.hook("claude", "PostToolUse", &value));
    }
    assert_eq!(
        bus.count("SELECT count(*) FROM messages WHERE kind='question'"),
        1
    );
    assert_eq!(
        bus.count("SELECT count(*) FROM messages WHERE kind='answer'"),
        1
    );
}
