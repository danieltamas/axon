use rusqlite::Connection;
use serde_json::{json, Value};

use super::*;
use crate::fed::testkit::{agent, fixture, node, repo, share, Fixture};

const SHARE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TO: &str = "bobsession12";
const FROM: &str = "alicesessio1";

/// A share of `p1` mapped to a real repo with one member, `bob`, whose session is `TO`.
fn ready() -> Fixture {
    let fx = fixture();
    let project = repo(fx.dir.path(), "project");
    share(&fx.conn, SHARE, "p1", &project, 4);
    agent(&fx.conn, "bob", &project, "active");
    fx.conn
        .execute("INSERT INTO fed_sessions VALUES (?1,'bob',?2)", [TO, SHARE])
        .unwrap();
    fx
}

fn message() -> Value {
    json!({"type": "msg", "v": 1, "generation": 7,
           "message_id": "11111111-1111-4111-8111-111111111111", "share_id": SHARE,
           "revision": 4, "from_session": FROM, "to_session": TO, "kind": "sync",
           "body": "hello", "thread": "t1", "reply_to": null, "refs": [],
           "created_at": now_ms(), "expires_at": now_ms() + 60_000})
}

fn run(conn: &mut Connection, limits: &Limits, frame: Value) -> Value {
    let msg: Msg = serde_json::from_value(frame).unwrap();
    receive(conn, limits, &node(1), msg).unwrap()
}

fn rejects(fx: &mut Fixture, change: impl FnOnce(&mut Value), reason: &str) {
    let mut frame = message();
    change(&mut frame);
    let reply = run(&mut fx.conn, &Limits::default(), frame);
    assert_eq!(
        (reply["status"].as_str(), reply["reason"].as_str()),
        (Some("rejected"), Some(reason))
    );
    let stored: i64 = fx
        .conn
        .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, 0, "{reason} must store nothing");
}

#[test]
fn each_check_rejects_with_its_reason_and_stores_nothing() {
    let mut fx = ready();
    rejects(&mut fx, |m| m["generation"] = json!(6), "stale_generation");
    rejects(&mut fx, |m| m["body"] = json!("bell\u{7}"), "bad_message");
    rejects(
        &mut fx,
        |m| m["refs"] = json!(["/etc/passwd"]),
        "bad_message",
    );
    rejects(&mut fx, |m| m["kind"] = json!("stop"), "kind_not_allowed");
    rejects(
        &mut fx,
        |m| m["created_at"] = json!(now_ms() + 3_600_000),
        "bad_time",
    );
    rejects(
        &mut fx,
        |m| {
            m["created_at"] = json!(now_ms() - 10_000);
            m["expires_at"] = json!(now_ms() - 1);
        },
        "expired",
    );
    rejects(
        &mut fx,
        |m| m["share_id"] = json!("b".repeat(32)),
        "unknown_share",
    );
    rejects(&mut fx, |m| m["revision"] = json!(3), "stale_revision");
    rejects(
        &mut fx,
        |m| m["to_session"] = json!("nobodyknows1"),
        "unknown_session",
    );
    rejects(
        &mut fx,
        |m| {
            m["kind"] = json!("answer");
            m["reply_to"] = json!("22222222-2222-4222-8222-222222222222");
        },
        "bad_reply",
    );
    fx.conn
        .execute("UPDATE peer_shares SET inbound=0", [])
        .unwrap();
    rejects(&mut fx, |_| {}, "inbound_off");
}

#[test]
fn an_accepted_message_is_stored_once_from_the_frozen_remote_name() {
    let mut fx = ready();
    let limits = Limits::default();
    let sent = message();
    let first = run(&mut fx.conn, &limits, sent.clone());
    assert_eq!(first["status"], "accepted");
    let (from, to, body, needs_reply): (String, String, String, bool) = fx
        .conn
        .query_row(
            "SELECT from_id,to_id,body,needs_reply FROM messages",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        (from.as_str(), to.as_str(), body.as_str(), needs_reply),
        ("peer:p1/alicesessio1", "bob", "hello", false)
    );
    let agents: i64 = fx
        .conn
        .query_row(
            "SELECT count(*) FROM agents WHERE id LIKE 'peer:%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(agents, 0);
    assert_eq!(
        run(&mut fx.conn, &limits, sent.clone())["status"],
        "duplicate"
    );
    let mut changed = sent;
    changed["body"] = json!("something else");
    assert_eq!(run(&mut fx.conn, &limits, changed)["reason"], "conflict");
    let stored: i64 = fx
        .conn
        .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, 1);
    let audited: i64 = fx
        .conn
        .query_row(
            "SELECT count(*) FROM fed_audit WHERE decision='accepted'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(audited, 1);
}

#[test]
fn an_answer_must_reply_to_what_this_side_sent_that_session() {
    let mut fx = ready();
    let ours = "33333333-3333-4333-8333-333333333333";
    let envelope = json!({"to_session": FROM}).to_string();
    fx.conn
        .execute(
            "INSERT INTO fed_outbox (message_id,peer_id,generation,share_id,revision,from_agent,
               envelope_json,bytes,created_at,expires_at,state)
             VALUES (?1,'p1',7,?2,4,'bob',?3,1,1,2,'accepted')",
            rusqlite::params![ours, SHARE, envelope],
        )
        .unwrap();
    let answer = |from: &str| {
        let mut frame = message();
        frame["kind"] = json!("answer");
        frame["reply_to"] = json!(ours);
        frame["from_session"] = json!(from);
        frame
    };
    let limits = Limits::default();
    assert_eq!(
        run(&mut fx.conn, &limits, answer("otherssessio"))["reason"],
        "bad_reply"
    );
    assert_eq!(
        run(&mut fx.conn, &limits, answer(FROM))["status"],
        "accepted"
    );
}

