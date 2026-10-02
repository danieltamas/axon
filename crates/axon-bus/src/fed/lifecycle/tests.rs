use super::*;
use crate::fed::testkit::{agent, fixture, repo, share, Fixture};

fn state(fx: &Fixture, peer: &str) -> String {
    fx.conn
        .query_row("SELECT state FROM peers WHERE peer_id=?1", [peer], |r| {
            r.get(0)
        })
        .unwrap()
}

fn count(fx: &Fixture, sql: &str) -> i64 {
    fx.conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn pause_and_resume_only_move_between_active_and_paused_and_are_audited() {
    let mut fx = fixture();
    assert!(matches!(
        resume(&mut fx.conn, "p1").unwrap(),
        Change::WrongState
    ));
    assert!(matches!(
        pause(&mut fx.conn, "p1").unwrap(),
        Change::Done(_)
    ));
    assert_eq!(state(&fx, "p1"), "paused");
    assert!(matches!(
        pause(&mut fx.conn, "p1").unwrap(),
        Change::WrongState
    ));
    assert!(matches!(
        resume(&mut fx.conn, "p1").unwrap(),
        Change::Done(_)
    ));
    assert_eq!(state(&fx, "p1"), "active");
    assert!(matches!(
        pause(&mut fx.conn, "nobody").unwrap(),
        Change::Unknown
    ));
    assert_eq!(
        count(&fx, "SELECT count(*) FROM fed_audit WHERE direction='local' AND peer_fingerprint IS NOT NULL"),
        2
    );
}

#[test]
fn remove_ends_every_grant_in_one_step_and_keeps_what_was_already_delivered() {
    let mut fx = fixture();
    let project = repo(fx.dir.path(), "project");
    let id = "a".repeat(32);
    share(&fx.conn, &id, "p1", &project, 3);
    agent(&fx.conn, "bob", &project, "active");
    fx.conn
        .execute(
            "INSERT INTO fed_sessions VALUES ('bbbbbbbbbbbb','bob',?1)",
            [&id],
        )
        .unwrap();
    fx.conn
        .execute(
            "INSERT INTO fed_remote_sessions VALUES ('p1',?1,'cccccccccccc','codex-c','idle',1)",
            [&id],
        )
        .unwrap();
    fx.conn
        .execute(
            "INSERT INTO fed_outbox (message_id,peer_id,generation,share_id,revision,from_agent,
               envelope_json,bytes,created_at,expires_at,state)
             VALUES ('m1','p1',7,?1,3,'bob','{}',2,1,2,'queued')",
            [&id],
        )
        .unwrap();
    for (local, delivered) in [("pending", None), ("shown", Some(5))] {
        fx.conn
            .execute(
                "INSERT INTO messages (id,thread,seq,from_id,to_id,kind,body,delivered_at)
                 VALUES (?1,'t',1,'peer:p1/cccccccccccc','bob','sync','hi',?2)",
                rusqlite::params![local, delivered],
            )
            .unwrap();
        fx.conn
            .execute(
                "INSERT INTO fed_inbox (peer_id,generation,message_id,content_hash,local_message_id,accepted_at,expires_at) VALUES ('p1',7,?1,'h',?1,1,2)",
                [local],
            )
            .unwrap();
    }
    assert!(matches!(
        remove(&mut fx.conn, "p1").unwrap(),
        Change::Done(_)
    ));
    assert_eq!(state(&fx, "p1"), "removed");
    assert_eq!(
        count(
            &fx,
            "SELECT count(*) FROM peers WHERE removed_at IS NOT NULL"
        ),
        1
    );
    assert_eq!(
        count(
            &fx,
            "SELECT count(*) FROM peer_shares WHERE state<>'removed'"
        ),
        0
    );
    assert_eq!(
        count(&fx, "SELECT count(*) FROM fed_outbox WHERE state='queued'"),
        0
    );
    assert_eq!(count(&fx, "SELECT count(*) FROM fed_remote_sessions"), 0);
    assert_eq!(
        count(&fx, "SELECT count(*) FROM messages WHERE id='pending'"),
        0,
        "undelivered inbound is deleted"
    );
    assert_eq!(
        count(&fx, "SELECT count(*) FROM messages WHERE id='shown'"),
        1
    );
    assert_eq!(
        count(
            &fx,
            "SELECT count(*) FROM fed_audit WHERE decision='removed'"
        ),
        1
    );
    assert!(matches!(
        remove(&mut fx.conn, "p1").unwrap(),
        Change::Unknown
    ));
    assert!(matches!(
        resume(&mut fx.conn, "p1").unwrap(),
        Change::Unknown
    ));
}

#[test]
fn a_label_is_unique_among_live_peers_and_changes_nothing_else() {
    let mut fx = fixture();
    assert!(matches!(
        relabel(&mut fx.conn, "p1", "p2").unwrap(),
        Change::LabelTaken
    ));
    assert!(matches!(
        relabel(&mut fx.conn, "p1", "p1").unwrap(),
        Change::Done(_)
    ));
    assert!(matches!(
        relabel(&mut fx.conn, "p1", "renamed").unwrap(),
        Change::Done(_)
    ));
    assert_eq!(
        count(
            &fx,
            "SELECT count(*) FROM peers WHERE label='renamed' AND state='active'"
        ),
        1
    );
    assert!(matches!(
        relabel(&mut fx.conn, "nobody", "x").unwrap(),
        Change::Unknown
    ));
}
