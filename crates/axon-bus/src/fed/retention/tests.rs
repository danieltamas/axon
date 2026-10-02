use super::*;
use crate::fed::testkit::fixture;

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn old_rows_go_and_live_ones_stay() {
    let fx = fixture();
    let (conn, now) = (&fx.conn, 400 * DAY_MS);
    for (id, state, created) in [
        ("old-done", "accepted", 1),
        ("old-queued", "queued", 1),
        ("fresh-done", "accepted", now),
    ] {
        conn.execute(
            "INSERT INTO fed_outbox (message_id,peer_id,generation,share_id,revision,from_agent,
               envelope_json,bytes,created_at,expires_at,state)
             VALUES (?1,'p1',7,'s',1,'a','{}',2,?2,?2,?3)",
            params![id, created, state],
        )
        .unwrap();
    }
    conn.execute_batch(&format!(
        "INSERT INTO fed_inbox (peer_id,generation,message_id,content_hash,local_message_id,accepted_at,expires_at)
           VALUES ('p1',7,'m1','h','l1',1,2), ('p1',7,'m2','h','l2',{now},{now});
         INSERT INTO fed_audit (ts,decision) VALUES (1,'accepted'), ({now},'accepted');
         INSERT INTO dashboard_sessions (session_hash,token_hash,created_at,last_used_at,expires_at)
           VALUES ('a','t',1,1,5), ('b','t',1,1,{now}+1000);
         INSERT INTO peer_invites (invite_id,secret_hash,expires_at) VALUES ('i1','h',5), ('i2','h',{now});"
    ))
    .unwrap();
    sweep(conn, now).unwrap();
    assert_eq!(count(conn, "SELECT count(*) FROM fed_outbox"), 2);
    assert_eq!(count(conn, "SELECT count(*) FROM fed_inbox"), 1);
    assert_eq!(count(conn, "SELECT count(*) FROM fed_audit"), 1);
    assert_eq!(count(conn, "SELECT count(*) FROM dashboard_sessions"), 1);
    assert_eq!(count(conn, "SELECT count(*) FROM peer_invites"), 1);
}

#[test]
fn only_the_newest_tombstones_of_each_peer_stay() {
    let fx = fixture();
    for peer in ["p1", "p2"] {
        for n in 0..40 {
            fx.conn
                .execute(
                    "INSERT INTO peer_shares (share_id,peer_id,label,inbound,outbound,revision,state)
                     VALUES (?1,?2,'x',0,0,1,'removed')",
                    params![format!("{peer}-{n}"), peer],
                )
                .unwrap();
        }
    }
    fx.conn
        .execute(
            "INSERT INTO peer_shares (share_id,peer_id,label,inbound,outbound,revision,state)
             VALUES ('live','p1','x',1,1,1,'active')",
            [],
        )
        .unwrap();
    sweep(&fx.conn, now_ms()).unwrap();
    for peer in ["p1", "p2"] {
        let kept = count(
            &fx.conn,
            &format!("SELECT count(*) FROM peer_shares WHERE peer_id='{peer}' AND state='removed'"),
        );
        assert_eq!(kept, shares::TOMBSTONES_KEPT);
    }
    assert_eq!(
        count(
            &fx.conn,
            "SELECT count(*) FROM peer_shares WHERE state='active'"
        ),
        1
    );
    assert_eq!(
        count(
            &fx.conn,
            "SELECT count(*) FROM peer_shares WHERE share_id='p1-39'"
        ),
        1,
        "the newest survive"
    );
}
