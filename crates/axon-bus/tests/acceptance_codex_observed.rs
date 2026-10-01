//! Frozen working-root contract: real process table -> real ingested rollout -> HTTP.
//! A tiny executable blocks on stdin, keeping its executable name, argv and OS cwd visible.
//! No Codex installation, credentials, network API, or changes to the parent's env are used.

#![cfg(unix)]

mod common;
use axon_core::{ingest, normalize, pricing::Pricing, store::Store};
use common::*;
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

fn sandbox() -> Bus {
    let bus = Bus::with_limit(Duration::from_secs(10));
    bus.init();
    for dir in ["bin", "repo", "repo-wt", "other", "repo-wt/nested"] {
        fs::create_dir_all(bus.root.join(dir)).unwrap();
    }
    bus
}

fn harness(bus: &Bus, name: &str, cwd: &Path, argv: &[String]) -> Running {
    // Copying an Apple platform binary can trigger SIGKILL, even after ad-hoc signing.
    // Compile once per test binary, outside the isolated HOME so rustup can find its toolchain.
    static FIXTURE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let fixture = FIXTURE.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fixture.rs");
        fs::write(
            &source,
            "fn main() { std::io::stdin().read_line(&mut String::new()).unwrap(); }",
        )
        .unwrap();
        let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg(&source)
            .arg("-o")
            .arg(dir.path().join("fixture"))
            .output()
            .expect("rustc must be available to compile the process fixture");
        assert!(
            output.status.success(),
            "compiling the process fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::read(dir.path().join("fixture")).unwrap()
    });
    let exe = bus.root.join("bin").join(name);
    if !exe.exists() {
        // A symlink resolves to its target on Linux and would not exercise harness discovery.
        fs::write(&exe, fixture).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut command = Command::new(exe);
    bus.isolate(&mut command);
    let child = command
        .current_dir(cwd)
        .args(argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Running(child)
}

fn argv(flag: &str, directory: &str) -> Vec<String> {
    let mut args = vec!["exec".to_owned()];
    if flag == "--cd=" {
        args.push(format!("--cd={directory}"));
    } else {
        args.extend([flag.to_owned(), directory.to_owned()]);
    }
    args
}

fn rollout(bus: &Bus, session: &str, cwd: &Path) {
    rollout_at(bus, session, cwd, chrono::Utc::now());
}

fn rollout_at(bus: &Bus, session: &str, cwd: &Path, now: chrono::DateTime<chrono::Utc>) {
    let mut records: Vec<Value> = include_str!("../../../tests/fixtures/codex.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for (index, record) in records.iter_mut().enumerate() {
        let ts = now - chrono::Duration::milliseconds(6 - index as i64);
        record["timestamp"] = json!(ts.to_rfc3339());
        if record["type"] == "session_meta" {
            record["payload"]["id"] = json!(session);
            record["payload"]["cwd"] = json!(cwd);
        }
    }
    let sessions = bus.home().join(".codex/sessions");
    let day = sessions.join(now.format("%Y/%m/%d").to_string());
    fs::create_dir_all(&day).unwrap();
    let path = day.join(format!(
        "rollout-{}-{session}.jsonl",
        now.format("%FT%H-%M-%S")
    ));
    write_jsonl(&path, &records);
    // Use the public parser/normalizer/store, not SQL invented for the matching code.
    let source = ingest::codex_sources(&sessions)
        .into_iter()
        .find(|source| source.path == path)
        .expect("the isolated rollout must be discoverable");
    let turns = source.parse().unwrap();
    assert_eq!(turns.len(), 2, "captured fixture contains two turns");
    let store = Store::open(bus.db_path().to_str().unwrap()).unwrap();
    for turn in turns {
        store
            .upsert(&normalize::to_event(&turn, &Pricing::bundled()).unwrap())
            .unwrap();
    }
}

fn node(snapshot: &Value, pid: u32) -> &Value {
    fn find(value: &Value, pid: u32) -> Option<&Value> {
        match value {
            Value::Object(object) => {
                if value["observed"] == true && value["pid"].as_u64() == Some(u64::from(pid)) {
                    Some(value)
                } else {
                    object.values().find_map(|value| find(value, pid))
                }
            }
            Value::Array(array) => array.iter().find_map(|value| find(value, pid)),
            _ => None,
        }
    }
    find(snapshot, pid).unwrap_or_else(|| panic!("process {pid} missing from snapshot: {snapshot}"))
}

fn assert_matched(node: &Value) {
    assert_eq!(node["harness"], "codex", "{node}");
    assert_eq!(node["model"], "gpt-5.5", "{node}");
    // Captured cumulative usage: 4,000 input (including 700 cached) + 1,200 output.
    assert_eq!(node["tokens"], 5_200, "{node}");
    assert_eq!(node["activity"]["turns"], 2, "{node}");
    assert_eq!(node["activity"]["lines_added"], 2, "{node}");
    assert_eq!(node["status"], "active", "{node}");
}

fn assert_unmatched(node: &Value) {
    assert_eq!(node["tokens"], 0, "{node}");
    assert!(node["model"].is_null(), "{node}");
    assert!(node["activity"].is_null(), "{node}");
    assert_eq!(node["status"], "idle", "{node}");
}

fn matches_working_root(flag: &str, relative: bool) {
    let bus = sandbox();
    let worktree = bus.root.join("repo-wt");
    let directory = if relative {
        "../repo-wt".to_owned()
    } else {
        worktree.to_str().unwrap().to_owned()
    };
    let mut process = harness(
        &bus,
        "codex",
        &bus.root.join("repo"),
        &argv(flag, &directory),
    );
    rollout(&bus, "working-root", &worktree);
    let server = Server::start(&bus, false);
    assert!(process.0.try_wait().unwrap().is_none());
    assert_matched(node(&server.snapshot(&bus), process.0.id()));
    assert_eq!(
        bus.count("SELECT count(*) FROM agents"),
        0,
        "observation must not register agents"
    );
}

#[test]
fn short_c_absolute_matches_sibling_rollout() {
    matches_working_root("-C", false);
}

#[test]
fn long_cd_absolute_matches_sibling_rollout() {
    matches_working_root("--cd", false);
}

#[test]
fn long_cd_equals_absolute_matches_sibling_rollout() {
    matches_working_root("--cd=", false);
}

#[test]
fn short_c_relative_resolves_against_process_cwd() {
    matches_working_root("-C", true);
}

#[test]
fn long_cd_relative_resolves_against_process_cwd() {
    matches_working_root("--cd", true);
}

#[test]
fn long_cd_equals_relative_resolves_against_process_cwd() {
    matches_working_root("--cd=", true);
}

#[test]
fn no_flag_keeps_os_cwd_matching() {
    let bus = sandbox();
    let cwd = bus.root.join("repo-wt");
    let process = harness(&bus, "codex", &cwd, &["exec".into()]);
    rollout(&bus, "no-flag", &cwd);
    let server = Server::start(&bus, false);
    assert_matched(node(&server.snapshot(&bus), process.0.id()));
}

#[test]
fn flag_to_another_directory_does_not_match_sibling_rollout() {
    let bus = sandbox();
    let process = harness(
        &bus,
        "codex",
        &bus.root.join("repo"),
        &argv("-C", "../other"),
    );
    rollout(&bus, "wrong-directory", &bus.root.join("repo-wt"));
    let server = Server::start(&bus, false);
    assert_unmatched(node(&server.snapshot(&bus), process.0.id()));
}

#[test]
fn flag_replaces_os_cwd_instead_of_adding_another_candidate() {
    let bus = sandbox();
    let cwd = bus.root.join("repo");
    let process = harness(&bus, "codex", &cwd, &argv("-C", "../other"));
    rollout(&bus, "old-os-cwd", &cwd);
    let server = Server::start(&bus, false);
    assert_unmatched(node(&server.snapshot(&bus), process.0.id()));
}

#[test]
fn effective_cwd_retains_descendant_project_matching() {
    let bus = sandbox();
    let process = harness(
        &bus,
        "codex",
        &bus.root.join("repo"),
        &argv("-C", "../repo-wt"),
    );
    rollout(&bus, "descendant", &bus.root.join("repo-wt/nested"));
    let server = Server::start(&bus, false);
    assert_matched(node(&server.snapshot(&bus), process.0.id()));
}

#[test]
fn flag_and_os_cwd_processes_compete_equally_so_neither_claims_the_rollout() {
    let bus = sandbox();
    let cwd = bus.root.join("repo-wt");
    let flagged = harness(
        &bus,
        "codex",
        &bus.root.join("repo"),
        &argv("-C", "../repo-wt"),
    );
    let direct = harness(&bus, "codex", &cwd, &["exec".into()]);
    // Both starts are within the existing start-time slack; neither was matched earlier.
    rollout(&bus, "ambiguous-owners", &cwd);
    let server = Server::start(&bus, false);
    let snapshot = server.snapshot(&bus);
    assert_unmatched(node(&snapshot, flagged.0.id()));
    assert_unmatched(node(&snapshot, direct.0.id()));
}

#[test]
fn effective_cwd_does_not_choose_between_two_eligible_rollouts() {
    let bus = sandbox();
    let cwd = bus.root.join("repo-wt");
    let process = harness(
        &bus,
        "codex",
        &bus.root.join("repo"),
        &argv("-C", "../repo-wt"),
    );
    let now = chrono::Utc::now();
    rollout_at(&bus, "ambiguous-first", &cwd, now);
    rollout_at(&bus, "ambiguous-second", &cwd, now);
    let server = Server::start(&bus, false);
    assert_unmatched(node(&server.snapshot(&bus), process.0.id()));
}

#[test]
fn codex_working_root_flags_leave_other_harness_cwds_unchanged() {
    let bus = sandbox();
    let cwd = bus.root.join("repo");
    rollout(&bus, "codex-only", &bus.root.join("repo-wt"));
    let mut processes = Vec::new();
    for name in ["claude", "opencode", "hermes"] {
        for flag in ["-C", "--cd", "--cd="] {
            processes.push(harness(&bus, name, &cwd, &argv(flag, "../repo-wt")));
        }
    }
    let server = Server::start(&bus, false);
    let snapshot = server.snapshot(&bus);
    for process in processes {
        let observed = node(&snapshot, process.0.id());
        assert_eq!(observed["repo"], json!(cwd), "{observed}");
        assert_unmatched(observed);
    }
}

#[test]
fn claude_still_matches_its_os_cwd_transcript_when_argv_contains_codex_flags() {
    let bus = sandbox();
    let cwd = bus.root.join("repo");
    let process = harness(&bus, "claude", &cwd, &argv("-C", "../other"));
    let mut record: Value = serde_json::from_str(
        include_str!("../../../tests/fixtures/claude_main.jsonl")
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    record["sessionId"] = json!("claude-os-cwd");
    record["cwd"] = json!(cwd);
    record["timestamp"] = json!(chrono::Utc::now().to_rfc3339());
    let projects = bus.home().join(".claude/projects");
    let project = projects.join("fixture");
    fs::create_dir_all(&project).unwrap();
    write_jsonl(&project.join("claude-os-cwd.jsonl"), &[record]);
    let turns = ingest::claude_sources(&projects)
        .pop()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(turns.len(), 1);
    let store = Store::open(bus.db_path().to_str().unwrap()).unwrap();
    store
        .upsert(&normalize::to_event(&turns[0], &Pricing::bundled()).unwrap())
        .unwrap();
    let server = Server::start(&bus, false);
    let snapshot = server.snapshot(&bus);
    let observed = node(&snapshot, process.0.id());
    assert_eq!(observed["harness"], "claude");
    // Captured input + output + one-hour cache creation, with no Codex fixture involved.
    assert_eq!(observed["tokens"], 11_341 + 10_498 + 29_928, "{observed}");
    assert_eq!(observed["activity"]["turns"], 1, "{observed}");
    assert_eq!(observed["status"], "active", "{observed}");
}
