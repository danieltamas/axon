//! Frozen BUS-PLAN §3b contract (2026-10-03), authored independently of implementation.
//! axon-bus is the workspace's CLI alias for `axon bus`; --agent selects the registered
//! caller in direct CLI tests, as on claim/peers. Hooks separately enforce that identity.
//! Only common::Bus sandboxes are touched. SQL moves timestamps, never creates a schema.
mod common;

use common::fed::{eventually, git, repo};
use common::{canonical, deny_reason, now_ms, parse_json, Bus, Server};
use rusqlite::params;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

fn setup() -> (Bus, PathBuf) {
    let bus = Bus::with_limit(Duration::from_secs(90));
    bus.init();
    let checkout = repo(&bus, "project");
    bus.register_at("alice", "claude", None, &checkout);
    bus.register_at("bob", "codex", None, &checkout);
    (bus, checkout)
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
    let diagnostic = if code == 2 {
        format!("{stdout}{stderr}")
    } else {
        stdout.clone()
    };
    assert!(
        diagnostic.contains(contains),
        "missing {contains:?}: {diagnostic}"
    );
    stdout
}

fn call(bus: &Bus, cwd: &Path, actor: &str, args: &[&str], code: i32, text: &str) -> String {
    expect(run(bus, cwd, actor, args), code, text)
}

fn entries(bus: &Bus, cwd: &Path, prefix: Option<&str>) -> Vec<Value> {
    let mut args = vec!["handled"];
    if let Some(prefix) = prefix {
        args.extend(["--prefix", prefix]);
    }
    call(bus, cwd, "bob", &args, 0, "")
        .lines()
        .map(|line| {
            let row: Value = serde_json::from_str(line).expect("one JSON object per line");
            for field in ["key", "state", "holder", "note", "at", "expires_at"] {
                assert!(row.get(field).is_some(), "missing {field}: {row}");
            }
            assert!(row["key"].is_string() && row["holder"].is_string());
            assert!(row["at"].is_i64());
            assert!(row["expires_at"].is_null() || row["expires_at"].is_i64());
            assert!(row["state"] == "taken" || row["state"] == "done");
            row
        })
        .collect()
}

fn taken(bus: &Bus, cwd: &Path, actor: &str, key: &str, note: &str) {
    assert_eq!(
        call(
            bus,
            cwd,
            actor,
            &["take", "--key", key, "--note", note],
            0,
            &format!("taken {key}")
        ),
        format!("taken {key}\n")
    );
}

fn occupied(line: &str, state: &str, holder: &str, note: &str) {
    let age = line
        .trim_end()
        .strip_prefix(&format!("{state} by {holder} "))
        .and_then(|line| line.strip_suffix(&format!(" ago: {note}")))
        .unwrap_or_else(|| panic!("expected {state} by {holder} <age> ago: {note}; got {line:?}"));
    assert!(!age.is_empty(), "holder line must include its age");
}

#[test]
fn take_is_exclusive_and_holder_refreshes_the_default_two_hour_lease() {
    let (bus, cwd) = setup();
    taken(&bus, &cwd, "alice", "lead:acme", "contacting purchasing");
    let first = entries(&bus, &cwd, None).remove(0);
    assert_eq!(
        first["expires_at"].as_i64().unwrap() - first["at"].as_i64().unwrap(),
        7_200_000
    );
    occupied(
        &call(
            &bus,
            &cwd,
            "bob",
            &["take", "--key", "lead:acme"],
            1,
            "taken by alice",
        ),
        "taken",
        "alice",
        "contacting purchasing",
    );
    bus.db()
        .execute(
            "UPDATE handled SET at=?1,expires_at=?2 WHERE repo=?3 AND key=?4",
            params![
                now_ms() - 60_000,
                now_ms() + 60_000,
                cwd.to_str().unwrap(),
                "lead:acme"
            ],
        )
        .unwrap();
    let before = entries(&bus, &cwd, None).remove(0);
    taken(&bus, &cwd, "alice", "lead:acme", "follow-up scheduled");
    let refreshed = entries(&bus, &cwd, None).remove(0);
    assert!(refreshed["expires_at"].as_i64().unwrap() > before["expires_at"].as_i64().unwrap());
    assert_eq!(refreshed["note"], "follow-up scheduled");
    assert_eq!(refreshed["holder"], "alice");
}

