//! OpenCode parser (DESIGN.md §6.3) — **verified against the real `opencode.db`**.
//!
//! Verified shape (NOT the spec's recon): the `message` table holds one row per message with
//! a JSON `data` column. Assistant rows carry `agent`, `modelID`/`providerID`,
//! `tokens {input, output, reasoning, cache:{read,write}}`, `time {created, completed}`
//! (epoch-ms, NOT ISO), `path.cwd`, and — crucially — a pre-computed `cost`. We read the DB
//! directly (preferred over the text logs) and trust OpenCode's own cost (its gateway models
//! have no researchable public rate; OpenCode already priced them). Ollama → cost 0 → free.

use std::path::Path;

use chrono::DateTime;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::RawTurn;
use crate::model::Harness;

/// Read all assistant turns from an `opencode.db`. Best-effort: returns empty if the DB is
/// missing or unreadable (e.g. locked) so Axon degrades gracefully.
pub fn parse_db(db_path: &Path) -> Vec<RawTurn> {
    let Ok(conn) = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    ) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare("SELECT id, session_id, time_created, data FROM message")
    else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, String>(3)?,
        ))
    });
    let Ok(rows) = rows else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for row in rows.flatten() {
        let (id, session_id, time_created, data) = row;
        if let Ok(v) = serde_json::from_str::<Value>(&data) {
            if let Some(t) = turn_from_data(&id, &session_id, time_created, &v) {
                out.push(t);
            }
        }
    }
    out
}

/// Build a [`RawTurn`] from one decoded `message.data` object (assistant rows only).
/// Pure — unit-testable without a database.
pub fn turn_from_data(id: &str, session_id: &str, fallback_ms: i64, d: &Value) -> Option<RawTurn> {
    if d.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let tok = d.get("tokens");
    let g = |key: &str| {
        tok.and_then(|t| t.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let cache = tok.and_then(|t| t.get("cache"));
    let gc = |key: &str| {
        cache
            .and_then(|c| c.get(key))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };

    let created = d
        .get("time")
        .and_then(|t| t.get("created"))
        .and_then(Value::as_i64)
        .unwrap_or(fallback_ms);
    let completed = d
        .get("time")
        .and_then(|t| t.get("completed"))
        .and_then(Value::as_i64)
        .unwrap_or(created);

    Some(RawTurn {
        harness: Harness::OpenCode,
        session_id: session_id.to_string(),
        agent_id: None,
        message_id: id.to_string(),
        model_raw: d
            .get("modelID")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        is_subagent: false,
        agent: d
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or("opencode")
            .to_string(),
        project: d
            .get("path")
            .and_then(|p| p.get("cwd"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        first_ts: ms_to_iso(created),
        last_ts: ms_to_iso(completed),
        tokens_in: g("input"),
        tokens_out: g("output") + g("reasoning"),
        cache_read: gc("read"),
        cache_write_5m: gc("write"),
        cache_write_1h: 0,
        loc_added: 0, // OpenCode LOC (from `part` tool calls) deferred; cost/tokens first.
        loc_removed: 0,
        loc_failed: false,
        skills: Vec::new(),
        // OpenCode computes an exact cost (USD); trust it over any rate table.
        reported_cost_usd: d.get("cost").and_then(Value::as_f64),
    })
}

fn ms_to_iso(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn assistant_message_maps_to_turn_with_reported_cost() {
        let d = json!({
            "role":"assistant","agent":"build","modelID":"gpt-5.5","providerID":"openai",
            "cost":0.0123,
            "tokens":{"total":900,"input":700,"output":150,"reasoning":50,"cache":{"read":40,"write":20}},
            "time":{"created":1781000000000i64,"completed":1781000005000i64},
            "path":{"cwd":"/repo"}
        });
        let t = turn_from_data("msg_1", "ses_1", 0, &d).unwrap();
        assert_eq!(t.harness, Harness::OpenCode);
        assert_eq!(t.agent, "build");
        assert_eq!(t.model_raw, "gpt-5.5");
        assert_eq!(t.tokens_in, 700);
        assert_eq!(t.tokens_out, 200, "output + reasoning");
        assert_eq!(t.cache_read, 40);
        assert_eq!(t.cache_write_5m, 20);
        assert_eq!(t.reported_cost_usd, Some(0.0123));
        assert!(
            t.first_ts.starts_with("2026-"),
            "epoch-ms → ISO: {}",
            t.first_ts
        );
    }

    #[test]
    fn non_assistant_rows_skipped() {
        let d = json!({"role":"user","content":"hi"});
        assert!(turn_from_data("m", "s", 0, &d).is_none());
    }

    #[test]
    fn ollama_zero_cost_is_free() {
        let d = json!({
            "role":"assistant","agent":"build","modelID":"qwen3.6-deepcoder","providerID":"gamerina-ollama",
            "cost":0.0,"tokens":{"input":10,"output":5,"reasoning":0,"cache":{"read":0,"write":0}},
            "time":{"created":1781000000000i64}
        });
        let t = turn_from_data("m", "s", 0, &d).unwrap();
        assert_eq!(t.reported_cost_usd, Some(0.0));
    }
}
