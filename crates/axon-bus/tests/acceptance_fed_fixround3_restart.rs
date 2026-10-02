//! R2-1: the owner wire barrier must survive a concurrent settings-driven service replacement.
mod common;
use common::delivery::*;
use common::fed::*;
use common::wire_probe::Probe;
use serde_json::json;
use std::sync::Barrier;
use std::time::{Duration, Instant};

#[test]
fn relay_change_racing_pause_keeps_the_busy_outbox_wire_barrier() {
    // Repeat a real scheduler race; this is not exhaustive proof of gate ownership.
    for attempt in 0..3 {
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
        for number in 0..16 {
            queued_ids.push(queued(
                &pair.a,
                "agent-a",
                &target,
                "sync",
                &format!("R2-1 restart race {attempt} message {number}"),
            ));
        }
        eventually(
            &pair.a,
            "busy outbox has reached the peer with a held message ack",
            || !observer.messages().is_empty(),
        );
        assert!(
            observer.messages().len() < queued_ids.len(),
            "unsent backlog required for restart race"
        );
        assert_eq!(
            count(
                &pair.a,
                "SELECT count(*) FROM fed_outbox WHERE state='queued'"
            ),
            16
        );
        assert_eq!(health(&pair.a, &pair.sa)["relay"], "default");
        // The process-level AXON_FED_RELAY=disabled seam keeps this settings change offline.
        let relay = "https://127.0.0.1:9443";
        let barrier = Barrier::new(3);
        let returned = std::thread::scope(|scope| {
            let restart = scope.spawn(|| {
                barrier.wait();
                api(
                    &pair.a,
                    &pair.sa,
                    "PUT",
                    "/api/settings/federation",
                    json!({"relay":relay}),
                )
            });
            let pause = scope.spawn(|| {
                barrier.wait();
                api(
                    &pair.a,
                    &pair.sa,
                    "POST",
                    &format!("/api/fed/peers/{}/pause", pair.pa),
                    json!({}),
                );
                Instant::now()
            });
            barrier.wait();
            observer.release_acks();
            let returned = pause.join().unwrap();
            let settings = restart.join().unwrap();
            assert_eq!(settings["federation"]["relay"], relay);
            returned
        });
        assert_eq!(
            text(
                &pair.a,
                "SELECT state FROM peers WHERE peer_id=?1",
                &pair.pa
            ),
            "paused"
        );
        assert_eq!(health(&pair.a, &pair.sa)["relay"], relay);
        let assert_barrier = || {
            for (arrived, frame) in observer.messages() {
                assert!(queued_ids.iter().any(|id| frame["message_id"] == *id));
                assert!(arrived <= returned,
                    "R2-1: settings restart sent after pause returned, attempt {attempt}: {frame}");
            }
        };
        // Includes the documented request timeout and an ensuing outbox retry opportunity.
        while returned.elapsed() < Duration::from_secs(12) {
            assert_barrier();
            assert!(pair.sa.process.0.try_wait().unwrap().is_none());
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_barrier();
        assert_eq!(peer(&pair.a, &pair.sa, &pair.pa)["state"], "paused");
        assert_eq!(
            pair.sa
                .request(&pair.a, "GET", "/api/health", &[], "")
                .status,
            200
        );
    }
}
