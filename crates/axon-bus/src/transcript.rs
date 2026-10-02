//! Narrative rows from native transcript records (BUS-PLAN §7): what an agent says and
//! thinks, exactly as its harness recorded it. Opaque reasoning (signatures, encrypted
//! content) becomes a "not recorded" row with its token count; it is never stored.

use chrono::DateTime;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::redact::redact;

/// Narrative rows are deleted after this many days unless Settings says otherwise (§7
/// privacy default).
pub const DEFAULT_NARRATIVE_DAYS: i64 = 7;
pub(crate) const DAY_MS: i64 = 24 * 3600 * 1000;

/// Longest tool detail kept (a path or a command).
const MAX_DETAIL_CHARS: usize = 200;

#[derive(Debug, PartialEq)]
pub enum Row {
    /// The brief a parent gave a subagent: the first line of its transcript.
    Task(String),
    Assistant(String),
    /// Text a model shows between tool calls; never reasoning (§7).
    Progress(String),
    /// None when the harness recorded no readable text; `tokens` is then its count.
    Reasoning {
        text: Option<String>,
        tokens: Option<i64>,
    },
    Tool {
        name: String,
        detail: Option<String>,
        failed: bool,
    },
}

fn readable(text: Option<&str>) -> Option<String> {
    text.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

fn reasoning(text: Option<&str>, tokens: Option<i64>) -> Row {
    match readable(text) {
        Some(text) => Row::Reasoning {
            text: Some(text),
            tokens: None,
        },
        None => Row::Reasoning { text: None, tokens },
    }
}

/// The path or command a tool call targets, from its input object.
fn tool(name: &str, input: &Value, failed: bool) -> Row {
    // Codex and Hermes pass arguments as a JSON string.
    let parsed;
    let input = match input.as_str() {
        Some(text) => {
            parsed = serde_json::from_str::<Value>(text).unwrap_or(Value::Null);
            &parsed
        }
        None => input,
    };
    // Redacted before it is cut, so a cut cannot leave a secret too short to recognise.
    let detail = ["file_path", "filePath", "path", "command", "cmd", "pattern"]
        .iter()
        .find_map(|key| input[key].as_str())
        .map(|d| redact(d).chars().take(MAX_DETAIL_CHARS).collect());
    Row::Tool {
        name: name.to_owned(),
        detail,
        failed,
    }
}

/// The record's own timestamp in epoch ms, when its harness writes one.
pub fn timestamp(harness: &str, record: &Value) -> Option<i64> {
    if harness == "opencode" {
        return record["info"]["time"]["created"].as_i64();
    }
    let iso = record["timestamp"].as_str()?;
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// The narrative rows of one native record. `reasoning_tokens` is the count a record
/// carries for reasoning its harness did not record as text.
pub fn rows(harness: &str, record: &Value, reasoning_tokens: Option<i64>) -> Vec<Row> {
    match harness {
        "claude" => claude(record, reasoning_tokens),
        "codex" => codex(&record["payload"], reasoning_tokens),
        "opencode" => opencode(record, reasoning_tokens),
        "hermes" => hermes(record, reasoning_tokens),
        _ => Vec::new(),
    }
}

fn claude(record: &Value, tokens: Option<i64>) -> Vec<Row> {
    if record["type"] == "user" && record["isSidechain"] == true && record["parentUuid"].is_null() {
        return brief(&record["message"]["content"])
            .map(Row::Task)
            .into_iter()
            .collect();
    }
    if record["type"] != "assistant" {
        return Vec::new();
    }
    let blocks = record["message"]["content"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    blocks
        .iter()
        .filter_map(|block| match block["type"].as_str()? {
            "text" => readable(block["text"].as_str()).map(Row::Assistant),
            "thinking" if block["display"] == "updates" || block["mode"] == "between_tools" => {
                readable(block["thinking"].as_str()).map(Row::Progress)
            }
            "thinking" | "redacted_thinking" => Some(reasoning(block["thinking"].as_str(), tokens)),
            // A background subagent reports to its parent through this tool, not in text.
            "tool_use" if block["name"] == "SubagentHandback" => {
                readable(block["input"]["message"].as_str()).map(Row::Assistant)
            }
            "tool_use" => Some(tool(block["name"].as_str()?, &block["input"], false)),
            _ => None,
        })
        .collect()
}

/// A user message's text, a string or a list of text blocks.
fn brief(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => readable(Some(text)),
        Value::Array(blocks) => readable(Some(
            &blocks
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        )),
        _ => None,
    }
}

fn codex(payload: &Value, tokens: Option<i64>) -> Vec<Row> {
    match payload["type"].as_str() {
        Some("message") if payload["role"] == "assistant" => payload["content"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter(|c| c["type"] == "output_text")
            .filter_map(|c| readable(c["text"].as_str()).map(Row::Assistant))
            .collect(),
        // `encrypted_content` belongs to the provider; only plaintext summaries show.
        Some("reasoning") => {
            let summary: Vec<&str> = payload["summary"]
                .as_array()
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .filter_map(|s| s["text"].as_str())
                .collect();
            vec![reasoning(Some(&summary.join("\n")), tokens)]
        }
        Some("function_call" | "custom_tool_call") => {
            let input = payload.get("arguments").or_else(|| payload.get("input"));
            payload["name"]
                .as_str()
                .map(|name| vec![tool(name, input.unwrap_or(&Value::Null), false)])
                .unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

fn opencode(record: &Value, tokens: Option<i64>) -> Vec<Row> {
    if record["info"]["role"] != "assistant" {
        return Vec::new();
    }
    let parts = record["parts"].as_array().map_or(&[][..], Vec::as_slice);
    parts
        .iter()
        .filter_map(|part| match part["type"].as_str()? {
            "text" => readable(part["text"].as_str()).map(Row::Assistant),
            "reasoning" => Some(reasoning(part["text"].as_str(), tokens)),
            "tool" => {
                let state = &part["state"];
                Some(tool(
                    part["tool"].as_str()?,
                    &state["input"],
                    state["status"] == "error",
                ))
            }
            _ => None,
        })
        .collect()
}

fn hermes(record: &Value, tokens: Option<i64>) -> Vec<Row> {
    if record["role"] != "assistant" {
        return Vec::new();
    }
    let mut rows: Vec<Row> = readable(record["content"].as_str())
        .map(Row::Assistant)
        .into_iter()
        .collect();
    // An absent field means no reasoning happened; an empty one means it was not recorded.
    if let Some(text) = record.get("reasoning_content").filter(|v| !v.is_null()) {
        rows.push(reasoning(text.as_str(), tokens));
    }
    let calls = record["tool_calls"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    rows.extend(calls.iter().filter_map(|call| {
        let function = &call["function"];
        Some(tool(
            function["name"].as_str()?,
            &function["arguments"],
            false,
        ))
    }));
    rows
}

/// How long one `serve --content` heartbeat keeps capture on; `serve` renews it well
/// before then, so capture ends within this long after `serve` stops (§7, opt-in).
pub const CAPTURE_LEASE_MS: i64 = 30_000;

/// Whether narrative text may be stored: only while a `serve --content` holds the lease.
pub fn content_enabled(conn: &Connection) -> rusqlite::Result<bool> {
    let until: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key='content_capture_until'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(until
        .and_then(|v| v.parse::<i64>().ok())
        .is_some_and(|until| until > crate::store::now_ms()))
}

/// Whether the owner has capture switched on in Settings (the default). `serve --no-content`
/// overrides it: a stored `true` never widens what the server was started with.
pub fn capture_enabled(conn: &Connection) -> rusqlite::Result<bool> {
    Ok(crate::store::setting(conn, "capture_enabled")?.is_none_or(|v| v != "0"))
}

pub fn set_capture_settings(
    conn: &Connection,
    enabled: bool,
    narrative_days: i64,
) -> rusqlite::Result<()> {
    crate::store::put_setting(
        conn,
        "capture_enabled",
        Some(if enabled { "1" } else { "0" }),
    )?;
    crate::store::put_setting(conn, "narrative_days", Some(&narrative_days.to_string()))
}

/// How long narrative is kept, in days.
pub fn narrative_days(conn: &Connection) -> rusqlite::Result<i64> {
    Ok(crate::store::setting(conn, "narrative_days")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_NARRATIVE_DAYS))
}

pub fn retention_ms(conn: &Connection) -> rusqlite::Result<i64> {
    Ok(narrative_days(conn)? * DAY_MS)
}

/// Hold the capture lease while this server was started with content and Settings has
/// capture on; end it otherwise.
pub fn apply_capture(conn: &Connection, content: bool) -> rusqlite::Result<()> {
    set_capture(conn, content && capture_enabled(conn)?)
}

/// Start or renew the capture lease (`serve --content`), or end it (`serve` without it).
pub fn set_capture(conn: &Connection, on: bool) -> rusqlite::Result<()> {
    if !on {
        conn.execute("DELETE FROM settings WHERE key='content_capture_until'", [])?;
        return Ok(());
    }
    let until = crate::store::now_ms() + CAPTURE_LEASE_MS;
    conn.execute(
        "INSERT INTO settings (key,value) VALUES ('content_capture_until',?1)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [until.to_string()],
    )?;
    Ok(())
}

/// Store rows; without content capture only their structure is kept (§7 privacy). The
/// capture setting is read on every call, so switching it off takes effect at once.
pub fn store_rows(
    conn: &Connection,
    agent: &str,
    source: &str,
    ts: i64,
    rows: &[Row],
) -> rusqlite::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let content = content_enabled(conn)?;
    let kept = |text: &Option<String>| text.as_deref().filter(|_| content).map(redact);
    for row in rows {
        let (kind, text, recorded, tokens, tool_name, tool_detail, failed) = match row {
            Row::Assistant(text) => (
                "assistant",
                kept(&Some(text.clone())),
                None,
                None,
                None,
                None,
                false,
            ),
            Row::Task(text) => (
                "task",
                kept(&Some(text.clone())),
                None,
                None,
                None,
                None,
                false,
            ),
            Row::Progress(text) => (
                "progress",
                kept(&Some(text.clone())),
                None,
                None,
                None,
                None,
                false,
            ),
            Row::Reasoning { text, tokens } => (
                "reasoning",
                kept(text),
                Some(text.is_some()),
                *tokens,
                None,
                None,
                false,
            ),
            Row::Tool {
                name,
                detail,
                failed,
            } => (
                "tool",
                None,
                None,
                None,
                Some(name.as_str()),
                kept(detail),
                *failed,
            ),
        };
        conn.execute(
            "INSERT INTO narrative (agent_id,ts,kind,source,text,recorded,tokens,tool_name,tool_detail,failed)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![agent, ts, kind, source, text, recorded, tokens, tool_name, tool_detail, failed],
        )?;
    }
    Ok(())
}

/// Retention runs at most this often, from whichever writer comes first.
const EXPIRE_EVERY_MS: i64 = 3600 * 1000;

/// Delete narrative past its retention.
pub fn expire(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM narrative WHERE ts < ?1",
        [crate::store::now_ms() - retention_ms(conn)?],
    )
}

/// `expire`, when the last run is over an hour old. Hooks call it, so retention holds
/// whether or not `serve` runs.
pub fn expire_if_due(conn: &Connection) -> rusqlite::Result<()> {
    let now = crate::store::now_ms();
    let last: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key='narrative_expired_at'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if last
        .and_then(|v| v.parse::<i64>().ok())
        .is_some_and(|at| now - at < EXPIRE_EVERY_MS)
    {
        return Ok(());
    }
    expire(conn)?;
    crate::usage::expire(conn)?;
    conn.execute(
        "INSERT INTO settings (key,value) VALUES ('narrative_expired_at',?1)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [now.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "transcript_tests.rs"]
mod tests;
