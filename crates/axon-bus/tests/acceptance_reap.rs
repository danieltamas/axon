//! Reaping contract from the review spec for 21f8789 (items 1–5).
//! Lifecycle tests use the real CLI, serve process, and binary-created database.
//! The private source modules expose existing seams for deterministic interleavings
//! and an unavailable process table, without changing src or copying its logic.

mod common;
use common::*;
use rusqlite::params;
use serde_json::json;
use std::process::Stdio;
use std::time::{Duration, Instant};

#[allow(dead_code)]
#[path = "../src/claims.rs"]
mod claims;
#[allow(dead_code)]
#[path = "../src/memory.rs"]
mod memory;
#[allow(dead_code)]
#[path = "../src/registry.rs"]
mod registry;
#[allow(dead_code)]
#[path = "../src/store.rs"]
mod store;

fn hub() -> Bus {
    // The periodic sample runs every five seconds; Bus::new's four seconds is too short.
    let bus = Bus::with_limit(Duration::from_secs(25));
    bus.init();
    bus
}

fn dead_pid(bus: &Bus) -> i64 {
    let mut child = bus.spawn(&["--help"]);
    let pid = i64::from(child.0.id());
    assert!(child.finish(bus.remaining()).status.success());
    pid
}

fn register(bus: &Bus, id: &str, parent: Option<&str>, status: &str, pid: Option<i64>) {
    bus.register(id, "codex", parent);
    bus.db()
        .execute(
            "UPDATE agents SET status=?2,pid=?3,last_seen_at=?4 WHERE id=?1",
            params![id, status, pid, now_ms()],
        )
        .unwrap();
    bus.ok(&["claim", "--agent", id, &format!("claims/{id}")]);
}

fn event_count(bus: &Bus, id: &str, verb: &str) -> i64 {
    bus.db()
        .query_row(
            "SELECT count(*) FROM events WHERE actor=?1 AND subject=?1 AND verb=?2",
            params![id, verb],
            |r| r.get(0),
        )
        .unwrap()
}

