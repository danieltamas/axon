//! P2P-SPEC §8: process-driven queue and hook capacity cases.
use super::delivery::*;
use super::fed::*;
use std::time::Duration;

pub fn hook_batch_caps_leave_excess_remote_messages_pending() {
    let (pair, _, target) = shared(80);
    // Spread arrivals across receiver token buckets: this test isolates batching.
    for number in 0..21 {
        let id = queued(
            &pair.a,
            "agent-a",
            &target,
            "sync",
            &format!("batch-{number}"),
        );
        received(&pair.b, &id);
        std::thread::sleep(Duration::from_millis(510));
    }
    let context = hook_context(&pair.b, "codex", "agent-b", &pair.rb);
    assert_eq!(
        context
            .lines()
            .filter(|l| l.starts_with("[remote message "))
            .count(),
        20
    );
    assert!(
        context.len() <= 16 * 1024 + 4096,
        "intro may accompany remote text"
    );
    assert_eq!(
        count(
            &pair.b,
            "SELECT count(*) FROM messages WHERE from_id LIKE 'peer:%' AND delivered_at IS NULL"
        ),
        1
    );
    let rest = hook_context(&pair.b, "codex", "agent-b", &pair.rb);
    assert_eq!(
        rest.lines()
            .filter(|l| l.starts_with("[remote message "))
            .count(),
        1
    );
}

pub fn remote_text_byte_cap_is_independent_of_twenty_message_count() {
    let (pair, _, target) = shared(80);
    let body = "🦀".repeat(400);
    for _ in 0..12 {
        received(&pair.b, &queued(&pair.a, "agent-a", &target, "sync", &body));
        std::thread::sleep(Duration::from_millis(510));
    }
    let context = hook_context(&pair.b, "codex", "agent-b", &pair.rb);
    let frames = context
        .lines()
        .filter(|l| l.starts_with("[remote message "))
        .count();
    assert!(frames > 0 && frames < 12);
    let start = context.find("[remote message ").unwrap();
    let end = context.rfind(']').unwrap() + 1;
    assert!(context[start..end].len() <= 16 * 1024);
    assert_eq!(
        count(
            &pair.b,
            "SELECT count(*) FROM messages WHERE from_id LIKE 'peer:%' AND delivered_at IS NULL"
        ),
        12 - frames as i64
    );
}

pub fn outbox_count() {
    let (mut pair, _, target) = shared(180);
    // No service means no transmission, retries, discovery expiry or receive-side rates.
    pair.sa.process.0.kill().unwrap();
    pair.sa.process.0.wait().unwrap();
    for number in 0..1000 {
        queued(
            &pair.a,
            "agent-a",
            &target,
            "sync",
            &format!("offline-{number}"),
        );
    }
    assert_eq!(
        count(
            &pair.a,
            "SELECT count(*) FROM fed_outbox WHERE state='queued'"
        ),
        1000
    );
    refused(
        &pair.a,
        "agent-a",
        &target,
        "sync",
        "one too many",
        "queue_full",
    );
    assert_eq!(count(&pair.b, "SELECT count(*) FROM fed_inbox"), 0);
}

pub fn inbound_count() {
    let (pair, _, target) = shared(150);
    for number in 0..100 {
        received(
            &pair.b,
            &queued(
                &pair.a,
                "agent-a",
                &target,
                "sync",
                &format!("pending-{number}"),
            ),
        );
        std::thread::sleep(Duration::from_millis(510));
    }
    assert_eq!(
        count(
            &pair.b,
            "SELECT count(*) FROM messages WHERE from_id LIKE 'peer:%' AND delivered_at IS NULL"
        ),
        100
    );
    let excess = queued(&pair.a, "agent-a", &target, "sync", "pending-101");
    outcome(&pair.a, &excess, "rejected");
    assert_eq!(
        count(
            &pair.b,
            "SELECT count(*) FROM messages WHERE body='pending-101'"
        ),
        0
    );
    assert_eq!(
        count(
            &pair.b,
            "SELECT count(*) FROM messages WHERE from_id LIKE 'peer:%' AND delivered_at IS NULL"
        ),
        100
    );
    hook_context(&pair.b, "codex", "agent-b", &pair.rb);
    std::thread::sleep(Duration::from_millis(510));
    received(
        &pair.b,
        &queued(&pair.a, "agent-a", &target, "sync", "space available"),
    );
}
