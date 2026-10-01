//! TEST-9: user edits survive install/reinstall/uninstall on every harness.
//! "Stale backup" means an earlier install's backup remains after the owner replaces
//! the live config with a newer, hook-free config. The live owner edits take precedence.

mod common;
use common::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

fn config(bus: &Bus, harness: &str) -> PathBuf {
    bus.root.join(match harness {
        "claude" => "home/.claude/settings.json",
        "codex" => "home/.codex/config.toml",
        "opencode" => "config/opencode/opencode.json",
        "hermes" => "home/.hermes/config.yaml",
        _ => unreachable!(),
    })
}

fn seed(bus: &Bus, harness: &str) -> String {
    let original = match harness {
        "claude" => "{\n  \"model\": \"owner-original\",\n  \"hooks\": {\"PreToolUse\": [{\"matcher\": \"Bash\", \"hooks\": [{\"type\": \"command\", \"command\": \"echo owner-hook\"}]}]}\n}\n",
        "codex" => "# owner formatting\nmodel = \"owner-original\"\n\n[hooks]\nPreToolUse = [{ command = \"echo owner-hook\" }]\n",
        "opencode" => "{\n  \"model\": \"owner-original\", \"plugin\": [\"owner-plugin\"]\n}\n",
        "hermes" => "# owner formatting\nmodel: owner-original\nplugins:\n  enabled:\n    - owner-plugin\n",
        _ => unreachable!(),
    }.to_owned();
    let path = config(bus, harness);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, &original).unwrap();
    original
}

fn edit_owner_model(bus: &Bus, harness: &str) {
    let path = config(bus, harness);
    let text = fs::read_to_string(&path).unwrap();
    let edited = match harness {
        "claude" | "opencode" => {
            let mut value: Value = serde_json::from_str(&text).unwrap();
            value["model"] = json!("owner-edited");
            serde_json::to_string_pretty(&value).unwrap() + "\n"
        }
        "codex" => {
            let mut value: toml::Value = toml::from_str(&text).unwrap();
            value["model"] = toml::Value::String("owner-edited".into());
            // A config editor may serialize inline hook tables as [[hooks.Event]].
            // They are the same TOML value; preserving edits must handle both forms.
            let edited = toml::to_string_pretty(&value).unwrap();
            assert_eq!(toml::from_str::<toml::Value>(&edited).unwrap(), value);
            edited
        }
        "hermes" => text.replace("model: owner-original", "model: owner-edited"),
        _ => unreachable!(),
    };
    fs::write(path, edited).unwrap();
}

fn assert_owner_config(bus: &Bus, harness: &str, model: &str) {
    let text = fs::read_to_string(config(bus, harness)).unwrap();
    match harness {
        "claude" | "opencode" => {
            let value: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["model"], model);
            if harness == "claude" {
                assert!(text.contains("echo owner-hook"));
            } else {
                assert!(value["plugin"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("owner-plugin")));
            }
        }
        "codex" => {
            let value: toml::Value = toml::from_str(&text).unwrap();
            assert_eq!(value["model"].as_str(), Some(model));
            assert!(text.contains("echo owner-hook"));
        }
        "hermes" => {
            assert!(
                text.lines().any(|line| line == format!("model: {model}")),
                "{text}"
            );
            assert!(text.contains("    - owner-plugin\n"));
        }
        _ => unreachable!(),
    }
}

fn count_commands(value: &Value, harness: &str, counts: &mut BTreeMap<String, usize>) {
    match value {
        Value::Object(object) => {
            if let Some(command) = object.get("command").and_then(Value::as_str) {
                let marker = format!(" hook {harness} ");
                if command.contains("axon-bus") {
                    if let Some((_, event)) = command.rsplit_once(&marker) {
                        *counts.entry(event.trim().to_owned()).or_default() += 1;
                    }
                }
            }
            for value in object.values() {
                count_commands(value, harness, counts);
            }
        }
        Value::Array(values) => {
            for value in values {
                count_commands(value, harness, counts);
            }
        }
        _ => {}
    }
}

fn assert_one_bus_entry_per_event(bus: &Bus, harness: &str) {
    let text = fs::read_to_string(config(bus, harness)).unwrap();
    if harness == "opencode" {
        let value: Value = serde_json::from_str(&text).unwrap();
        let plugins = value["plugin"].as_array().unwrap();
        assert_eq!(
            plugins
                .iter()
                .filter(|v| v.as_str().unwrap().contains("axon-bus"))
                .count(),
            1
        );
        assert!(bus
            .root
            .join("config/opencode/axon-bus/plugin.js")
            .is_file());
        return;
    }
    let mut counts = BTreeMap::new();
    let events: &[&str] = match harness {
        "claude" => {
            count_commands(&serde_json::from_str(&text).unwrap(), harness, &mut counts);
            &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "SubagentStart",
                "SubagentStop",
                "Stop",
                "SessionEnd",
            ]
        }
        "codex" => {
            let value: toml::Value = toml::from_str(&text).unwrap();
            count_commands(&serde_json::to_value(value).unwrap(), harness, &mut counts);
            &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "SubagentStart",
                "SubagentStop",
                "Stop",
            ]
        }
        "hermes" => {
            for line in text
                .lines()
                .filter(|line| line.contains("axon-bus") && line.contains(" hook hermes "))
            {
                let event = line
                    .split(" hook hermes ")
                    .last()
                    .unwrap()
                    .trim_end_matches(['\'', '"']);
                *counts.entry(event.to_owned()).or_default() += 1;
            }
            &[
                "on_session_start",
                "pre_llm_call",
                "pre_tool_call",
                "post_tool_call",
                "post_llm_call",
                "subagent_start",
                "subagent_stop",
                "on_session_end",
            ]
        }
        _ => unreachable!(),
    };
    let expected: BTreeMap<String, usize> = events
        .iter()
        .map(|event| ((*event).to_owned(), 1))
        .collect();
    assert_eq!(
        counts, expected,
        "exactly one bus command per {harness} event"
    );
}

