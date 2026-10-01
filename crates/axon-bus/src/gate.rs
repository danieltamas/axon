//! What a hook answers the harness (BUS-PLAN §3): deny a tool call on a pending stop or a
//! non-edge native send, introduce the agent to the bus, inject undelivered messages, or
//! say nothing (allow).
//! The reply shapes are each harness's own wire format (docs/ACCEPTANCE-BRIEF.md).

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::{budget, cli_guard, msg, roster, route};

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
        // The stop and budget gate fails closed on its own errors (§2); nothing else does.
        let stopped = msg::pending_stop(conn, actor)
            .and_then(|stop| match stop {
                Some(reason) => Ok(Some(reason)),
                None => budget::check(conn, actor),
            })
            .unwrap_or_else(|err| {
                Some(format!(
                    "axon-bus could not check stops and budgets: {err:#}"
                ))
            });
        if let Some(reason) = stopped.or_else(|| cli_guard::refusal(actor, payload)) {
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
    let context = |text: String| json!({"hookSpecificOutput": {"hookEventName": event, "additionalContext": text}});
    match (harness, event) {
        // A start, or a resume or compaction that wiped what the agent was told.
        ("claude", "SessionStart" | "SubagentStart") => {
            Ok(roster::introduce(conn, actor)?.map(context))
        }
        ("claude" | "codex", "PostToolUse" | "UserPromptSubmit") => {
            if let Some(to) = native_target(harness, payload) {
                msg::log_native(conn, actor, to)?;
            }
            Ok(news(conn, actor)?.map(context))
        }
        ("hermes", "pre_llm_call") | ("opencode", "tool.execute.after") => {
            Ok(news(conn, actor)?.map(|text| json!({"context": text})))
        }
        _ => Ok(None),
    }
}

/// The introduction if `actor` has not had it, then its undelivered messages.
fn news(conn: &Connection, actor: &str) -> anyhow::Result<Option<String>> {
    let intro = if roster::unintroduced(conn, actor)? {
        roster::introduce(conn, actor)?
    } else {
        None
    };
    let parts: Vec<String> = intro
        .into_iter()
        .chain(msg::deliver(conn, actor)?)
        .collect();
    Ok((!parts.is_empty()).then(|| parts.join("\n")))
}
