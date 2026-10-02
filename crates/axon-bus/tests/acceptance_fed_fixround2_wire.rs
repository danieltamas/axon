//! RR-7/RR-11: retention capacity and admission limits at the authenticated wire boundary.
mod common;
use common::delivery::*;
use common::fed::*;
use common::wire_probe::{frame, Probe};
use common::*;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

fn flood(kind: &str) {
    let mut pair = Pair::paired(Duration::from_secs(60));
    let generation: i64 = db(&pair.a)
        .query_row(
            "SELECT generation FROM peers WHERE peer_id=?1",
            [&pair.pa],
            |r| r.get(0),
        )
        .unwrap();
    let probe = Probe::replace(&pair.a, &mut pair.sa, &pair.pa);
    let mut request = json!({"type":kind,"v":1,"generation":generation});
    if kind == "ping" {
        request["t"] = json!(now_ms());
    }
    let ordinary = probe.request(&request);
    if kind == "ping" {
        assert_eq!(ordinary, json!({"type":"pong","t":request["t"]}));
    } else {
        assert_eq!(ordinary, json!({"type":"error","reason":"unknown_frame"}));
    }
    let before = peer(&pair.b, &pair.sb, &pair.pb)["counters"]["rejected"]
        .as_u64()
        .unwrap();
    let started = Instant::now();
    let responses = probe.burst(&request, 100);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(responses.len(), 100);
    let limited = responses
        .iter()
        .filter(|response| **response == json!({"type":"error","reason":"rate_limited"}))
        .count();
    assert!(
        limited > 0,
        "RR-11: {kind} bypassed admission for 100 frames in under 3 s"
    );
    for response in responses {
        assert!(
            response == ordinary || response == json!({"type":"error","reason":"rate_limited"}),
            "unexpected flood response: {response}"
        );
    }
    eventually(&pair.b, "rate rejections visible in health", || {
        peer(&pair.b, &pair.sb, &pair.pb)["counters"]["rejected"]
            .as_u64()
            .unwrap()
            >= before + limited as u64
    });
    assert_eq!(count(&pair.b, "SELECT count(*) FROM fed_inbox"), 0);
    assert!(pair.sb.process.0.try_wait().unwrap().is_none());
    assert_eq!(
        pair.sb
            .request(&pair.b, "GET", "/api/health", &[], "")
            .status,
        200
    );
}

#[test]
fn ping_flood_is_rate_limited_and_rejections_are_counted() {
    flood("ping");
}

#[test]
fn unknown_frame_flood_is_rate_limited_and_rejections_are_counted() {
    flood("rr2_unknown");
}

#[test]
fn retention_of_undelivered_inbox_provenance_does_not_steal_the_hundredth_slot() {
    const OFFSET: i64 = 91 * 24 * 60 * 60 * 1000;
    let (mut pair, _, target) = shared(180);
    let old = queued(
        &pair.a,
        "agent-a",
        &target,
        "sync",
        "undelivered before retention",
    );
    let old_local = received(&pair.b, &old);
    outcome(&pair.a, &old, "accepted");
    let mut message: Value = frame(&pair.a, &pair.pa, envelope(&pair.a, &old));
    assert_eq!(
        db(&pair.b)
            .query_row(
                "SELECT count(*) FROM messages WHERE id=?1 AND delivered_at IS NULL",
                [&old_local],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    pair.sb.process.0.kill().unwrap();
    pair.sb.process.0.wait().unwrap();
    // Startup runs the retention sweep. No hook is called to clean up the expired message.
    pair.sb = Server::shifted(&pair.b, true, OFFSET);
    eventually(
        &pair.b,
        "91-day-old inbox provenance has been swept",
        || {
            db(&pair.b)
                .query_row(
                    "SELECT count(*) FROM fed_inbox WHERE message_id=?1",
                    [&old],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
                == 0
        },
    );
    pair.connected();
    let probe = Probe::replace(&pair.a, &mut pair.sa, &pair.pa);
    let _control_responder = probe.observe(message["generation"].as_i64().unwrap());
    for number in 1..=100 {
        message["message_id"] = json!(format!("00000000-0000-4000-8001-{number:012x}"));
        message["body"] = json!(format!("fresh pending {number}"));
        message["created_at"] = json!(now_ms() + OFFSET);
        message["expires_at"] = json!(now_ms() + OFFSET + 3_600_000);
        assert_eq!(
            probe.request(&message),
            json!({"type":"ack","status":"accepted"}),
            "RR-7: fresh message {number} must fit after retention"
        );
        // section 8: isolate recipient capacity from the independent 2/s token bucket.
        std::thread::sleep(Duration::from_millis(510));
    }
    assert_eq!(count(&pair.b, "SELECT count(*) FROM messages WHERE to_id='agent-b' AND from_id LIKE 'peer:%' AND delivered_at IS NULL"), 100);
    assert_eq!(
        db(&pair.b)
            .query_row(
                "SELECT count(*) FROM messages WHERE id=?1",
                [&old_local],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    message["message_id"] = json!("00000000-0000-4000-8001-000000000101");
    message["body"] = json!("over capacity");
    assert_eq!(
        probe.request(&message),
        json!({"type":"ack","status":"rejected","reason":"recipient_full"}),
        "capacity enforcement must still reject the 101st live message"
    );
}