#[test]
fn rates_refill_slowly_and_a_burst_is_bounded() {
    let limits = Limits::default();
    let allowed = (0..10).filter(|_| limits.allow("p1", "bob")).count();
    assert_eq!(allowed, 5, "the recipient's burst is 5");
    assert!(
        limits.allow("p1", "carol"),
        "another recipient is unaffected"
    );
}

#[test]
fn an_old_database_is_rebuilt_once_and_keeps_its_rows_and_the_local_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("axon.db");
    {
        let old = rusqlite::Connection::open(&path).unwrap();
        old.execute_batch(
                "CREATE TABLE agents (id TEXT PRIMARY KEY, harness TEXT NOT NULL, session_id TEXT NOT NULL,
                    agent_ref TEXT, pid INTEGER, parent_id TEXT, root_id TEXT NOT NULL, role TEXT,
                    mission TEXT, model TEXT, repo TEXT, cwd TEXT, worktree TEXT,
                    status TEXT NOT NULL, started_at INTEGER NOT NULL, last_seen_at INTEGER NOT NULL,
                    ended_at INTEGER);
                 INSERT INTO agents (id,harness,session_id,root_id,status,started_at,last_seen_at)
                     VALUES ('a1','claude','s','a1','active',1,1);
                 CREATE TABLE messages (id TEXT PRIMARY KEY, thread TEXT NOT NULL, seq INTEGER NOT NULL,
                    from_id TEXT NOT NULL REFERENCES agents(id), to_id TEXT NOT NULL REFERENCES agents(id),
                    kind TEXT NOT NULL, body TEXT NOT NULL, refs_json TEXT NOT NULL DEFAULT '[]',
                    needs_reply INTEGER NOT NULL DEFAULT 0, deadline INTEGER, default_reply TEXT,
                    delivered_at INTEGER, acked_at INTEGER);
                 INSERT INTO messages (id,thread,seq,from_id,to_id,kind,body) VALUES ('m1','t',1,'a1','a1','note','hi');",
            )
            .unwrap();
    }
    let conn = crate::store::init(&path).unwrap();
    let keyed: i64 = conn
        .query_row(
            "SELECT count(*) FROM pragma_foreign_key_list('messages') WHERE \"from\"='from_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(keyed, 0);
    let insert = |from: &str| {
        conn.execute(
                "INSERT INTO messages (id,thread,seq,from_id,to_id,kind,body) VALUES (?1,'t',2,?2,'a1','note','x')",
                [from, from],
            )
    };
    assert!(
        insert("peer:bob/k3j9x2pq7m4a").is_ok(),
        "a remote sender needs no agents row"
    );
    assert!(
        insert("nobody").is_err(),
        "a local sender must still be a registered agent"
    );
    assert_eq!(
        conn.query_row("SELECT body FROM messages WHERE id='m1'", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "hi"
    );
    drop(conn);
    assert!(
        crate::store::init(&path).is_ok(),
        "a second init finds nothing to rebuild"
    );
}

#[test]
fn a_full_inbox_is_a_final_rejection_unlike_a_busy_receiver() {
    let mut fx = ready();
    for n in 0..PENDING_PER_RECIPIENT {
        fx.conn
            .execute(
                "INSERT INTO messages (id,thread,seq,from_id,to_id,kind,body)
                 VALUES (?1,'t',?2,'peer:p1/alicesessio1','bob','sync','x')",
                rusqlite::params![format!("old-{n}"), n],
            )
            .unwrap();
    }
    let reply = run(&mut fx.conn, &Limits::default(), message());
    assert_eq!(
        (reply["status"].as_str(), reply["reason"].as_str()),
        (Some("rejected"), Some("recipient_full"))
    );
}
