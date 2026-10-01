//! Frozen Codex hook contract. Format changes require evidence from Codex 0.153.
//! All CLI calls inherit common::Bus isolation; the real ~/.codex is never accessed.

mod common;
use common::*;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

// Codex 0.153.4 accepts this nested shape in hooks.json or equivalent config.toml. Substitute
// JSON-encoded values, not raw shell strings. Extra group metadata (e.g. matcher) is OK.
const CODEX_HOOK_FORMAT: &str =
    r#"{"hooks":{EVENT:[{"hooks":[{"type":"command","command":COMMAND}]}]}}"#;
const EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "SubagentStart",
    "SubagentStop",
    "Stop",
];
const OWNER_CONFIG: &str = "# owner formatting must survive uninstall\nmodel = \"owner-model\"\n\n[hooks]\nSessionStart = [{ command = \"echo owner-config-start\", timeout = 17 }]\nPreToolUse = [{ command = \"echo owner-config-pre\" }]\nOwnerEvent = [{ command = \"echo owner-config-custom\" }]\n";
const OWNER_HOOKS: &str = r#"{
  "owner_setting": {"enabled": true},
  "hooks": {
    "SessionStart": [
      {"matcher": "startup", "hooks": [{"type": "command", "command": "echo owner-json-start", "timeout": 23}]},
      {"matcher": "resume", "hooks": [{"type": "command", "command": "echo owner-json-resume"}]}
    ],
    "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "echo owner-json-pre"}]}],
    "OwnerEvent": [{"hooks": [{"type": "command", "command": "echo owner-json-custom"}]}]
  }
}
"#;

fn config(bus: &Bus) -> PathBuf {
    bus.home().join(".codex/config.toml")
}

fn hook_file(bus: &Bus) -> PathBuf {
    bus.home().join(".codex/hooks.json")
}

fn command(event: &str) -> String {
    // The package's supported axon-bus alias takes `hook` directly, without `bus`.
    let path = assert_cmd::cargo::cargo_bin!("axon-bus").to_str().unwrap();
    let path = if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    let quoted = if path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+:@%,=".contains(c))
    {
        path
    } else if cfg!(windows) {
        format!("\"{path}\"")
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    };
    format!("{quoted} hook codex {event}")
}

fn event_document(event: &str) -> Value {
    serde_json::from_str(
        &CODEX_HOOK_FORMAT
            .replace("EVENT", &serde_json::to_string(event).unwrap())
            .replace("COMMAND", &serde_json::to_string(&command(event)).unwrap()),
    )
    .unwrap()
}

fn contains_shape(actual: &Value, required: &Value) -> bool {
    match required {
        Value::Object(fields) => fields.iter().all(|(key, value)| {
            actual
                .get(key)
                .is_some_and(|found| contains_shape(found, value))
        }),
        Value::Array(items) => actual.as_array().is_some_and(|array| {
            items
                .iter()
                .all(|item| array.iter().any(|found| contains_shape(found, item)))
        }),
        _ => actual == required,
    }
}

fn commands(value: &Value) -> Vec<&str> {
    match value {
        Value::Object(fields) => fields
            .iter()
            .flat_map(|(key, value)| {
                if key == "command" {
                    value.as_str().into_iter().collect()
                } else {
                    commands(value)
                }
            })
            .collect(),
        Value::Array(items) => items.iter().flat_map(commands).collect(),
        _ => Vec::new(),
    }
}

fn read_config(bus: &Bus) -> Value {
    let text = fs::read_to_string(config(bus)).unwrap_or_default();
    let value: toml::Value = toml::from_str(&text).unwrap();
    serde_json::to_value(value).unwrap()
}

