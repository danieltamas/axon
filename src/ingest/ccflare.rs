//! ccflare-family parser — reads a CC-proxy's `requests` table (better-ccflare / ccflare).
//!
//! Unlike the harness parsers, the source here is a **proxy**, not a coding-agent log. That
//! shapes what is and isn't available:
//! - **Per-agent attribution IS available** — better-ccflare's agent-interceptor records the
//!   matched Claude Code agent in `requests.agent_used`. (Upstream ccflare omits the column;
//!   then every row is `main`.) This is what keeps the brain's agent tier alive from a proxy.
//! - **LOC is NOT available** — a proxy never sees Edit/Write tool results, so `loc_*` is 0.
//! - **No session concept** — we bucket by `account_used` so the summary's "sessions" count is
//!   meaningful (≈ distinct accounts) rather than collapsing to one.
//!
//! Cost: ccflare computes `cost_usd` itself. We trust it (× fx) **only when > 0**; a `0`
//! cost is ambiguous (free local model vs. an unpriced row), so we hand those to the pricing
//! table — which prices known models and flags unknown ones `unpriced` (loud, per §8), and
//! correctly yields €0 for local/Ollama-style models.
//!
//! Schema tolerance: the column set differs across the family (and across migrations), so we
//! introspect `PRAGMA table_info(requests)` and build the SELECT from the columns that exist,
//! preferring the detailed token columns (`input_tokens`/`output_tokens`) and falling back to
//! the legacy ones (`prompt_tokens`/`completion_tokens`). Verified against a real
//! `better-ccflare.db` (timestamps are `Date.now()` epoch-ms).

use std::collections::HashSet;
use std::path::Path;

use chrono::DateTime;
use rusqlite::{Connection, OpenFlags};

use super::RawTurn;
use crate::model::Harness;

/// One `requests` row, already projected to the fields Axon needs. Decoupled from SQLite so
/// [`turn_from_row`] is pure and unit-testable without a database.
#[derive(Debug, Clone, Default)]
pub struct CcflareRow {
    pub id: String,
    /// Epoch (ms for the whole family; seconds tolerated — see [`norm_ms`]).
    pub timestamp: i64,
    pub model: Option<String>,
    pub agent_used: Option<String>,
    pub project: Option<String>,
    pub account_used: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub cost_usd: f64,
    pub response_time_ms: Option<i64>,
}

/// Read every priced completion request from a ccflare-family DB. Best-effort: returns empty
/// if the file is missing, locked, or has no `requests(id, timestamp)` table — so Axon
/// degrades gracefully when no proxy is in use.
pub fn parse_db(db_path: &Path) -> Vec<RawTurn> {
    let Ok(conn) = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    ) else {
        return Vec::new();
    };
    let cols = table_columns(&conn, "requests");
    if !cols.contains("id") || !cols.contains("timestamp") {
        return Vec::new(); // not a ccflare-family DB (or empty/foreign schema)
    }

    let sql = build_select(&cols);
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        Ok(CcflareRow {
            id: r.get(0)?,
            timestamp: r.get(1)?,
            model: r.get(2)?,
            agent_used: r.get(3)?,
            project: r.get(4)?,
            account_used: r.get(5)?,
            input_tokens: r.get::<_, i64>(6)?.max(0) as u64,
            output_tokens: r.get::<_, i64>(7)?.max(0) as u64,
            cache_read: r.get::<_, i64>(8)?.max(0) as u64,
            cache_creation: r.get::<_, i64>(9)?.max(0) as u64,
            cost_usd: r.get(10)?,
            response_time_ms: r.get(11)?,
        })
    });
    let Ok(rows) = rows else {
        return Vec::new();
    };
    rows.flatten()
        .filter_map(|row| turn_from_row(&row))
        .collect()
}

/// Build a SELECT that reads only the columns this DB actually has, normalizing the family's
/// schema variants. Token columns prefer the detailed form and fall back to the legacy one;
/// columns absent in this DB are selected as a literal `NULL`/`0`.
fn build_select(cols: &HashSet<String>) -> String {
    let opt = |name: &str| {
        if cols.contains(name) {
            name.to_string()
        } else {
            "NULL".to_string()
        }
    };
    // Prefer detailed token columns; fall back to legacy (prompt/completion); else 0.
    let tokens =
        |detailed: &str, legacy: &str| match (cols.contains(detailed), cols.contains(legacy)) {
            (true, true) => format!("COALESCE(NULLIF({detailed}, 0), {legacy}, 0)"),
            (true, false) => detailed.to_string(),
            (false, true) => legacy.to_string(),
            (false, false) => "0".to_string(),
        };
    let count = |name: &str| {
        if cols.contains(name) {
            name.to_string()
        } else {
            "0".to_string()
        }
    };
    let cost = if cols.contains("cost_usd") {
        "CAST(cost_usd AS REAL)"
    } else {
        "0.0"
    };

    format!(
        "SELECT id, timestamp, {model}, {agent}, {project}, {account}, \
         {input}, {output}, {cread}, {ccreate}, {cost}, {rt} FROM requests",
        model = opt("model"),
        agent = opt("agent_used"),
        project = opt("project"),
        account = opt("account_used"),
        input = tokens("input_tokens", "prompt_tokens"),
        output = tokens("output_tokens", "completion_tokens"),
        cread = count("cache_read_input_tokens"),
        ccreate = count("cache_creation_input_tokens"),
        cost = cost,
        rt = opt("response_time_ms"),
    )
}

