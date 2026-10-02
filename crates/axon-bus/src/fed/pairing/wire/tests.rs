use super::*;
use crate::fed::testkit::{fixture, node};

fn peer_row(conn: &Connection) -> (String, Option<String>, bool) {
    conn.query_row(
        "SELECT state, removed_reason, remote_paused FROM peers WHERE peer_id='p1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .unwrap()
}

#[test]
fn a_removal_notice_ends_a_live_pairing_and_says_who_ended_it() {
    let mut fx = fixture();
    let reply = remote_removed(&mut fx.conn, &node(1), 7).unwrap();
    assert_eq!(reply["status"], "accepted");
    assert_eq!(
        peer_row(&fx.conn),
        ("removed".into(), Some("remote_removed".into()), false)
    );
    assert_eq!(
        remote_removed(&mut fx.conn, &node(1), 7).unwrap()["reason"],
        "not_pending",
        "a second notice finds nothing live"
    );
}

#[test]
fn a_removal_notice_for_another_generation_changes_nothing() {
    let mut fx = fixture();
    assert_eq!(
        remote_removed(&mut fx.conn, &node(1), 6).unwrap()["reason"],
        "not_pending"
    );
    assert_eq!(peer_row(&fx.conn).0, "active");
}

#[test]
fn pause_and_resume_notices_set_and_clear_the_flag_without_touching_our_state() {
    let mut fx = fixture();
    set_remote_paused(&mut fx.conn, &node(1), 7, true).unwrap();
    assert_eq!(peer_row(&fx.conn), ("active".into(), None, true));
    set_remote_paused(&mut fx.conn, &node(1), 7, false).unwrap();
    assert_eq!(peer_row(&fx.conn), ("active".into(), None, false));
    assert_eq!(
        set_remote_paused(&mut fx.conn, &node(9), 7, true).unwrap()["reason"],
        "unknown_peer"
    );
}