fn read_hooks(bus: &Bus) -> Value {
    let path = hook_file(bus);
    match fs::read(&path) {
        Ok(bytes) => parse_json(&bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(error) => panic!("cannot read Codex hooks at {}: {error}", path.display()),
    }
}

fn assert_installed(bus: &Bus) -> [Value; 2] {
    let documents = [read_config(bus), read_hooks(bus)];
    let all_commands: Vec<_> = documents.iter().flat_map(commands).collect();
    for event in EVENTS {
        let expected = event_document(event);
        assert!(
            documents.iter().any(|doc| contains_shape(doc, &expected)),
            "{event} lacks a readable command group in config.toml or hooks.json: {documents:?}"
        );
        let suffix = format!(" hook codex {event}");
        assert_eq!(
            all_commands
                .iter()
                .filter(|cmd| cmd.ends_with(&suffix))
                .copied()
                .collect::<Vec<_>>(),
            vec![command(event).as_str()],
            "exactly one current-binary command for {event}, with no obsolete flat copy"
        );
    }
    documents
}

fn seed_owner(bus: &Bus) {
    fs::create_dir_all(config(bus).parent().unwrap()).unwrap();
    fs::write(config(bus), OWNER_CONFIG).unwrap();
    fs::write(hook_file(bus), OWNER_HOOKS).unwrap();
}

fn assert_owner_entries(bus: &Bus) {
    let original_toml: toml::Value = toml::from_str(OWNER_CONFIG).unwrap();
    for (after, before) in [
        (
            read_config(bus),
            serde_json::to_value(original_toml).unwrap(),
        ),
        (
            read_hooks(bus),
            serde_json::from_str::<Value>(OWNER_HOOKS).unwrap(),
        ),
    ] {
        for (key, value) in before.as_object().unwrap() {
            if key != "hooks" {
                assert_eq!(&after[key], value, "owner setting {key} changed");
                continue;
            }
            for (event, entries) in value.as_object().unwrap() {
                let original = entries.as_array().unwrap();
                let retained: Vec<_> = after["hooks"][event]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|entry| original.contains(entry))
                    .cloned()
                    .collect();
                // Exact values, multiplicity and order, including matcher/timeout metadata.
                assert_eq!(&retained, original, "owner entries changed for {event}");
            }
        }
    }
}

fn legacy_config(current_binary: bool, table_arrays: bool) -> String {
    let mut doc: toml_edit::DocumentMut = OWNER_CONFIG.parse().unwrap();
    for event in EVENTS {
        let entries = doc["hooks"]
            .as_table_mut()
            .unwrap()
            .entry(event)
            .or_insert(toml_edit::value(toml_edit::Array::new()));
        let mut flat = toml_edit::InlineTable::new();
        let cmd = if current_binary {
            command(event)
        } else {
            format!("/retired/bin/axon bus hook codex {event}")
        };
        flat.insert("command", cmd.into());
        entries.as_array_mut().unwrap().push(flat);
    }
    let text = doc.to_string();
    if table_arrays {
        toml::to_string_pretty(&toml::from_str::<toml::Value>(&text).unwrap()).unwrap()
    } else {
        text
    }
}

fn doctor(bus: &Bus, healthy: bool) {
    let output = bus.cmd().arg("doctor").output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let codex = stdout
        .lines()
        .find(|line| line.split_whitespace().nth(1) == Some("codex"))
        .unwrap_or_else(|| panic!("doctor omitted Codex: {stdout}"));
    assert_eq!(
        codex.split_whitespace().next() == Some("ok"),
        healthy,
        "{stdout}"
    );
    assert_eq!(output.status.success(), healthy, "{stdout}");
}

#[test]
fn fresh_install_wires_every_codex_event_in_readable_groups() {
    let bus = Bus::new();
    bus.init();
    bus.ok(&["install", "--harness", "codex"]);
    assert_installed(&bus);
    doctor(&bus, true);
}

#[test]
fn install_preserves_non_axon_entries_in_both_files() {
    let bus = Bus::new();
    seed_owner(&bus);
    bus.ok(&["install", "--harness", "codex"]);
    assert_installed(&bus);
    assert_owner_entries(&bus);
}

#[test]
fn reinstall_is_byte_identical_and_does_not_duplicate_events() {
    let bus = Bus::with_limit(Duration::from_secs(8));
    seed_owner(&bus);
    bus.ok(&["install", "--harness", "codex"]);
    assert_installed(&bus);
    let before = (
        fs::read(config(&bus)).unwrap(),
        fs::read(hook_file(&bus)).unwrap(),
    );
    for _ in 0..2 {
        bus.ok(&["install", "--harness", "codex"]);
        assert_installed(&bus);
        assert_owner_entries(&bus);
        assert_eq!(
            (
                fs::read(config(&bus)).unwrap(),
                fs::read(hook_file(&bus)).unwrap()
            ),
            before
        );
    }
}

fn upgrade_legacy(table_arrays: bool, with_backup: bool) {
    let bus = Bus::with_limit(Duration::from_secs(8));
    seed_owner(&bus);
    let legacy = legacy_config(false, table_arrays);
    if with_backup {
        fs::write(
            config(&bus).with_file_name("config.toml.axon-bus.bak"),
            OWNER_CONFIG,
        )
        .unwrap();
    }
    fs::write(config(&bus), &legacy).unwrap();
    for _ in 0..2 {
        bus.ok(&["install", "--harness", "codex"]);
        assert_installed(&bus);
        assert_owner_entries(&bus);
    }
    bus.ok(&["uninstall", "--harness", "codex"]);
    let original = if with_backup { OWNER_CONFIG } else { &legacy };
    assert_eq!(fs::read(config(&bus)).unwrap(), original.as_bytes());
    assert_eq!(fs::read(hook_file(&bus)).unwrap(), OWNER_HOOKS.as_bytes());
}

