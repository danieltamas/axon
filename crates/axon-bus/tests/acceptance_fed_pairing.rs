//! P2P-SPEC §§0, 3–5: A03–A06, A29 filesystem portion and A33.
//! Unix permission bits are skipped on Windows; cross-account/ACL checks are manual (§11).
//! §4 wins over A33's "no peer row": a removed tombstone conveys no authority.
mod common;
use common::fed::*;
use common::invite::{decode, encode};
use common::*;
use serde_json::{json, Value};
use std::time::Duration;

fn instance() -> (Bus, Server) {
    let bus = Bus::with_limit(Duration::from_secs(50));
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
fn invite(bus: &Bus, server: &Server) -> Value {
    api(bus, server, "POST", "/api/fed/invites", json!({}))
}
fn join(bus: &Bus, server: &Server, code: &Value, label: &str) -> Response {
    request(
        bus,
        server,
        "POST",
        "/api/fed/join",
        json!({"invite":code,"label":label}),
    )
}
fn invalid(response: Response) {
    assert_eq!(response.status, 400);
    assert_eq!(
        parse_json(&response.body),
        json!({"error":"invalid_invite"})
    );
}

#[test]
fn settings_pairing_needs_both_confirmations_and_starts_with_zero_grants() {
    let pair = Pair::pending(Duration::from_secs(45));
    for bus in [&pair.a, &pair.b] {
        assert_eq!(count(bus, "SELECT count(*) FROM peers WHERE state='pending_confirm' AND local_confirmed_at IS NULL AND remote_confirmed_at IS NULL"), 1);
        assert_eq!(count(bus, "SELECT count(*) FROM peer_shares"), 0);
    }
    pair.confirm();
    for bus in [&pair.a, &pair.b] {
        assert_eq!(count(bus, "SELECT count(*) FROM peers WHERE state='active' AND local_confirmed_at IS NOT NULL AND remote_confirmed_at IS NOT NULL"), 1);
        assert_eq!(count(bus, "SELECT count(*) FROM peer_shares"), 0);
        assert_eq!(count(bus, "SELECT count(*) FROM fed_remote_sessions"), 0);
    }
    let listing = pair.a.ok(&["peers", "--agent", "agent-a"]);
    assert!(!String::from_utf8_lossy(&listing.stdout).contains("peer:"));
    let label = peer(&pair.a, &pair.sa, &pair.pa)["label"]
        .as_str()
        .unwrap()
        .to_owned();
    refused(
        &pair.a,
        "agent-a",
        &format!("peer:{label}/abcdefghijkl"),
        "question",
        "no shares",
        "unknown_session",
    );
}

#[test]
fn invitation_has_fixed_version_ten_minute_lifetime_and_no_public_secret_in_health_or_sql() {
    let (bus, server) = instance();
    let before = now_ms();
    let created = invite(&bus, &server);
    let dto = decode(created["invite"].as_str().unwrap());
    assert_eq!(dto["v"], 1);
    assert_eq!(dto["invite_id"], created["invite_id"]);
    assert_eq!(dto["expires_at"], created["expires_at"]);
    assert!((before + 600_000..=now_ms() + 600_000).contains(&dto["expires_at"].as_i64().unwrap()));
    assert_eq!(dto["node_id"], health(&bus, &server)["node_id"]);
    assert!(dto["relay_url"].is_null(), "debug seam disables relays");
    assert!(!dto["direct_addrs"].as_array().unwrap().is_empty());
    for address in dto["direct_addrs"].as_array().unwrap() {
        assert!(address
            .as_str()
            .unwrap()
            .parse::<std::net::SocketAddr>()
            .unwrap()
            .ip()
            .is_loopback());
    }
    let secret = dto["secret"].as_str().unwrap();
    assert_eq!(
        secret.len(),
        43,
        "32 bytes, same base64url representation as nonces"
    );
    let hash = text(
        &bus,
        "SELECT secret_hash FROM peer_invites WHERE invite_id=?1",
        dto["invite_id"].as_str().unwrap(),
    );
    assert_eq!(hash.len(), 64);
    assert!(hash.bytes().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(hash, secret);
    assert!(!health(&bus, &server).to_string().contains(secret));
}

#[test]
fn replacing_and_cancelling_invites_invalidates_them_without_consuming_the_new_one() {
    let (a, sa) = instance();
    let (b, sb) = instance();
    let first = invite(&a, &sa);
    let second = invite(&a, &sa);
    invalid(join(&b, &sb, &first["invite"], "alice"));
    assert_eq!(
        count(
            &a,
            "SELECT count(*) FROM peer_invites WHERE cancelled_at IS NULL AND consumed_by IS NULL"
        ),
        1
    );
    api(
        &a,
        &sa,
        "DELETE",
        &format!("/api/fed/invites/{}", second["invite_id"].as_str().unwrap()),
        json!({}),
    );
    invalid(join(&b, &sb, &second["invite"], "alice"));
    assert_eq!(count(&a, "SELECT count(*) FROM peers"), 0);
    assert_eq!(count(&b, "SELECT count(*) FROM peers"), 0);
}

#[test]
fn fifth_wrong_secret_kills_invite_and_attempts_survive_restart() {
    let (a, sa) = instance();
    let (b, sb) = instance();
    let valid = invite(&a, &sa);
    let mut bad = decode(valid["invite"].as_str().unwrap());
    let mut secret = bad["secret"].as_str().unwrap().as_bytes().to_vec();
    secret[0] = if secret[0] == b'A' { b'B' } else { b'A' };
    bad["secret"] = json!(String::from_utf8(secret).unwrap());
    for attempt in 1..=4 {
        invalid(join(&b, &sb, &json!(encode(&bad)), "alice"));
        assert_eq!(
            count(
                &a,
                "SELECT attempts FROM peer_invites WHERE cancelled_at IS NULL"
            ),
            attempt
        );
    }
    invalid(join(&b, &sb, &json!(encode(&bad)), "alice"));
    assert_eq!(count(&a, "SELECT max(attempts) FROM peer_invites"), 5);
    invalid(join(&b, &sb, &valid["invite"], "alice"));
    assert_eq!(
        count(&a, "SELECT count(*) FROM peers WHERE state='active'"),
        0
    );
    drop(sa);
    let sa = Server::start(&a, true);
    assert_eq!(count(&a, "SELECT max(attempts) FROM peer_invites"), 5);
    assert_eq!(health(&a, &sa)["enabled"], true);
}

#[test]
fn four_failed_attempts_still_allow_one_valid_join_and_same_node_retry_is_idempotent() {
    let (a, sa) = instance();
    let (b, sb) = instance();
    let code = invite(&a, &sa);
    let mut bad = decode(code["invite"].as_str().unwrap());
    bad["secret"] = json!("A".repeat(43));
    for _ in 0..4 {
        invalid(join(&b, &sb, &json!(encode(&bad)), "alice"));
    }
    assert!((200..300).contains(&join(&b, &sb, &code["invite"], "alice").status));
    let id = live_id(&a);
    assert!((200..300).contains(&join(&b, &sb, &code["invite"], "alice").status));
    assert_eq!(live_id(&a), id);
    assert_eq!(count(&a, "SELECT count(*) FROM peers"), 1);
    assert_eq!(count(&b, "SELECT count(*) FROM peers"), 1);
    assert_eq!(
        count(
            &a,
            "SELECT count(*) FROM peer_invites WHERE consumed_by IS NOT NULL"
        ),
        1
    );
}

#[test]
fn simultaneous_distinct_joiners_consume_an_invite_exactly_once() {
    let (a, sa) = instance();
    let (b, sb) = instance();
    let (c, sc) = instance();
    let invitation = invite(&a, &sa);
    let (rb, rc) = std::thread::scope(|scope| {
        let one = scope.spawn(|| join(&b, &sb, &invitation["invite"], "alice"));
        let two = scope.spawn(|| join(&c, &sc, &invitation["invite"], "alice"));
        (one.join().unwrap(), two.join().unwrap())
    });
    if (200..300).contains(&rb.status) {
        invalid(rc);
    } else {
        invalid(rb);
        assert!((200..300).contains(&rc.status));
    }
    assert_eq!(
        count(
            &a,
            "SELECT count(*) FROM peers WHERE state='pending_confirm'"
        ),
        1
    );
    assert_eq!(
        count(
            &a,
            "SELECT count(*) FROM peer_invites WHERE consumed_by IS NOT NULL"
        ),
        1
    );
}

#[test]
fn wrong_pair_code_or_explicit_reject_removes_authority_and_consumes_invitation() {
    for mismatch in [true, false] {
        let pair = Pair::pending(Duration::from_secs(45));
        if mismatch {
            let response = request(
                &pair.a,
                &pair.sa,
                "POST",
                &format!("/api/fed/peers/{}/confirm", pair.pa),
                json!({"pair_code":"wrong"}),
            );
            assert_eq!(response.status, 400);
            assert_eq!(
                parse_json(&response.body),
                json!({"error":"pair_code_mismatch"})
            );
        } else {
            api(
                &pair.a,
                &pair.sa,
                "POST",
                &format!("/api/fed/peers/{}/reject", pair.pa),
                json!({}),
            );
        }
        for bus in [&pair.a, &pair.b] {
            eventually(bus, "rejected peer conveys no authority", || {
                count(bus, "SELECT count(*) FROM peers WHERE state<>'removed'") == 0
            });
            assert_eq!(
                count(bus, "SELECT count(*) FROM peer_shares WHERE state='active'"),
                0
            );
        }
        assert_eq!(
            count(
                &pair.a,
                "SELECT count(*) FROM peer_invites WHERE consumed_by IS NOT NULL"
            ),
            1
        );
    }
}

#[test]
fn invalid_label_boundaries_and_protocol_version_do_not_create_a_peer() {
    let (a, sa) = instance();
    let (b, sb) = instance();
    let invitation = invite(&a, &sa);
    for label in [
        "".to_owned(),
        "a".repeat(33),
        "two words".into(),
        "../x/y".into(),
        "é".into(),
    ] {
        assert_eq!(join(&b, &sb, &invitation["invite"], &label).status, 400);
        assert_eq!(count(&b, "SELECT count(*) FROM peers"), 0);
    }
    let mut future = decode(invitation["invite"].as_str().unwrap());
    future["v"] = json!(2);
    assert_eq!(join(&b, &sb, &json!(encode(&future)), "alice").status, 400);
    assert_eq!(count(&a, "SELECT count(*) FROM peers"), 0);
    assert!((200..300).contains(&join(&b, &sb, &invitation["invite"], &"a".repeat(32)).status));
}

#[test]
fn identity_is_lazy_stable_and_missing_or_corrupt_keys_never_regenerate_beside_peers() {
    for missing in [true, false] {
        let pair = Pair::paired(Duration::from_secs(50));
        let Pair {
            a,
            sa,
            b: _b,
            sb: _sb,
            pa,
            ..
        } = pair;
        let key = a.root.join("data/axon/fed/identity.key");
        let original = std::fs::read(&key).unwrap();
        let node = health(&a, &sa)["node_id"].clone();
        drop(sa);
        let restarted = Server::start(&a, true);
        assert_eq!(health(&a, &restarted)["node_id"], node);
        assert_eq!(std::fs::read(&key).unwrap(), original);
        drop(restarted);
        if missing {
            std::fs::remove_file(&key).unwrap();
        } else {
            std::fs::write(&key, b"corrupt identity").unwrap();
        }
        let broken = Server::start(&a, true);
        assert_eq!(health(&a, &broken)["enabled"], false);
        assert_eq!(
            text(&a, "SELECT last_error FROM peers WHERE peer_id=?1", &pa),
            "identity key missing or unreadable"
        );
        if missing {
            assert!(!key.exists());
        } else {
            assert_eq!(std::fs::read(&key).unwrap(), b"corrupt identity");
        }
        assert_eq!(
            broken.request(&a, "GET", "/api/settings", &[], "").status,
            200
        );
    }
}

#[cfg(unix)]
#[test]
fn identity_has_owner_only_modes_and_symlink_destination_is_never_overwritten() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let (bus, server) = instance();
    let directory = bus.root.join("data/axon/fed");
    let key = directory.join("identity.key");
    assert_eq!(
        std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(server);
    std::fs::remove_file(&key).unwrap();
    let sentinel = bus.root.join("sentinel");
    std::fs::write(&sentinel, b"do not replace").unwrap();
    symlink(&sentinel, &key).unwrap();
    let restarted = Server::start(&bus, true);
    assert_eq!(health(&bus, &restarted)["enabled"], false);
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"do not replace");
    assert!(std::fs::symlink_metadata(&key)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn schema_exposes_every_contract_table_column_and_the_live_identity_unique_index() {
    common::pairing_checks::schema();
}

#[test]
fn pending_confirmation_survives_restart_and_expires_at_ten_minutes() {
    common::pairing_checks::confirmation_timeout();
}

#[test]
fn invitation_pin_mismatch_does_not_send_the_secret_or_replace_a_pin() {
    common::pairing_checks::pinned_key();
}

#[test]
fn peer_labels_accept_one_and_thirty_two_characters_but_must_be_unique() {
    common::pairing_checks::label_uniqueness();
}

#[test]
fn invitation_is_still_live_before_ten_minutes_and_refused_afterwards() {
    common::pairing_checks::invite_expiry();
}
