//! Ingestion: turn raw harness logs into [`RawTurn`]s (one per collapsed assistant turn),
//! which `normalize` then converts into [`crate::model::Event`]s.
//!
//! M1 implements Claude Code only ([`claude`]); Codex/OpenCode arrive in M2.

pub mod claude;
pub mod loc;

use std::path::Path;

use crate::model::Harness;

/// One assistant turn after collapse-by-`message.id`, before normalization.
/// Timestamps are still ISO-8601 strings and the model id is still raw.
#[derive(Debug, Clone)]
pub struct RawTurn {
    pub harness: Harness,
    pub session_id: String,
    /// Sub-agent file id (from `agentId`); used in the idempotent event-id hash for sub-agents.
    pub agent_id: Option<String>,
    pub message_id: String,
    pub model_raw: String,
    pub is_subagent: bool,
    pub agent: String,
    pub project: String,
    pub first_ts: String,
    pub last_ts: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub loc_added: u32,
    pub loc_removed: u32,
    pub loc_failed: bool,
    pub skills: Vec<String>,
}

/// Walk `~/.claude/projects/<ENCODED_CWD>/` and parse every main thread plus its
/// `subagents/agent-*.jsonl` (joined to `agent-*.meta.json`). Unreadable files are skipped.
pub fn scan_claude_root(projects_dir: &Path) -> Vec<RawTurn> {
    let mut turns = Vec::new();
    let Ok(projects) = std::fs::read_dir(projects_dir) else {
        return turns;
    };
    for project in projects.flatten() {
        let ppath = project.path();
        if !ppath.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&ppath) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    turns.extend(claude::parse_main_jsonl(&content));
                }
            } else if path.is_dir() {
                scan_subagents(&path.join("subagents"), &mut turns);
            }
        }
    }
    turns
}

fn scan_subagents(dir: &Path, turns: &mut Vec<RawTurn>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if !(name.starts_with("agent-") && name.ends_with(".jsonl")) {
            continue;
        }
        let meta_path = path.with_extension("meta.json");
        let meta = std::fs::read_to_string(&meta_path)
            .ok()
            .and_then(|s| claude::SubagentMeta::from_json_str(&s).ok())
            .unwrap_or_default();
        if let Ok(content) = std::fs::read_to_string(&path) {
            turns.extend(claude::parse_subagent_jsonl(&content, &meta));
        }
    }
}
