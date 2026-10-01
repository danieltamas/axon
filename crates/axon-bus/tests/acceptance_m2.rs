//! Contract decisions
//! - Non-edge CLI sends exit 3 and print the ordered route on stderr (` -> ` or ` → `).
//! - `ask --from ID --to ID --body TEXT --wait SECONDS --default TEXT` prints one final
//!   JSON object {body,timed_out,thread}; the pending question is committed before waiting.
//! - `reply QUESTION_ID --from ID --body TEXT` resolves that question, preserving thread.
//! - `grant --from ID --to ID --thread ID --ttl 60s` creates a bidirectional edge scoped
//!   to that thread; expiration is enforced by sends without requiring serve or a sweep.
//!   Its audit event uses verb=grant.
//! - `send ... --kind stop` puts the body in the next denial reason. Delivery injects
//!   peer messages with the literal label `untrusted peer`; delivered_at becomes non-null.
//! - ask timing tests have an 8-second total safety bound; reply latency is still <=1 s,
//!   and a 1-second timeout must wait >=1 s and finish within 2.5 s.

mod common;
use common::*;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

// BUS-PLAN §3 and §9 M2: non-edge sends fail with exit 3 and an explicit forwarding route.
#[test]
fn non_edge_send_exits_3_and_prints_route() {
    let bus = Bus::new();
    bus.init();
    bus.linked_roots();
    let before = bus.count("SELECT count(*) FROM messages");
    let output = bus
        .cmd()
        .args([
            "send",
            "--from",
            "sub1",
            "--to",
            "sub2",
            "--kind",
            "redirect",
            "--body",
            "review this",
        ])
        .assert()
        .code(3)
        .get_output()
        .clone();
    assert_route(
        &String::from_utf8(output.stderr).unwrap(),
        &["sub1", "orch1", "orch2", "sub2"],
    );
    assert_eq!(bus.count("SELECT count(*) FROM messages"), before);
}

