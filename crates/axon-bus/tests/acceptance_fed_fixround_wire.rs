//! Work order SEC-3/CDX-3, CDX-9/BUG-5 and C3: exercise the real receiving boundary.
mod common;
use common::delivery::*;
use common::fed::*;
use common::wire_probe::{frame, Probe};
use common::*;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

#[test]
fn remote_answer_with_local_question_thread_and_refs_cannot_satisfy_blocking_ask() {
    let (mut pair, _, target) = shared(60);
    pair.a
        .register_at("local-answerer", "claude", Some("agent-a"), &pair.ra);
    let mut asker = pair.a.spawn(&[
        "ask",
        "--from",
        "agent-a",
        "--to",
        "local-answerer",
        "--body",
        "local decision",
        "--wait",
        "10",
        "--default",
        "decline",
    ]);
    eventually(&pair.a, "local blocking question committed", || {
        assert!(asker.0.try_wait().unwrap().is_none());
        count(
            &pair.a,
            "SELECT count(*) FROM messages WHERE body='local decision' AND needs_reply=1",
        ) == 1
    });
    let (question, thread): (String, String) = db(&pair.a)
        .query_row(
            "SELECT id,thread FROM messages WHERE body='local decision' AND needs_reply=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    // Give the remote answer legitimate federation provenance, then forge only the local refs.
    let remote_question = queued_extra(
        &pair.a,
        "agent-a",
        &target,
        "question",
        "separate remote decision",
        &["--thread", &thread],
        0,
    );
    received(&pair.b, &remote_question);
    outcome(&pair.a, &remote_question, "accepted");
    let mut answer = envelope(&pair.a, &remote_question);
    let from = answer["from_session"].clone();
    answer["from_session"] = answer["to_session"].clone();
    answer["to_session"] = from;
    answer["message_id"] = json!("00000000-0000-4000-8000-000000000001");
    answer["kind"] = json!("answer");
    answer["body"] = json!("untrusted remote approval");
    answer["reply_to"] = json!(remote_question);
    answer["refs"] = json!([question]);
    let answer = frame(&pair.b, &pair.pb, answer);
    let probe = Probe::replace(&pair.b, &mut pair.sb, &pair.pb);
    let response = probe.request(&answer);
    assert_eq!(response["type"], "ack");
    match response["status"].as_str() {
        Some("accepted") => {
            let local = received(&pair.a, answer["message_id"].as_str().unwrap());
            assert!(
                text(&pair.a, "SELECT from_id FROM messages WHERE id=?1", &local)
                    .starts_with("peer:")
            );
            assert_eq!(
                text(
                    &pair.a,
                    "SELECT refs_json FROM messages WHERE id=?1",
                    &local
                ),
                json!([question]).to_string()
            );
        }
        // SEC-3 also permits rejecting a collision, or namespacing its stored thread.
        Some("rejected") => assert!(response["reason"].as_str().is_some_and(|r| !r.is_empty())),
        _ => panic!("unexpected attack response: {response}"),
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        asker.0.try_wait().unwrap().is_none(),
        "SEC-3: remote answer escaped remote framing into local ask"
    );
    let output = asker.finish(Duration::from_secs(11));
    assert!(output.status.success());
    let result = parse_json(&output.stdout);
    assert_eq!(result["body"], "decline");
    assert_eq!(result["timed_out"], true);
    assert_eq!(result["thread"], thread);
}

#[test]
fn local_blocking_ask_accepts_only_the_questions_addressee() {
    let bus = Bus::with_limit(Duration::from_secs(12));
    bus.init();
    bus.register("asker", "claude", None);
    bus.register("answerer", "claude", Some("asker"));
    bus.register("impostor", "codex", Some("asker"));
    let mut asker = bus.spawn(&[
        "ask",
        "--from",
        "asker",
        "--to",
        "answerer",
        "--body",
        "participants matter",
        "--wait",
        "8",
        "--default",
        "decline",
    ]);
    eventually(&bus, "question committed", || {
        count(&bus, "SELECT count(*) FROM messages WHERE needs_reply=1") == 1
    });
    let (question, thread): (String, String) = db(&bus)
        .query_row(
            "SELECT id,thread FROM messages WHERE needs_reply=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    bus.ok(&[
        "send",
        "--from",
        "impostor",
        "--to",
        "asker",
        "--kind",
        "answer",
        "--body",
        "not the answerer",
        "--thread",
        &thread,
        "--ref",
        &question,
    ]);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        asker.0.try_wait().unwrap().is_none(),
        "SEC-3: a different local participant satisfied ask"
    );
    let started = Instant::now();
    bus.ok(&[
        "reply",
        &question,
        "--from",
        "answerer",
        "--body",
        "trusted local answer",
    ]);
    let output = asker.finish(Duration::from_secs(1).saturating_sub(started.elapsed()));
    assert!(output.status.success());
    let result = parse_json(&output.stdout);
    assert_eq!(result["body"], "trusted local answer");
    assert_eq!(result["timed_out"], false);
    assert_eq!(result["thread"], thread);
}

