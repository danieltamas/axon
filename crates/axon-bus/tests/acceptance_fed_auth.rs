//! P2P-SPEC §§0–1, 5: owner login, public shell, protected APIs; A01/A02 dashboard side.
//! Sessions survive restart (§1 overrides the older A01 per-boot expectation).
mod common;
use common::fed::*;
use common::*;
use serde_json::json;
use std::time::Duration;

fn anonymous(seconds: u64) -> (Bus, Server) {
    let bus = Bus::with_limit(Duration::from_secs(seconds));
    bus.init();
    let server = Server::anonymous(&bus, true, 0);
    (bus, server)
}
fn denied(response: &Response, exchange: bool) {
    assert_eq!(response.status, 401);
    assert_eq!(
        parse_json(&response.body),
        if exchange {
            json!({"error":"sign_in"})
        } else {
            json!({"error":"sign_in","hint":"run axon open"})
        }
    );
    assert_eq!(
        response.headers.get("cache-control").map(String::as_str),
        Some("no-store")
    );
}

#[test]
fn public_assets_disclose_no_page_token_or_registered_agent_data() {
    let (bus, server) = anonymous(12);
    bus.register("private-agent-marker", "claude", None);
    for path in [
        "/",
        "/app.js",
        "/manifest.webmanifest",
        "/sw.js",
        "/offline.html",
    ] {
        let response = server.raw_request(&bus, "GET", path, &[], "");
        assert_eq!(response.status, 200, "public {path}");
        let body = String::from_utf8(response.body).unwrap();
        // §1 Removed: a publicly readable page must never bootstrap an owner credential.
        for secret in ["axon-token", "x-axon-token", "private-agent-marker"] {
            assert!(
                !body.to_lowercase().contains(secret),
                "{path} exposes {secret}"
            );
        }
    }
}

#[test]
fn every_api_read_write_and_sse_requires_a_session_even_with_legacy_header() {
    let (bus, server) = anonymous(15);
    let before = count(&bus, "SELECT count(*) FROM messages");
    for (method, path) in [
        ("GET", "/api/snapshot"),
        ("GET", "/api/health"),
        ("GET", "/api/fed"),
        ("GET", "/api/settings"),
        ("GET", "/api/stream"),
        ("POST", "/api/fed/invites"),
        ("POST", "/api/fed/join"),
        ("POST", "/api/fed/peers/nonexistent/shares"),
        ("PUT", "/api/settings/federation"),
        ("POST", "/api/msg"),
    ] {
        for headers in [
            vec![("Origin", server.url.as_str())],
            vec![
                ("Origin", server.url.as_str()),
                ("x-axon-token", "legacy-token"),
            ],
            vec![
                ("Origin", server.url.as_str()),
                ("Cookie", "axon_session=wrong"),
            ],
        ] {
            denied(
                &server.raw_request(&bus, method, path, &headers, "{}"),
                false,
            );
        }
    }
    assert_eq!(count(&bus, "SELECT count(*) FROM messages"), before);
}

