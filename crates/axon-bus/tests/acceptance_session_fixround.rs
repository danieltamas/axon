//! Work order C1/SEC-1 and SEC-2/CDX-5: origin-scoped session proof and live revocation.
mod common;
use common::fed::api;
use common::sse::Events;
use common::*;
use serde_json::json;
use std::time::{Duration, Instant};

fn sign_in(response: Response) {
    assert_eq!(response.status, 401);
    assert_eq!(parse_json(&response.body)["error"], "sign_in");
    assert_eq!(response.headers["cache-control"], "no-store");
}

#[test]
fn cookie_without_session_token_cannot_get_post_or_open_sse() {
    let bus = Bus::with_limit(Duration::from_secs(15));
    bus.init();
    let server = Server::start(&bus, true);
    for (method, path) in [
        ("GET", "/api/health"),
        ("POST", "/api/fed/invites"),
        ("GET", "/api/stream"),
    ] {
        sign_in(server.raw_request(
            &bus,
            method,
            path,
            &[
                ("Cookie", &server.cookie),
                ("Origin", &server.url),
                ("Content-Type", "application/json"),
            ],
            "{}",
        ));
    }
    assert_eq!(
        server.request(&bus, "GET", "/api/health", &[], "").status,
        200
    );
}

#[test]
fn token_from_another_session_cannot_authorize_cookie_or_stream() {
    let bus = Bus::with_limit(Duration::from_secs(15));
    bus.init();
    let server = Server::start(&bus, true);
    let (other_cookie, other_token) = server.login(&bus);
    assert_ne!(other_token, server.token);
    for (cookie, token) in [
        (&server.cookie, &other_token),
        (&other_cookie, &server.token),
    ] {
        for (method, path) in [("GET", "/api/health"), ("POST", "/api/fed/invites")] {
            sign_in(server.raw_request(
                &bus,
                method,
                path,
                &[
                    ("Cookie", cookie),
                    ("x-axon-session", token),
                    ("Origin", &server.url),
                    ("Content-Type", "application/json"),
                ],
                "{}",
            ));
        }
        sign_in(server.raw_request(
            &bus,
            "GET",
            &format!("/api/stream?t={token}"),
            &[("Cookie", cookie)],
            "",
        ));
    }
    assert_eq!(
        server
            .raw_request(
                &bus,
                "GET",
                "/api/health",
                &[("Cookie", &other_cookie), ("x-axon-session", &other_token)],
                ""
            )
            .status,
        200
    );
}

#[test]
fn revoked_session_token_is_denied_and_its_live_stream_closes_within_one_second() {
    let bus = Bus::with_limit(Duration::from_secs(15));
    bus.init();
    let mut server = Server::start(&bus, true);
    let mut stream = Events::open(&bus, &server);
    let revoked_cookie = server.cookie.clone();
    let revoked_token = server.token.clone();
    (server.cookie, server.token) = server.login(&bus);
    api(
        &bus,
        &server,
        "POST",
        "/api/settings/sessions/revoke_others",
        json!({}),
    );
    let revoked_at = Instant::now();
    stream.assert_closed_within(Duration::from_secs(1));
    assert!(revoked_at.elapsed() <= Duration::from_secs(1));
    sign_in(server.raw_request(
        &bus,
        "GET",
        "/api/health",
        &[
            ("Cookie", &revoked_cookie),
            ("x-axon-session", &revoked_token),
        ],
        "",
    ));
    sign_in(server.raw_request(
        &bus,
        "GET",
        &format!("/api/stream?t={revoked_token}"),
        &[("Cookie", &revoked_cookie)],
        "",
    ));
    assert_eq!(
        server.request(&bus, "GET", "/api/health", &[], "").status,
        200
    );
    assert!(
        server.process.0.try_wait().unwrap().is_none(),
        "revocation must not stop the server"
    );
    let _survivor_stream = Events::open(&bus, &server);
}