#[test]
fn reinstall_upgrades_flat_inline_entries_and_keeps_original_backup() {
    upgrade_legacy(false, true);
}

#[test]
fn reinstall_upgrades_flat_table_arrays_and_keeps_original_backup() {
    upgrade_legacy(true, true);
}

#[test]
fn install_migrates_flat_inline_entries_even_without_a_backup() {
    upgrade_legacy(false, false);
}

#[test]
fn install_migrates_flat_table_arrays_even_without_a_backup() {
    upgrade_legacy(true, false);
}

#[test]
fn uninstall_restores_both_original_files_byte_for_byte() {
    let bus = Bus::new();
    seed_owner(&bus);
    bus.ok(&["install", "--harness", "codex"]);
    assert_installed(&bus);
    bus.ok(&["uninstall", "--harness", "codex"]);
    assert_eq!(fs::read(config(&bus)).unwrap(), OWNER_CONFIG.as_bytes());
    assert_eq!(fs::read(hook_file(&bus)).unwrap(), OWNER_HOOKS.as_bytes());
}

#[test]
fn uninstall_restores_original_absence_of_hooks_json() {
    let bus = Bus::new();
    fs::create_dir_all(config(&bus).parent().unwrap()).unwrap();
    fs::write(config(&bus), OWNER_CONFIG).unwrap();
    bus.ok(&["install", "--harness", "codex"]);
    assert_installed(&bus);
    bus.ok(&["uninstall", "--harness", "codex"]);
    assert!(
        !hook_file(&bus).exists(),
        "restore the original absence of hooks.json"
    );
    assert_eq!(fs::read(config(&bus)).unwrap(), OWNER_CONFIG.as_bytes());
}

fn reject_flat(table_arrays: bool) {
    let bus = Bus::new();
    bus.init();
    seed_owner(&bus);
    // All seven current-binary commands are present: only their unreadable shape is wrong.
    fs::write(config(&bus), legacy_config(true, table_arrays)).unwrap();
    doctor(&bus, false);
}

#[test]
fn doctor_rejects_old_flat_inline_entries_even_at_current_binary() {
    reject_flat(false);
}

#[test]
fn doctor_rejects_old_flat_table_arrays_even_at_current_binary() {
    reject_flat(true);
}

#[test]
fn doctor_rejects_a_nested_command_with_the_wrong_type() {
    let bus = Bus::new();
    bus.init();
    seed_owner(&bus);
    bus.ok(&["install", "--harness", "codex"]);
    let [mut config_doc, mut hooks] = assert_installed(&bus);
    doctor(&bus, true);
    fn change_type(value: &mut Value, command: &str) {
        match value {
            Value::Object(fields) => {
                if fields.get("command").and_then(Value::as_str) == Some(command) {
                    fields.insert("type".into(), json!("prompt"));
                } else {
                    fields
                        .values_mut()
                        .for_each(|value| change_type(value, command));
                }
            }
            Value::Array(items) => items
                .iter_mut()
                .for_each(|value| change_type(value, command)),
            _ => {}
        }
    }
    if contains_shape(&config_doc, &event_document("SessionStart")) {
        change_type(&mut config_doc, &command("SessionStart"));
        fs::write(config(&bus), toml::to_string_pretty(&config_doc).unwrap()).unwrap();
    } else {
        change_type(&mut hooks, &command("SessionStart"));
        fs::write(hook_file(&bus), serde_json::to_vec_pretty(&hooks).unwrap()).unwrap();
    }
    doctor(&bus, false);
}

#[cfg(unix)]
#[test]
fn installed_session_start_command_registers_a_codex_agent() {
    let bus = Bus::new();
    bus.init();
    bus.ok(&["install", "--harness", "codex"]);
    let documents = assert_installed(&bus);
    let expected = command("SessionStart");
    let installed = documents
        .iter()
        .flat_map(commands)
        .find(|cmd| *cmd == expected)
        .unwrap();
    let payload = bus.fixture("codex", "SessionStart");
    let mut shell = std::process::Command::new("/bin/sh");
    bus.isolate(&mut shell);
    assert_cmd::Command::from(shell)
        .args(["-c", installed])
        .timeout(bus.remaining())
        .write_stdin(payload.to_string())
        .assert()
        .success();
    let agent = bus.agent(payload["session_id"].as_str().unwrap());
    assert_eq!(agent["harness"], "codex");
    assert_eq!(agent["session_id"], payload["session_id"]);
    assert_eq!(agent["status"], "idle");
    assert_eq!(
        bus.count("SELECT count(*) FROM agents WHERE harness='codex'"),
        1
    );
}