/// Column-name set for `table`, lowercased. Empty if the table does not exist.
fn table_columns(conn: &Connection, table: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    if let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(1)) {
            for name in rows.flatten() {
                out.insert(name.to_lowercase());
            }
        }
    }
    out
}

/// Map one projected request row to a [`RawTurn`]. Returns `None` for rows with no model
/// (token refreshes, health checks, OAuth) — those are not completions and carry no usage.
/// Pure — no database, fully unit-testable.
pub fn turn_from_row(r: &CcflareRow) -> Option<RawTurn> {
    let model = r
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;

    // better-ccflare's agent-interceptor sets `agent_used` to the matched Claude Code agent;
    // a missing/`main` value is the main thread.
    let agent_raw = r
        .agent_used
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let is_subagent = matches!(agent_raw, Some(a) if a != "main");
    let agent = agent_raw.unwrap_or("main").to_string();

    let first_ms = norm_ms(r.timestamp);
    // response_time_ms gives a real duration → real tps, without a second timestamp.
    let last_ms = first_ms + r.response_time_ms.unwrap_or(0).max(0);

    // Trust ccflare's own cost only when it priced the row; hand €0 rows to the pricing table.
    let reported_cost_usd = (r.cost_usd > 0.0).then_some(r.cost_usd);

    Some(RawTurn {
        harness: Harness::Ccflare,
        // No proxy-level session id; bucket by account so "sessions" stays meaningful.
        session_id: r.account_used.clone().unwrap_or_default(),
        agent_id: agent_raw.map(str::to_string),
        message_id: r.id.clone(),
        model_raw: model.to_string(),
        is_subagent,
        agent,
        project: r.project.clone().unwrap_or_default(),
        first_ts: ms_to_iso(first_ms),
        last_ts: ms_to_iso(last_ms),
        tokens_in: r.input_tokens,
        tokens_out: r.output_tokens,
        cache_read: r.cache_read,
        // ccflare reports a single cache-creation figure; attribute it to the 5m window
        // (the default ephemeral cache), matching the Claude no-breakdown path.
        cache_write_5m: r.cache_creation,
        cache_write_1h: 0,
        // A proxy never sees Edit/Write tool results.
        loc_added: 0,
        loc_removed: 0,
        loc_failed: false,
        skills: Vec::new(),
        chatgpt_plan_type: None,
        reported_cost_usd,
    })
}

/// Tolerate seconds-vs-milliseconds: the whole ccflare family writes `Date.now()` (ms), but a
/// value below ~`1e12` would be seconds and is scaled up rather than parsed as a 1970 stamp.
fn norm_ms(ts: i64) -> i64 {
    if ts > 0 && ts < 1_000_000_000_000 {
        ts * 1000
    } else {
        ts
    }
}

fn ms_to_iso(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> CcflareRow {
        CcflareRow {
            id: "req_1".into(),
            timestamp: 1_781_000_000_000, // ms
            model: Some("claude-sonnet-4-5".into()),
            account_used: Some("acct-a".into()),
            input_tokens: 700,
            output_tokens: 150,
            cache_read: 40,
            cache_creation: 20,
            cost_usd: 0.0123,
            response_time_ms: Some(5000),
            ..Default::default()
        }
    }

    #[test]
    fn maps_main_request_with_reported_cost() {
        let t = turn_from_row(&base()).unwrap();
        assert_eq!(t.harness, Harness::Ccflare);
        assert_eq!(t.agent, "main");
        assert!(!t.is_subagent);
        assert_eq!(t.model_raw, "claude-sonnet-4-5");
        assert_eq!(t.session_id, "acct-a");
        assert_eq!(t.tokens_in, 700);
        assert_eq!(t.tokens_out, 150);
        assert_eq!(t.cache_read, 40);
        assert_eq!(t.cache_write_5m, 20);
        assert_eq!(t.reported_cost_usd, Some(0.0123));
        assert_eq!(t.loc_added, 0, "a proxy can't see edits");
        assert!(t.first_ts.starts_with("2026-"), "ms → ISO: {}", t.first_ts);
        assert_ne!(t.first_ts, t.last_ts, "response_time_ms → real duration");
    }

    #[test]
    fn agent_used_marks_subagent() {
        let mut r = base();
        r.agent_used = Some("security-reviewer".into());
        let t = turn_from_row(&r).unwrap();
        assert!(t.is_subagent);
        assert_eq!(t.agent, "security-reviewer");
        assert_eq!(t.agent_id.as_deref(), Some("security-reviewer"));
    }

    #[test]
    fn zero_cost_defers_to_pricing_table() {
        let mut r = base();
        r.cost_usd = 0.0; // ambiguous: free-local OR unpriced → let normalize decide
        assert_eq!(turn_from_row(&r).unwrap().reported_cost_usd, None);
    }

    #[test]
    fn non_completion_rows_skipped() {
        let mut r = base();
        r.model = None; // token refresh / health / oauth
        assert!(turn_from_row(&r).is_none());
    }

    #[test]
    fn seconds_timestamp_scaled_to_ms() {
        let mut r = base();
        r.timestamp = 1_781_000_000; // seconds
        let t = turn_from_row(&r).unwrap();
        assert!(
            t.first_ts.starts_with("2026-"),
            "sec → ms → ISO: {}",
            t.first_ts
        );
    }
}
