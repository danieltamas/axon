//! Claude Code JSONL parser (DESIGN.md §6.1) — **verified against real logs**.
//!
//! Load-bearing rules this encodes:
//! - **Collapse by `message.id`**: one turn is 2–11 lines sharing an id, each repeating the
//!   identical `usage`. Take `usage` once; union every line's `content[]`. (Naive per-line
//!   summing overcounts tokens and cost up to 11×.)
//! - **Timestamps are ISO-8601 strings**; the leading summary line has none → skipped here,
//!   parsed to epoch-ms in `normalize`.
//! - **Sub-agent attribution is exact**: `agent-<id>.meta.json` gives the `agentType` (roster
//!   name) and the `toolUseId` joining to the parent `Agent` spawn.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::{loc, RawTurn};
use crate::model::Harness;

/// `agent-<id>.meta.json`: the join key + roster name for a sub-agent file.
#[derive(Debug, Clone, Default)]
pub struct SubagentMeta {
    pub agent_type: String,
    pub tool_use_id: String,
}

impl SubagentMeta {
    pub fn from_json_str(s: &str) -> anyhow::Result<Self> {
        let v: Value = serde_json::from_str(s)?;
        Ok(SubagentMeta {
            agent_type: v
                .get("agentType")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            tool_use_id: v
                .get("toolUseId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
    }
}

/// Parse a main-thread `<session>.jsonl`. Agent is always `"main"`.
pub fn parse_main_jsonl(content: &str) -> Vec<RawTurn> {
    collapse(content, false, None)
}

/// Parse a `subagents/agent-<id>.jsonl`, attributing every turn to `meta.agentType`
/// (falling back to the line's `attributionAgent`, then `"unknown-sub"`).
pub fn parse_subagent_jsonl(content: &str, meta: &SubagentMeta) -> Vec<RawTurn> {
    collapse(content, true, Some(meta))
}

/// Core: split JSONL into assistant turns keyed by `message.id`, union their content,
/// and take `usage` once per turn.
fn collapse(content: &str, is_subagent: bool, meta: Option<&SubagentMeta>) -> Vec<RawTurn> {
    let mut failed_ids: HashSet<String> = HashSet::new();
    // Preserve first-seen order of message ids so output is deterministic.
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<Value>> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue; // skip malformed lines rather than abort the whole file
        };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();

        if kind == "user" {
            collect_failed_tool_results(&v, &mut failed_ids);
            continue;
        }
        if kind != "assistant" {
            continue;
        }
        // Assistant turn lines must carry both a message.id and a timestamp.
        let has_ts = v.get("timestamp").and_then(Value::as_str).is_some();
        let msg_id = v
            .get("message")
            .and_then(|m| m.get("id"))
            .and_then(Value::as_str);
        let (Some(msg_id), true) = (msg_id, has_ts) else {
            continue;
        };
        let msg_id = msg_id.to_string();
        groups.entry(msg_id.clone()).or_insert_with(|| {
            order.push(msg_id.clone());
            Vec::new()
        });
        groups.get_mut(&msg_id).unwrap().push(v);
    }

    order
        .into_iter()
        .filter_map(|id| build_turn(&id, &groups[&id], is_subagent, meta, &failed_ids))
        .collect()
}

fn collect_failed_tool_results(line: &Value, failed_ids: &mut HashSet<String>) {
    let Some(blocks) = line
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for b in blocks {
        if b.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        if b.get("is_error").and_then(Value::as_bool) == Some(true) {
            if let Some(tid) = b.get("tool_use_id").and_then(Value::as_str) {
                failed_ids.insert(tid.to_string());
            }
        }
    }
}

fn build_turn(
    message_id: &str,
    lines: &[Value],
    is_subagent: bool,
    meta: Option<&SubagentMeta>,
    failed_ids: &HashSet<String>,
) -> Option<RawTurn> {
    let first = lines.first()?;
    let msg = first.get("message")?;

    // Union content[] across all lines sharing this message.id.
    let mut content: Vec<Value> = Vec::new();
    for l in lines {
        if let Some(arr) = l
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        {
            content.extend(arr.iter().cloned());
        }
    }

    let usage = msg.get("usage").cloned().unwrap_or(Value::Null);
    let (tokens_in, tokens_out, cache_read, cache_write_5m, cache_write_1h) = usage_buckets(&usage);

    let loc = loc::extract(&content, failed_ids);

    let str_field = |key: &str| first.get(key).and_then(Value::as_str).map(str::to_string);
    let agent_id = str_field("agentId");
    let attribution_agent = str_field("attributionAgent");

    let agent = if is_subagent {
        meta.map(|m| m.agent_type.clone())
            .filter(|s| !s.is_empty())
            .or(attribution_agent)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown-sub".to_string())
    } else {
        "main".to_string()
    };

    Some(RawTurn {
        harness: Harness::ClaudeCode,
        session_id: str_field("sessionId").unwrap_or_default(),
        agent_id,
        message_id: message_id.to_string(),
        model_raw: msg
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        is_subagent,
        agent,
        project: str_field("cwd").unwrap_or_default(),
        first_ts: first
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        last_ts: lines
            .last()
            .and_then(|l| l.get("timestamp").and_then(Value::as_str))
            .unwrap_or_default()
            .to_string(),
        tokens_in,
        tokens_out,
        cache_read,
        cache_write_5m,
        cache_write_1h,
        loc_added: loc.added,
        loc_removed: loc.removed,
        loc_failed: loc.failed,
        skills: loc.skills,
    })
}

/// Extract the five token buckets from a `message.usage` object.
/// Cache-creation is split into 5m/1h when the breakdown object is present.
fn usage_buckets(usage: &Value) -> (u64, u64, u64, u64, u64) {
    let u = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let input = u("input_tokens");
    let output = u("output_tokens");
    let cache_read = u("cache_read_input_tokens");

    let (cw5, cw1) = match usage.get("cache_creation") {
        Some(cc) => (
            cc.get("ephemeral_5m_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cc.get("ephemeral_1h_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        ),
        // No breakdown: attribute the lump sum to the 5m window (the default ephemeral cache).
        None => (u("cache_creation_input_tokens"), 0),
    };
    (input, output, cache_read, cw5, cw1)
}
