//! R2-2/R2-3: revocation of old streams and isolation of lifecycle probe connections.
mod common;
use common::delivery::*;
use common::fed::*;
use common::now_ms;
use common::wire_probe::{frame, Probe};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

fn no_success(reply: Option<Value>, action: &str) {
    if let Some(reply) = reply {
        assert!(
            reply["type"] == "error" || (reply["type"] == "ack" && reply["status"] == "rejected"),
            "R2-2: old partial request succeeded after {action} returned: {reply}"
        );
    }
}

fn revoke_older_partial_requests(action: &str) {
    let (mut pair, _, target) = shared(75);
    let original = queued(
        &pair.a,
        "agent-a",
        &target,
        "sync",
        "capture a genuine wire envelope",
    );
    received(&pair.b, &original);
    outcome(&pair.a, &original, "accepted");
    let mut message = frame(&pair.a, &pair.pa, envelope(&pair.a, &original));
    let probe = Probe::replace(&pair.a, &mut pair.sa, &pair.pa);
    let _responder = probe.observe(message["generation"].as_i64().unwrap());
    let older = probe.initial_connection();
    message["message_id"] = json!("00000000-0000-4000-8302-000000000001");
    message["body"] = json!("positive control before revocation");
    message["created_at"] = json!(now_ms());
    message["expires_at"] = json!(now_ms() + 3_600_000);
    assert_eq!(
        older.request(&message),
        json!({"type":"ack","status":"accepted"})
    );
    received(&pair.b, message["message_id"].as_str().unwrap());

    message["message_id"] = json!("00000000-0000-4000-8302-000000000002");
    message["body"] = json!("must never finish admission after revocation");
    let ping = json!({"type":"ping","v":1,"generation":message["generation"],"t":42});
    // The declared frame length and all but its final byte arrive before revocation.
    let partial_ping = older.partial(&ping);
    let partial_message = older.partial(&message);
    assert_eq!(older.request(&ping), json!({"type":"pong","t":42}));
    let newer = probe.another_connection();
    assert_ne!(older.id(), newer.id());
    assert_eq!(newer.request(&ping), json!({"type":"pong","t":42}));
    let (method, path, state) = match action {
        "pause" => (
            "POST",
            format!("/api/fed/peers/{}/pause", pair.pb),
            "paused",
        ),
        "remove" => ("DELETE", format!("/api/fed/peers/{}", pair.pb), "removed"),
        _ => unreachable!(),
    };
    api(&pair.b, &pair.sb, method, &path, json!({}));
    assert_eq!(
        text(
            &pair.b,
            "SELECT state FROM peers WHERE peer_id=?1",
            &pair.pb
        ),
        state
    );
    // Complete both already-open requests only after the owner acknowledges revocation.
    std::thread::scope(|scope| {
        let ping_reply = scope.spawn(|| partial_ping.finish());
        let message_reply = scope.spawn(|| partial_message.finish());
        no_success(ping_reply.join().unwrap(), action);
        no_success(message_reply.join().unwrap(), action);
    });
    assert_eq!(
        db(&pair.b)
            .query_row(
                "SELECT count(*) FROM fed_inbox WHERE message_id=?1",
                [message["message_id"].as_str().unwrap()],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
        0,
        "revoked partial message must not be committed"
    );
    assert_eq!(
        pair.sb
            .request(&pair.b, "GET", "/api/health", &[], "")
            .status,
        200
    );
}

#[test]
fn older_partial_ping_and_message_cannot_succeed_after_pause_returns() {
    revoke_older_partial_requests("pause");
}

#[test]
fn older_partial_ping_and_message_cannot_succeed_after_remove_returns() {
    revoke_older_partial_requests("remove");
}

#[test]
fn remote_pause_reconciliation_never_sends_queued_messages_on_its_control_probe() {
    let (mut pair, _, target) = shared(90);
    let generation = db(&pair.b)
        .query_row(
            "SELECT generation FROM peers WHERE peer_id=?1",
            [&pair.pb],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    let probe = Probe::replace(&pair.b, &mut pair.sb, &pair.pb);
    let observer = probe.observe(generation);
    let mut queued_ids = Vec::new();
    for number in 0..8 {
        queued_ids.push(queued(
            &pair.a,
            "agent-a",
            &target,
            "sync",
            &format!("R2-3 pending {number}"),
        ));
    }
    eventually(
        &pair.a,
        "queued traffic reaches the observer before remote pause",
        || !observer.messages().is_empty(),
    );
    assert!(
        observer.messages().len() < queued_ids.len(),
        "fixture needs an unsent backlog"
    );
    let previous_seq = db(&pair.b)
        .query_row(
            "SELECT lifecycle_seq FROM peers WHERE peer_id=?1",
            [&pair.pb],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    let paused_seq = previous_seq + 1;
    observer.lifecycle(true, paused_seq, true);
    let paused = probe.request(&json!({
        "type":"notice", "v":1, "generation":generation, "what":"paused", "seq":paused_seq
    }));
    assert_eq!(paused["type"], "ack");
    assert_eq!(paused["status"], "accepted");
    eventually(&pair.a, "sender remembers remote pause", || {
        peer(&pair.a, &pair.sa, &pair.pa)["remote_paused"] == true
    });
    let original_connection = probe.initial_connection();
    let paused_at = Instant::now();
    observer.close_connections();
    original_connection.close();
    assert_eq!(
        count(
            &pair.a,
            "SELECT count(*) FROM fed_outbox WHERE state='queued'"
        ),
        8
    );

    // section 12: a remotely paused peer is probed every 5 s to trade lifecycle state.
    // Hold that response so the connection can be positively identified before resuming.
    let deadline = Instant::now() + Duration::from_secs(15);
    let control_connection = loop {
        if let Some(event) = observer.frames().iter().find(|event| {
            event.arrived >= paused_at
                && event.connection != original_connection.id()
                && event.frame["type"] == "notice"
                && event.frame["what"] == "state"
                && observer
                    .connections()
                    .iter()
                    .any(|(opened, id)| *opened >= paused_at && *id == event.connection)
        }) {
            break event.connection;
        }
        assert!(
            Instant::now() < deadline,
            "R2-3 fixture never observed a remote-pause control probe"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(peer(&pair.a, &pair.sa, &pair.pa)["remote_paused"], true);
    // A valid newer lifecycle reply unblocks the queue. Delivery must use a fresh traffic path.
    observer.lifecycle(false, paused_seq + 1, false);
    observer.release_acks();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut delivered_elsewhere = false;
    loop {
        for event in observer.frames() {
            if event.frame["type"] != "msg" {
                continue;
            }
            assert_ne!(
                event.connection, control_connection,
                "R2-3: queued application frame used a control probe: {:?}",
                event.frame
            );
            if event.arrived >= paused_at && event.connection != original_connection.id() {
                assert!(queued_ids.iter().any(|id| event.frame["message_id"] == *id));
                delivered_elsewhere = true;
            }
        }
        if delivered_elsewhere
            && peer(&pair.a, &pair.sa, &pair.pa)["remote_paused"] == false
            && count(
                &pair.a,
                "SELECT count(*) FROM fed_outbox WHERE state='accepted'",
            ) == 8
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "R2-3: lifecycle must recover and deliver over a separate connection"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
