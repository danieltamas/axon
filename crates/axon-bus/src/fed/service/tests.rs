use std::sync::Once;

use super::*;

/// The §0 seams: no relay, no discovery, loopback only. No test reaches the internet.
fn loopback_only() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::env::set_var("AXON_FED_RELAY", "disabled");
        std::env::set_var("AXON_FED_BIND", "127.0.0.1:0");
    });
}

struct Node {
    dir: tempfile::TempDir,
    db: PathBuf,
}

impl Node {
    fn new() -> Self {
        loopback_only();
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("axon.db");
        store::init(&db).unwrap();
        let node = Node { dir, db };
        super::super::enable(node.dir.path(), &node.conn(), true).unwrap();
        node
    }

    fn conn(&self) -> rusqlite::Connection {
        store::open(&self.db).unwrap()
    }

    fn id(&self) -> EndpointId {
        identity::read_key(&identity::fed_dir(self.dir.path()).join(identity::KEY_FILE))
            .unwrap()
            .unwrap()
            .public()
    }

    fn add_peer(&self, other: &Node, state: &str, generation: i64) {
        self.conn()
            .execute(
                "INSERT INTO peers (peer_id,node_id,label,generation,state,paired_at)
                 VALUES (?1,?2,'other',?3,?4,1)",
                rusqlite::params![
                    format!("p-{}", other.id().fmt_short()),
                    other.id().to_string(),
                    generation,
                    state
                ],
            )
            .unwrap();
    }

    async fn start(&self) -> Handle {
        start(self.dir.path(), &self.db)
            .await
            .expect("federation starts")
    }
}

async fn connected_pair() -> (Node, Handle, Node, Handle) {
    let (a, b) = (Node::new(), Node::new());
    a.add_peer(&b, "active", 1);
    b.add_peer(&a, "active", 1);
    let (ha, hb) = (a.start().await, b.start().await);
    ha.add_addr(hb.addr());
    hb.add_addr(ha.addr());
    for handle in [&ha, &hb] {
        wait_for(handle, |peer| {
            peer["state"] == "connected" && !peer["rtt_ms"].is_null()
        })
        .await;
    }
    (a, ha, b, hb)
}

async fn wait_for(handle: &Handle, ready: impl Fn(&Value) -> bool) {
    for _ in 0..150 {
        if handle.health()["peers"]
            .as_array()
            .unwrap()
            .iter()
            .any(&ready)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("peer never became ready: {}", handle.health());
}

#[tokio::test]
async fn two_nodes_on_loopback_ping_and_report_health() {
    let (_a, ha, b, hb) = connected_pair().await;
    let peer = &ha.health()["peers"][0];
    assert_eq!(peer["path"], "direct");
    assert_eq!(peer["last_error"], Value::Null);
    assert!(peer["heartbeat_age_ms"].as_i64().unwrap() < 10_000);
    assert_eq!(
        ha.health()["fingerprint"],
        identity::fingerprint(&ha.shared.endpoint.id())
    );
    assert_eq!(
        hb.health()["peers"][0]["fingerprint"],
        identity::fingerprint(&ha.shared.endpoint.id())
    );
    let _ = b;
    ha.shutdown().await;
    hb.shutdown().await;
}

#[tokio::test]
async fn a_revocation_waits_for_the_write_of_a_frame_already_cleared() {
    let (_a, ha, b, hb) = connected_pair().await;
    let transmitting = ha.transmit_gate().read_owned().await;
    let gate = ha.transmit_gate();
    let revoked = tokio::spawn(async move { drop(gate.write_owned().await) });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!revoked.is_finished(), "revoked while a frame was cleared");
    let frame = json!({"type": "ping", "v": 1, "generation": 1, "t": 0});
    ha.request_then(&b.id(), &frame, move || drop(transmitting))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), revoked)
        .await
        .expect("the revocation proceeds once the frame is written")
        .unwrap();
    ha.shutdown().await;
    hb.shutdown().await;
}

