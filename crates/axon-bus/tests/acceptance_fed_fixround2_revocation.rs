//! RR-1: observe transmitted frames, including retries, after owner revocation returns.
mod common;
use common::delivery::*;
use common::fed::*;
use common::wire_probe::Probe;
use serde_json::json;
use std::sync::Barrier;
use std::time::{Duration, Instant};

fn revoke_busy_outbox(action: &str) {
    let (mut pair, share, target) = shared(75);
    let generation: i64 = db(&pair.b)
        .query_row(
            "SELECT generation FROM peers WHERE peer_id=?1",
            [&pair.pb],
            |r| r.get(0),
        )
        .unwrap();
    let probe = Probe::replace(&pair.b, &mut pair.sb, &pair.pb);
    let observer = probe.observe(generation);
    let mut queued_ids = Vec::new();
    for number in 0..12 {
        queued_ids.push(queued(
            &pair.a,
            "agent-a",
            &target,
            "sync",
            &format!("RR-1 {action} backlog {number}"),
        ));
    }
    eventually(
        &pair.a,
        "positive control: a queued message reaches the wire probe",
        || !observer.messages().is_empty(),
    );
    let before = observer.messages();
    assert!(before
        .iter()
        .all(|(_, frame)| queued_ids.iter().any(|id| frame["message_id"] == *id)));
    assert!(
        before.len() < queued_ids.len(),
        "fixture must retain unsent backlog while the first ack is held"
    );
    assert_eq!(
        count(
            &pair.a,
            "SELECT count(*) FROM fed_outbox WHERE state='queued'"
        ),
        12
    );

    let (method, path, body) = match action {
        "pause" => (
            "POST",
            format!("/api/fed/peers/{}/pause", pair.pa),
            json!({}),
        ),
        "remove" => ("DELETE", format!("/api/fed/peers/{}", pair.pa), json!({})),
        "unshare" => ("DELETE", format!("/api/fed/shares/{share}"), json!({})),
        "outbound-off" => (
            "PUT",
            format!("/api/fed/shares/{share}"),
            json!({"inbound":true,"outbound":false}),
        ),
        _ => unreachable!(),
    };
    // Race the busy sender against the API. Keeping the ack held until *after* return
    // would hide a clearance/transmit race by forcing every next send to recheck later.
    let barrier = Barrier::new(2);
    let returned = std::thread::scope(|scope| {
        let revocation = scope.spawn(|| {
            barrier.wait();
            api(&pair.a, &pair.sa, method, &path, body);
            Instant::now()
        });
        barrier.wait();
        observer.release_acks();
        revocation.join().unwrap()
    });
    // Exceeds the section 12 dial-backoff cap, covering a subsequent send/retry opportunity.
    while returned.elapsed() < Duration::from_secs(12) {
        for (arrived, frame) in observer.messages() {
            assert!(
                arrived <= returned,
                "RR-1: {action} returned before message frame reached the peer: {frame}"
            );
        }
        assert!(pair.sa.process.0.try_wait().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(20));
    }
    for (arrived, frame) in observer.messages() {
        assert!(
            arrived <= returned,
            "RR-1: late message after {action}: {frame}"
        );
    }
    assert_eq!(
        pair.sa
            .request(&pair.a, "GET", "/api/health", &[], "")
            .status,
        200
    );
}

#[test]
fn pause_return_is_a_wire_barrier_for_a_busy_outbox() {
    revoke_busy_outbox("pause");
}

#[test]
fn remove_return_is_a_wire_barrier_for_a_busy_outbox() {
    revoke_busy_outbox("remove");
}

#[test]
fn unshare_return_is_a_wire_barrier_for_a_busy_outbox() {
    revoke_busy_outbox("unshare");
}

#[test]
fn outbound_off_return_is_a_wire_barrier_for_a_busy_outbox() {
    revoke_busy_outbox("outbound-off");
}
