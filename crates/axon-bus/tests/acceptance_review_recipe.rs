//! Independent contract: BUS-PLAN §3 and USAGE "Cross-vendor review over the bus".
//! Expectations come from the 2026-10-03 spec; plain guide stays a compact playbook.

mod common;
use common::fed::{db, git};
use common::*;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const CHANGE_REF: &str = "src/change.rs:L10-40@0123456789abcdef";
const FINDINGS: &str = "review/findings.md";
const SECURITY: &str = "review/security notes.md";

fn hub() -> Bus {
    let bus = Bus::with_limit(Duration::from_secs(15));
    bus.init();
    bus
}

fn payload(bus: &Bus, harness: &str, event: &str, id: &str) -> Value {
    let mut value = bus.fixture(harness, event);
    value["session_id"] = json!(id);
    value["cwd"] = json!(bus.root.join("work"));
    if matches!(event, "PreToolUse" | "PostToolUse") {
        value["tool_name"] = json!("Bash");
        value["tool_input"] = json!({"command":"echo fixture"});
        value["tool_response"] = json!({"stdout":"fixture"});
    }
    value
}

fn pair() -> Bus {
    let bus = hub();
    git(&bus, &bus.root.join("work"), &["init"]);
    for (harness, id) in [("claude", "writer"), ("codex", "reviewer")] {
        bus.hook(
            harness,
            "SessionStart",
            &payload(&bus, harness, "SessionStart", id),
        );
        assert_eq!(bus.agent(id)["harness"], harness);
    }
    // Consume introductions so delivery assertions cannot be satisfied by guide text.
    for (harness, id) in [("claude", "writer"), ("codex", "reviewer")] {
        bus.hook(
            harness,
            "PostToolUse",
            &payload(&bus, harness, "PostToolUse", id),
        );
    }
    let review = bus.root.join("work/review");
    std::fs::create_dir(&review).unwrap();
    std::fs::write(review.join("findings.md"), "One correctness finding.\n").unwrap();
    std::fs::write(review.join("security notes.md"), "One security finding.\n").unwrap();
    bus
}

