use super::*;
use crate::fed::testkit::{agent, fixture, repo, share, Fixture};

const SHARE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SESSION: &str = "carolsessio1";

fn ready() -> (Fixture, std::path::PathBuf) {
    let fx = fixture();
    let project = repo(fx.dir.path(), "project");
    share(&fx.conn, SHARE, "p1", &project, 2);
    agent(&fx.conn, "ann", &project, "active");
    fx.conn
        .execute(
            "INSERT INTO fed_remote_sessions VALUES ('p1',?1,?2,'codex-caro','idle',1)",
            [SHARE, SESSION],
        )
        .unwrap();
    (fx, project)
}

fn send_as(fx: &mut Fixture, from: &str, to: &str, kind: &str, body: &str) -> &'static str {
    let request = Request {
        from,
        to,
        kind,
        body,
        thread: None,
        refs: &[],
        reply_to: None,
    };
    match send(&mut fx.conn, &request).unwrap() {
        Some(Outcome::Refused(reason)) => reason,
        Some(Outcome::Queued { .. }) => "queued",
        None => "local",
    }
}

#[test]
fn a_send_is_refused_with_the_first_reason_that_applies() {
    let (mut fx, _) = ready();
    let to = format!("peer:p1/{SESSION}");
    assert_eq!(send_as(&mut fx, "ann", "someone", "sync", "x"), "local");
    assert_eq!(
        send_as(&mut fx, "ann", "peer:nobody/abcdefghijkl", "sync", "x"),
        "unknown_peer"
    );
    assert_eq!(
        send_as(&mut fx, "ann", "peer:p1/abcdefghijkl", "sync", "x"),
        "unknown_session"
    );
    agent(&fx.conn, "outsider", fx.dir.path(), "active");
    assert_eq!(
        send_as(&mut fx, "outsider", &to, "sync", "x"),
        "not_a_member"
    );
    assert_eq!(
        send_as(&mut fx, "ann", &to, "stop", "x"),
        "kind_not_allowed"
    );
    assert_eq!(
        send_as(&mut fx, "ann", &to, "sync", &"x".repeat(401)),
        "too_long"
    );
    fx.conn
        .execute("UPDATE peer_shares SET remote_inbound=0", [])
        .unwrap();
    assert_eq!(
        send_as(&mut fx, "ann", &to, "sync", "x"),
        "remote_inbound_off"
    );
    fx.conn
        .execute("UPDATE peer_shares SET outbound=0", [])
        .unwrap();
    assert_eq!(send_as(&mut fx, "ann", &to, "sync", "x"), "outbound_off");
    fx.conn
        .execute("UPDATE peers SET state='paused'", [])
        .unwrap();
    assert_eq!(send_as(&mut fx, "ann", &to, "sync", "x"), "peer_paused");
    fx.conn
        .execute("UPDATE peers SET state='removed'", [])
        .unwrap();
    assert_eq!(send_as(&mut fx, "ann", &to, "sync", "x"), "peer_removed");
    store::put_setting(&fx.conn, "fed_enabled", Some("0")).unwrap();
    assert_eq!(send_as(&mut fx, "ann", &to, "sync", "x"), "federation_off");
    let rows: i64 = fx
        .conn
        .query_row("SELECT count(*) FROM fed_outbox", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0, "a refusal queues nothing");
}

#[test]
fn a_queued_message_carries_the_wire_envelope_and_no_local_detail() {
    let (mut fx, project) = ready();
    let to = format!("peer:p1/{SESSION}");
    assert_eq!(
        send_as(&mut fx, "ann", &to, "question", "which table?"),
        "queued"
    );
    let (generation, revision, envelope): (i64, i64, String) = fx
        .conn
        .query_row(
            "SELECT generation, revision, envelope_json FROM fed_outbox",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((generation, revision), (7, 2));
    let sent: serde_json::Value = serde_json::from_str(&envelope).unwrap();
    assert_eq!(sent["to_session"], SESSION);
    assert_eq!(sent["kind"], "question");
    assert_eq!(
        sent["expires_at"].as_i64().unwrap() - sent["created_at"].as_i64().unwrap(),
        LIFETIME_MS
    );
    assert!(
        !envelope.contains("ann"),
        "the sender's agent id stays local"
    );
    assert!(!envelope.contains(project.to_str().unwrap()));
    assert_eq!(sent["from_session"].as_str().unwrap().len(), 12);
}

#[test]
fn a_full_queue_refuses() {
    let (mut fx, _) = ready();
    let to = format!("peer:p1/{SESSION}");
    assert_eq!(send_as(&mut fx, "ann", &to, "sync", "first"), "queued");
    fx.conn
        .execute("UPDATE fed_outbox SET bytes=?1", [PEER_BYTES])
        .unwrap();
    assert_eq!(
        send_as(&mut fx, "ann", &to, "sync", "no room"),
        "queue_full"
    );
}
