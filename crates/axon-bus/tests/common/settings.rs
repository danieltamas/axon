//! Settings fixtures enter through captured hook transcripts, never SQL inserts.
use super::{write_jsonl, Bus};
use serde_json::{json, Value};
use std::path::PathBuf;

pub fn capture(bus: &Bus, id: &str, timestamp: i64, body: &str) {
    let mut record: Value = serde_json::from_str(
        include_str!("../../../../tests/fixtures/claude_subagent.jsonl")
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    record["sessionId"] = json!("capture-root");
    record["uuid"] = json!(id);
    record["message"]["id"] = json!(id);
    record["message"]["content"] = json!([{"type":"text","text":body}]);
    record["cwd"] = json!(bus.root.join("work"));
    record["timestamp"] = json!(chrono::DateTime::from_timestamp_millis(timestamp)
        .unwrap()
        .to_rfc3339());
    for key in [
        "agentId",
        "isSidechain",
        "attributionAgent",
        "sourceToolAssistantUUID",
    ] {
        record.as_object_mut().unwrap().remove(key);
    }
    let path = bus.root.join(format!("{id}.jsonl"));
    write_jsonl(&path, &[record]);
    let mut hook = bus.fixture("claude", "PostToolUse.child");
    hook.as_object_mut().unwrap().remove("agent_id");
    hook["session_id"] = json!("capture-root");
    hook["cwd"] = json!(bus.root.join("work"));
    hook["transcript_path"] = json!(path);
    bus.hook("claude", "PostToolUse", &hook);
}
pub fn config(bus: &Bus, harness: &str) -> PathBuf {
    bus.root.join(match harness {
        "claude" => "home/.claude/settings.json",
        "codex" => "home/.codex/config.toml",
        "opencode" => "config/opencode/opencode.json",
        "hermes" => "home/.hermes/config.yaml",
        _ => unreachable!(),
    })
}
pub fn owner_config(bus: &Bus, harness: &str) -> String {
    let original = match harness {
        "claude" => r#"{"model":"owner-model","hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo owner-hook"}]}]}}"#,
        "codex" => "# owner comment\nmodel = \"owner-model\"\n[hooks]\nPreToolUse = [{ command = \"echo owner-hook\" }]\n",
        "opencode" => r#"{"model":"owner-model","plugin":["owner-plugin"]}"#,
        "hermes" => "# owner comment\nmodel: owner-model\nplugins:\n  enabled:\n    - owner-plugin\n",
        _ => unreachable!(),
    };
    let path = config(bus, harness);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, original).unwrap();
    original.to_owned()
}