fn delivered(bus: &Bus, harness: &str, id: &str) -> String {
    let event = "PostToolUse";
    let output = bus.hook(harness, event, &payload(bus, harness, event, id));
    let reply = parse_json(&output.stdout);
    assert_eq!(reply["hookSpecificOutput"]["hookEventName"], event);
    reply["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("delivered message context")
        .to_owned()
}

fn contains_all(text: &str, parts: &[&str]) {
    for part in parts {
        assert!(text.contains(part), "missing {part:?}: {text}");
    }
}

fn refs(bus: &Bus, id: &str) -> Value {
    let encoded: String = db(bus)
        .query_row("SELECT refs_json FROM messages WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}

fn acknowledged(bus: &Bus, id: &str) -> bool {
    db(bus)
        .query_row(
            "SELECT acked_at IS NOT NULL FROM messages WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

fn review_roundtrip(through_relay: bool) {
    let bus = pair();
    let (asker, asker_harness, reviewer, reviewer_harness) = if through_relay {
        ("reviewer", "codex", "writer", "claude")
    } else {
        ("writer", "claude", "reviewer", "codex")
    };
    let question = bus.json(&[
        "send",
        "--from",
        asker,
        "--to",
        reviewer,
        "--kind",
        "question",
        "--body",
        "Review this change and reply by reference.",
        "--ref",
        CHANGE_REF,
    ]);
    let id = question["id"].as_str().unwrap();
    assert_eq!(refs(&bus, id), json!([CHANGE_REF]));
    assert!(!acknowledged(&bus, id));
    let request = delivered(&bus, reviewer_harness, reviewer);
    contains_all(&request, &[id, CHANGE_REF, "untrusted peer", "reply"]);
    assert!(!acknowledged(&bus, id), "delivery alone is not an answer");
    let body = "2 findings, worst high";
    let answer = if through_relay {
        let mut value = payload(&bus, reviewer_harness, "PreToolUse", reviewer);
        value["tool_input"]["command"] = json!(format!(
            "axon bus reply {id} --body '{body}' --ref '{FINDINGS}' --ref '{SECURITY}'"
        ));
        let output = bus.hook(reviewer_harness, "PreToolUse", &value);
        let reason = deny_reason(reviewer_harness, &output);
        assert!(reason.contains("do not run it again"), "{reason}");
        assert!(!reason.to_lowercase().contains("failed"), "{reason}");
        reason
            .lines()
            .find_map(|line| serde_json::from_str::<Value>(line).ok())
            .expect("relay returns reply JSON")
    } else {
        bus.json(&[
            "reply", id, "--from", reviewer, "--body", body, "--ref", FINDINGS, "--ref", SECURITY,
        ])
    };
    let answer_id = answer["id"].as_str().unwrap();
    assert_eq!(answer["thread"], question["thread"]);
    assert_eq!(refs(&bus, answer_id), json!([id, FINDINGS, SECURITY]));
    let stored: (String, String, String, String) = db(&bus)
        .query_row(
            "SELECT from_id,to_id,kind,body FROM messages WHERE id=?1",
            [answer_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        stored,
        (reviewer.into(), asker.into(), "answer".into(), body.into())
    );
    assert!(
        acknowledged(&bus, id),
        "reply with refs must close the question"
    );
    let received = delivered(&bus, asker_harness, asker);
    contains_all(
        &received,
        &[reviewer, body, "untrusted peer", id, FINDINGS, SECURITY],
    );
    for (harness, actor) in [(asker_harness, asker), (reviewer_harness, reviewer)] {
        assert_allowed(&bus.hook(
            harness,
            "PostToolUse",
            &payload(&bus, harness, "PostToolUse", actor),
        ));
    }
    assert_eq!(
        bus.count("SELECT count(*) FROM messages WHERE kind='answer'"),
        1
    );
}

#[test]
fn cli_reply_preserves_repeated_refs_delivers_them_and_answers_question() {
    review_roundtrip(false);
}

#[test]
fn hook_relay_reply_preserves_repeated_refs_and_answers_cross_vendor_question() {
    review_roundtrip(true);
}

#[test]
fn ask_returns_answer_body_when_reply_contains_file_refs() {
    let bus = pair();
    let mut asker = bus.spawn(&[
        "ask",
        "--from",
        "writer",
        "--to",
        "reviewer",
        "--body",
        "Review now?",
        "--wait",
        "5",
        "--default",
        "review timed out",
    ]);
    let deadline = Instant::now() + Duration::from_secs(1);
    let (id, thread): (String, String) = loop {
        let pending = db(&bus)
            .query_row(
                "SELECT id,thread FROM messages WHERE from_id='writer' AND to_id='reviewer'
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
            "ask exited before reply"
        );
        assert!(Instant::now() < deadline, "ask must publish before waiting");
        std::thread::sleep(Duration::from_millis(5));
    };
    contains_all(&delivered(&bus, "codex", "reviewer"), &[&id, "Review now?"]);
    assert!(asker.0.try_wait().unwrap().is_none());
    let body = "Review complete: 2 findings.";
    let reply = bus.json(&[
        "reply", &id, "--from", "reviewer", "--body", body, "--ref", FINDINGS, "--ref", SECURITY,
    ]);
    assert_eq!(
        refs(&bus, reply["id"].as_str().unwrap()),
        json!([id, FINDINGS, SECURITY])
    );
    let output = asker.finish(Duration::from_secs(1));
    assert!(output.status.success(), "{output:?}");
    let answer = parse_json(&output.stdout);
    assert_eq!(answer["body"], body);
    assert_eq!(answer["thread"], thread);
    assert_eq!(answer["timed_out"], false);
    assert!(acknowledged(&bus, &id));
}

#[test]
fn guide_review_prints_the_cross_vendor_question_reply_and_ref_recipe() {
    let bus = hub();
    let output = bus.ok(&["guide", "review"]);
    let text = std::str::from_utf8(&output.stdout).unwrap();
    contains_all(
        text,
        &[
            "--kind question",
            "reply <message-id>",
            "--ref",
            "peers",
            "Claude",
            "Codex",
        ],
    );
    assert!(
        text.matches("--ref").count() >= 2,
        "refs go both ways: {text}"
    );
    let lower = text.to_lowercase();
    contains_all(&lower, &["active", "hook", "idle"]);
    assert!(
        lower.contains("cannot start") || lower.contains("does not start"),
        "{text}"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn guide_unknown_topic_exits_two_and_names_available_topics() {
    let bus = hub();
    let output = bus
        .cmd()
        .args(["guide", "not-a-topic"])
        .assert()
        .code(2)
        .get_output()
        .clone();
    let error = String::from_utf8(output.stderr).unwrap();
    contains_all(&error, &["not-a-topic", "topics", "review"]);
    assert!(
        output.stdout.is_empty(),
        "an unknown topic must not print a recipe"
    );
}

#[test]
fn plain_guide_keeps_playbook_sections_and_refusals_and_fits_one_reply() {
    let bus = hub();
    let output = bus.ok(&["guide"]);
    let text = std::str::from_utf8(&output.stdout).unwrap();
    // Normalize only the Cargo build location to the specified long installed command;
    // otherwise CARGO_TARGET_DIR, rather than the playbook, determines the length.
    let executable = canonical(std::path::Path::new(assert_cmd::cargo::cargo_bin!(
        "axon-bus"
    )))
    .display()
    .to_string();
    let command = if executable.contains(' ') {
        format!("\"{executable}\"")
    } else {
        executable
    };
    assert!(text.contains(&command), "guide must show the bus command");
    let guide = text.replace(&command, "/usr/local/bin/axon bus");
    let characters = guide.chars().count();
    assert!(
        characters < 4000,
        "plain guide must fit one relayed reply with a long binary path: {characters} characters"
    );
    for section in [
        "WHO YOU CAN REACH",
        "MESSAGES",
        "REMOTE COLLABORATORS",
        "SHARED CHECKOUTS",
        "WORK ALREADY HANDLED",
        "STOPS AND BUDGETS",
        "HOW COMMANDS RUN",
    ] {
        assert!(
            guide.lines().any(|line| line.trim() == section),
            "plain guide must retain section {section:?}: {guide}"
        );
    }
    contains_all(
        &guide,
        &[
            "federation_off",
            "peer_paused",
            "peer_removed",
            "unknown_peer",
            "outbound_off",
            "remote_inbound_off",
            "not_a_member",
            "unknown_session",
            "kind_not_allowed",
            "too_long",
            "rate_limited",
            "queue_full",
            "/usr/local/bin/axon bus guide review",
        ],
    );
    assert!(
        guide
            .lines()
            .any(|line| line.contains(" reply <message-id>")
                && line.contains("--body")
                && line.contains("--ref")),
        "plain guide must document --ref on reply itself: {guide}"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}
