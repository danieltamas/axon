use serde_json::json;

use super::*;
use crate::fed::testkit::{agent, fixture, node, repo, share};

const SHARE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn ask(conn: &mut Connection, page: usize) -> Value {
    let frame = json!({"type": "discovery", "v": 1, "generation": 7, "share_id": SHARE,
                       "revision": 2, "page": page});
    answer(conn, &node(1), frame).unwrap()
}

#[test]
fn only_registered_open_agents_of_the_share_repo_are_listed_and_only_as_opaque_ids() {
    let mut fx = fixture();
    let project = repo(fx.dir.path(), "project");
    let secret = repo(fx.dir.path(), "secret-repo");
    share(&fx.conn, SHARE, "p1", &project, 2);
    agent(&fx.conn, "in-share", &project, "active");
    agent(&fx.conn, "resting", &project, "idle");
    agent(&fx.conn, "closed-one", &project, "closed");
    agent(&fx.conn, "private", &secret, "active");
    let reply = ask(&mut fx.conn, 0);
    let listed = reply["sessions"].as_array().unwrap();
    assert_eq!(listed.len(), 2);
    for entry in listed {
        assert_eq!(entry["session"].as_str().unwrap().len(), 12);
        assert!(entry["label"].as_str().unwrap().starts_with("claude-"));
    }
    let text = reply.to_string();
    for secret in [
        "in-share",
        "resting",
        "closed-one",
        "private",
        "secret-repo",
        project.to_str().unwrap(),
    ] {
        assert!(!text.contains(secret), "{secret} crossed");
    }
    assert!(reply["next_page"].is_null());
    let again = ask(&mut fx.conn, 0);
    assert_eq!(again["sessions"], reply["sessions"], "sessions are stable");
}

#[test]
fn a_page_holds_a_hundred_and_names_the_next_and_a_stale_ask_is_refused() {
    let mut fx = fixture();
    let project = repo(fx.dir.path(), "project");
    share(&fx.conn, SHARE, "p1", &project, 2);
    for n in 0..101 {
        agent(&fx.conn, &format!("a{n}"), &project, "active");
    }
    let first = ask(&mut fx.conn, 0);
    assert_eq!(first["sessions"].as_array().unwrap().len(), PAGE);
    assert_eq!(first["next_page"], 1);
    let last = ask(&mut fx.conn, 1);
    assert_eq!(last["sessions"].as_array().unwrap().len(), 1);
    assert!(last["next_page"].is_null());
    fx.conn
        .execute("UPDATE peer_shares SET revision=3", [])
        .unwrap();
    assert_eq!(ask(&mut fx.conn, 0)["reason"], "stale_revision");
}

#[test]
fn the_remote_block_lists_an_active_outbound_share_and_nothing_once_it_is_off() {
    let fx = fixture();
    let project = repo(fx.dir.path(), "project");
    share(&fx.conn, SHARE, "p1", &project, 2);
    agent(&fx.conn, "me", &project, "active");
    fx.conn
        .execute(
            "INSERT INTO fed_remote_sessions VALUES ('p1',?1,'k3j9x2pq7m4a','claude-k3j9','active',1)",
            [SHARE],
        )
        .unwrap();
    let block = remote_block(&fx.conn, "me").unwrap().unwrap();
    assert_eq!(
        block,
        "Remote (another person's agents, on their machine; project project):\n  peer:p1/k3j9x2pq7m4a  claude-k3j9  active\n"
    );
    fx.conn
        .execute("UPDATE peer_shares SET outbound=0", [])
        .unwrap();
    assert!(remote_block(&fx.conn, "me").unwrap().is_none());
}
