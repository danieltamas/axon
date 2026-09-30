//! What a hook answers the harness (BUS-PLAN §3): deny a tool call on a pending stop or a
//! non-edge native send, inject undelivered messages, or say nothing (allow).
//! The reply shapes are each harness's own wire format (docs/ACCEPTANCE-BRIEF.md).

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::{budget, msg, route, usage};

pub fn is_pre_tool(harness: &str, event: &str) -> bool {
    matches!(
        (harness, event),
        ("claude" | "codex", "PreToolUse")
            | ("hermes", "pre_tool_call")
            | ("opencode", "tool.execute.before")
    )
}

pub fn deny(harness: &str, reason: String) -> Value {
    match harness {
        "hermes" => json!({"decision": "block", "reason": reason}),
        "opencode" => json!({"decision": "deny", "reason": reason}),
        _ => json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
            "permissionDecision": "deny", "permissionDecisionReason": reason}}),
    }
}

/// The `to` of a Claude `SendMessage` call, when this payload is one.
fn native_target<'a>(harness: &str, payload: &'a Value) -> Option<&'a str> {
    if harness != "claude" || payload["tool_name"] != "SendMessage" {
        return None;
    }
    let input = &payload["tool_input"];
    input["to"].as_str().or_else(|| input["recipient"].as_str())
}

/// The reply for `actor`'s hook event; None means allow silently. Call inside the hook's
/// write transaction.
pub fn verdict(
    conn: &Connection,
    harness: &str,
    event: &str,
    actor: &str,
    payload: &Value,
) -> anyhow::Result<Option<Value>> {
    if is_pre_tool(harness, event) {
        if let Some(reason) = msg::pending_stop(conn, actor)? {
            return Ok(Some(deny(harness, reason)));
        }
        if let Some(reason) = budget::check(conn, actor)? {
            return Ok(Some(deny(harness, reason)));
        }
        if let Some(to) = native_target(harness, payload) {
            // An unregistered target is a native peer the bus does not govern.
            let known = crate::registry::root_of(conn, to)?.is_some();
            if known && !route::allowed(conn, actor, to, None)? {
                return Ok(Some(deny(harness, route::refusal(conn, actor, to)?)));
            }
        }
        return Ok(None);
    }
    match (harness, event) {
        ("claude" | "codex", "PostToolUse" | "UserPromptSubmit") => {
            if let Some(to) = native_target(harness, payload) {
                msg::log_native(conn, actor, to)?;
            }
            if harness == "claude" && event == "PostToolUse" {
                record_claude_usage(conn, actor, payload)?;
            }
            Ok(msg::deliver(conn, actor)?.map(|text| {
                json!({"hookSpecificOutput": {"hookEventName": event, "additionalContext": text}})
            }))
        }
        ("hermes", "pre_llm_call") => {
            Ok(msg::deliver(conn, actor)?.map(|text| json!({"context": text})))
        }
        _ => Ok(None),
    }
}

/// After a Claude tool call: keep the model a spawn resolved for its child (raw, so an
/// unknown model stays unpriced), then ingest the actor's new transcript turns.
fn record_claude_usage(conn: &Connection, actor: &str, payload: &Value) -> anyhow::Result<()> {
    let spawned = &payload["tool_response"];
    if let (Some(child), Some(model)) = (spawned["agentId"].as_str(), spawned["resolvedModel"].as_str()) {
        conn.execute("UPDATE agents SET model=?2 WHERE id=?1", [child, model])?;
    }
    if let Some(transcript) = payload["transcript_path"].as_str() {
        let child = payload["agent_id"].as_str();
        usage::ingest_claude(conn, actor, child, std::path::Path::new(transcript))?;
    }
    Ok(())
}
