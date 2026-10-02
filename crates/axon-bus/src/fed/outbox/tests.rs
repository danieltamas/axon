use super::*;
use crate::fed::testkit::{agent, fixture, repo, share, Fixture};

const SHARE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A queued message from `ann` on a share at revision 2, with the repo `ann` is a member of.
fn queued() -> (Fixture, Due) {
    let fx = fixture();
    let project = repo(fx.dir.path(), "project");
    share(&fx.conn, SHARE, "p1", &project, 2);
    agent(&fx.conn, "ann", &project, "active");
    fx.conn
        .execute(
            "INSERT INTO fed_outbox (message_id,peer_id,generation,share_id,revision,from_agent,
               envelope_json,bytes,created_at,expires_at,state)
             VALUES ('m1','p1',7,?1,2,'ann','{\"revision\":2}',2,1,?2,'queued')",
            params![SHARE, now_ms() + 60_000],
        )
        .unwrap();
    let node = fx
        .conn
        .query_row("SELECT node_id FROM peers WHERE peer_id='p1'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap();
    let due = Due {
        message_id: "m1".into(),
        peer_id: "p1".into(),
        share_id: SHARE.into(),
        sender: "ann".into(),
        node: node.parse().unwrap(),
        generation: 7,
        frame: json!({"revision": 2}),
        attempts: 0,
    };
    (fx, due)
}

#[test]
fn a_batch_holds_at_most_a_few_rows_of_one_peer_and_none_of_a_backed_off_peer() {
    let (fx, row) = queued();
    for n in 2..=30 {
        fx.conn
            .execute(
                "INSERT INTO fed_outbox (message_id,peer_id,generation,share_id,revision,from_agent,
                   envelope_json,bytes,created_at,expires_at,state)
                 VALUES (?1,'p1',7,?2,2,'ann','{\"revision\":2}',2,?3,?4,'queued')",
                params![format!("m{n}"), SHARE, n, now_ms() + 60_000],
            )
            .unwrap();
    }
    let mut conn = store::open(&db(&fx)).unwrap();
    assert_eq!(
        due(&mut conn, now_ms(), &[]).unwrap().len(),
        PER_PEER as usize
    );
    let skip = [row.node.to_string()];
    assert!(due(&mut conn, now_ms(), &skip).unwrap().is_empty());
}

fn db(fx: &Fixture) -> std::path::PathBuf {
    fx.dir.path().join("axon.db")
}

fn state(fx: &Fixture) -> (String, Option<String>) {
    fx.conn
        .query_row(
            "SELECT state, last_error FROM fed_outbox WHERE message_id='m1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

#[test]
fn an_authorized_message_is_cleared_to_send() {
    let (fx, mut row) = queued();
    assert!(clear_to_send(&db(&fx), &mut row).unwrap());
    assert_eq!(state(&fx).0, "queued");
}

#[test]
fn each_revoked_permission_ends_the_row_and_tells_the_sender() {
    for (change, reason) in [
        ("UPDATE peer_shares SET outbound=0", "outbound_off"),
        (
            "UPDATE peer_shares SET remote_inbound=0",
            "remote_inbound_off",
        ),
        ("UPDATE peer_shares SET state='removed'", "unshared"),
        ("UPDATE agents SET status='closed'", "not_a_member"),
        ("UPDATE peers SET generation=8", "peer_changed"),
        ("UPDATE peers SET state='removed'", "peer_changed"),
    ] {
        let (fx, mut row) = queued();
        fx.conn.execute(change, []).unwrap();
        assert!(!clear_to_send(&db(&fx), &mut row).unwrap(), "{change}");
        assert_eq!(
            state(&fx),
            ("cancelled".into(), Some(reason.into())),
            "{change}"
        );
        let told: i64 = fx
            .conn
            .query_row(
                "SELECT count(*) FROM messages WHERE to_id='ann' AND body LIKE '%m1%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(told, 1, "{change}");
    }
}

#[test]
fn a_paused_peer_keeps_the_row_queued() {
    let (fx, mut row) = queued();
    fx.conn
        .execute("UPDATE peers SET state='paused'", [])
        .unwrap();
    assert!(!clear_to_send(&db(&fx), &mut row).unwrap());
    assert_eq!(state(&fx).0, "queued");
}

#[test]
fn an_expired_row_is_not_sent() {
    let (fx, mut row) = queued();
    fx.conn
        .execute("UPDATE fed_outbox SET expires_at=1", [])
        .unwrap();
    assert!(!clear_to_send(&db(&fx), &mut row).unwrap());
    assert_eq!(state(&fx).0, "expired");
}

#[test]
fn a_revision_bump_restamps_the_queued_message() {
    let (fx, mut row) = queued();
    fx.conn
        .execute("UPDATE peer_shares SET revision=5", [])
        .unwrap();
    assert!(clear_to_send(&db(&fx), &mut row).unwrap());
    assert_eq!(row.frame["revision"], 5);
    let stored: String = fx
        .conn
        .query_row("SELECT envelope_json FROM fed_outbox", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&stored).unwrap()["revision"],
        5
    );
}

#[test]
fn a_rejection_by_the_peer_tells_the_sender() {
    let (fx, row) = queued();
    settle(&db(&fx), &row, &Verdict::Rejected("stale_revision".into())).unwrap();
    assert_eq!(state(&fx).0, "rejected");
    let told: String = fx
        .conn
        .query_row("SELECT body FROM messages WHERE to_id='ann'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(told.contains("stale_revision"), "{told}");
}

#[test]
fn a_row_of_a_peer_with_a_send_in_flight_is_not_expired_until_that_send_settles() {
    let (fx, row) = queued();
    fx.conn
        .execute(
            "UPDATE fed_outbox SET expires_at=1 WHERE message_id='m1'",
            [],
        )
        .unwrap();
    let mut conn = store::open(&db(&fx)).unwrap();
    due(&mut conn, now_ms(), &[row.node.to_string()]).unwrap();
    assert_eq!(state(&fx).0, "queued");
    due(&mut conn, now_ms(), &[]).unwrap();
    assert_eq!(state(&fx).0, "expired");
}