// BUS-PLAN §3 and §9 M2: ask waits for its answer and returns within one second of reply.
#[test]
fn ask_wait_returns_the_matching_reply_within_one_second() {
    let bus = Bus::with_limit(Duration::from_secs(8));
    bus.init();
    bus.register("root", "claude", None);
    bus.register("child", "codex", Some("root"));
    let mut asker = bus.spawn(&[
        "ask",
        "--from",
        "child",
        "--to",
        "root",
        "--body",
        "Proceed?",
        "--wait",
        "5",
        "--default",
        "decline",
    ]);
    let deadline = Instant::now() + Duration::from_secs(1);
    let (id, thread): (String, String) = loop {
        use rusqlite::OptionalExtension;
        let pending = bus
            .db()
            .query_row(
                "SELECT id,thread FROM messages WHERE from_id='child' AND to_id='root'
             AND kind='question' AND needs_reply=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .unwrap();
        if let Some(question) = pending {
            break question;
        }
        assert!(
            asker.0.try_wait().unwrap().is_none(),
            "ask exited before a reply or deadline"
        );
        assert!(
            Instant::now() < deadline,
            "ask must commit its question before blocking"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(asker.0.try_wait().unwrap().is_none());
    let start = Instant::now();
    bus.ok(&["reply", &id, "--from", "root", "--body", "yes, proceed"]);
    let output = asker.finish(Duration::from_secs(1).saturating_sub(start.elapsed()));
    assert!(start.elapsed() <= Duration::from_secs(1));
    assert!(output.status.success(), "{output:?}");
    let answer = parse_json(&output.stdout);
    assert_eq!(answer["body"], "yes, proceed");
    assert_eq!(answer["timed_out"], false);
    assert_eq!(answer["thread"], thread);
}

// BUS-PLAN §3: an unanswered ask returns its exact default only after the requested wait.
#[test]
fn ask_timeout_waits_then_returns_the_default() {
    let bus = Bus::with_limit(Duration::from_secs(8));
    bus.init();
    bus.register("root", "claude", None);
    bus.register("child", "codex", Some("root"));
    let start = Instant::now();
    let mut asker = bus.spawn(&[
        "ask",
        "--from",
        "child",
        "--to",
        "root",
        "--body",
        "Proceed?",
        "--wait",
        "1",
        "--default",
        "do not proceed",
    ]);
    let output = asker.finish(Duration::from_millis(2500));
    assert!(
        start.elapsed() >= Duration::from_secs(1),
        "default returned before timeout"
    );
    assert!(start.elapsed() < Duration::from_millis(2500));
    assert!(output.status.success(), "{output:?}");
    let answer = parse_json(&output.stdout);
    assert_eq!(answer["body"], "do not proceed");
    assert_eq!(answer["timed_out"], true);
    assert!(answer["thread"].as_str().is_some_and(|s| !s.is_empty()));
}

fn stop_denies(harness: &str) {
    let bus = Bus::new();
    bus.init();
    bus.register("root", "claude", None);
    bus.register("worker", harness, Some("root"));
    assert_allowed(&bus.gate(harness, "worker"));
    bus.send("root", "worker", "stop", "acceptance stop reason");
    assert!(deny_reason(harness, &bus.gate(harness, "worker")).contains("acceptance stop reason"));
}

// BUS-PLAN §3 and §9 M0: Claude stop uses PreToolUse permissionDecision=deny.
#[test]
fn stop_denies_the_next_claude_tool_call() {
    stop_denies("claude");
}

// BUS-PLAN §3 and §9 M0: Codex shares the PreToolUse deny wire format.
#[test]
fn stop_denies_the_next_codex_tool_call() {
    stop_denies("codex");
}

// BUS-PLAN §3 and §9 M0: OpenCode's shim receives decision=deny and a reason.
#[test]
fn stop_denies_the_next_opencode_tool_call() {
    stop_denies("opencode");
}

// BUS-PLAN §3 and §9 M0: Hermes pre_tool_call receives decision=block and a reason.
#[test]
fn stop_denies_the_next_hermes_tool_call() {
    stop_denies("hermes");
}

// BUS-PLAN §3: 400 characters are accepted, 401 rejected, including multibyte characters.
#[test]
fn body_limit_accepts_400_characters_and_rejects_401_without_inserting() {
    let bus = Bus::new();
    bus.init();
    bus.register("root", "claude", None);
    bus.register("child", "codex", Some("root"));
    for symbol in ["x", "🦀"] {
        let accepted = symbol.repeat(400);
        let sent = bus.send("root", "child", "redirect", &accepted);
        let stored: String = bus
            .db()
            .query_row(
                "SELECT body FROM messages WHERE id=?1",
                [sent["id"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, accepted);
        let before = bus.count("SELECT count(*) FROM messages");
        let output = bus
            .cmd()
            .args([
                "send",
                "--from",
                "root",
                "--to",
                "child",
                "--kind",
                "redirect",
                "--body",
                &symbol.repeat(401),
            ])
            .assert()
            .code(2)
            .get_output()
            .clone();
        assert!(!output.stderr.is_empty());
        assert_eq!(bus.count("SELECT count(*) FROM messages"), before);
    }
}

// BUS-PLAN §3: a root link opens only after the other root accepts, in both directions.
#[test]
fn link_requires_acceptance_before_either_direction_can_send() {
    let bus = Bus::new();
    bus.init();
    for id in ["one", "two"] {
        bus.register(id, "claude", None);
    }
    bus.ok(&["link", "--from", "one", "--to", "two"]);
    for (from, to) in [("one", "two"), ("two", "one")] {
        bus.cmd()
            .args([
                "send", "--from", from, "--to", to, "--kind", "redirect", "--body", "pending",
            ])
            .assert()
            .code(3);
    }
    bus.ok(&["accept", "--from", "two", "--to", "one"]);
    for (from, to) in [("one", "two"), ("two", "one")] {
        bus.send(from, to, "redirect", "accepted");
    }
}

// BUS-PLAN §3: a logged grant is thread-scoped and expires without a running server.
#[test]
fn grant_opens_only_its_thread_and_expiration_closes_the_edge() {
    let bus = Bus::new();
    bus.init();
    bus.linked_roots();
    let sent = bus.send("sub1", "orch1", "question", "request direct review");
    let thread = sent["thread"].as_str().unwrap();
    let start = now_ms();
    bus.ok(&[
        "grant", "--from", "sub1", "--to", "sub2", "--thread", thread, "--ttl", "60s",
    ]);
    let expires: i64 = bus
        .db()
        .query_row(
            "SELECT expires_at FROM edges WHERE from_id='sub1' AND to_id='sub2' AND kind='grant' AND thread=?1",
            [thread],
            |r| r.get(0),
        )
        .unwrap();
    assert!(expires >= start + 60_000 && expires <= now_ms() + 60_000);
    assert!(bus.count("SELECT count(*) FROM events WHERE verb='grant'") > 0);
    for (from, to) in [("sub1", "sub2"), ("sub2", "sub1")] {
        bus.ok(&[
            "send", "--from", from, "--to", to, "--kind", "redirect", "--body", "in scope",
            "--thread", thread,
        ]);
    }
    bus.cmd()
        .args([
            "send",
            "--from",
            "sub1",
            "--to",
            "sub2",
            "--kind",
            "redirect",
            "--body",
            "wrong scope",
            "--thread",
            "another-thread",
        ])
        .assert()
        .code(3);
    // Age persisted policy, not wall-clock sleeps, to make the expiry branch deterministic.
    bus.db()
        .execute(
            "UPDATE edges SET expires_at=?1 WHERE kind='grant' AND thread=?2",
            rusqlite::params![now_ms() - 1, thread],
        )
        .unwrap();
    let output = bus
        .cmd()
        .args([
            "send", "--from", "sub1", "--to", "sub2", "--kind", "redirect", "--body", "expired",
            "--thread", thread,
        ])
        .assert()
        .code(3)
        .get_output()
        .clone();
    assert_route(
        &String::from_utf8(output.stderr).unwrap(),
        &["sub1", "orch1", "orch2", "sub2"],
    );
}

fn native_payload(bus: &Bus) -> (Value, String) {
    let mut payload = bus.fixture("claude", "PreToolUse");
    let root = payload["session_id"].as_str().unwrap().to_owned();
    bus.hook("claude", "PreToolUse", &payload);
    payload["tool_name"] = json!("SendMessage");
    (payload, root)
}

// BUS-PLAN §3, spike Q2: native SendMessage resolves sender by session and denies non-edges.
#[test]
fn claude_native_send_message_denies_a_non_edge_with_the_route() {
    let bus = Bus::new();
    bus.init();
    let (mut payload, root) = native_payload(&bus);
    bus.register("orch2", "claude", None);
    bus.register("sub2", "claude", Some("orch2"));
    bus.ok(&["link", "--from", &root, "--to", "orch2"]);
    bus.ok(&["accept", "--from", "orch2", "--to", &root]);
    payload["tool_input"] = json!({"to":"sub2","message":"native body"});
    let reason = deny_reason("claude", &bus.hook("claude", "PreToolUse", &payload));
    assert_route(&reason, &[&root, "orch2", "sub2"]);
}

// BUS-PLAN §3, spike Q2: the hub lets native edge transport proceed without a decision.
#[test]
fn claude_native_send_message_allows_a_direct_edge_silently() {
    let bus = Bus::new();
    bus.init();
    let (mut payload, root) = native_payload(&bus);
    bus.register("direct-child", "claude", Some(&root));
    payload["tool_input"] = json!({"to":"direct-child","message":"native body"});
    assert_allowed(&bus.hook("claude", "PreToolUse", &payload));
}

// BUS-PLAN §3 and §00 spike Q1: cross-harness delivery uses the measured injection formats.
#[test]
fn hook_delivery_frames_peer_text_and_marks_it_delivered_once() {
    let bus = Bus::new();
    bus.init();
    bus.register("sender", "opencode", None);
    for harness in ["claude", "codex", "hermes"] {
        bus.register(harness, harness, Some("sender"));
        let sent = bus.send("sender", harness, "redirect", "peer-text-marker");
        let event = if harness == "hermes" {
            "pre_llm_call"
        } else {
            "PostToolUse"
        };
        let mut payload = bus.fixture(harness, event);
        payload["session_id"] = json!(harness);
        // Use a child tool completion for Claude so replaying delivery does not spawn an agent.
        if harness == "claude" {
            payload["tool_name"] = json!("Bash");
            payload["tool_response"] = json!({"stdout":"ok"});
        }
        let reply = parse_json(&bus.hook(harness, event, &payload).stdout);
        let context = if harness == "hermes" {
            &reply["context"]
        } else {
            &reply["hookSpecificOutput"]["additionalContext"]
        };
        let context = context.as_str().unwrap();
        assert!(context.contains("peer-text-marker"));
        assert!(context.to_lowercase().contains("untrusted peer"));
        let delivered: Option<i64> = bus
            .db()
            .query_row(
                "SELECT delivered_at FROM messages WHERE id=?1",
                [sent["id"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(delivered.is_some());
        assert_allowed(&bus.hook(harness, event, &payload));
    }
}