fn assert_uninstalled(bus: &Bus, harness: &str) {
    let text = fs::read_to_string(config(bus, harness)).unwrap();
    assert!(
        !text.contains("axon-bus"),
        "bus entry survived uninstall: {text}"
    );
    assert!(!PathBuf::from(format!("{}.axon-bus.bak", config(bus, harness).display())).exists());
    if harness == "opencode" {
        assert!(!bus.root.join("config/opencode/axon-bus/plugin.js").exists());
    }
}

fn edited_roundtrip(harness: &str) {
    let bus = Bus::with_limit(Duration::from_secs(8));
    seed(&bus, harness);
    bus.ok(&["install", "--harness", harness]);
    edit_owner_model(&bus, harness);
    for _ in 0..2 {
        bus.ok(&["install", "--harness", harness]);
        assert_owner_config(&bus, harness, "owner-edited");
        assert_one_bus_entry_per_event(&bus, harness);
    }
    bus.ok(&["uninstall", "--harness", harness]);
    assert_owner_config(&bus, harness, "owner-edited");
    assert_uninstalled(&bus, harness);
}

fn unedited_roundtrip(harness: &str) {
    let bus = Bus::new();
    let original = seed(&bus, harness);
    bus.ok(&["install", "--harness", harness]);
    bus.ok(&["install", "--harness", harness]);
    assert_one_bus_entry_per_event(&bus, harness);
    bus.ok(&["uninstall", "--harness", harness]);
    assert_eq!(
        fs::read(config(&bus, harness)).unwrap(),
        original.as_bytes()
    );
    assert_uninstalled(&bus, harness);
}

fn stale_backup(harness: &str) {
    let bus = Bus::with_limit(Duration::from_secs(8));
    let original = seed(&bus, harness);
    bus.ok(&["install", "--harness", harness]);
    let backup = PathBuf::from(format!("{}.axon-bus.bak", config(&bus, harness).display()));
    assert_eq!(fs::read_to_string(&backup).unwrap(), original);
    // Simulate the owner restoring their config while leaving the previous backup.
    fs::write(config(&bus, harness), original).unwrap();
    edit_owner_model(&bus, harness);
    bus.ok(&["install", "--harness", harness]);
    assert_owner_config(&bus, harness, "owner-edited");
    assert_one_bus_entry_per_event(&bus, harness);
    bus.ok(&["uninstall", "--harness", harness]);
    assert_owner_config(&bus, harness, "owner-edited");
    assert_uninstalled(&bus, harness);
}

#[test]
fn test_9_codex_inline_edit_roundtrip_control() {
    let bus = Bus::new();
    seed(&bus, "codex");
    bus.ok(&["install", "--harness", "codex"]);
    let path = config(&bus, "codex");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("owner-original", "owner-edited")).unwrap();
    bus.ok(&["install", "--harness", "codex"]);
    assert_owner_config(&bus, "codex", "owner-edited");
    assert_one_bus_entry_per_event(&bus, "codex");
    bus.ok(&["uninstall", "--harness", "codex"]);
    assert_owner_config(&bus, "codex", "owner-edited");
    assert_uninstalled(&bus, "codex");
}

#[test]
fn test_9_codex_uninstall_after_reserialized_edit_removes_hooks() {
    let bus = Bus::new();
    seed(&bus, "codex");
    bus.ok(&["install", "--harness", "codex"]);
    edit_owner_model(&bus, "codex");
    assert_one_bus_entry_per_event(&bus, "codex");
    // Uninstall has its own contract even if reinstall of this form is broken.
    bus.ok(&["uninstall", "--harness", "codex"]);
    assert_owner_config(&bus, "codex", "owner-edited");
    assert_uninstalled(&bus, "codex");
}

macro_rules! harness_cases {
    ($edited:ident, $unedited:ident, $stale:ident, $harness:literal) => {
        #[test]
        fn $edited() {
            edited_roundtrip($harness);
        }
        #[test]
        fn $unedited() {
            unedited_roundtrip($harness);
        }
        #[test]
        fn $stale() {
            stale_backup($harness);
        }
    };
}

harness_cases!(
    test_9_claude_preserves_edits,
    test_9_claude_byte_exact_roundtrip,
    test_9_claude_stale_backup,
    "claude"
);
harness_cases!(
    test_9_codex_preserves_edits,
    test_9_codex_byte_exact_roundtrip,
    test_9_codex_stale_backup,
    "codex"
);
harness_cases!(
    test_9_opencode_preserves_edits,
    test_9_opencode_byte_exact_roundtrip,
    test_9_opencode_stale_backup,
    "opencode"
);
harness_cases!(
    test_9_hermes_preserves_edits,
    test_9_hermes_byte_exact_roundtrip,
    test_9_hermes_stale_backup,
    "hermes"
);
