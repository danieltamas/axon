//! Frozen P2P-SPEC §7b / BUS-PLAN §3b contract (2026-10-03).
//! Real paired loopback nodes, owner HTTP, CLI and isolated SQLite only; no feature APIs.
//! Direct axon-bus commands use the existing --agent convention to identify their caller.
mod common;

use common::fed::*;
use common::{now_ms, Bus};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Output;
use std::time::{Duration, Instant};

fn paired() -> Pair {
    Pair::paired(Duration::from_secs(150))
}

fn run(bus: &Bus, cwd: &Path, actor: &str, args: &[&str]) -> Output {
    bus.cmd()
        .current_dir(cwd)
        .arg(args[0])
        .args(["--agent", actor])
        .args(&args[1..])
        .output()
        .unwrap()
}

fn expect(output: Output, code: i32, contains: &str) -> String {
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains(contains), "missing {contains:?}: {stdout}");
    stdout
}

fn change(bus: &Bus, cwd: &Path, actor: &str, operation: &str, key: &str, note: &str) {
    let mut args = vec![operation, "--key", key];
    if operation != "drop" {
        args.extend(["--note", note]);
    }
    let verb = match operation {
        "take" => "taken",
        "done" => "done",
        "drop" => "dropped",
        _ => unreachable!(),
    };
    assert_eq!(
        expect(run(bus, cwd, actor, &args), 0, &format!("{verb} {key}")),
        format!("{verb} {key}\n")
    );
}

fn entry(bus: &Bus, cwd: &Path, actor: &str, key: &str) -> Option<Value> {
    expect(run(bus, cwd, actor, &["handled"]), 0, "")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("handled emits JSON lines"))
        .find(|row| row["key"] == key)
}

fn wait_entry(
    bus: &Bus,
    cwd: &Path,
    actor: &str,
    key: &str,
    state: &str,
    holder: &str,
    note: &str,
) -> Value {
    let mut found = None;
    eventually(
        bus,
        &format!("{key} becomes {state} held by {holder}"),
        || {
            found = entry(bus, cwd, actor, key);
            found.as_ref().is_some_and(|row| {
                row["state"] == state && row["holder"] == holder && row["note"] == note
            })
        },
    );
    found.unwrap()
}

