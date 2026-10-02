//! Round 3: peer fairness and transactional admission, from the work-order done-when column.
mod common;
use common::delivery::*;
use common::fed::*;
use common::wire_probe::Probe;
use common::{parse_json, Bus, Server};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

fn peer_for_node(bus: &Bus, node: &str) -> String {
    assert_eq!(
        db(bus)
            .query_row(
                "SELECT count(*) FROM peers WHERE node_id=?1 AND state<>'removed'",
                [node],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
        1,
        "a successful join must create exactly one live peer row"
    );
    text(
        bus,
        "SELECT peer_id FROM peers WHERE node_id=?1 AND state<>'removed'",
        node,
    )
}

fn confirm_pair(a: &Bus, sa: &Server, pa: &str, b: &Bus, sb: &Server, pb: &str) {
    let first = peer(a, sa, pa);
    let second = peer(b, sb, pb);
    assert_eq!(first["state"], "pending_confirm");
    assert_eq!(second["state"], "pending_confirm");
    let code = first["pair_code"]
        .as_str()
        .expect("confirmable peer has a pair code");
    assert!(!code.is_empty());
    assert_eq!(second["pair_code"], code);
    for (bus, server, id) in [(a, sa, pa), (b, sb, pb)] {
        api(
            bus,
            server,
            "POST",
            &format!("/api/fed/peers/{id}/confirm"),
            json!({"pair_code":code}),
        );
    }
    for (bus, server, id) in [(a, sa, pa), (b, sb, pb)] {
        eventually(bus, "new pairing is connected", || {
            peer(bus, server, id)["state"] == "connected"
        });
    }
}

fn enable(bus: &Bus, server: &Server) -> Value {
    api(
        bus,
        server,
        "PUT",
        "/api/settings/federation",
        json!({"enabled":true}),
    );
    health(bus, server)
}

#[test]
fn heartbeat_responsive_peer_with_twenty_four_stalled_messages_cannot_starve_healthy_peer(
) {
    let (mut pair, _, target_x) = shared(100);
    let y = Bus::with_limit(Duration::from_secs(100));
    y.init();
    let repo_y = repo(&y, "project-y");
    y.register_at("agent-y", "codex", None, &repo_y);
    let sy = Server::start(&y, true);
    let identity_y = enable(&y, &sy);
    let invite = api(&pair.a, &pair.sa, "POST", "/api/fed/invites", json!({}));
    api(
        &y,
        &sy,
        "POST",
        "/api/fed/join",
        json!({"invite":invite["invite"],"label":"sender"}),
    );
    let ay = peer_for_node(&pair.a, identity_y["node_id"].as_str().unwrap());
    let ya = live_id(&y);
    confirm_pair(&pair.a, &pair.sa, &ay, &y, &sy, &ya);
    api(
        &pair.a,
        &pair.sa,
        "POST",
        &format!("/api/fed/peers/{ay}/shares"),
        json!({"local_repo":pair.ra,"label":"healthy","inbound":true,"outbound":true}),
    );
    let share = text(
        &pair.a,
        "SELECT share_id FROM peer_shares WHERE peer_id=?1 AND state='offered_out'",
        &ay,
    );
    eventually(&y, "healthy peer receives share offer", || {
        count(
            &y,
            "SELECT count(*) FROM peer_shares WHERE state='offered_in'",
        ) == 1
    });
    api(
        &y,
        &sy,
        "POST",
        &format!("/api/fed/shares/{share}/accept"),
        json!({"local_repo":repo_y,"inbound":true,"outbound":true}),
    );
    eventually(&pair.a, "healthy peer discovery is ready", || {
        db(&pair.a)
            .query_row(
                "SELECT count(*) FROM fed_remote_sessions WHERE peer_id=?1",
                [&ay],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    });
    let label = peer(&pair.a, &pair.sa, &ay)["label"]
        .as_str()
        .unwrap()
        .to_owned();
    let session = text(
        &y,
        "SELECT session FROM fed_sessions WHERE agent_id=?1",
        "agent-y",
    );
    let target_y = format!("peer:{label}/{session}");
    let warmup = queued(
        &pair.a,
        "agent-a",
        &target_y,
        "sync",
        "healthy path before stalled backlog",
    );
    received(&y, &warmup);
    outcome(&pair.a, &warmup, "accepted");

    let generation = db(&pair.b)
        .query_row(
            "SELECT generation FROM peers WHERE peer_id=?1",
            [&pair.pb],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    let probe = Probe::replace(&pair.b, &mut pair.sb, &pair.pb);
    let observer = probe.observe(generation);
    let mut backlog = Vec::new();
    for number in 0..24 {
        backlog.push(queued_extra(
            &pair.a,
            "agent-a",
            &target_x,
            "sync",
            &format!("R2-4 older stalled message {number}"),
            &[],
            -60_000,
        ));
    }
    eventually(
        &pair.a,
        "stalled peer receives a message but holds its ack",
        || !observer.messages().is_empty(),
    );
    let first_message = observer.messages()[0].0;
    eventually(&pair.a, "stalled peer still answers heartbeats", || {
        observer.pongs().iter().any(|sent| *sent >= first_message)
    });
    assert_eq!(
        db(&pair.a)
            .query_row(
                "SELECT count(*) FROM fed_outbox WHERE peer_id=?1 AND state='queued'",
                [&pair.pa],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
        24
    );

    let started = Instant::now();
    let wanted = queued(
        &pair.a,
        "agent-a",
        &target_y,
        "sync",
        "R2-4 healthy peer must not starve",
    );
    loop {
        let delivered = db(&y)
            .query_row(
                "SELECT count(*) FROM fed_inbox WHERE message_id=?1",
                [&wanted],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            == 1;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "R2-4: healthy peer missed its 5 s delivery bound behind 24 stalled messages"
        );
        if delivered {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(db(&pair.a).query_row(
        "SELECT count(*) FROM fed_outbox WHERE peer_id=?1 AND state='queued' AND created_at < (SELECT created_at FROM fed_outbox WHERE message_id=?2)",
        [&pair.pa, &wanted], |r| r.get::<_, i64>(0),
    ).unwrap(), 24, "all stalled messages must remain pending and older than Y's message");
    assert!(observer
        .messages()
        .iter()
        .all(|(_, frame)| backlog.iter().any(|id| frame["message_id"] == *id)));
    let local = received(&y, &wanted);
    assert_eq!(
        text(&y, "SELECT body FROM messages WHERE id=?1", &local),
        "R2-4 healthy peer must not starve"
    );
}

#[test]
fn neutral_label_collision_creates_a_confirmable_peer_or_preserves_the_invite() {
    let pair = Pair::paired(Duration::from_secs(90));
    let joining = Bus::with_limit(Duration::from_secs(90));
    joining.init();
    let sj = Server::start(&joining, true);
    let identity = enable(&joining, &sj);
    let node = identity["node_id"].as_str().unwrap();
    let fingerprint = identity["fingerprint"].as_str().unwrap().replace(' ', "");
    assert_eq!(fingerprint.len(), 16);
    assert!(fingerprint.bytes().all(|b| b.is_ascii_hexdigit()));
    let collision = format!("axon-{fingerprint}");
    api(
        &pair.a,
        &pair.sa,
        "PUT",
        &format!("/api/fed/peers/{}/label", pair.pa),
        json!({"label":collision}),
    );
    let incumbent = peer(&pair.a, &pair.sa, &pair.pa);
    assert_eq!(incumbent["label"], collision);
    let invite = api(&pair.a, &pair.sa, "POST", "/api/fed/invites", json!({}));
    let response = request(
        &joining,
        &sj,
        "POST",
        "/api/fed/join",
        json!({"invite":invite["invite"],"label":"inviter"}),
    );
    assert_eq!(
        peer(&pair.a, &pair.sa, &pair.pa)["label"],
        collision,
        "admitting a different node must not rename the existing peer"
    );
    if !(200..300).contains(&response.status) {
        assert!(
            (400..500).contains(&response.status),
            "join must fail cleanly, not with a server error"
        );
        assert!(parse_json(&response.body)["error"].as_str().is_some());
        assert_eq!(
            count(
                &joining,
                "SELECT count(*) FROM peers WHERE state<>'removed'"
            ),
            0
        );
        assert_eq!(
            count(&pair.a, "SELECT count(*) FROM peers WHERE state<>'removed'"),
            1
        );
        let reusable: bool = db(&pair.a).query_row(
            "SELECT consumed_by IS NULL AND cancelled_at IS NULL AND attempts=0 FROM peer_invites WHERE invite_id=?1",
            [invite["invite_id"].as_str().unwrap()], |r| r.get(0),
        ).unwrap();
        assert!(
            reusable,
            "R2-5: failed row insertion must not consume or burn the invitation"
        );
        api(
            &pair.a,
            &pair.sa,
            "PUT",
            &format!("/api/fed/peers/{}/label", pair.pa),
            json!({"label":"incumbent"}),
        );
        // The exact same invitation must work once its label conflict is removed.
        api(
            &joining,
            &sj,
            "POST",
            "/api/fed/join",
            json!({"invite":invite["invite"],"label":"inviter"}),
        );
    }
    let new_peer = peer_for_node(&pair.a, node);
    let remote_peer = live_id(&joining);
    assert_ne!(new_peer, pair.pa);
    assert_eq!(
        count(&pair.a, "SELECT count(*) FROM peers WHERE state<>'removed'"),
        2
    );
    let assigned = peer(&pair.a, &pair.sa, &new_peer)["label"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(assigned, peer(&pair.a, &pair.sa, &pair.pa)["label"]);
    assert_eq!(
        text(
            &pair.a,
            "SELECT consumed_by FROM peer_invites WHERE invite_id=?1",
            invite["invite_id"].as_str().unwrap()
        ),
        node
    );
    confirm_pair(&pair.a, &pair.sa, &new_peer, &joining, &sj, &remote_peer);
}
