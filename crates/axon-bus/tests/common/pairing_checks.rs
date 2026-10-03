//! P2P-SPEC §§3–5: schema and pairing time/pin checks, without source dependencies.
use super::fed::*;
use super::invite::{decode, encode};
use super::{now_ms, parse_json, Bus, Server};
use serde_json::json;
use std::time::Duration;

fn instance(seconds: u64) -> (Bus, Server) {
    let bus = Bus::with_limit(Duration::from_secs(seconds));
    bus.init();
    let server = Server::start(&bus, true);
    api(
        &bus,
        &server,
        "PUT",
        "/api/settings/federation",
        json!({"enabled":true}),
    );
    (bus, server)
}
pub fn schema() {
    let (bus, _server) = instance(15);
    let expected = [
        ("dashboard_sessions", "session_hash created_at last_used_at expires_at"),
        ("login_nonces", "nonce_hash expires_at"),
        ("peers", "peer_id node_id label generation state local_confirmed_at remote_confirmed_at paired_at paused_at removed_at removed_reason last_error"),
        ("peer_invites", "invite_id secret_hash expires_at attempts consumed_by cancelled_at"),
        ("peer_shares", "share_id peer_id label local_repo inbound outbound remote_inbound remote_outbound revision state root_commit"),
        ("fed_sessions", "session agent_id share_id"),
        ("fed_outbox", "message_id peer_id generation share_id revision from_agent envelope_json bytes created_at expires_at state attempts next_attempt_at last_error"),
        ("fed_inbox", "peer_id generation message_id content_hash local_message_id accepted_at expires_at"),
        ("fed_remote_sessions", "peer_id share_id session label availability seen_at"),
        ("fed_audit", "seq ts peer_fingerprint generation share_id message_id direction decision reason"),
    ];
    for (table, names) in expected {
        let columns: Vec<String> = db(&bus)
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for name in names.split_whitespace() {
            assert!(
                columns.iter().any(|column| column == name),
                "{table}.{name}"
            );
        }
    }
    let index: (i64, i64) = db(&bus).query_row("SELECT \"unique\",partial FROM pragma_index_list('peers') WHERE name='peers_live_node'", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(index, (1, 1));
}
pub fn confirmation_timeout() {
    let pair = Pair::pending(Duration::from_secs(45));
    let Pair {
        a,
        sa,
        b: _b,
        sb: _sb,
        pa,
        ..
    } = pair;
    drop(sa);
    let before = Server::shifted(&a, true, 590_000);
    assert_eq!(
        text(&a, "SELECT state FROM peers WHERE peer_id=?1", &pa),
        "pending_confirm"
    );
    drop(before);
    let after = Server::shifted(&a, true, 601_000);
    eventually(&a, "confirmation expires", || {
        text(&a, "SELECT state FROM peers WHERE peer_id=?1", &pa) == "removed"
    });
    assert_eq!(
        text(&a, "SELECT removed_reason FROM peers WHERE peer_id=?1", &pa),
        "confirm_timeout"
    );
    assert_eq!(count(&a, "SELECT count(*) FROM peer_shares"), 0);
    assert_eq!(peer(&a, &after, &pa)["state"], "removed");
}
pub fn pinned_key() {
    let (a, sa) = instance(45);
    let (b, sb) = instance(45);
    let (c, sc) = instance(45);
    let invitation = api(&a, &sa, "POST", "/api/fed/invites", json!({}));
    let mut tampered = decode(invitation["invite"].as_str().unwrap());
    tampered["node_id"] = health(&c, &sc)["node_id"].clone();
    let response = request(
        &b,
        &sb,
        "POST",
        "/api/fed/join",
        json!({"invite":encode(&tampered),"label":"alice"}),
    );
    assert!((400..600).contains(&response.status));
    assert!(parse_json(&response.body)["error"].is_string());
    assert_eq!(
        count(
            &a,
            "SELECT attempts FROM peer_invites WHERE consumed_by IS NULL"
        ),
        0
    );
    for bus in [&a, &b, &c] {
        assert_eq!(count(bus, "SELECT count(*) FROM peers"), 0);
    }
    assert!(!String::from_utf8_lossy(&response.body).contains(tampered["secret"].as_str().unwrap()));
}
pub fn label_uniqueness() {
    let (a, sa) = instance(45);
    let (b, sb) = instance(45);
    let (c, sc) = instance(45);
    let invitation = api(&a, &sa, "POST", "/api/fed/invites", json!({}));
    let max_label = "a".repeat(32);
    api(
        &b,
        &sb,
        "POST",
        "/api/fed/join",
        json!({"invite":invitation["invite"],"label":max_label}),
    );
    let another = api(&c, &sc, "POST", "/api/fed/invites", json!({}));
    let response = request(
        &b,
        &sb,
        "POST",
        "/api/fed/join",
        json!({"invite":another["invite"],"label":max_label}),
    );
    assert_eq!(response.status, 400);
    assert_eq!(
        count(&b, "SELECT count(*) FROM peers WHERE state<>'removed'"),
        1
    );
    api(
        &b,
        &sb,
        "POST",
        "/api/fed/join",
        json!({"invite":another["invite"],"label":"b"}),
    );
    assert_eq!(
        count(&b, "SELECT count(*) FROM peers WHERE state<>'removed'"),
        2
    );
}
pub fn invite_expiry() {
    let (a, sa) = instance(45);
    let (b, sb) = instance(45);
    let offset_file = a.root.join("now-offset-ms");
    let invitation = api(&a, &sa, "POST", "/api/fed/invites", json!({}));
    let expires = invitation["expires_at"].as_i64().unwrap();
    let mut tampered = decode(invitation["invite"].as_str().unwrap());
    tampered["secret"] = json!("A".repeat(43));
    // Keep the issuer's advertised :0 address and the joiner's clock unchanged.
    std::fs::write(&offset_file, (expires - 5_000 - now_ms()).to_string()).unwrap();
    let wrong = request(
        &b,
        &sb,
        "POST",
        "/api/fed/join",
        json!({"invite":encode(&tampered),"label":"alice"}),
    );
    assert_eq!(wrong.status, 400);
    assert_eq!(parse_json(&wrong.body), json!({"error":"invalid_invite"}));
    assert_eq!(
        count(&a, "SELECT attempts FROM peer_invites"),
        1,
        "not expired before the cut"
    );
    std::fs::write(&offset_file, (expires + 1 - now_ms()).to_string()).unwrap();
    let late = request(
        &b,
        &sb,
        "POST",
        "/api/fed/join",
        json!({"invite":invitation["invite"],"label":"alice"}),
    );
    assert_eq!(late.status, 400);
    assert_eq!(parse_json(&late.body), json!({"error":"invalid_invite"}));
    assert_eq!(count(&a, "SELECT count(*) FROM peers"), 0);
}
