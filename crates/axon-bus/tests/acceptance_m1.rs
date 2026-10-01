//! Contract decisions
//! - See common/mod.rs for shared CLI, identity, time and hook contracts.
//! - Lazy pre-tool registration is active; OpenCode tool.execute.before and Hermes
//!   pre_llm_call mark active. Tool completion does not imply turn completion.
//! - `install --harness H` is repeatable. Explicitly selected harnesses need no installed
//!   executable. Config locations/shapes below are offline samples, never live configs.
//! - An existing config is backed up adjacent as `<filename>.axon-bus.bak`; uninstall
//!   restores bytes and removes installer-created config/plugin/backup files.
//! - `audit --verify [--db PATH]` exits 1 on corruption, with `audit verification failed`
//!   and the offending sequence number on stderr. A verified chain exits 0.
//! - §2 calls the append-only bus audit table `events`; §0 requires preserving Axon's
//!   existing usage data. Any migration of the old Axon events table must preserve it;
//!   these tests target the bus audit schema, never axon-core's mutable Store API.

mod common;
use common::*;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

fn replay_claude_or_codex(bus: &Bus, harness: &str) {
    let start = bus.fixture(harness, "SessionStart");
    let start_id = start["session_id"].as_str().unwrap();
    bus.hook(harness, "SessionStart", &start);
    assert_eq!(bus.agent(start_id)["status"], "idle");
    let prompt = bus.fixture(harness, "UserPromptSubmit");
    let root = prompt["session_id"].as_str().unwrap();
    bus.hook(harness, "UserPromptSubmit", &prompt);
    assert_eq!(bus.agent(root)["status"], "active");
    bus.replay_hook(harness, "PreToolUse");
    let spawn = bus.fixture(harness, "SubagentStart");
    let child = spawn["agent_id"].as_str().unwrap();
    bus.hook(harness, "SubagentStart", &spawn);
    let agent = bus.agent(child);
    assert_eq!(agent["parent_id"], root);
    assert_eq!(agent["root_id"], root);
    assert_eq!(agent["status"], "active");
    for file in ["PostToolUse", "PreToolUse.child", "PostToolUse.child"] {
        bus.replay_hook(harness, file);
    }
    assert_eq!(bus.agent(child)["status"], "active");
    bus.replay_hook(harness, "SubagentStop");
    assert_eq!(bus.agent(child)["status"], "closed");
    assert_eq!(bus.agent(root)["status"], "active");
    bus.replay_hook(harness, "Stop");
    assert_eq!(bus.agent(root)["status"], "idle");
    // Claude's interactive SessionStart is a different capture from its -p tool run.
    assert_eq!(
        bus.count("SELECT count(*) FROM agents"),
        if harness == "claude" { 3 } else { 2 }
    );
}

// BUS-PLAN §2 and §9 M0: Claude captures preserve distinct sessions and child lifecycle.
#[test]
fn claude_fixture_replay_registers_roots_and_closes_only_the_child() {
    let bus = Bus::new();
    bus.init();
    replay_claude_or_codex(&bus, "claude");
}

// BUS-PLAN §2 and §9 M0: Codex child agent_id is under the session root.
#[test]
fn codex_fixture_replay_preserves_parent_and_turn_statuses() {
    let bus = Bus::new();
    bus.init();
    replay_claude_or_codex(&bus, "codex");
}

// BUS-PLAN §2 and §9 M0: OpenCode's captured root is idle until a tool starts.
#[test]
fn opencode_fixture_replay_has_one_root_and_no_invented_child() {
    let bus = Bus::new();
    bus.init();
    let payload = bus.fixture("opencode", "session.created");
    let id = payload["properties"]["sessionID"].as_str().unwrap();
    bus.hook("opencode", "session.created", &payload);
    assert_eq!(bus.agent(id)["status"], "idle");
    bus.replay_hook("opencode", "tool.execute.before");
    assert_eq!(bus.agent(id)["status"], "active");
    bus.replay_hook("opencode", "tool.execute.after");
    assert_eq!(bus.agent(id)["status"], "active");
    assert_eq!(bus.agent(id)["parent_id"], serde_json::Value::Null);
    assert_eq!(bus.agent(id)["root_id"], id);
    assert_eq!(bus.count("SELECT count(*) FROM agents"), 1);
}