#[tokio::test]
async fn a_pairing_paused_on_both_sides_and_resumed_on_both_connects_again() {
    let (a, ha, b, hb) = connected_pair().await;
    crate::fed::pairing::install(&ha, &a.db);
    crate::fed::pairing::install(&hb, &b.db);
    // The state both sides end in when each resume notice was refused by a still-paused peer:
    // active, each believing the other paused (sequence 1) and holding its own resume (2).
    for node in [&a, &b] {
        node.conn()
            .execute(
                "UPDATE peers SET remote_paused=1, remote_lifecycle_seq=1, lifecycle_seq=2",
                [],
            )
            .unwrap();
    }
    ha.reload();
    hb.reload();
    let cleared = |node: &Node| {
        !node
            .conn()
            .query_row::<bool, _, _>("SELECT remote_paused FROM peers", [], |r| r.get(0))
            .unwrap()
    };
    for _ in 0..200 {
        if cleared(&a) && cleared(&b) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(cleared(&a) && cleared(&b), "a pause outlived both resumes");
    for handle in [&ha, &hb] {
        wait_for(handle, |peer| peer["state"] == "connected").await;
    }
    ha.shutdown().await;
    hb.shutdown().await;
}

#[tokio::test]
async fn a_peer_that_paused_us_is_not_redialed_and_is_not_a_fault() {
    let (a, ha, b, hb) = connected_pair().await;
    a.conn()
        .execute("UPDATE peers SET state='paused'", [])
        .unwrap();
    ha.reload();
    b.conn()
        .execute("UPDATE peers SET remote_paused=1", [])
        .unwrap();
    hb.reload();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let peer = &hb.health()["peers"][0];
    assert_eq!(peer["state"], "offline");
    assert_eq!(peer["last_error"], Value::Null, "{peer}");
    assert_eq!(peer["next_retry_at"], Value::Null);
    b.conn()
        .execute("UPDATE peers SET remote_paused=0", [])
        .unwrap();
    a.conn()
        .execute("UPDATE peers SET state='active'", [])
        .unwrap();
    ha.reload();
    hb.reload();
    wait_for(&hb, |peer| peer["state"] == "connected").await;
    ha.shutdown().await;
    hb.shutdown().await;
}

#[tokio::test]
async fn frames_route_by_type_and_a_wrong_generation_is_refused() {
    let (_a, ha, b, hb) = connected_pair().await;
    hb.on_frame(
        "echo",
        Arc::new(|_node, request| {
            Box::pin(async move { json!({"type": "echoed", "got": request["n"]}) })
        }),
    );
    let to_b = b.id();
    assert_eq!(
        ha.request(
            &to_b,
            &json!({"type": "echo", "v": 1, "generation": 1, "n": 7})
        )
        .await
        .unwrap(),
        json!({"type": "echoed", "got": 7})
    );
    assert_eq!(
        ha.request(&to_b, &json!({"type": "nope", "v": 1, "generation": 1}))
            .await
            .unwrap(),
        json!({"type": "error", "reason": "unknown_frame"})
    );
    assert_eq!(
        ha.request(
            &to_b,
            &json!({"type": "ping", "v": 1, "generation": 9, "t": 0})
        )
        .await
        .unwrap()["reason"],
        "stale_generation"
    );
    ha.shutdown().await;
    hb.shutdown().await;
}

#[tokio::test]
async fn a_node_that_is_not_a_peer_is_closed_before_it_can_send() {
    let (_a, ha, _b, hb) = connected_pair().await;
    let stranger = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .clear_ip_transports()
        .bind_addr("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .unwrap()
        .bind()
        .await
        .unwrap();
    let outcome = match stranger.connect(ha.addr(), FED_ALPN).await {
        Ok(conn) => {
            transport::request(
                &conn,
                &json!({"type": "ping", "v": 1, "generation": 1, "t": 0}),
                || {},
            )
            .await
        }
        Err(err) => Err(err.into()),
    };
    assert!(outcome.is_err(), "a stranger got an answer: {outcome:?}");
    ha.shutdown().await;
    hb.shutdown().await;
}

#[tokio::test]
async fn off_means_no_key_no_endpoint_no_lock() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("axon.db");
    store::init(&db).unwrap();
    assert!(start(dir.path(), &db).await.is_none());
    assert!(!identity::fed_dir(dir.path()).exists());
}

#[tokio::test]
async fn a_second_service_on_one_data_dir_does_not_start_until_the_first_stops() {
    let node = Node::new();
    let first = node.start().await;
    assert!(start(node.dir.path(), &node.db).await.is_none());
    first.shutdown().await;
    let again = node.start().await;
    again.shutdown().await;
}

#[tokio::test]
async fn a_lost_key_next_to_peers_refuses_to_start_and_marks_the_peer() {
    let (a, b) = (Node::new(), Node::new());
    a.add_peer(&b, "active", 1);
    std::fs::remove_file(identity::fed_dir(a.dir.path()).join(identity::KEY_FILE)).unwrap();
    assert!(start(a.dir.path(), &a.db).await.is_none());
    let error: String = a
        .conn()
        .query_row("SELECT last_error FROM peers", [], |r| r.get(0))
        .unwrap();
    assert_eq!(error, identity::KEY_LOST);
    assert!(!identity::fed_dir(a.dir.path())
        .join(identity::KEY_FILE)
        .exists());
}