#[test]
fn done_by_holder_is_final_and_the_first_done_record_stands() {
    let (bus, cwd) = setup();
    taken(&bus, &cwd, "alice", "issue:142", "investigating");
    occupied(
        &call(
            &bus,
            &cwd,
            "bob",
            &["done", "--key", "issue:142"],
            1,
            "taken by alice",
        ),
        "taken",
        "alice",
        "investigating",
    );
    assert_eq!(
        call(
            &bus,
            &cwd,
            "alice",
            &["done", "--key", "issue:142", "--note", "triaged"],
            0,
            "done issue:142"
        ),
        "done issue:142\n"
    );
    let first = entries(&bus, &cwd, None);
    for actor in ["alice", "bob"] {
        for command in ["take", "done"] {
            occupied(
                &call(
                    &bus,
                    &cwd,
                    actor,
                    &[command, "--key", "issue:142", "--note", "overwrite"],
                    1,
                    "done by alice",
                ),
                "done",
                "alice",
                "triaged",
            );
            assert_eq!(
                entries(&bus, &cwd, None),
                first,
                "first done must remain unchanged"
            );
        }
    }
    call(&bus, &cwd, "alice", &["drop", "--key", "issue:142"], 1, "");
    assert_eq!(entries(&bus, &cwd, None), first);
}

#[test]
fn done_does_not_require_a_prior_take() {
    let (bus, cwd) = setup();
    call(
        &bus,
        &cwd,
        "bob",
        &[
            "done",
            "--key",
            "url:https://example.com/a",
            "--note",
            "crawled",
        ],
        0,
        "done url:https://example.com/a",
    );
    occupied(
        &call(
            &bus,
            &cwd,
            "alice",
            &["handled", "--key", "url:https://example.com/a"],
            0,
            "done by bob",
        ),
        "done",
        "bob",
        "crawled",
    );
}

#[test]
fn drop_only_releases_the_callers_own_take() {
    let (bus, cwd) = setup();
    taken(&bus, &cwd, "alice", "lead:drop", "reserved");
    call(&bus, &cwd, "bob", &["drop", "--key", "lead:drop"], 1, "");
    call(
        &bus,
        &cwd,
        "bob",
        &["handled", "--key", "lead:drop"],
        0,
        "taken by alice",
    );
    assert_eq!(
        call(
            &bus,
            &cwd,
            "alice",
            &["drop", "--key", "lead:drop"],
            0,
            "dropped lead:drop"
        ),
        "dropped lead:drop\n"
    );
    call(&bus, &cwd, "alice", &["drop", "--key", "lead:drop"], 1, "");
    call(&bus, &cwd, "bob", &["handled", "--key", "lead:drop"], 1, "");
    taken(&bus, &cwd, "bob", "lead:drop", "new owner");
}

#[test]
fn ttl_expiry_frees_the_key_for_take_and_done() {
    let (bus, cwd) = setup();
    for operation in ["take", "done"] {
        let key = format!("expiry:{operation}");
        call(
            &bus,
            &cwd,
            "alice",
            &["take", "--key", &key, "--ttl", "30m"],
            0,
            "taken ",
        );
        let row = entries(&bus, &cwd, Some(&key)).remove(0);
        assert_eq!(
            row["expires_at"].as_i64().unwrap() - row["at"].as_i64().unwrap(),
            1_800_000
        );
        // The suite clock seam belongs to federation; move this isolated local lease instead.
        assert_eq!(
            bus.db()
                .execute(
                    "UPDATE handled SET expires_at=?1 WHERE repo=?2 AND key=?3",
                    params![now_ms() - 1, cwd.to_str().unwrap(), key]
                )
                .unwrap(),
            1
        );
        call(&bus, &cwd, "bob", &["handled", "--key", &key], 1, "");
        call(
            &bus,
            &cwd,
            "bob",
            &[operation, "--key", &key],
            0,
            &format!(
                "{} {key}",
                if operation == "take" { "taken" } else { "done" }
            ),
        );
    }
}

