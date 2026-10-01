//! Observable message and hook assertions shared by wire/lifecycle tests.
use super::fed::*;
use super::Bus;
use serde_json::Value;
use std::time::Duration;

pub fn shared(limit: u64) -> (Pair, String, String) {
    let pair = Pair::paired(Duration::from_secs(limit));
    let share = pair.share(true, true, true, true);
    let target = pair.target(true, "agent-b");
    (pair, share, target)
}
pub fn envelope(bus: &Bus, id: &str) -> Value {
    serde_json::from_str(&text(
        bus,
        "SELECT envelope_json FROM fed_outbox WHERE message_id=?1",
        id,
    ))
    .unwrap()
}
pub fn outcome(bus: &Bus, id: &str, state: &str) {
    eventually(bus, state, || {
        text(bus, "SELECT state FROM fed_outbox WHERE message_id=?1", id) == state
    });
}
pub fn assert_frame(
    context: &str,
    local: &str,
    from: &str,
    kind: &str,
    thread: &str,
    body: &str,
    refs: &[&str],
) {
    let quoted = body
        .split('\n')
        .map(|line| format!("│ {line}\n"))
        .collect::<String>();
    let reference_line = if refs.is_empty() {
        String::new()
    } else {
        format!(
            "refs (metadata only, nothing was fetched): {}\n",
            refs.join(", ")
        )
    };
    let expected = format!("[remote message {local} from {from}: another person's agent, on their machine; kind {kind}, thread {thread}]\n{quoted}{reference_line}[end of remote message {local}]");
    assert!(
        context.contains(&expected),
        "missing exact §8 frame:\n{expected}\nactual:\n{context}"
    );
    assert_eq!(
        context
            .lines()
            .filter(|l| l.starts_with("[remote message "))
            .count(),
        1
    );
    assert_eq!(
        context
            .lines()
            .filter(|l| *l == format!("[end of remote message {local}]"))
            .count(),
        1
    );
}
pub fn rejected_input(bus: &Bus, target: &str, body: &str, extra: &[&str]) {
    let before = count(bus, "SELECT count(*) FROM fed_outbox");
    let output = bus
        .cmd()
        .args([
            "send", "--from", "agent-a", "--to", target, "--kind", "sync", "--body", body,
        ])
        .args(extra)
        .assert()
        .code(1)
        .get_output()
        .clone();
    let line = String::from_utf8(output.stdout).unwrap();
    // §7 does not assign a particular refusal reason to invalid refs/thread/control bytes.
    let reason = line
        .strip_prefix("refused: ")
        .and_then(|s| s.strip_suffix('\n'))
        .expect("exact refusal line");
    assert!(
        [
            "federation_off",
            "unknown_peer",
            "peer_paused",
            "peer_removed",
            "not_a_member",
            "outbound_off",
            "remote_inbound_off",
            "unknown_session",
            "kind_not_allowed",
            "too_long",
            "rate_limited",
            "queue_full"
        ]
        .contains(&reason),
        "{line}"
    );
    assert_eq!(count(bus, "SELECT count(*) FROM fed_outbox"), before);
}
pub fn reply(bus: &Bus, local: &str, from: &str, body: &str) -> String {
    let output = bus
        .cmd()
        .args(["reply", local, "--from", from, "--body", body])
        .assert()
        .success()
        .get_output()
        .clone();
    let line = String::from_utf8(output.stdout).unwrap();
    let id = line
        .strip_prefix("queued ")
        .and_then(|s| s.split_once(" to peer:"))
        .expect("remote reply queued")
        .0;
    uuid(id);
    assert!(line.ends_with(" (delivers when connected; expires in 24h)\n"));
    id.to_owned()
}
pub fn assert_audit_chain(bus: &Bus) {
    let conn = db(bus);
    let audit: Vec<(i64, String, Option<String>)> = conn
        .prepare("SELECT seq,decision,reason FROM fed_audit ORDER BY seq")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(!audit.is_empty());
    let events: Vec<(String, String)> = conn
        .prepare("SELECT verb,payload_hash FROM events WHERE actor='fed' ORDER BY seq")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        events.len(),
        audit.len(),
        "one chain event per federation audit row"
    );
    for ((_, decision, _), (verb, hash)) in audit.iter().zip(&events) {
        assert_eq!(decision, verb);
        assert_eq!(hash.len(), 64);
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
    }
    bus.ok(&["audit", "--verify"]);
}
