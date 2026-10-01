//! Narrative rows from native transcript records (BUS-PLAN §7): what an agent says and
//! thinks, exactly as its harness recorded it. Opaque reasoning (signatures, encrypted
//! content) becomes a "not recorded" row with its token count; it is never stored.

use chrono::DateTime;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

/// Narrative rows are deleted after 7 days (§7 privacy default).
pub const RETENTION_MS: i64 = 7 * 24 * 3600 * 1000;

/// Longest tool detail kept (a path or a command).
const MAX_DETAIL_CHARS: usize = 200;

#[derive(Debug, PartialEq)]
pub enum Row {
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
            "tool_use" => Some(tool(block["name"].as_str()?, &block["input"], false)),
            _ => None,
        })
        .collect()
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
        [crate::store::now_ms() - RETENTION_MS],
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
    conn.execute(
        "INSERT INTO settings (key,value) VALUES ('narrative_expired_at',?1)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [now.to_string()],
    )?;
    Ok(())
}

/// Credential shapes, masked before anything reaches the database. Each pattern's last
/// group is the secret; anything before it (a key name, `Bearer`, a URL's user) stays.
fn secret_patterns() -> &'static [regex::Regex] {
    static PATTERNS: std::sync::OnceLock<Vec<regex::Regex>> = std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            // Vendor-prefixed API keys and tokens.
            r"\b((?:sk-|sk_live_|sk_test_|rk_live_|rk_test_|ghp_|gho_|ghs_|ghu_|ghr_|github_pat_|glpat-|xox[abpsr]-|xapp-|npm_|hf_|AKIA|ASIA|AIza|ya29\.)[A-Za-z0-9_\-.]{12,})",
            // JSON Web Tokens.
            r"\b(eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,})",
            // Authorization headers.
            r"(?i)\b(?:bearer|basic|token)\s+([A-Za-z0-9_\-.=+/]{16,})",
            // Passwords in URLs: scheme://user:secret@host.
            r"[A-Za-z][A-Za-z0-9+.-]*://[^/\s:@]+:([^@\s/]+)@",
            // Assignments to secret-named keys: PASSWORD=…, api_key: "…".
            // The key may be quoted, as in JSON: {"password":"…"}.
            r#"(?i)\b[A-Z0-9_]*(?:password|passwd|secret|token|api_?key|access_?key|private_?key|credential)[A-Z0-9_]*["']?\s*[=:]\s*["'`]?([^\s"'`,}]{6,})"#,
        ]
        .iter()
        .map(|p| regex::Regex::new(p).expect("valid secret pattern"))
        .collect()
    })
}

pub fn redact(text: &str) -> String {
    if text.contains("PRIVATE KEY-----") {
        return "[redacted: private key]".to_owned();
    }
    let mut out = text.to_owned();
    for pattern in secret_patterns() {
        out = pattern
            .replace_all(&out, |caps: &regex::Captures| {
                let whole = caps.get(0).expect("match");
                let secret = caps.iter().flatten().last().expect("secret group");
                let head = &out[whole.start()..secret.start()];
                // A bare number after `tokens:` is a count; after `password=` it is a secret.
                let key = head.to_ascii_lowercase();
                let counts = key.contains("token")
                    && !["password", "passwd", "secret", "key", "credential"]
                        .iter()
                        .any(|k| key.contains(k));
                if counts && secret.as_str().bytes().all(|b| b.is_ascii_digit()) {
                    return whole.as_str().to_owned();
                }
                let tail = &out[secret.end()..whole.end()];
                format!("{head}[redacted]{tail}")
            })
            .into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redact_masks_tokens_and_keeps_prose() {
        assert_eq!(
            redact("use sk-abcdefghijklmnopqrstu now"),
            "use [redacted] now"
        );
        assert_eq!(
            redact("KEY=sk-abcdefghijklmnopqrstu `ghp_abcdefghijklmnopqrst`"),
            "KEY=[redacted] `[redacted]`"
        );
        for (text, expected) in [
            (
                "curl -H 'Authorization: Bearer abcdefghijklmnopqrstuv'",
                "Bearer [redacted]",
            ),
            (
                "postgres://app:hunter22@db/main",
                "postgres://app:[redacted]@db/main",
            ),
            (
                "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIK7MDENG",
                "AWS_SECRET_ACCESS_KEY=[redacted]",
            ),
            ("password: 'correct-horse'", "password: '[redacted]'"),
            (
                "jwt eyJhbGciOiJIUzI1.eyJzdWIiOiIxMjM0.SflKxwRJSMeKKF2Q",
                "jwt [redacted]",
            ),
            (
                "ghs_abcdefghijklmnop and hf_abcdefghijklmnop",
                "[redacted] and [redacted]",
            ),
        ] {
            let redacted = redact(text);
            assert!(redacted.contains(expected), "{text} -> {redacted}");
        }
        assert_eq!(redact("input_tokens: 123456"), "input_tokens: 123456");
        assert_eq!(
            redact(r#"{"output_tokens":123456}"#),
            r#"{"output_tokens":123456}"#
        );
        assert_eq!(
            redact(r#"{"password":"correct-horse-battery"}"#),
            r#"{"password":"[redacted]"}"#
        );
        assert_eq!(redact("password=12345678"), "password=[redacted]");
        assert_eq!(redact("plain words stay"), "plain words stay");
    }

    #[test]
    fn claude_signature_only_thinking_is_not_recorded() {
        let record = json!({"type":"assistant","message":{"content":[
            {"type":"thinking","thinking":"","signature":"sig"}]}});
        assert_eq!(
            rows("claude", &record, Some(3)),
            vec![Row::Reasoning {
                text: None,
                tokens: Some(3)
            }]
        );
    }
}