// BUS-PLAN §2 and §9 M0: Hermes explicitly names parent and child sessions.
#[test]
fn hermes_fixture_replay_links_child_session_and_closes_it() {
    let bus = Bus::new();
    bus.init();
    let start = bus.fixture("hermes", "on_session_start");
    let root = start["session_id"].as_str().unwrap();
    bus.hook("hermes", "on_session_start", &start);
    assert_eq!(bus.agent(root)["status"], "idle");
    for event in ["pre_llm_call", "pre_tool_call", "post_tool_call"] {
        bus.replay_hook("hermes", event);
    }
    assert_eq!(bus.agent(root)["status"], "active");
    let spawn = bus.fixture("hermes", "subagent_start");
    let child = spawn["extra"]["child_session_id"].as_str().unwrap();
    bus.hook("hermes", "subagent_start", &spawn);
    let agent = bus.agent(child);
    assert_eq!(agent["session_id"], child);
    assert_eq!(agent["parent_id"], root);
    assert_eq!(agent["root_id"], root);
    assert_eq!(agent["status"], "active");
    bus.replay_hook("hermes", "subagent_stop");
    assert_eq!(bus.agent(child)["status"], "closed");
    assert_eq!(bus.agent(root)["status"], "active");
    assert_eq!(bus.count("SELECT count(*) FROM agents"), 2);
}

// BUS-PLAN §9 M0: claude -p has no SessionStart, so the first hook registers lazily.
#[test]
fn pre_tool_use_alone_registers_exactly_one_active_agent() {
    let bus = Bus::new();
    bus.init();
    let payload = bus.fixture("claude", "PreToolUse");
    assert_allowed(&bus.hook("claude", "PreToolUse", &payload));
    assert_allowed(&bus.hook("claude", "PreToolUse", &payload));
    assert_eq!(bus.count("SELECT count(*) FROM agents"), 1);
    let id = payload["session_id"].as_str().unwrap();
    assert_eq!(bus.agent(id)["status"], "active");
    assert_eq!(bus.agent(id)["root_id"], id);
}

// BUS-PLAN §9 M0: AXON_BUS_PARENT resolves Codex depth beyond the captured first level.
#[test]
fn codex_nested_child_uses_explicit_parent_and_inherits_root() {
    let bus = Bus::new();
    bus.init();
    bus.replay_hook("codex", "SessionStart");
    bus.replay_hook("codex", "SubagentStart");
    let mut payload = bus.fixture("codex", "SubagentStart");
    let parent = payload["agent_id"].as_str().unwrap().to_owned();
    let root = payload["session_id"].as_str().unwrap().to_owned();
    payload["agent_id"] = json!("nested-codex-child");
    bus.cmd()
        .env("AXON_BUS_PARENT", &parent)
        .args(["hook", "codex", "SubagentStart"])
        .write_stdin(payload.to_string())
        .assert()
        .success();
    assert_eq!(bus.agent("nested-codex-child")["parent_id"], parent);
    assert_eq!(bus.agent("nested-codex-child")["root_id"], root);
}

// BUS-PLAN §00: absent hub preserves vanilla behavior for every captured hook.
#[test]
fn every_hook_without_a_hub_exits_zero_silently_and_creates_no_database() {
    let bus = Bus::new();
    for harness in HARNESSES {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/hooks")
            .join(harness);
        let mut paths: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|p| p.unwrap().path())
            .collect();
        paths.sort();
        for path in paths {
            let file = path.file_stem().unwrap().to_str().unwrap();
            let output = bus.replay_hook(harness, file);
            assert_allowed(&output);
            assert!(
                output.stderr.is_empty(),
                "absent hub must be completely silent"
            );
            assert!(!bus.db_path().exists());
        }
    }
    let mut native = bus.fixture("claude", "PreToolUse");
    native["tool_name"] = json!("SendMessage");
    native["tool_input"] = json!({"to":"unregistered-native-peer","message":"hello"});
    assert_allowed(&bus.hook("claude", "PreToolUse", &native));
}

const CONFIGS: [(&str, &str); 4] = [
    ("home/.claude/settings.json", "{\n  \"model\": \"haiku\",\n  \"hooks\": {\"PreToolUse\": [{\"matcher\": \"Bash\", \"hooks\": [{\"type\": \"command\", \"command\": \"echo owner-hook\"}]}]}\n}\n"),
    ("home/.codex/config.toml", "# owner formatting\nmodel = \"gpt-5.5\"\n[hooks]\nPreToolUse = [{ command = \"echo owner-hook\" }]\n"),
    ("config/opencode/opencode.json", "{\n  \"model\": \"ollama/local\", \"plugin\": [\"owner-plugin\"]\n}\n"),
    ("home/.hermes/config.yaml", "# owner formatting\nmodel: local\nplugins:\n  enabled:\n    - owner-plugin\n"),
];
const INSTALL: [&str; 9] = [
    "install",
    "--harness",
    "claude",
    "--harness",
    "codex",
    "--harness",
    "opencode",
    "--harness",
    "hermes",
];