fn probe_to_b() -> (Pair, Probe, Value) {
    let (mut pair, _, target) = shared(60);
    let id = queued(&pair.a, "agent-a", &target, "sync", "probe control");
    received(&pair.b, &id);
    outcome(&pair.a, &id, "accepted");
    let template = frame(&pair.a, &pair.pa, envelope(&pair.a, &id));
    let probe = Probe::replace(&pair.a, &mut pair.sa, &pair.pa);
    (pair, probe, template)
}

#[test]
fn extreme_envelope_timestamps_are_bad_time_without_panicking_receiver() {
    let (mut pair, probe, mut message) = probe_to_b();
    message["message_id"] = json!("00000000-0000-4000-8000-000000000002");
    message["body"] = json!("overflow must not arrive");
    message["created_at"] = json!(i64::MIN);
    message["expires_at"] = json!(i64::MAX);
    assert_eq!(
        probe.request(&message),
        json!({"type":"ack","status":"rejected","reason":"bad_time"})
    );
    assert_eq!(
        count(
            &pair.b,
            "SELECT count(*) FROM messages WHERE body='overflow must not arrive'"
        ),
        0
    );
    assert!(pair.sb.process.0.try_wait().unwrap().is_none());
    assert_eq!(
        pair.sb
            .request(&pair.b, "GET", "/api/health", &[], "")
            .status,
        200
    );
    message["message_id"] = json!("00000000-0000-4000-8000-000000000003");
    message["body"] = json!("receiver still accepts valid traffic");
    message["created_at"] = json!(now_ms());
    message["expires_at"] = json!(now_ms() + 60_000);
    assert_eq!(
        probe.request(&message),
        json!({"type":"ack","status":"accepted"})
    );
    received(&pair.b, message["message_id"].as_str().unwrap());
}

#[test]
fn over_audit_limit_rejections_increment_health_counter_without_more_audit_rows() {
    let (pair, probe, mut message) = probe_to_b();
    let initial = peer(&pair.b, &pair.sb, &pair.pb)["counters"]["rejected"]
        .as_u64()
        .unwrap();
    let initial_audit = count(
        &pair.b,
        "SELECT count(*) FROM fed_audit WHERE direction='in' AND decision='rejected'",
    );
    message["created_at"] = json!(now_ms() + 300_000);
    message["expires_at"] = json!(now_ms() + 360_000);
    for number in 1..=12 {
        message["message_id"] = json!(format!("00000000-0000-4000-8000-{number:012x}"));
        assert_eq!(
            probe.request(&message),
            json!({"type":"ack","status":"rejected","reason":"bad_time"})
        );
        assert_eq!(
            peer(&pair.b, &pair.sb, &pair.pb)["counters"]["rejected"],
            initial + number,
            "C3: rejection {number} must be counted even after audit quota is exhausted"
        );
        assert_eq!(
            count(
                &pair.b,
                "SELECT count(*) FROM fed_audit WHERE direction='in' AND decision='rejected'"
            ),
            initial_audit + (number as i64).min(10)
        );
    }
    assert_eq!(
        count(&pair.b, "SELECT count(*) FROM fed_inbox"),
        1,
        "only the valid control message arrived"
    );
}
