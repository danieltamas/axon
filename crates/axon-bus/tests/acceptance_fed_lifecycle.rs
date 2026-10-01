//! P2P-SPEC §§3, 5, 9–10: A18–A20/A26–A28/A31; A32/A34 lifecycle API.
//! A29 other-account access: manual, see spec §11.
//! A30 power loss: manual, see spec §11.
//! Relayed path across two real networks: manual, see spec §11.
//! Unix blackhole cases use SIGSTOP/SIGCONT. Windows skips only those cases because
//! std provides no suspend/resume; kill/restart and all non-signal cases run there.
mod common;
use common::delivery::*;
use common::fed::*;
use common::sse::Events;
use common::*;
use serde_json::json;
use std::time::{Duration, Instant};

#[test]
fn federation_is_off_until_settings_enables_it_and_doctor_reports_the_state() {
    let bus = Bus::with_limit(Duration::from_secs(20));
    bus.init();
    let server = Server::start(&bus, true);
    assert_eq!(health(&bus, &server)["enabled"], false);
    let output = bus.cmd().args(["doctor"]).assert().get_output().clone();
    assert!(String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|l| l == "federation: off"));
    bus.register("local", "claude", None);
    refused(
        &bus,
        "local",
        "peer:nobody/abcdefghijkl",
        "sync",
        "off",
        "federation_off",
    );
    assert!(!bus.root.join("data/axon/fed/identity.key").exists());
    api(
        &bus,
        &server,
        "PUT",
        "/api/settings/federation",
        json!({"enabled":true}),
    );
    let output = bus.cmd().args(["doctor"]).assert().get_output().clone();
    assert!(String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|l| l == "federation: on (0 peers)"));
    refused(
        &bus,
        "local",
        "peer:nobody/abcdefghijkl",
        "sync",
        "unknown",
        "unknown_peer",
    );
}

#[test]
fn pause_survives_both_restarts_and_resume_allows_only_unexpired_pending_delivery() {
    let (pair, _, target) = shared(75);
    let reverse = pair.target(false, "agent-a");
    let id = queued(&pair.a, "agent-a", &target, "sync", "held while paused");
    received(&pair.b, &id);
    api(
        &pair.b,
        &pair.sb,
        "POST",
        &format!("/api/fed/peers/{}/pause", pair.pb),
        json!({}),
    );
    assert_eq!(
        text(
            &pair.b,
            "SELECT state FROM peers WHERE peer_id=?1",
            &pair.pb
        ),
        "paused"
    );
    assert_eq!(peer(&pair.b, &pair.sb, &pair.pb)["state"], "paused");
    assert!(!hook_context(&pair.b, "codex", "agent-b", &pair.rb).contains("held while paused"));
    refused(
        &pair.b,
        "agent-b",
        &reverse,
        "sync",
        "paused sender",
        "peer_paused",
    );
    let Pair {
        a,
        sa,
        b,
        sb,
        pb,
        rb,
        ..
    } = pair;
    drop(sa);
    drop(sb);
    let _restarted_a = Server::start(&a, true);
    let restarted_b = Server::start(&b, true);
    assert_eq!(peer(&b, &restarted_b, &pb)["state"], "paused");
    assert!(!hook_context(&b, "codex", "agent-b", &rb).contains("held while paused"));
    api(
        &b,
        &restarted_b,
        "POST",
        &format!("/api/fed/peers/{pb}/resume"),
        json!({}),
    );
    assert_eq!(
        text(&b, "SELECT state FROM peers WHERE peer_id=?1", &pb),
        "active"
    );
    assert!(hook_context(&b, "codex", "agent-b", &rb).contains("held while paused"));
}

#[cfg(unix)]
#[test]
fn removing_offline_peer_cancels_queues_shares_and_discovery_without_remote_ack() {
    let (pair, _, target) = shared(60);
    let reverse = pair.target(false, "agent-a");
    let inbound = queued(
        &pair.b,
        "agent-b",
        &reverse,
        "sync",
        "never deliver after removal",
    );
    let local = received(&pair.a, &inbound);
    let stopped = Stopped::new(&pair.sb);
    let queued_id = queued(&pair.a, "agent-a", &target, "sync", "cancel this queue");
    let start = Instant::now();
    api(
        &pair.a,
        &pair.sa,
        "DELETE",
        &format!("/api/fed/peers/{}", pair.pa),
        json!({}),
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "removal cannot wait for an offline peer"
    );
    // One read transaction observes the complete post-removal invariant.
    let conn = db(&pair.a);
    conn.execute_batch("BEGIN").unwrap();
    let totals: (i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT (SELECT count(*) FROM peers WHERE state='removed' AND removed_at IS NOT NULL),
        (SELECT count(*) FROM peer_shares WHERE state<>'removed'),
        (SELECT count(*) FROM fed_outbox WHERE state='queued'),
        (SELECT count(*) FROM fed_remote_sessions),
        (SELECT count(*) FROM messages WHERE id=?1)",
            [&local],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(totals, (1, 0, 0, 0, 0));
    conn.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        text(
            &pair.a,
            "SELECT state FROM fed_outbox WHERE message_id=?1",
            &queued_id
        ),
        "cancelled"
    );
    assert!(!hook_context(&pair.a, "claude", "agent-a", &pair.ra)
        .contains("never deliver after removal"));
    refused(
        &pair.a,
        "agent-a",
        &target,
        "sync",
        "removed",
        "peer_removed",
    );
    assert_audit_chain(&pair.a);
    drop(stopped);
}

