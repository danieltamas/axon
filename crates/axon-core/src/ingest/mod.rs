//! Ingestion: turn raw harness logs into [`RawTurn`]s (one per collapsed assistant turn),
//! which `normalize` then converts into [`crate::model::Event`]s.
//!
//! M1 implements Claude Code only ([`claude`]); Codex/OpenCode arrive in M2.

pub mod ccflare;
pub mod claude;
pub mod codex;
pub mod loc;
pub mod opencode;

use std::path::{Path, PathBuf};

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
    pub chatgpt_plan_type: Option<String>,
    /// Cost in USD already computed by the source harness (OpenCode), if any. When present,
    /// `normalize` uses it (× fx) instead of computing from `pricing.toml`.
    pub reported_cost_usd: Option<f64>,
}

/// One log a scan reads: a transcript file, or a harness's own database. A scan can
/// skip a source whose size and modification time are unchanged since it was last read.
#[derive(Debug, Clone)]
pub struct Source {
    pub kind: SourceKind,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    ClaudeMain,
    ClaudeSubagent,
    Codex,
    OpenCode,
    Ccflare,
}

impl Source {
    /// Every turn in this source; an unreadable source has none.
    pub fn parse(&self) -> Vec<RawTurn> {
        let read = || std::fs::read_to_string(&self.path).ok();
        match self.kind {
            SourceKind::ClaudeMain => read()
                .map(|content| claude::parse_main_jsonl(&content))
                .unwrap_or_default(),
            SourceKind::ClaudeSubagent => {
                let meta = std::fs::read_to_string(self.path.with_extension("meta.json"))
                    .ok()
                    .and_then(|s| claude::SubagentMeta::from_json_str(&s).ok())
                    .unwrap_or_default();
                read()
                    .map(|content| claude::parse_subagent_jsonl(&content, &meta))
                    .unwrap_or_default()
            }
            SourceKind::Codex => {
                let stem = self
                    .path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("codex");
                read()
                    .map(|content| codex::parse_session(&content, stem))
                    .unwrap_or_default()
            }
            SourceKind::OpenCode => opencode::parse_db(&self.path),
            SourceKind::Ccflare => ccflare::parse_db(&self.path),
        }
    }
}

/// Every main thread under `~/.claude/projects/<ENCODED_CWD>/`, plus each session's
/// `subagents/agent-*.jsonl` (joined to `agent-*.meta.json` when parsed).
pub fn claude_sources(projects_dir: &Path) -> Vec<Source> {
    let mut sources = Vec::new();
    let Ok(projects) = std::fs::read_dir(projects_dir) else {
        return sources;
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
                sources.push(Source {
                    kind: SourceKind::ClaudeMain,
                    path,
                });
            } else if path.is_dir() {
                subagent_sources(&path.join("subagents"), &mut sources);
            }
        }
    }
    sources
}

/// Every Codex session `.jsonl` under `~/.codex/sessions/**/`.
pub fn codex_sources(sessions_dir: &Path) -> Vec<Source> {
    let mut sources = Vec::new();
    let mut stack = vec![sessions_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                sources.push(Source {
                    kind: SourceKind::Codex,
                    path,
                });
            }
        }
    }
    sources
}

/// Parse every main thread and subagent under `~/.claude/projects/`.
pub fn scan_claude_root(projects_dir: &Path) -> Vec<RawTurn> {
    claude_sources(projects_dir)
        .iter()
        .flat_map(Source::parse)
        .collect()
}

/// Parse every Codex session under `~/.codex/sessions/`.
pub fn scan_codex_root(sessions_dir: &Path) -> Vec<RawTurn> {
    codex_sources(sessions_dir)
        .iter()
        .flat_map(Source::parse)
        .collect()
}

/// Read all OpenCode assistant turns from its SQLite database.
pub fn scan_opencode_db(db_path: &Path) -> Vec<RawTurn> {
    opencode::parse_db(db_path)
}

/// Read all completion requests from a ccflare-family proxy DB (better-ccflare / ccflare).
/// Each priced request becomes one turn; non-completion rows (token refresh, health) are skipped.
pub fn scan_ccflare_db(db_path: &Path) -> Vec<RawTurn> {
    ccflare::parse_db(db_path)
}

fn subagent_sources(dir: &Path, sources: &mut Vec<Source>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.starts_with("agent-") && name.ends_with(".jsonl") {
            sources.push(Source {
                kind: SourceKind::ClaudeSubagent,
                path,
            });
        }
    }
}
