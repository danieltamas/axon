//! P2P-SPEC §§6–7: receive/delivery membership and discovery capacity cases.
use super::delivery::*;
use super::fed::*;
use serde_json::json;
use std::time::{Duration, Instant};

pub fn fresh_delivery_membership() {
    let (pair, _, target) = shared(60);
    let id = queued(
        &pair.a,
        "agent-a",
        &target,
        "sync",
        "pending before repo disappears",
    );
    let local = received(&pair.b, &id);
    pair.sb.snapshot(&pair.b);
    std::fs::rename(&pair.rb, pair.b.root.join("renamed-project")).unwrap();
    let context = hook_context(&pair.b, "codex", "agent-b", &pair.rb);
    assert!(!context.contains("pending before repo disappears"));
    let delivered: Option<i64> = db(&pair.b)
        .query_row(
            "SELECT delivered_at FROM messages WHERE id=?1",
            [&local],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(delivered, None);
}
pub fn discovery_capacity() {
    let (pair, _, _) = shared(180);
    for number in 1..=100 {
        pair.b
            .register_at(&format!("eligible-{number}"), "codex", None, &pair.rb);
    }
    eventually(&pair.a, "discovery crosses the 100-item page", || {
        count(&pair.a, "SELECT count(*) FROM fed_remote_sessions") == 101
    });
    for number in 101..=1000 {
        pair.b
            .register_at(&format!("eligible-{number}"), "codex", None, &pair.rb);
    }
    eventually(&pair.a, "1000-session peer cap", || {
        count(&pair.a, "SELECT count(*) FROM fed_remote_sessions") == 1000
    });
    assert_eq!(
        count(
            &pair.a,
            "SELECT count(DISTINCT session) FROM fed_remote_sessions"
        ),
        1000
    );
    // Observe another complete refresh: the 1001st eligible agent cannot grow the cache.
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(31) {
        pair.a.remaining();
        assert!(count(&pair.a, "SELECT count(*) FROM fed_remote_sessions") <= 1000);
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn closed_recipient_is_not_delivered_to_or_retargeted_to_a_new_session() {
    let (pair, _, target) = shared(75);
    pair.b
        .hook("codex", "SessionEnd", &json!({"session_id":"agent-b"}));
    pair.b.register_at("replacement", "codex", None, &pair.rb);
    // The old cached target may still enqueue until the next discovery refresh.
    // It must never commit to a different local recipient.
    let output = pair
        .a
        .cmd()
        .args([
            "send",
            "--from",
            "agent-a",
            "--to",
            &target,
            "--kind",
            "sync",
            "--body",
            "old recipient only",
        ])
        .assert()
        .get_output()
        .clone();
    assert!(output.status.success() || output.status.code() == Some(1));
    eventually(
        &pair.a,
        "closed session dropped within refresh/staleness budget",
        || {
            let output = pair.a.ok(&["peers", "--agent", "agent-a"]);
            !String::from_utf8_lossy(&output.stdout).contains(&target)
        },
    );
    assert_eq!(
        count(
            &pair.b,
            "SELECT count(*) FROM messages WHERE body='old recipient only'"
        ),
        0
    );
    assert!(!hook_context(&pair.b, "codex", "replacement", &pair.rb).contains("old recipient only"));
    assert_ne!(
        pair.target(true, "replacement"),
        target,
        "opaque session IDs are never reused"
    );
}
