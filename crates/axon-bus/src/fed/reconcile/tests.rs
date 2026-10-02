use super::*;
use crate::store;

fn hub() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let conn = store::init(&dir.path().join("axon.db")).unwrap();
    conn.execute(
        "INSERT INTO peers (peer_id,node_id,label,generation,state,paired_at)
         VALUES ('p','node','other',1,'active',1)",
        [],
    )
    .unwrap();
    (dir, conn)
}

fn remote_paused(conn: &Connection) -> bool {
    conn.query_row("SELECT remote_paused FROM peers", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn a_late_notice_cannot_undo_a_later_one() {
    let (_dir, conn) = hub();
    assert!(learn(&conn, "node", 1, false, 2).unwrap());
    assert!(!learn(&conn, "node", 1, true, 1).unwrap(), "stale pause");
    assert!(!remote_paused(&conn));
    assert!(learn(&conn, "node", 1, true, 3).unwrap());
    assert!(remote_paused(&conn));
    assert!(
        !learn(&conn, "node", 1, false, 3).unwrap(),
        "same seq is a repeat"
    );
}

#[test]
fn the_acknowledgement_carries_our_state_for_the_sender_to_learn() {
    let (_dir, conn) = hub();
    conn.execute("UPDATE peers SET state='paused', lifecycle_seq=4", [])
        .unwrap();
    let reply = acknowledge(&conn, "node").unwrap();
    assert_eq!(reply["lifecycle"], json!({"paused": true, "seq": 4}));
    assert!(learn_reply(&conn, "node", 1, &reply).unwrap());
    assert!(
        remote_paused(&conn),
        "mutual pause is recorded on both sides"
    );
    assert_eq!(
        acknowledge(&conn, "nobody").unwrap()["reason"],
        "unknown_peer"
    );
}