#[test]
fn closed_or_orphaned_takers_release_work_but_done_survives() {
    for status in ["closed", "orphaned"] {
        let (bus, cwd) = setup();
        taken(&bus, &cwd, "alice", "lead:abandoned", "unfinished");
        call(
            &bus,
            &cwd,
            "alice",
            &["done", "--key", "lead:finished"],
            0,
            "done lead:finished",
        );
        if status == "closed" {
            bus.hook(
                "claude",
                "SessionEnd",
                &json!({"session_id":"alice","cwd":cwd}),
            );
        } else {
            bus.db()
                .execute("UPDATE agents SET status='orphaned' WHERE id='alice'", [])
                .unwrap();
        }
        assert_eq!(bus.agent("alice")["status"], status);
        call(
            &bus,
            &cwd,
            "bob",
            &["handled", "--key", "lead:abandoned"],
            1,
            "",
        );
        taken(&bus, &cwd, "bob", "lead:abandoned", "resumed");
        call(
            &bus,
            &cwd,
            "bob",
            &["take", "--key", "lead:finished"],
            1,
            "done by alice",
        );
    }
}

#[test]
fn main_checkout_and_worktree_share_a_ledger_but_other_repos_do_not() {
    let (bus, cwd) = setup();
    let worktree = bus.root.join("linked-worktree");
    git(
        &bus,
        &cwd,
        &[
            "worktree",
            "add",
            "-b",
            "acceptance",
            worktree.to_str().unwrap(),
        ],
    );
    let worktree = canonical(&worktree);
    let other = repo(&bus, "other-project");
    bus.register_at("worktree-agent", "codex", None, &worktree);
    bus.register_at("other-agent", "claude", None, &other);
    taken(&bus, &cwd, "alice", "lead:shared", "main checkout");
    call(
        &bus,
        &worktree,
        "worktree-agent",
        &["take", "--key", "lead:shared"],
        1,
        "taken by alice",
    );
    taken(&bus, &other, "other-agent", "lead:shared", "separate repo");
    call(
        &bus,
        &worktree,
        "worktree-agent",
        &["done", "--key", "issue:worktree"],
        0,
        "done issue:worktree",
    );
    call(
        &bus,
        &cwd,
        "bob",
        &["handled", "--key", "issue:worktree"],
        0,
        "done by worktree-agent",
    );
    call(
        &bus,
        &other,
        "other-agent",
        &["handled", "--key", "issue:worktree"],
        1,
        "",
    );
}

#[test]
fn every_ledger_command_rejects_a_caller_outside_a_repository() {
    let (bus, _) = setup();
    let outside = bus.root.join("work");
    bus.register_at("outside", "claude", None, &outside);
    for command in ["take", "done", "drop", "handled"] {
        call(
            &bus,
            &outside,
            "outside",
            &[command, "--key", "lead:acme"],
            2,
            "not in a repository",
        );
    }
    call(
        &bus,
        &outside,
        "outside",
        &["handled"],
        2,
        "not in a repository",
    );
}

#[test]
fn key_validation_is_shared_by_all_four_commands() {
    let (bus, cwd) = setup();
    for key in [
        String::new(),
        "   ".into(),
        "x".repeat(201),
        "lead:bad\u{7}key".into(),
    ] {
        for command in ["take", "done", "drop", "handled"] {
            call(
                &bus,
                &cwd,
                "alice",
                &[command, "--key", &key],
                2,
                "invalid key",
            );
        }
    }
    assert!(entries(&bus, &cwd, None).is_empty());
}

#[test]
fn keys_are_trimmed_and_limits_count_characters() {
    let (bus, cwd) = setup();
    call(
        &bus,
        &cwd,
        "alice",
        &["take", "--key", "  lead:trimmed  "],
        0,
        "taken lead:trimmed\n",
    );
    call(
        &bus,
        &cwd,
        "bob",
        &["handled", "--key", "lead:trimmed"],
        0,
        "taken by alice",
    );
    call(
        &bus,
        &cwd,
        "alice",
        &["drop", "--key", " lead:trimmed "],
        0,
        "dropped lead:trimmed\n",
    );
    for key in ["x".to_owned(), "é".repeat(200)] {
        taken(&bus, &cwd, "alice", &key, &"é".repeat(400));
    }
}

#[test]
fn ttl_over_twenty_four_hours_and_overlong_notes_are_rejected() {
    let (bus, cwd) = setup();
    call(
        &bus,
        &cwd,
        "alice",
        &["take", "--key", "ttl:too-long", "--ttl", "25h"],
        2,
        "",
    );
    for command in ["take", "done"] {
        call(
            &bus,
            &cwd,
            "alice",
            &[
                command,
                "--key",
                "note:too-long",
                "--note",
                &"n".repeat(401),
            ],
            2,
            "",
        );
    }
    assert!(entries(&bus, &cwd, None).is_empty());
    call(
        &bus,
        &cwd,
        "alice",
        &["take", "--key", "ttl:maximum", "--ttl", "24h"],
        0,
        "taken ttl:maximum",
    );
    let row = entries(&bus, &cwd, None).remove(0);
    assert_eq!(
        row["expires_at"].as_i64().unwrap() - row["at"].as_i64().unwrap(),
        86_400_000
    );
}

