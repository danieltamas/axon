use rusqlite::{params, Connection};
use serde_json::json;

use super::*;
use crate::fed::testkit::{agent, fixture, node, repo, share};
use crate::store;

fn share_row(conn: &Connection, id: &str, peer: &str, state: &str, revision: i64) {
    share(conn, id, peer, std::path::Path::new("/repo"), revision);
    conn.execute(
        "UPDATE peer_shares SET state=?2 WHERE share_id=?1",
        params![id, state],
    )
    .unwrap();
}

fn id(n: char) -> String {
    n.to_string().repeat(32)
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn unshare_cancels_queued_rows_and_deletes_only_undelivered_inbound() {
    let mut fx = fixture();
    let share = id('a');
    share_row(&fx.conn, &share, "p1", "active", 3);
    fx.conn
        .execute(
            "INSERT INTO fed_sessions VALUES ('aaaaaaaaaaaa','agent',?1)",
            [&share],
        )
        .unwrap();
    fx.conn
        .execute(
            "INSERT INTO fed_remote_sessions VALUES ('p1',?1,'bbbbbbbbbbbb','codex-bbbb','idle',1)",
            [&share],
        )
        .unwrap();
    fx.conn
        .execute(
            "INSERT INTO fed_outbox (message_id,peer_id,generation,share_id,revision,from_agent,
               envelope_json,bytes,created_at,expires_at,state)
             VALUES ('m1','p1',7,?1,3,'agent','{}',2,1,2,'queued')",
            [&share],
        )
        .unwrap();
    for (local, delivered) in [("pending", None), ("shown", Some(5))] {
        fx.conn
            .execute(
                "INSERT INTO messages (id,thread,seq,from_id,to_id,kind,body,delivered_at)
                 VALUES (?1,'t',1,'peer:p1/bbbbbbbbbbbb','agent','sync','hi',?2)",
                params![local, delivered],
            )
            .unwrap();
        fx.conn
            .execute(
                "INSERT INTO fed_inbox VALUES ('p1',7,?1,'h',?1,1,2)",
                [local],
            )
            .unwrap();
    }
    let tx = store::write_tx(&mut fx.conn).unwrap();
    let removed = remove(&tx, &share).unwrap();
    tx.commit().unwrap();
    assert_eq!((removed.state.as_str(), removed.revision), ("removed", 4));
    assert_eq!(
        count(&fx.conn, "SELECT count(*) FROM fed_remote_sessions"),
        0
    );
    assert_eq!(
        count(
            &fx.conn,
            "SELECT count(*) FROM fed_outbox WHERE state='cancelled'"
        ),
        1
    );
    let left: String = fx
        .conn
        .query_row("SELECT id FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, "shown");
}

fn frame(kind: &str, share: &str, revision: i64, inbound: bool) -> serde_json::Value {
    json!({"type": kind, "v": 1, "generation": 7, "share_id": share,
           "revision": revision, "inbound": inbound, "outbound": true})
}

#[test]
fn a_frame_only_touches_shares_of_the_peer_it_came_from() {
    let mut fx = fixture();
    let share = id('b');
    share_row(&fx.conn, &share, "p1", "active", 2);
    let theirs = apply_frame(
        &mut fx.conn,
        &node(2),
        frame("share_update", &share, 9, false),
    )
    .unwrap();
    assert_eq!(theirs["reason"], "unknown_share");
    let own = apply_frame(
        &mut fx.conn,
        &node(1),
        frame("share_update", &share, 9, false),
    )
    .unwrap();
    assert_eq!(own["status"], "accepted");
    let (revision, remote_inbound): (i64, bool) = fx
        .conn
        .query_row(
            "SELECT revision, remote_inbound FROM peer_shares WHERE share_id=?1",
            [&share],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((revision, remote_inbound), (9, false));
}

#[test]
fn revisions_only_move_forward_and_the_generation_must_match() {
    let mut fx = fixture();
    let share = id('c');
    share_row(&fx.conn, &share, "p1", "active", 5);
    let old = apply_frame(
        &mut fx.conn,
        &node(1),
        frame("share_update", &share, 5, false),
    )
    .unwrap();
    assert_eq!(old["status"], "duplicate");
    let mut stale = frame("share_update", &share, 6, false);
    stale["generation"] = json!(6);
    let reply = apply_frame(&mut fx.conn, &node(1), stale).unwrap();
    assert_eq!(reply["reason"], "stale_generation");
    assert_eq!(
        count(&fx.conn, "SELECT revision FROM peer_shares"),
        5,
        "neither frame changed the share"
    );
}

#[test]
fn an_offer_is_recorded_unmapped_and_an_unknown_field_is_refused() {
    let mut fx = fixture();
    let offer = json!({"type": "share_offer", "v": 1, "generation": 7, "share_id": id('d'),
                       "label": "project", "revision": 1, "inbound": true, "outbound": false,
                       "root_commit": null});
    let reply = apply_frame(&mut fx.conn, &node(1), offer.clone()).unwrap();
    assert_eq!(reply["status"], "accepted");
    let (state, repo, remote_inbound): (String, Option<String>, bool) = fx
        .conn
        .query_row(
            "SELECT state, local_repo, remote_inbound FROM peer_shares",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (state.as_str(), repo, remote_inbound),
        ("offered_in", None, true)
    );
    let again = apply_frame(&mut fx.conn, &node(1), offer.clone()).unwrap();
    assert_eq!(again["status"], "duplicate");
    let mut extra = offer;
    extra["local_repo"] = json!("/home/alice/secret");
    let reply = apply_frame(&mut fx.conn, &node(1), extra).unwrap();
    assert_eq!(reply["reason"], "bad_frame");
}

#[test]
fn membership_is_resolved_from_the_cwd_each_time() {
    let fx = fixture();
    let project = repo(fx.dir.path(), "project");
    let inside = project.join("src");
    std::fs::create_dir(&inside).unwrap();
    agent(&fx.conn, "in-repo", &inside, "active");
    agent(&fx.conn, "elsewhere", fx.dir.path(), "active");
    agent(&fx.conn, "closed", &project, "closed");
    let root = project.to_str().unwrap();
    assert!(is_member(&fx.conn, "in-repo", root).unwrap());
    assert!(!is_member(&fx.conn, "elsewhere", root).unwrap());
    assert!(!is_member(&fx.conn, "closed", root).unwrap());
    assert!(!is_member(&fx.conn, "nobody", root).unwrap());
    std::fs::rename(&project, fx.dir.path().join("moved")).unwrap();
    assert!(
        !is_member(&fx.conn, "in-repo", root).unwrap(),
        "a vanished repo holds no one"
    );
}
