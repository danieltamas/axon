//! CDX-11: simultaneous joins to different nodes must arbitrate the live label atomically.
#![cfg(unix)] // SIGSTOP holds both inviters until both join requests are in flight.
mod common;
use common::fed::*;
use common::*;
use serde_json::json;
use std::sync::Barrier;
use std::time::Duration;

#[test]
fn concurrent_joins_with_one_label_have_exactly_one_winner() {
    let a = Bus::with_limit(Duration::from_secs(45));
    let b = Bus::with_limit(Duration::from_secs(45));
    let joining = Bus::with_limit(Duration::from_secs(45));
    for bus in [&a, &b, &joining] {
        bus.init();
    }
    let sa = Server::start(&a, true);
    let sb = Server::start(&b, true);
    let sj = Server::start(&joining, true);
    for (bus, server) in [(&a, &sa), (&b, &sb), (&joining, &sj)] {
        api(
            bus,
            server,
            "PUT",
            "/api/settings/federation",
            json!({"enabled":true}),
        );
    }
    let first = api(&a, &sa, "POST", "/api/fed/invites", json!({}));
    let second = api(&b, &sb, "POST", "/api/fed/invites", json!({}));
    let stopped_a = Stopped::new(&sa);
    let stopped_b = Stopped::new(&sb);
    let barrier = Barrier::new(3);
    let responses = std::thread::scope(|scope| {
        let join = |invite: &serde_json::Value| {
            barrier.wait();
            request(
                &joining,
                &sj,
                "POST",
                "/api/fed/join",
                json!({"invite":invite,"label":"same-label"}),
            )
        };
        let one = scope.spawn(move || join(&first["invite"]));
        let two = scope.spawn(move || join(&second["invite"]));
        barrier.wait();
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            !one.is_finished() || !two.is_finished(),
            "a successful join must wait on its stopped inviter"
        );
        drop(stopped_a);
        drop(stopped_b);
        [one.join().unwrap(), two.join().unwrap()]
    });
    assert_eq!(
        responses
            .iter()
            .filter(|r| (200..300).contains(&r.status))
            .count(),
        1,
        "CDX-11: exactly one concurrent join may acquire a live label"
    );
    let refused = responses
        .iter()
        .find(|r| !(200..300).contains(&r.status))
        .unwrap();
    assert_eq!(refused.status, 400);
    assert_eq!(parse_json(&refused.body)["error"], "label_taken");
    assert_eq!(
        count(
            &joining,
            "SELECT count(*) FROM peers WHERE label='same-label' AND state<>'removed'"
        ),
        1
    );
}