fn remote_holder(
    sender: &Bus,
    receiver: &Bus,
    receiver_peer: &str,
    share: &str,
    agent: &str,
    harness: &str,
) -> String {
    let mut session = None;
    eventually(sender, "holder has a discovery label", || {
        session = db(sender)
            .query_row(
                "SELECT session FROM fed_sessions WHERE share_id=?1 AND agent_id=?2",
                params![share, agent],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .unwrap();
        session.is_some()
    });
    let label = text(
        receiver,
        "SELECT label FROM peers WHERE peer_id=?1",
        receiver_peer,
    );
    format!("peer:{label}/{harness}-{}", &session.unwrap()[..4])
}

fn wait_free(bus: &Bus, cwd: &Path, actor: &str, key: &str) {
    eventually(bus, &format!("{key} becomes free"), || {
        let output = run(bus, cwd, actor, &["handled", "--key", key]);
        if output.status.code() == Some(0) {
            return false;
        }
        expect(output, 1, "");
        entry(bus, cwd, actor, key).is_none()
    });
}

fn reconnect_after(pair: Pair, offline: impl FnOnce(&Bus, &Path, &Bus, &Path)) -> Pair {
    // §9 pause closes connections and forbids dialing. Keep the bound loopback ports:
    // restarting both :0 endpoints with relays disabled loses both known addresses.
    for (bus, server, id) in [(&pair.a, &pair.sa, &pair.pa), (&pair.b, &pair.sb, &pair.pb)] {
        api(
            bus,
            server,
            "POST",
            &format!("/api/fed/peers/{id}/pause"),
            json!({}),
        );
        assert_eq!(peer(bus, server, id)["state"], "paused");
    }
    offline(&pair.a, &pair.ra, &pair.b, &pair.rb);
    for (bus, server, id) in [(&pair.a, &pair.sa, &pair.pa), (&pair.b, &pair.sb, &pair.pb)] {
        api(
            bus,
            server,
            "POST",
            &format!("/api/fed/peers/{id}/resume"),
            json!({}),
        );
    }
    pair.connected();
    pair
}

// Absence has no eventual success signal. Check continuously through an observation
// window after positive reverse-direction traffic, and repeat after reconnect/full-send.
fn remains_absent(bus: &Bus, cwd: &Path, actor: &str, key: &str) {
    let deadline = Instant::now() + Duration::from_secs(3).min(bus.remaining());
    loop {
        expect(run(bus, cwd, actor, &["handled", "--key", key]), 1, "");
        let count: i64 = db(bus)
            .query_row("SELECT count(*) FROM handled WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 0,
            "forbidden entry crossed, possibly under the wrong repo"
        );
        if Instant::now() >= deadline {
            break;
        }
        std::thread::yield_now();
    }
}

#[test]
fn take_and_done_propagate_with_peer_holder_and_refuse_duplicate_work() {
    let pair = paired();
    let share = pair.share(true, true, true, true);
    change(
        &pair.a,
        &pair.ra,
        "agent-a",
        "take",
        "lead:acme",
        "contacting purchasing",
    );
    let holder = remote_holder(&pair.a, &pair.b, &pair.pb, &share, "agent-a", "claude");
    assert!(holder.starts_with("peer:"));
    let remote = wait_entry(
        &pair.b,
        &pair.rb,
        "agent-b",
        "lead:acme",
        "taken",
        &holder,
        "contacting purchasing",
    );
    let local = entry(&pair.a, &pair.ra, "agent-a", "lead:acme").unwrap();
    assert_eq!(remote["at"], local["at"]);
    assert_eq!(remote["expires_at"], local["expires_at"]);
    let stored_peer: String = db(&pair.b)
        .query_row(
            "SELECT peer_id FROM handled WHERE repo=?1 AND key=?2",
            params![pair.rb.to_str().unwrap(), "lead:acme"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored_peer, pair.pb);
    expect(
        run(
            &pair.b,
            &pair.rb,
            "agent-b",
            &["handled", "--key", "lead:acme"],
        ),
        0,
        &format!("taken by {holder}"),
    );
    expect(
        run(
            &pair.b,
            &pair.rb,
            "agent-b",
            &["take", "--key", "lead:acme"],
        ),
        1,
        &format!("taken by {holder}"),
    );
    change(
        &pair.a,
        &pair.ra,
        "agent-a",
        "done",
        "lead:acme",
        "contacted",
    );
    wait_entry(
        &pair.b,
        &pair.rb,
        "agent-b",
        "lead:acme",
        "done",
        &holder,
        "contacted",
    );
    expect(
        run(
            &pair.b,
            &pair.rb,
            "agent-b",
            &["take", "--key", "lead:acme"],
        ),
        1,
        &format!("done by {holder}"),
    );
    expect(
        run(
            &pair.b,
            &pair.rb,
            "agent-b",
            &["done", "--key", "lead:acme", "--note", "overwrite"],
        ),
        1,
        &format!("done by {holder}"),
    );
    assert_eq!(
        entry(&pair.a, &pair.ra, "agent-a", "lead:acme").unwrap()["holder"],
        "agent-a"
    );
    assert_eq!(
        count(
            &pair.a,
            "SELECT count(*) FROM handled WHERE peer_id IS NOT NULL"
        ),
        0,
        "remote entry must not echo back"
    );
}

#[test]
fn drop_propagates_as_free_and_the_peer_can_take_it_in_the_reverse_direction() {
    let pair = paired();
    let share = pair.share(true, true, true, true);
    change(
        &pair.a,
        &pair.ra,
        "agent-a",
        "take",
        "lead:released",
        "reserved",
    );
    let a_holder = remote_holder(&pair.a, &pair.b, &pair.pb, &share, "agent-a", "claude");
    wait_entry(
        &pair.b,
        &pair.rb,
        "agent-b",
        "lead:released",
        "taken",
        &a_holder,
        "reserved",
    );
    change(&pair.a, &pair.ra, "agent-a", "drop", "lead:released", "");
    wait_free(&pair.b, &pair.rb, "agent-b", "lead:released");
    change(
        &pair.b,
        &pair.rb,
        "agent-b",
        "take",
        "lead:released",
        "new owner",
    );
    let b_holder = remote_holder(&pair.b, &pair.a, &pair.pa, &share, "agent-b", "codex");
    wait_entry(
        &pair.a,
        &pair.ra,
        "agent-a",
        "lead:released",
        "taken",
        &b_holder,
        "new owner",
    );
    expect(
        run(
            &pair.a,
            &pair.ra,
            "agent-a",
            &["handled", "--key", "lead:released"],
        ),
        0,
        &format!("taken by {b_holder}"),
    );
}

#[test]
fn accepting_a_share_sends_existing_live_taken_and_done_entries_both_ways() {
    let pair = paired();
    change(
        &pair.a,
        &pair.ra,
        "agent-a",
        "take",
        "before:take",
        "existing take",
    );
    change(
        &pair.b,
        &pair.rb,
        "agent-b",
        "done",
        "before:done",
        "existing done",
    );
    let share = pair.share(true, true, true, true);
    let a_holder = remote_holder(&pair.a, &pair.b, &pair.pb, &share, "agent-a", "claude");
    let b_holder = remote_holder(&pair.b, &pair.a, &pair.pa, &share, "agent-b", "codex");
    wait_entry(
        &pair.b,
        &pair.rb,
        "agent-b",
        "before:take",
        "taken",
        &a_holder,
        "existing take",
    );
    wait_entry(
        &pair.a,
        &pair.ra,
        "agent-a",
        "before:done",
        "done",
        &b_holder,
        "existing done",
    );
}

fn direction_blocked(a_outbound: bool, b_inbound: bool) {
    let pair = paired();
    let share = pair.share(true, a_outbound, b_inbound, true);
    change(
        &pair.a,
        &pair.ra,
        "agent-a",
        "take",
        "blocked:take",
        "must stay local",
    );
    change(
        &pair.a,
        &pair.ra,
        "agent-a",
        "done",
        "blocked:done",
        "must stay local",
    );
    change(
        &pair.b,
        &pair.rb,
        "agent-b",
        "take",
        "control:reverse",
        "transport is live",
    );
    let holder = remote_holder(&pair.b, &pair.a, &pair.pa, &share, "agent-b", "codex");
    wait_entry(
        &pair.a,
        &pair.ra,
        "agent-a",
        "control:reverse",
        "taken",
        &holder,
        "transport is live",
    );
    remains_absent(&pair.b, &pair.rb, "agent-b", "blocked:take");
    remains_absent(&pair.b, &pair.rb, "agent-b", "blocked:done");
    let pair = reconnect_after(pair, |_, _, _, _| {});
    change(
        &pair.b,
        &pair.rb,
        "agent-b",
        "done",
        "control:reverse",
        "reconnected",
    );
    wait_entry(
        &pair.a,
        &pair.ra,
        "agent-a",
        "control:reverse",
        "done",
        &holder,
        "reconnected",
    );
    remains_absent(&pair.b, &pair.rb, "agent-b", "blocked:take");
    remains_absent(&pair.b, &pair.rb, "agent-b", "blocked:done");
}

#[test]
fn outbound_off_blocks_live_push_and_reconnect_full_send() {
    direction_blocked(false, true);
}

#[test]
fn inbound_off_blocks_live_push_and_reconnect_full_send() {
    direction_blocked(true, false);
}

#[test]
fn an_unshared_repository_never_crosses_an_active_share() {
    let pair = paired();
    let share = pair.share(true, true, true, true);
    let private = repo(&pair.a, "private-project");
    pair.a
        .register_at("private-agent", "claude", None, &private);
    change(
        &pair.a,
        &private,
        "private-agent",
        "take",
        "private:lead",
        "not shared",
    );
    change(
        &pair.a,
        &pair.ra,
        "agent-a",
        "take",
        "control:shared",
        "visible",
    );
    let holder = remote_holder(&pair.a, &pair.b, &pair.pb, &share, "agent-a", "claude");
    wait_entry(
        &pair.b,
        &pair.rb,
        "agent-b",
        "control:shared",
        "taken",
        &holder,
        "visible",
    );
    remains_absent(&pair.b, &pair.rb, "agent-b", "private:lead");
    let pair = reconnect_after(pair, |_, _, _, _| {});
    change(
        &pair.a,
        &pair.ra,
        "agent-a",
        "done",
        "control:shared",
        "reconnected",
    );
    wait_entry(
        &pair.b,
        &pair.rb,
        "agent-b",
        "control:shared",
        "done",
        &holder,
        "reconnected",
    );
    remains_absent(&pair.b, &pair.rb, "agent-b", "private:lead");
}

fn ending_share(unpair: bool) {
    let pair = paired();
    let share = pair.share(true, true, true, true);
    let holder = remote_holder(&pair.a, &pair.b, &pair.pb, &share, "agent-a", "claude");
    for (operation, key) in [("take", "remove:take"), ("done", "remove:done")] {
        change(&pair.a, &pair.ra, "agent-a", operation, key, "remote");
        wait_entry(
            &pair.b,
            &pair.rb,
            "agent-b",
            key,
            if operation == "take" { "taken" } else { "done" },
            &holder,
            "remote",
        );
    }
    change(
        &pair.b,
        &pair.rb,
        "agent-b",
        "done",
        "local:keep",
        "local work survives",
    );
    if unpair {
        api(
            &pair.b,
            &pair.sb,
            "DELETE",
            &format!("/api/fed/peers/{}", pair.pb),
            json!({}),
        );
    } else {
        api(
            &pair.a,
            &pair.sa,
            "DELETE",
            &format!("/api/fed/shares/{share}"),
            json!({}),
        );
    }
    for key in ["remove:take", "remove:done"] {
        wait_free(&pair.b, &pair.rb, "agent-b", key);
    }
    assert_eq!(
        entry(&pair.b, &pair.rb, "agent-b", "local:keep").unwrap()["holder"],
        "agent-b"
    );
    assert_eq!(
        count(
            &pair.b,
            "SELECT count(*) FROM handled WHERE peer_id IS NOT NULL"
        ),
        0
    );
}

#[test]
fn share_remove_deletes_received_taken_and_done_but_preserves_local_work() {
    ending_share(false);
}

#[test]
fn unpair_deletes_received_taken_and_done_but_preserves_local_work() {
    ending_share(true);
}

fn conflict(a_done: bool, b_done: bool, earlier_a: bool, tie: bool) {
    let pair = paired();
    let share = pair.share(true, true, true, true);
    let a_holder = remote_holder(&pair.a, &pair.b, &pair.pb, &share, "agent-a", "claude");
    let b_holder = remote_holder(&pair.b, &pair.a, &pair.pa, &share, "agent-b", "codex");
    let a_node = health(&pair.a, &pair.sa)["node_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let b_node = health(&pair.b, &pair.sb)["node_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let earlier = now_ms() - 20_000;
    let later = if tie { earlier } else { earlier + 10_000 };
    let (a_at, b_at) = if earlier_a {
        (earlier, later)
    } else {
        (later, earlier)
    };
    let pair = reconnect_after(pair, |a, ra, b, rb| {
        for (bus, cwd, agent, done, at, note) in [
            (a, ra, "agent-a", a_done, a_at, "A's record"),
            (b, rb, "agent-b", b_done, b_at, "B's record"),
        ] {
            change(
                bus,
                cwd,
                agent,
                if done { "done" } else { "take" },
                "conflict:key",
                note,
            );
            assert_eq!(
                bus.db()
                    .execute(
                        "UPDATE handled SET at=?1 WHERE repo=?2 AND key='conflict:key'",
                        params![at, cwd.to_str().unwrap()]
                    )
                    .unwrap(),
                1
            );
        }
        // Both accepted their own independent claim while peer transport was paused.
        assert_eq!(
            entry(a, ra, "agent-a", "conflict:key").unwrap()["holder"],
            "agent-a"
        );
        assert_eq!(
            entry(b, rb, "agent-b", "conflict:key").unwrap()["holder"],
            "agent-b"
        );
    });
    let winner_a = if a_done != b_done {
        a_done
    } else if tie {
        a_node < b_node
    } else {
        earlier_a
    };
    let state = if a_done || b_done { "done" } else { "taken" };
    let note = if winner_a { "A's record" } else { "B's record" };
    let a = wait_entry(
        &pair.a,
        &pair.ra,
        "agent-a",
        "conflict:key",
        state,
        if winner_a { "agent-a" } else { &b_holder },
        note,
    );
    let b = wait_entry(
        &pair.b,
        &pair.rb,
        "agent-b",
        "conflict:key",
        state,
        if winner_a { &a_holder } else { "agent-b" },
        note,
    );
    // Local and remote spellings differ; they must denote the same winning principal.
    assert_eq!(a["at"], b["at"]);
    assert_eq!(a["at"], if winner_a { a_at } else { b_at });
    assert_eq!(a["expires_at"], b["expires_at"]);
    let (loser, cwd, actor, holder) = if winner_a {
        (&pair.b, &pair.rb, "agent-b", &a_holder)
    } else {
        (&pair.a, &pair.ra, "agent-a", &b_holder)
    };
    expect(
        run(loser, cwd, actor, &["handled", "--key", "conflict:key"]),
        0,
        &format!("{state} by {holder}"),
    );
    expect(
        run(loser, cwd, actor, &["take", "--key", "conflict:key"]),
        1,
        &format!("{state} by {holder}"),
    );
}

#[test]
fn disconnected_takes_converge_to_as_earlier_take() {
    conflict(false, false, true, false);
}

#[test]
fn disconnected_takes_converge_to_bs_earlier_take() {
    conflict(false, false, false, false);
}

#[test]
fn later_done_beats_an_earlier_take_after_reconnect() {
    conflict(false, true, true, false);
}

#[test]
fn competing_done_records_converge_to_the_earlier_done() {
    conflict(true, true, false, false);
}

#[test]
fn equal_timestamp_takes_converge_to_the_lower_node_id() {
    conflict(false, false, true, true);
}

#[test]
fn closing_a_local_taker_releases_the_remote_take() {
    let pair = paired();
    let share = pair.share(true, true, true, true);
    change(
        &pair.a,
        &pair.ra,
        "agent-a",
        "take",
        "closing:key",
        "unfinished",
    );
    let holder = remote_holder(&pair.a, &pair.b, &pair.pb, &share, "agent-a", "claude");
    wait_entry(
        &pair.b,
        &pair.rb,
        "agent-b",
        "closing:key",
        "taken",
        &holder,
        "unfinished",
    );
    pair.a.hook(
        "claude",
        "SessionEnd",
        &json!({"session_id":"agent-a","cwd":pair.ra}),
    );
    assert_eq!(pair.a.agent("agent-a")["status"], "closed");
    wait_free(&pair.b, &pair.rb, "agent-b", "closing:key");
    change(
        &pair.b,
        &pair.rb,
        "agent-b",
        "take",
        "closing:key",
        "resumed",
    );
}