#[test]
fn login_nonces_are_random_single_use_and_sessions_are_hashed_with_thirty_day_expiry() {
    let (bus, server) = anonymous(12);
    let first = server.nonce(&bus);
    let second = server.nonce(&bus);
    assert_ne!(first, second);
    let nonce_rows: Vec<(String, i64)> = db(&bus)
        .prepare("SELECT nonce_hash,expires_at FROM login_nonces")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(nonce_rows.len() >= 2);
    for (hash, expiry) in nonce_rows {
        assert_eq!(hash.len(), 64);
        assert!(hash.bytes().all(|c| c.is_ascii_hexdigit()));
        assert!(!hash.contains(&first) && !hash.contains(&second));
        assert!((55_000..=60_000).contains(&(expiry - now_ms())));
    }
    let before = now_ms();
    let response = server.exchange(&bus, &first);
    assert_eq!(response.status, 204);
    let cookie = response.headers["set-cookie"].split(';').next().unwrap();
    let secret = cookie.strip_prefix("axon_session=").unwrap();
    assert_eq!(secret.len(), 43);
    assert_eq!(count(&bus, "SELECT count(*) FROM dashboard_sessions"), 1);
    let (hash, created, used, expiry): (String, i64, i64, i64) = db(&bus)
        .query_row(
            "SELECT session_hash,created_at,last_used_at,expires_at FROM dashboard_sessions",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(hash.len(), 64);
    assert!(hash.bytes().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(hash, secret);
    assert!((before..=now_ms()).contains(&created));
    assert_eq!(expiry - created, 30 * 24 * 60 * 60 * 1000);
    assert!(used >= created);
    denied(&server.exchange(&bus, &first), true);
    denied(&server.exchange(&bus, &"x".repeat(43)), true);
    let authenticated = server.raw_request(&bus, "GET", "/api/settings", &[("Cookie", cookie)], "");
    assert_eq!(authenticated.status, 200);
    let after: i64 = db(&bus)
        .query_row("SELECT last_used_at FROM dashboard_sessions", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(after >= used);
    // The helper separately checks every required cookie attribute.
    assert_ne!(server.login(&bus), cookie);
}

#[test]
fn nonce_is_usable_before_sixty_seconds_and_expired_after_sixty_seconds() {
    // §0 shifts only the federation clock, so session expiry uses real wall time.
    let (bus, server) = anonymous(75);
    let early = server.nonce(&bus);
    let late = server.nonce(&bus);
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(58) {
        bus.remaining();
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(server.exchange(&bus, &early).status, 204);
    while start.elapsed() < Duration::from_secs(61) {
        bus.remaining();
        std::thread::sleep(Duration::from_millis(100));
    }
    denied(&server.exchange(&bus, &late), true);
    denied(&server.exchange(&bus, &early), true);
}

#[test]
fn login_requires_loopback_host_and_matching_origin_without_consuming_nonce_on_failure() {
    let (bus, server) = anonymous(12);
    let nonce = server.nonce(&bus);
    let body = json!({"nonce":nonce}).to_string();
    for headers in [
        vec![("Host", "evil.invalid"), ("Origin", server.url.as_str())],
        vec![("Origin", "https://evil.invalid")],
        vec![],
    ] {
        let response = server.raw_request(&bus, "POST", "/api/session", &headers, &body);
        assert_eq!(response.status, 403);
        assert_eq!(count(&bus, "SELECT count(*) FROM dashboard_sessions"), 0);
    }
    assert_eq!(server.exchange(&bus, &nonce).status, 204);
}

#[test]
fn authenticated_writes_still_enforce_host_and_origin_and_dashboard_is_loopback() {
    let (bus, mut server) = anonymous(12);
    server.cookie = server.login(&bus);
    assert!(server.address.ip().is_loopback());
    for (method, path) in [
        ("PUT", "/api/settings/capture"),
        ("POST", "/api/fed/invites"),
        ("DELETE", "/api/fed/peers/unknown"),
    ] {
        for headers in [
            vec![("Host", "evil.invalid"), ("Origin", server.url.as_str())],
            vec![("Origin", "https://evil.invalid")],
            vec![],
        ] {
            let response = server.request(&bus, method, path, &headers, "{}");
            assert_eq!(response.status, 403);
        }
    }
}

#[test]
fn restart_keeps_sessions_and_revoke_others_preserves_exactly_the_calling_browser() {
    let (bus, mut first) = anonymous(20);
    first.cookie = first.login(&bus);
    let retained = first.cookie.clone();
    let other = first.login(&bus);
    drop(first);
    let mut second = Server::anonymous(&bus, true, 0);
    second.cookie = retained.clone();
    assert_eq!(
        second.request(&bus, "GET", "/api/settings", &[], "").status,
        200
    );
    assert_eq!(
        second
            .raw_request(&bus, "GET", "/api/settings", &[("Cookie", &other)], "")
            .status,
        200
    );
    let settings = api(
        &bus,
        &second,
        "POST",
        "/api/settings/sessions/revoke_others",
        json!({}),
    );
    assert_eq!(settings["storage"]["sessions"], 1);
    assert_eq!(count(&bus, "SELECT count(*) FROM dashboard_sessions"), 1);
    denied(
        &second.raw_request(&bus, "GET", "/api/settings", &[("Cookie", &other)], ""),
        false,
    );
    assert_eq!(
        second.request(&bus, "GET", "/api/settings", &[], "").status,
        200
    );
}

#[test]
fn operator_send_refuses_remote_principals_without_creating_any_message() {
    let (bus, mut server) = anonymous(12);
    server.cookie = server.login(&bus);
    bus.register("local", "claude", None);
    let before = count(&bus, "SELECT count(*) FROM messages");
    let response = request(
        &bus,
        &server,
        "POST",
        "/api/msg",
        json!({"from_id":"local",
        "to_id":"peer:alice/abcdefghijkl","kind":"question","body":"private"}),
    );
    assert_eq!(response.status, 400);
    assert_eq!(parse_json(&response.body), json!({"error":"remote_target"}));
    assert_eq!(count(&bus, "SELECT count(*) FROM messages"), before);
}