#[test]
fn handled_key_distinguishes_free_own_other_and_done() {
    let (bus, cwd) = setup();
    call(
        &bus,
        &cwd,
        "alice",
        &["handled", "--key", "lead:state"],
        1,
        "",
    );
    taken(&bus, &cwd, "alice", "lead:state", "working");
    call(
        &bus,
        &cwd,
        "alice",
        &["handled", "--key", "lead:state"],
        1,
        "",
    );
    occupied(
        &call(
            &bus,
            &cwd,
            "bob",
            &["handled", "--key", "lead:state"],
            0,
            "taken by alice",
        ),
        "taken",
        "alice",
        "working",
    );
    call(
        &bus,
        &cwd,
        "alice",
        &["done", "--key", "lead:state", "--note", "complete"],
        0,
        "done lead:state",
    );
    for actor in ["alice", "bob"] {
        occupied(
            &call(
                &bus,
                &cwd,
                actor,
                &["handled", "--key", "lead:state"],
                0,
                "done by alice",
            ),
            "done",
            "alice",
            "complete",
        );
    }
}

#[test]
fn json_lines_have_documented_fields_newest_first_and_prefix_filtering() {
    let (bus, cwd) = setup();
    for (index, key) in ["lead:old", "issue:middle", "lead:new"].iter().enumerate() {
        taken(&bus, &cwd, "alice", key, key);
        bus.db()
            .execute(
                "UPDATE handled SET at=?1 WHERE repo=?2 AND key=?3",
                params![
                    now_ms() - 30_000 + index as i64 * 1_000,
                    cwd.to_str().unwrap(),
                    key
                ],
            )
            .unwrap();
    }
    let rows = entries(&bus, &cwd, None);
    assert_eq!(
        rows.iter()
            .map(|r| r["key"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["lead:new", "issue:middle", "lead:old"]
    );
    for row in &rows {
        assert_eq!(row["note"], row["key"]);
        assert_eq!(row["holder"], "alice");
    }
    let filtered = entries(&bus, &cwd, Some("lead:"));
    assert_eq!(filtered, vec![rows[0].clone(), rows[2].clone()]);
    assert!(entries(&bus, &cwd, Some("unknown:")).is_empty());
}

#[test]
fn listings_return_only_the_newest_five_hundred_entries() {
    let (bus, cwd) = setup();
    let oldest = now_ms() - 10_000;
    for i in 0..501 {
        let key = format!("bulk:{i:03}");
        taken(&bus, &cwd, "alice", &key, "bulk fixture");
        bus.db()
            .execute(
                "UPDATE handled SET at=?1 WHERE repo=?2 AND key=?3",
                params![oldest + i, cwd.to_str().unwrap(), key],
            )
            .unwrap();
    }
    let rows = entries(&bus, &cwd, None);
    assert_eq!(rows.len(), 500);
    assert_eq!(rows.first().unwrap()["key"], "bulk:500");
    assert_eq!(rows.last().unwrap()["key"], "bulk:001");
    assert_eq!(entries(&bus, &cwd, Some("bulk:")), rows);
}

#[test]
fn simultaneous_takes_have_exactly_one_winner() {
    let (bus, cwd) = setup();
    let barrier = std::sync::Barrier::new(2);
    let outputs = std::thread::scope(|scope| {
        let workers: Vec<_> = ["alice", "bob"]
            .into_iter()
            .map(|actor| {
                let (bus, cwd, barrier) = (&bus, &cwd, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    (actor, run(bus, cwd, actor, &["take", "--key", "race"]))
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .collect::<Vec<_>>()
    });
    let winner = outputs
        .iter()
        .find(|(_, o)| o.status.code() == Some(0))
        .unwrap_or_else(|| panic!("one take must win: {outputs:?}"))
        .0;
    assert_eq!(
        outputs
            .iter()
            .filter(|(_, o)| o.status.code() == Some(0))
            .count(),
        1
    );
    for (actor, output) in outputs {
        expect(
            output,
            if actor == winner { 0 } else { 1 },
            &if actor == winner {
                "taken race".into()
            } else {
                format!("taken by {winner}")
            },
        );
    }
}

#[test]
fn root_and_subagent_introductions_teach_take_and_done_and_guide_lists_commands() {
    let (bus, cwd) = setup();
    for (event, child) in [("SessionStart", false), ("SubagentStart", true)] {
        let mut payload = bus.fixture("claude", event);
        payload["session_id"] = json!("intro-root");
        payload["cwd"] = json!(cwd);
        if child {
            payload["agent_id"] = json!("intro-child");
        }
        let reply = parse_json(&bus.hook("claude", event, &payload).stdout);
        let context = reply["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        for phrase in [
            "Before working an item others might also pick up (a lead, an issue, a URL)",
            "take --key <kind:id>",
            "done --key <kind:id>",
        ] {
            assert!(
                context.contains(phrase),
                "{event} missing {phrase:?}: {context}"
            );
        }
    }
    let guide = String::from_utf8(bus.ok(&["guide"]).stdout).unwrap();
    for command in ["take", "done", "drop", "handled"] {
        assert!(
            guide.contains(&format!(" {command}")),
            "guide missing {command}: {guide}"
        );
    }
}

#[test]
fn gate_refuses_forged_ledger_callers() {
    let (bus, cwd) = setup();
    taken(&bus, &cwd, "alice", "guarded", "do not alter");
    let before = entries(&bus, &cwd, None);
    for command in ["take", "done", "drop", "handled"] {
        let mut payload = bus.fixture("claude", "PreToolUse");
        payload["session_id"] = json!("alice");
        payload["cwd"] = json!(cwd);
        payload["tool_input"] =
            json!({"command":format!("axon bus {command} --agent bob --key guarded")});
        let reason = deny_reason("claude", &bus.hook("claude", "PreToolUse", &payload));
        assert!(reason.contains("as yourself"), "{reason}");
    }
    assert_eq!(entries(&bus, &cwd, None), before);
}

#[test]
fn dashboard_ledger_requires_owner_session_and_returns_only_the_requested_repo() {
    let (bus, cwd) = setup();
    let server = Server::start(&bus, true);
    let path = format!("/api/handled?repo={}", cwd.to_str().unwrap());
    let anonymous = server.raw_request(&bus, "GET", &path, &[], "");
    assert_eq!(
        anonymous.status,
        401,
        "{}",
        String::from_utf8_lossy(&anonymous.body)
    );
    taken(&bus, &cwd, "alice", "lead:old", "older");
    bus.db()
        .execute(
            "UPDATE handled SET at=?1 WHERE key='lead:old'",
            [now_ms() - 60_000],
        )
        .unwrap();
    call(
        &bus,
        &cwd,
        "bob",
        &["done", "--key", "issue:new", "--note", "newer"],
        0,
        "done issue:new",
    );
    let other = repo(&bus, "private-project");
    bus.register_at("private", "claude", None, &other);
    taken(&bus, &other, "private", "private:key", "other repository");
    let response = server.request(&bus, "GET", &path, &[], "");
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    assert_eq!(
        parse_json(&response.body),
        json!({"entries":entries(&bus, &cwd, None)})
    );
    assert_eq!(parse_json(&response.body)["entries"][0]["key"], "issue:new");
}

#[test]
fn done_entries_are_kept_for_ninety_days_then_deleted() {
    let (bus, cwd) = setup();
    for (key, days) in [("done:retained", 89), ("done:expired", 91)] {
        call(
            &bus,
            &cwd,
            "alice",
            &["done", "--key", key],
            0,
            &format!("done {key}"),
        );
        let at = now_ms() - days * 86_400_000;
        bus.db()
            .execute(
                "UPDATE handled SET at=?1, expires_at=CASE WHEN expires_at IS NULL
            THEN NULL ELSE ?2 END WHERE key=?3",
                params![at, at + 90 * 86_400_000, key],
            )
            .unwrap();
    }
    let _server = Server::start(&bus, true);
    eventually(
        &bus,
        "ninety-day retention deletes expired done entries",
        || {
            call(&bus, &cwd, "bob", &["handled"], 0, "");
            bus.count("SELECT count(*) FROM handled WHERE key='done:expired'") == 0
        },
    );
    call(
        &bus,
        &cwd,
        "bob",
        &["handled", "--key", "done:expired"],
        1,
        "",
    );
    call(
        &bus,
        &cwd,
        "bob",
        &["handled", "--key", "done:retained"],
        0,
        "done by alice",
    );
}
