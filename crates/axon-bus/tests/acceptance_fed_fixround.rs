//! Work order CDX-2/BUG-2, CDX-6 and C2: revocation and accepted-share provenance.
mod common;
use common::delivery::*;
use common::fed::*;
use common::*;
use serde_json::json;
use std::time::{Duration, Instant};

#[cfg(unix)]
fn revoke_offline_queue(action: &str) {
    let (mut pair, share, target) = shared(75);
    // No stopped receiver may buffer an already transmitted frame and accept it later.
    pair.sb.process.0.kill().unwrap();
    pair.sb.process.0.wait().unwrap();
    // Freeze only while the test owns the write reservation, so the stopped server
    // cannot hold a SQLite write lock needed by the queueing CLI processes.
    let reservation = pair.a.db();
    reservation.busy_timeout(Duration::from_secs(2)).unwrap();
    reservation.execute_batch("BEGIN IMMEDIATE").unwrap();
    let sender_stopped = Stopped::new(&pair.sa);
    reservation.execute_batch("ROLLBACK").unwrap();
    drop(reservation);
    let ids: Vec<_> = (0..6)
        .map(|n| {
            queued(
                &pair.a,
                "agent-a",
                &target,
                "sync",
                &format!("revoked-offline-{action}-{n}"),
            )
        })
        .collect();
    assert_eq!(
        count(
            &pair.a,
            "SELECT count(*) FROM fed_outbox WHERE state='queued'"
        ),
        6
    );
    assert_eq!(count(&pair.b, "SELECT count(*) FROM fed_inbox"), 0);
    drop(sender_stopped);
    // Allow a delivery batch to be selected while the real receiver remains dead.
    std::thread::sleep(Duration::from_millis(750));
    match action {
        "outbound" => {
            api(
                &pair.a,
                &pair.sa,
                "PUT",
                &format!("/api/fed/shares/{share}"),
                json!({"inbound":true,"outbound":false}),
            );
        }
        "pause" => {
            api(
                &pair.a,
                &pair.sa,
                "POST",
                &format!("/api/fed/peers/{}/pause", pair.pa),
                json!({}),
            );
        }
        "remove" => {
            api(
                &pair.a,
                &pair.sa,
                "DELETE",
                &format!("/api/fed/peers/{}", pair.pa),
                json!({}),
            );
        }
        "membership" => {
            let outside = repo(&pair.a, "outside-share");
            let mut hook = pair.a.pre("claude", "agent-a");
            hook["cwd"] = json!(outside);
            pair.a.hook("claude", "PreToolUse", &hook);
        }
        _ => unreachable!(),
    }
    pair.sb = Server::start(&pair.b, true);
    if matches!(action, "outbound" | "membership") {
        pair.connected();
    }
    // Cover the outstanding request's 10 s timeout and another outbox tick/retry.
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        pair.a.remaining();
        for id in &ids {
            let state = text(
                &pair.a,
                "SELECT state FROM fed_outbox WHERE message_id=?1",
                id,
            );
            assert_ne!(
                state, "accepted",
                "CDX-2: revoked {action} row was delivered"
            );
            assert_eq!(
                db(&pair.b)
                    .query_row(
                        "SELECT count(*) FROM fed_inbox WHERE message_id=?1",
                        [id],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap(),
                0,
                "CDX-2: {action} allowed queued message {id} after revocation"
            );
        }
        assert_eq!(
            count(
                &pair.b,
                "SELECT count(*) FROM messages WHERE body LIKE 'revoked-offline-%'"
            ),
            0
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        pair.sb
            .request(&pair.b, "GET", "/api/health", &[], "")
            .status,
        200
    );
    assert!(pair.sb.process.0.try_wait().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn outbound_off_prevents_offline_queue_delivery_after_reconnect() {
    revoke_offline_queue("outbound");
}

#[cfg(unix)]
#[test]
fn pause_prevents_offline_queue_delivery_after_receiver_restart() {
    revoke_offline_queue("pause");
}

#[cfg(unix)]
#[test]
fn remove_prevents_offline_queue_delivery_after_receiver_restart() {
    revoke_offline_queue("remove");
}

#[cfg(unix)]
#[test]
fn membership_change_prevents_offline_queue_delivery_after_reconnect() {
    revoke_offline_queue("membership");
}

#[test]
fn unsharing_old_project_preserves_pending_messages_accepted_on_new_project() {
    let (mut pair, old_share, target) = shared(90);
    let old = queued(&pair.a, "agent-a", &target, "sync", "old share pending");
    let old_local = received(&pair.b, &old);
    pair.ra = repo(&pair.a, "second-a");
    pair.rb = repo(&pair.b, "second-b");
    for (bus, harness, agent, cwd) in [
        (&pair.a, "claude", "agent-a", &pair.ra),
        (&pair.b, "codex", "agent-b", &pair.rb),
    ] {
        let mut hook = bus.pre(harness, agent);
        hook["cwd"] = json!(cwd);
        bus.hook(harness, pre_event(harness), &hook);
    }
    let new_share = pair.share(true, true, true, true);
    eventually(&pair.a, "new project's discovery", || {
        db(&pair.a)
            .query_row(
                "SELECT count(*) FROM fed_remote_sessions WHERE share_id=?1",
                [&new_share],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    });
    let session = text(
        &pair.b,
        "SELECT session FROM fed_sessions WHERE agent_id='agent-b' AND share_id=?1",
        &new_share,
    );
    let label = peer(&pair.a, &pair.sa, &pair.pa)["label"]
        .as_str()
        .unwrap()
        .to_owned();
    let new_target = format!("peer:{label}/{session}");
    let mut pending = Vec::new();
    for body in ["new share pending one", "new share pending two"] {
        let id = queued(&pair.a, "agent-a", &new_target, "sync", body);
        pending.push((received(&pair.b, &id), body));
    }
    api(
        &pair.b,
        &pair.sb,
        "DELETE",
        &format!("/api/fed/shares/{old_share}"),
        json!({}),
    );
    assert_eq!(
        db(&pair.b)
            .query_row(
                "SELECT count(*) FROM messages WHERE id=?1",
                [&old_local],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0,
        "old share's pending message is removed"
    );
    for (local, body) in &pending {
        assert_eq!(db(&pair.b).query_row(
            "SELECT count(*) FROM messages WHERE id=?1 AND body=?2 AND delivered_at IS NULL",
            rusqlite::params![local, body], |r| r.get::<_, i64>(0),
        ).unwrap(), 1, "CDX-6: unshare A must preserve B's pending messages");
    }
    assert_eq!(
        text(
            &pair.b,
            "SELECT state FROM peer_shares WHERE share_id=?1",
            &new_share
        ),
        "active"
    );
    let context = hook_context(&pair.b, "codex", "agent-b", &pair.rb);
    for (_, body) in pending {
        assert!(context.contains(body));
    }
    assert!(!context.contains("old share pending"));
}

#[test]
fn removal_notice_marks_remote_peer_removed_with_reason() {
    let pair = Pair::paired(Duration::from_secs(40));
    api(
        &pair.a,
        &pair.sa,
        "DELETE",
        &format!("/api/fed/peers/{}", pair.pa),
        json!({}),
    );
    eventually(&pair.b, "C2 remote removal visible", || {
        peer(&pair.b, &pair.sb, &pair.pb)["state"] == "removed"
    });
    assert_eq!(
        peer(&pair.b, &pair.sb, &pair.pb)["removed_reason"],
        "remote_removed"
    );
    assert_eq!(
        text(
            &pair.b,
            "SELECT state FROM peers WHERE peer_id=?1",
            &pair.pb
        ),
        "removed"
    );
}

#[test]
fn remote_pause_is_visible_blocks_sends_and_resume_clears_it() {
    let (pair, _, target) = shared(60);
    let reverse = pair.target(false, "agent-a");
    assert_eq!(peer(&pair.b, &pair.sb, &pair.pb)["remote_paused"], false);
    api(
        &pair.a,
        &pair.sa,
        "POST",
        &format!("/api/fed/peers/{}/pause", pair.pa),
        json!({}),
    );
    eventually(&pair.b, "C2 remote pause visible", || {
        peer(&pair.b, &pair.sb, &pair.pb)["remote_paused"] == true
    });
    assert_eq!(
        text(
            &pair.b,
            "SELECT state FROM peers WHERE peer_id=?1",
            &pair.pb
        ),
        "active"
    );
    refused(
        &pair.b,
        "agent-b",
        &reverse,
        "sync",
        "remote owner paused",
        "peer_paused",
    );
    api(
        &pair.a,
        &pair.sa,
        "POST",
        &format!("/api/fed/peers/{}/resume", pair.pa),
        json!({}),
    );
    eventually(&pair.b, "C2 remote resume visible", || {
        peer(&pair.b, &pair.sb, &pair.pb)["remote_paused"] == false
    });
    pair.connected();
    received(
        &pair.a,
        &queued(&pair.b, "agent-b", &reverse, "sync", "resumed reverse send"),
    );
    received(
        &pair.b,
        &queued(&pair.a, "agent-a", &target, "sync", "resumed forward send"),
    );
}
