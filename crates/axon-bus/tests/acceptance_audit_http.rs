//! TEST-10 and TEST-11: audit chain heads, HTTP Origin and self-send boundaries.
//! TEST-11 UI draft preservation is manual-only: type an unsent draft, receive a
//! snapshot update, and verify that the draft survives. No DOM test is claimed here.

mod common;
use common::*;
use serde_json::json;

fn head(bus: &Bus) -> String {
    let output = bus.ok(&["audit", "--verify"]);
    let text = String::from_utf8(output.stdout).unwrap();
    let fields: Vec<&str> = text.split_whitespace().collect();
    let position = fields
        .iter()
        .position(|field| *field == "head")
        .unwrap_or_else(|| panic!("audit stdout has no 'head <64-hex>': {text}"));
    let hash = fields.get(position + 1).expect("missing head hash");
    assert_eq!(hash.len(), 64, "{text}");
    assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()), "{text}");
    (*hash).to_owned()
}

#[test]
fn test_10_audit_stdout_head_equals_last_event_hash() {
    let bus = Bus::new();
    bus.init();
    bus.register("first", "claude", None);
    let seq = bus.count("SELECT max(seq) FROM events");
    let reported = head(&bus);
    assert_eq!(
        bus.count("SELECT max(seq) FROM events"),
        seq,
        "audit must be read-only"
    );
    // §2 stores prev_hash, not a redundant row hash. The next append exposes the
    // previous last event's hash without copying the implementation's hash algorithm.
    bus.register("second", "claude", None);
    let next_previous: String = bus
        .db()
        .query_row(
            "SELECT prev_hash FROM events WHERE seq>?1 ORDER BY seq LIMIT 1",
            [seq],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reported, next_previous);
    assert_ne!(reported, "0".repeat(64));
    assert_ne!(head(&bus), reported);
}

#[test]
fn test_10_different_event_chains_have_different_heads() {
    let one = Bus::new();
    let two = Bus::new();
    one.init();
    two.init();
    one.register("one-chain", "claude", None);
    two.register("different-chain", "claude", None);
    assert_ne!(head(&one), head(&two));
}

#[test]
fn test_11_localhost_origin_with_valid_session_returns_201() {
    let bus = Bus::new();
    bus.init();
    bus.register("root", "claude", None);
    bus.register("child", "codex", Some("root"));
    let server = Server::start(&bus, false);
    let origin = format!("http://localhost:{}", server.address.port());
    let body = json!({"from_id":"root","to_id":"child","kind":"redirect","body":"localhost origin accepted"}).to_string();
    let before = bus.count("SELECT count(*) FROM messages");
    let response = server.request(
        &bus,
        "POST",
        "/api/msg",
        &[
            ("Origin", &origin),
            ("Cookie", &server.cookie),
            ("Content-Type", "application/json"),
        ],
        &body,
    );
    assert_eq!(
        response.status,
        201,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let sent = parse_json(&response.body);
    let stored: String = bus
        .db()
        .query_row(
            "SELECT body FROM messages WHERE id=?1",
            [sent["id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, "localhost origin accepted");
    assert_eq!(bus.count("SELECT count(*) FROM messages"), before + 1);
}

#[test]
fn test_11_localhost_origin_wrong_port_returns_403_without_inserting() {
    let bus = Bus::new();
    bus.init();
    bus.register("root", "claude", None);
    bus.register("child", "codex", Some("root"));
    let server = Server::start(&bus, false);
    let port = if server.address.port() == 65535 {
        65534
    } else {
        server.address.port() + 1
    };
    let origin = format!("http://localhost:{port}");
    let before = bus.count("SELECT count(*) FROM messages");
    let body =
        json!({"from_id":"root","to_id":"child","kind":"redirect","body":"wrong port"}).to_string();
    let response = server.request(
        &bus,
        "POST",
        "/api/msg",
        &[
            ("Origin", &origin),
            ("Cookie", &server.cookie),
            ("Content-Type", "application/json"),
        ],
        &body,
    );
    assert_eq!(response.status, 403);
    assert_eq!(bus.count("SELECT count(*) FROM messages"), before);
}

#[test]
fn test_11_self_send_fails_without_panic_or_new_row() {
    let bus = Bus::new();
    bus.init();
    bus.register("a", "claude", None);
    let messages = bus.count("SELECT count(*) FROM messages");
    let events = bus.count("SELECT count(*) FROM events");
    let output = bus
        .cmd()
        .args([
            "send",
            "--from",
            "a",
            "--to",
            "a",
            "--kind",
            "redirect",
            "--body",
            "self-send",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    for bytes in [&output.stdout, &output.stderr] {
        assert!(
            !String::from_utf8_lossy(bytes).contains("panicked"),
            "{output:?}"
        );
    }
    assert_eq!(bus.count("SELECT count(*) FROM messages"), messages);
    assert_eq!(bus.count("SELECT count(*) FROM events"), events);
}
