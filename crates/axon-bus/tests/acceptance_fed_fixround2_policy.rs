//! RR-14 and RR-13: refuse unsafe identity permissions and plaintext relay invitations.
mod common;
use common::fed::*;
use common::invite::{decode, encode};
use common::*;
use serde_json::json;
use std::time::Duration;

fn enabled(bus: &Bus) -> Server {
    bus.init();
    let server = Server::start(bus, true);
    api(
        bus,
        &server,
        "PUT",
        "/api/settings/federation",
        json!({"enabled":true}),
    );
    assert_eq!(health(bus, &server)["enabled"], true);
    server
}

#[cfg(unix)]
fn unsafe_key_is_refused(on_start: bool) {
    use std::os::unix::fs::PermissionsExt;
    let bus = Bus::with_limit(Duration::from_secs(30));
    let mut server = enabled(&bus);
    let path = bus.root.join("data/axon/fed/identity.key");
    let original = std::fs::read(&path).unwrap();
    assert_eq!(original.len(), 32);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let mut diagnostic = String::new();
    if on_start {
        server.process.0.kill().unwrap();
        server.process.0.wait().unwrap();
    } else {
        api(
            &bus,
            &server,
            "PUT",
            "/api/settings/federation",
            json!({"enabled":false}),
        );
        assert_eq!(health(&bus, &server)["enabled"], false);
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    if on_start {
        server = Server::start(&bus, true);
    } else {
        let response = request(
            &bus,
            &server,
            "PUT",
            "/api/settings/federation",
            json!({"enabled":true}),
        );
        assert!(
            (400..600).contains(&response.status),
            "RR-14: enable accepted an identity readable by others"
        );
        diagnostic.push_str(&String::from_utf8_lossy(&response.body));
    }
    let state = health(&bus, &server);
    assert_eq!(
        state["enabled"], false,
        "RR-14: unsafe identity must not start federation"
    );
    diagnostic.push_str(&state.to_string());
    server.process.0.kill().unwrap();
    let output = server.process.finish(Duration::from_secs(2));
    diagnostic.push_str(&String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o644,
        "RR-14: refusal must not silently chmod the key"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "identity must not be replaced"
    );
    assert!(
        diagnostic.contains("identity.key"),
        "RR-14: error must name the unsafe file: {diagnostic}"
    );
    assert!(
        diagnostic.contains("0600") || diagnostic.contains("chmod 600"),
        "RR-14: error must explain the permission repair: {diagnostic}"
    );
}

#[cfg(unix)]
#[test]
fn enabling_federation_refuses_0644_identity_without_repairing_it() {
    unsafe_key_is_refused(false);
}

#[cfg(unix)]
#[test]
fn starting_federation_refuses_0644_identity_without_repairing_it() {
    unsafe_key_is_refused(true);
}

#[test]
fn join_refuses_http_relay_invite_without_consuming_the_valid_invite() {
    let a = Bus::with_limit(Duration::from_secs(40));
    let b = Bus::with_limit(Duration::from_secs(40));
    let sa = enabled(&a);
    let sb = enabled(&b);
    let invitation = api(&a, &sa, "POST", "/api/fed/invites", json!({}));
    let mut tampered = decode(invitation["invite"].as_str().unwrap());
    // All addresses stay on loopback; the malformed scheme must be rejected before dialing.
    tampered["relay_url"] = json!("http://127.0.0.1:9");
    let response = request(
        &b,
        &sb,
        "POST",
        "/api/fed/join",
        json!({"invite":encode(&tampered),"label":"alice"}),
    );
    assert_eq!(
        response.status, 400,
        "RR-13: plaintext invite relay must be invalid"
    );
    assert_eq!(parse_json(&response.body)["error"], "invalid_invite");
    assert_eq!(count(&a, "SELECT count(*) FROM peers"), 0);
    assert_eq!(count(&b, "SELECT count(*) FROM peers"), 0);
    api(
        &b,
        &sb,
        "POST",
        "/api/fed/join",
        json!({"invite":invitation["invite"],"label":"alice"}),
    );
    for bus in [&a, &b] {
        eventually(bus, "untampered invite still joins", || {
            count(
                bus,
                "SELECT count(*) FROM peers WHERE state='pending_confirm'",
            ) == 1
        });
    }
}