#[test]
fn re_pairing_same_identity_creates_new_generation_with_zero_old_grants() {
    let (pair, _, target) = shared(75);
    let node_a = health(&pair.a, &pair.sa)["node_id"].clone();
    let generation = count(&pair.a, "SELECT generation FROM peers WHERE state='active'");
    api(
        &pair.a,
        &pair.sa,
        "DELETE",
        &format!("/api/fed/peers/{}", pair.pa),
        json!({}),
    );
    // Removal is local authority; independently remove the other side for re-enrollment.
    api(
        &pair.b,
        &pair.sb,
        "DELETE",
        &format!("/api/fed/peers/{}", pair.pb),
        json!({}),
    );
    let Pair {
        a,
        sa,
        b,
        sb,
        ra,
        rb,
        ..
    } = pair;
    drop(sa);
    drop(sb);
    let sa = Server::start(&a, true);
    let sb = Server::start(&b, true);
    assert_eq!(health(&a, &sa)["node_id"], node_a);
    refused(&a, "agent-a", &target, "sync", "old target", "peer_removed");
    let invitation = api(&a, &sa, "POST", "/api/fed/invites", json!({}));
    api(
        &b,
        &sb,
        "POST",
        "/api/fed/join",
        json!({"invite":invitation["invite"],"label":"alice"}),
    );
    let pa = live_id(&a);
    let pb = live_id(&b);
    let pair = Pair {
        a,
        sa,
        b,
        sb,
        pa,
        pb,
        ra,
        rb,
    };
    pair.confirm();
    let next = count(&pair.a, "SELECT generation FROM peers WHERE state='active'");
    assert_ne!(next, generation);
    for bus in [&pair.a, &pair.b] {
        assert_eq!(
            count(
                bus,
                "SELECT count(*) FROM peer_shares WHERE state<>'removed'"
            ),
            0
        );
        assert_eq!(count(bus, "SELECT count(*) FROM fed_remote_sessions"), 0);
        assert_eq!(
            count(bus, "SELECT count(*) FROM fed_outbox WHERE state='queued'"),
            0
        );
    }
}