fn ended_at(bus: &Bus, id: &str) -> Option<i64> {
    bus.db()
        .query_row("SELECT ended_at FROM agents WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
}

fn assert_closed(bus: &Bus, id: &str) {
    assert_eq!(bus.agent(id)["status"], "closed", "{id}");
    assert!(
        ended_at(bus, id).is_some_and(|ts| ts > 0 && ts <= now_ms()),
        "{id}"
    );
    assert_eq!(
        bus.db()
            .query_row::<i64, _, _>("SELECT count(*) FROM claims WHERE agent_id=?1", [id], |r| r
                .get(0))
            .unwrap(),
        0,
        "{id} must release its claims"
    );
    let closed_hash = blake3::hash(br#"{"status":"closed"}"#).to_hex().to_string();
    let closed_events: i64 = bus
        .db()
        .query_row(
            "SELECT count(*) FROM events WHERE actor=?1 AND subject=?1
         AND verb='status' AND payload_hash=?2",
            params![id, closed_hash],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(closed_events, 1, "{id} must record exactly one close");
    assert_eq!(event_count(bus, id, "release"), 1, "{id}");
}

fn assert_open(bus: &Bus, id: &str, status: &str) {
    assert_eq!(bus.agent(id)["status"], status, "{id}");
    assert_eq!(ended_at(bus, id), None, "{id}");
    assert_eq!(event_count(bus, id, "status"), 0, "{id}");
    assert_eq!(event_count(bus, id, "release"), 0, "{id}");
    assert_eq!(
        bus.db()
            .query_row::<i64, _, _>("SELECT count(*) FROM claims WHERE agent_id=?1", [id], |r| r
                .get(0))
            .unwrap(),
        1,
        "{id} must keep its claim"
    );
}

fn wait_closed(bus: &Bus, ids: &[&str]) {
    let deadline = Instant::now() + bus.remaining().min(Duration::from_secs(12));
    while ids.iter().any(|id| bus.agent(id)["status"] != "closed") {
        assert!(Instant::now() < deadline, "sample did not close {ids:?}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn reap_sample(bus: &Bus, sampler: &memory::Sampler) {
    let mut conn = bus.db();
    let tx = conn.transaction().unwrap();
    registry::reap(&tx, |pid, seen| sampler.running(pid, seen)).unwrap();
    tx.commit().unwrap();
}

#[test]
fn startup_closes_dead_active_and_idle_roots_with_all_descendants() {
    let bus = hub();
    let dead = dead_pid(&bus);
    let live = i64::from(std::process::id());
    for status in ["active", "idle"] {
        register(&bus, status, None, status, Some(dead));
        register(
            &bus,
            &format!("{status}-child"),
            Some(status),
            "active",
            Some(live),
        );
        register(
            &bus,
            &format!("{status}-grandchild"),
            Some(&format!("{status}-child")),
            "idle",
            None,
        );
    }
    register(&bus, "live", None, "active", Some(live));
    register(&bus, "live-child", Some("live"), "idle", Some(live));
    let _server = Server::start(&bus, false);
    // Readiness follows the startup sample; waiting here would hide a missing startup reap.
    for id in [
        "active",
        "active-child",
        "active-grandchild",
        "idle",
        "idle-child",
        "idle-grandchild",
    ] {
        assert_closed(&bus, id);
    }
    assert_open(&bus, "live", "active");
    assert_open(&bus, "live-child", "idle");
    bus.ok(&["audit", "--verify"]);
}

#[test]
fn periodic_sample_closes_a_process_that_exits_after_startup() {
    let bus = hub();
    // A hook waits for EOF on stdin, giving us a portable child with an owned lifetime.
    let mut child = Running(
        bus.process()
            .args(["hook", "codex", "SessionStart"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let pid = i64::from(child.0.id());
    register(&bus, "root", None, "idle", Some(pid));
    register(&bus, "child", Some("root"), "active", None);
    let _server = Server::start(&bus, false);
    assert!(child.0.try_wait().unwrap().is_none());
    assert_open(&bus, "root", "idle");
    assert_open(&bus, "child", "active");
    drop(child.0.stdin.take());
    assert!(child.finish(bus.remaining()).status.success());
    wait_closed(&bus, &["root", "child"]);
    assert_closed(&bus, "root");
    assert_closed(&bus, "child");
    // A subsequent startup must not duplicate close/release events or the end time.
    let ended = ended_at(&bus, "root");
    drop(_server);
    let _restarted = Server::start(&bus, false);
    assert_eq!(ended_at(&bus, "root"), ended);
    assert_closed(&bus, "root");
    assert_closed(&bus, "child");
    bus.ok(&["audit", "--verify"]);
}

#[test]
fn startup_treats_a_newer_process_at_the_recorded_pid_as_reuse() {
    let bus = hub();
    let live = i64::from(std::process::id());
    register(&bus, "old-session", None, "active", Some(live));
    register(&bus, "child", Some("old-session"), "idle", Some(live));
    register(&bus, "current-session", None, "idle", Some(live));
    // Model a previous PID owner; do not rely on the OS recycling a chosen PID.
    bus.db()
        .execute(
            "UPDATE agents SET last_seen_at=1 WHERE id='old-session'",
            [],
        )
        .unwrap();
    let _server = Server::start(&bus, false);
    assert_closed(&bus, "old-session");
    assert_closed(&bus, "child");
    assert_open(&bus, "current-session", "idle");
    bus.ok(&["audit", "--verify"]);
}

#[test]
fn closed_roots_close_descendants_at_startup_and_on_the_next_sample() {
    let bus = hub();
    let live = i64::from(std::process::id());
    for root in ["ended-before", "ended-during"] {
        register(&bus, root, None, "idle", None);
        register(
            &bus,
            &format!("{root}-child"),
            Some(root),
            "active",
            Some(live),
        );
        register(
            &bus,
            &format!("{root}-grandchild"),
            Some(&format!("{root}-child")),
            "idle",
            None,
        );
    }
    bus.hook("codex", "SessionEnd", &json!({"session_id":"ended-before"}));
    let _server = Server::start(&bus, false);
    for id in [
        "ended-before",
        "ended-before-child",
        "ended-before-grandchild",
    ] {
        assert_closed(&bus, id);
    }
    bus.hook("codex", "SessionEnd", &json!({"session_id":"ended-during"}));
    wait_closed(&bus, &["ended-during-child", "ended-during-grandchild"]);
    for id in [
        "ended-during",
        "ended-during-child",
        "ended-during-grandchild",
    ] {
        assert_closed(&bus, id);
    }
    bus.ok(&["audit", "--verify"]);
}

#[test]
fn live_and_pidless_trees_survive_startup_and_periodic_sampling() {
    let bus = hub();
    let live = i64::from(std::process::id());
    for (root, status, pid) in [
        ("live-active", "active", Some(live)),
        ("live-idle", "idle", Some(live)),
        ("pidless", "active", None),
    ] {
        register(&bus, root, None, status, pid);
        register(
            &bus,
            &format!("{root}-child"),
            Some(root),
            "active",
            Some(live),
        );
    }
    let _server = Server::start(&bus, false);
    // Closing this post-startup sentinel proves that a periodic sample actually happened.
    let dead = dead_pid(&bus);
    register(&bus, "sentinel", None, "idle", Some(dead));
    wait_closed(&bus, &["sentinel"]);
    assert_closed(&bus, "sentinel");
    for (root, status) in [
        ("live-active", "active"),
        ("live-idle", "idle"),
        ("pidless", "active"),
    ] {
        assert_open(&bus, root, status);
        assert_open(&bus, &format!("{root}-child"), "active");
    }
    bus.ok(&["audit", "--verify"]);
}

#[test]
fn session_start_clears_the_previous_end_time() {
    let bus = hub();
    for harness in ["claude", "codex"] {
        let start = bus.fixture(harness, "SessionStart");
        let id = start["session_id"].as_str().unwrap();
        bus.hook(harness, "SessionStart", &start);
        bus.hook(harness, "SessionEnd", &json!({"session_id":id}));
        assert!(ended_at(&bus, id).is_some());
        bus.hook(harness, "SessionStart", &start);
        assert_eq!(bus.agent(id)["status"], "idle");
        assert_eq!(ended_at(&bus, id), None);
    }
    bus.ok(&["audit", "--verify"]);
}

#[test]
fn active_and_idle_upserts_clear_the_previous_end_time() {
    let bus = hub();
    for status in [registry::Status::Active, registry::Status::Idle] {
        let id = status.as_str();
        bus.register(id, "codex", None);
        bus.hook("codex", "SessionEnd", &json!({"session_id":id}));
        assert!(ended_at(&bus, id).is_some());
        let mut conn = bus.db();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        registry::upsert(
            &tx,
            &registry::Agent {
                id,
                harness: "codex",
                session_id: id,
                pid: Some(i64::from(std::process::id())),
                ..Default::default()
            },
            status,
        )
        .unwrap();
        tx.commit().unwrap();
        assert_eq!(bus.agent(id)["status"], status.as_str());
        assert_eq!(ended_at(&bus, id), None);
    }
    bus.ok(&["audit", "--verify"]);
}

#[test]
fn unavailable_process_table_changes_no_agents_claims_or_events() {
    let bus = hub();
    register(&bus, "dead", None, "active", Some(dead_pid(&bus)));
    register(&bus, "dead-child", Some("dead"), "idle", None);
    register(&bus, "ended", None, "idle", None);
    register(&bus, "ended-child", Some("ended"), "active", None);
    bus.hook("codex", "SessionEnd", &json!({"session_id":"ended"}));
    let events = bus.count("SELECT count(*) FROM events");
    let claims = bus.count("SELECT count(*) FROM claims");
    // None is the existing reaper boundary for a sample that cannot see processes.
    let mut conn = bus.db();
    let tx = conn.transaction().unwrap();
    registry::reap(&tx, |_, _| None).unwrap();
    tx.commit().unwrap();
    assert_open(&bus, "dead", "active");
    assert_open(&bus, "dead-child", "idle");
    assert_open(&bus, "ended-child", "active");
    assert_eq!(bus.count("SELECT count(*) FROM events"), events);
    assert_eq!(bus.count("SELECT count(*) FROM claims"), claims);
}

#[test]
fn a_live_root_registered_between_sample_and_reap_is_not_closed() {
    let bus = hub();
    let mut sampler = memory::Sampler::new();
    sampler.sample(&bus.db()).unwrap();
    let live = i64::from(std::process::id());
    assert!(
        sampler.running(live, now_ms()).is_some(),
        "requires process-table visibility"
    );
    register(&bus, "new-root", None, "active", Some(live));
    register(&bus, "new-child", Some("new-root"), "idle", Some(live));
    reap_sample(&bus, &sampler);
    assert_open(&bus, "new-root", "active");
    assert_open(&bus, "new-child", "idle");
}

#[test]
fn a_live_root_reopened_between_sample_and_reap_is_not_closed() {
    let bus = hub();
    register(&bus, "resumed", None, "idle", Some(dead_pid(&bus)));
    bus.hook("codex", "SessionEnd", &json!({"session_id":"resumed"}));
    let mut sampler = memory::Sampler::new();
    sampler.sample(&bus.db()).unwrap();
    let live = i64::from(std::process::id());
    assert!(
        sampler.running(live, now_ms()).is_some(),
        "requires process-table visibility"
    );
    let mut conn = bus.db();
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    registry::upsert(
        &tx,
        &registry::Agent {
            id: "resumed",
            harness: "codex",
            session_id: "resumed",
            pid: Some(live),
            ..Default::default()
        },
        registry::Status::Idle,
    )
    .unwrap();
    tx.commit().unwrap();
    bus.ok(&["claim", "--agent", "resumed", "claims/resumed"]);
    let events = bus.count("SELECT count(*) FROM events");
    reap_sample(&bus, &sampler);
    assert_eq!(bus.agent("resumed")["status"], "idle");
    assert_eq!(ended_at(&bus, "resumed"), None);
    assert_eq!(bus.count("SELECT count(*) FROM claims"), 1);
    assert_eq!(bus.count("SELECT count(*) FROM events"), events);
}

#[test]
fn a_hook_writing_during_reap_cannot_fork_the_audit_chain_or_partially_close() {
    let bus = hub();
    let dead = dead_pid(&bus);
    register(&bus, "dead", None, "active", Some(dead));
    register(&bus, "child", Some("dead"), "idle", None);
    let mut conn = bus.db();
    let tx = conn.transaction().unwrap();
    let result = registry::reap(&tx, |_, _| {
        // The root SELECT already established the reaper's read snapshot. This hook
        // commits a status/registration event before the reaper tries its first write.
        bus.hook("codex", "SessionStart", &json!({"session_id":"writer"}));
        Some(false)
    });
    assert!(
        result.is_err(),
        "a stale SQLite snapshot must not become a writer"
    );
    drop(tx);
    assert_eq!(bus.agent("writer")["status"], "idle");
    assert_open(&bus, "dead", "active");
    assert_open(&bus, "child", "idle");
    bus.ok(&["audit", "--verify"]);
    // The next actual sample retries successfully, including the concurrent hook's PID.
    let mut sampler = memory::Sampler::new();
    sampler.sample(&bus.db()).unwrap();
    reap_sample(&bus, &sampler);
    assert_closed(&bus, "dead");
    assert_closed(&bus, "child");
    assert_eq!(bus.agent("writer")["status"], "idle");
    assert_eq!(ended_at(&bus, "writer"), None);
    bus.ok(&["audit", "--verify"]);
}