fn seed_configs(bus: &Bus) {
    for (name, bytes) in CONFIGS {
        let path = bus.root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
}
fn config_files(bus: &Bus) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_string_lossy().into(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    for dir in ["home", "config"] {
        walk(&bus.root, &bus.root.join(dir), &mut files);
    }
    files
}

// BUS-PLAN §9 M1: installing twice leaves hooks, plugins and original backups unchanged.
#[test]
fn install_twice_is_a_byte_identical_no_op() {
    let bus = Bus::new();
    seed_configs(&bus);
    bus.ok(&INSTALL);
    let once = config_files(&bus);
    for (name, original) in CONFIGS {
        assert_ne!(fs::read(bus.root.join(name)).unwrap(), original.as_bytes());
        assert_eq!(
            fs::read(bus.root.join(format!("{name}.axon-bus.bak"))).unwrap(),
            original.as_bytes()
        );
    }
    assert!(once
        .values()
        .any(|v| String::from_utf8_lossy(v).contains("axon-bus")));
    bus.ok(&INSTALL);
    assert_eq!(config_files(&bus), once);
}

// BUS-PLAN §9 M1: uninstall restores the user's original config bytes and removes additions.
#[test]
fn uninstall_restores_all_four_original_configs_byte_for_byte() {
    let bus = Bus::new();
    seed_configs(&bus);
    let before = config_files(&bus);
    bus.ok(&INSTALL);
    assert_ne!(config_files(&bus), before);
    bus.ok(&["uninstall"]);
    assert_eq!(config_files(&bus), before);
}

// BUS-PLAN §2: append-only events reject UPDATE, including apparently harmless updates.
#[test]
fn events_reject_update_and_preserve_the_original_actor() {
    let bus = Bus::new();
    bus.init();
    bus.register("audit-root", "claude", None);
    let db = bus.db();
    let before: String = db
        .query_row("SELECT actor FROM events ORDER BY seq LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(db
        .execute(
            "UPDATE events SET actor='tampered' WHERE seq=(SELECT min(seq) FROM events)",
            []
        )
        .is_err());
    assert_eq!(
        db.query_row("SELECT actor FROM events ORDER BY seq LIMIT 1", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        before
    );
}

// BUS-PLAN §2: append-only events reject DELETE and retain the complete chain.
#[test]
fn events_reject_delete_and_preserve_the_row_count() {
    let bus = Bus::new();
    bus.init();
    bus.register("audit-root", "claude", None);
    let count = bus.count("SELECT count(*) FROM events");
    assert!(count > 0);
    assert!(bus.db().execute("DELETE FROM events", []).is_err());
    assert_eq!(bus.count("SELECT count(*) FROM events"), count);
}

// BUS-PLAN §9 M1: verification detects content tampering even after audit triggers are restored.
#[test]
fn audit_verify_detects_an_edited_row_in_a_trigger_bypassing_copy() {
    let bus = Bus::new();
    bus.init();
    bus.register("audit-one", "claude", None);
    bus.register("audit-two", "codex", None);
    bus.ok(&["audit", "--verify"]);
    let copy = bus.root.join("edited.db");
    bus.db()
        .execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
        .unwrap();
    bus.ok(&["audit", "--verify", "--db", copy.to_str().unwrap()]);
    let db = rusqlite::Connection::open(&copy).unwrap();
    let seq: i64 = db
        .query_row("SELECT min(seq) FROM events", [], |r| r.get(0))
        .unwrap();
    let triggers: Vec<(String, String)> = db
        .prepare("SELECT name,sql FROM sqlite_master WHERE type='trigger' AND tbl_name='events'")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(!triggers.is_empty());
    for (name, _) in &triggers {
        db.execute_batch(&format!("DROP TRIGGER \"{}\"", name.replace('"', "\"\"")))
            .unwrap();
    }
    assert_eq!(
        db.execute(
            "UPDATE events SET actor=?1 WHERE seq=?2",
            rusqlite::params!["tampered-actor", seq]
        )
        .unwrap(),
        1
    );
    for (_, sql) in &triggers {
        db.execute_batch(sql).unwrap();
    }
    drop(db);
    let output = bus
        .cmd()
        .args(["audit", "--verify", "--db", copy.to_str().unwrap()])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("audit verification failed"), "{stderr}");
    assert!(
        stderr.contains(&seq.to_string()),
        "must identify the damaged row: {stderr}"
    );
    bus.ok(&["audit", "--verify"]);
}