#[cfg(unix)]
#[test]
fn blackhole_becomes_offline_and_stale_by_thirty_seconds_while_http_stays_healthy() {
    let pair = Pair::paired(Duration::from_secs(130));
    eventually(&pair.a, "first measured heartbeat RTT", || {
        let current = peer(&pair.a, &pair.sa, &pair.pa);
        current["rtt_ms"].as_f64().is_some_and(|rtt| rtt >= 0.0) && current["rtt_stale"] == false
    });
    let last = peer(&pair.a, &pair.sa, &pair.pa);
    assert_eq!(last["rtt_stale"], false);
    let messages = count(&pair.a, "SELECT count(*) FROM messages");
    let stopped = Stopped::new(&pair.sb);
    let started = Instant::now();
    let mut events = Events::open(&pair.a, &pair.sa);
    loop {
        let value = events.fed(&pair.a, Duration::from_millis(5500));
        let current = value["peers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["peer_id"] == pair.pa)
            .unwrap();
        assert_eq!(
            pair.sa
                .request(&pair.a, "GET", "/api/health", &[], "")
                .status,
            200
        );
        if current["state"] == "offline" {
            assert_eq!(current["rtt_stale"], true);
            assert!(current["heartbeat_age_ms"].as_u64().unwrap() >= 30_000);
            assert!(current["last_error"].is_string());
            let retry = current["next_retry_at"].as_i64().unwrap();
            assert!(retry <= now_ms() + 60_000);
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(35),
            "30s offline deadline plus one SSE interval"
        );
    }
    assert_eq!(count(&pair.a, "SELECT count(*) FROM messages"), messages);
    assert!(started.elapsed() <= Duration::from_millis(35_500));
    drop(stopped);
    pair.connected();
    assert_eq!(peer(&pair.a, &pair.sa, &pair.pa)["rtt_stale"], false);
    assert_eq!(
        peer(&pair.a, &pair.sa, &pair.pa)["counters"],
        last["counters"]
    );
}

#[test]
fn fed_sse_updates_at_least_every_five_seconds_without_message_database_writes() {
    let pair = Pair::paired(Duration::from_secs(35));
    let mut events = Events::open(&pair.a, &pair.sa);
    let first = events.fed(&pair.a, Duration::from_millis(5500));
    let before = count(&pair.a, "SELECT count(*) FROM messages");
    let started = Instant::now();
    let next = events.fed(&pair.a, Duration::from_millis(5500));
    assert!(started.elapsed() <= Duration::from_millis(5500));
    for value in [&first, &next] {
        assert_eq!(value["enabled"], true);
        let p = value["peers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["peer_id"] == pair.pa)
            .unwrap();
        for field in [
            "fingerprint",
            "state",
            "path",
            "last_handshake_at",
            "heartbeat_age_ms",
            "rtt_ms",
            "rtt_stale",
            "last_error",
            "next_retry_at",
            "queue",
            "counters",
            "shares",
        ] {
            assert!(p.get(field).is_some(), "missing health field {field}");
        }
        assert_eq!(p["queue"], json!({"count":0,"bytes":0,"oldest_at":null}));
        assert_eq!(
            p["counters"],
            json!({"sent_accepted":0,"received":0,"expired":0,"rejected":0,"cancelled":0})
        );
    }
    assert_eq!(count(&pair.a, "SELECT count(*) FROM messages"), before);
}

#[cfg(unix)]
#[test]
fn blackholed_network_does_not_stall_local_hooks_or_local_message_delivery() {
    let (pair, _, _) = shared(45);
    let stopped = Stopped::new(&pair.sb);
    pair.a
        .register_at("local-child", "claude", Some("agent-a"), &pair.ra);
    pair.a
        .send("agent-a", "local-child", "sync", "local still works");
    let start = Instant::now();
    let context = hook_context(&pair.a, "claude", "local-child", &pair.ra);
    assert!(
        start.elapsed() <= Duration::from_millis(300),
        "ordinary hook waits on remote IO"
    );
    assert!(context.contains("local still works"));
    drop(stopped);
}

#[test]
fn offline_cli_enqueues_with_no_service_and_hooks_do_not_transmit_it() {
    let (mut pair, _, target) = shared(60);
    pair.sa.process.0.kill().unwrap();
    pair.sa.process.0.wait().unwrap();
    let id = queued(&pair.a, "agent-a", &target, "sync", "no service queued");
    let before = envelope(&pair.a, &id);
    hook_context(&pair.a, "claude", "agent-a", &pair.ra);
    assert_eq!(
        text(
            &pair.a,
            "SELECT state FROM fed_outbox WHERE message_id=?1",
            &id
        ),
        "queued"
    );
    assert_eq!(
        count(
            &pair.b,
            "SELECT count(*) FROM messages WHERE body='no service queued'"
        ),
        0
    );
    let restarted = Server::start(&pair.a, true);
    received(&pair.b, &id);
    outcome(&pair.a, &id, "accepted");
    assert_eq!(envelope(&pair.a, &id), before);
    assert_eq!(
        peer(&pair.a, &restarted, &pair.pa)["counters"]["sent_accepted"],
        1
    );
}

#[test]
fn second_server_cannot_own_the_same_federation_service_lock() {
    common::faults::second_server_cannot_own_the_same_federation_service_lock();
}

#[cfg(unix)]
#[test]
fn storage_read_failure_never_reports_a_successful_enqueue_or_sends_directly() {
    common::faults::unreadable_database();
}

#[test]
fn audit_and_chain_keep_immutable_peer_identity_without_messages_or_credentials() {
    let (pair, _, target) = shared(60);
    let body = "body-that-must-never-be-in-audit";
    let message = queued(&pair.a, "agent-a", &target, "sync", body);
    received(&pair.b, &message);
    outcome(&pair.a, &message, "accepted");
    let fingerprint = peer(&pair.a, &pair.sa, &pair.pa)["fingerprint"].clone();
    api(
        &pair.a,
        &pair.sa,
        "PUT",
        &format!("/api/fed/peers/{}/label", pair.pa),
        json!({"label":"renamed"}),
    );
    api(
        &pair.a,
        &pair.sa,
        "POST",
        &format!("/api/fed/peers/{}/pause", pair.pa),
        json!({}),
    );
    api(
        &pair.a,
        &pair.sa,
        "DELETE",
        &format!("/api/fed/peers/{}", pair.pa),
        json!({}),
    );
    assert_eq!(
        peer(&pair.a, &pair.sa, &pair.pa)["fingerprint"],
        fingerprint
    );
    for bus in [&pair.a, &pair.b] {
        assert_audit_chain(bus);
        let connection = db(bus);
        for table in ["fed_audit", "events"] {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM {table}"))
                .unwrap();
            let columns = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    Ok((0..columns)
                        .map(|i| format!("{:?}", row.get_ref(i).unwrap()))
                        .collect::<String>())
                })
                .unwrap();
            for row in rows {
                let row = row.unwrap();
                assert!(!row.contains(body));
                assert!(!row.contains(pair.sa.cookie.strip_prefix("axon_session=").unwrap()));
                assert!(!row.contains(pair.sb.cookie.strip_prefix("axon_session=").unwrap()));
                assert!(!row.contains("axon1:"));
            }
        }
    }
}

#[test]
fn corrupt_storage_never_falls_back_to_sending_without_a_durable_outbox() {
    common::faults::corrupt_database();
}
