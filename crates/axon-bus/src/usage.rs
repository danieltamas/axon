//! Usage ingest (BUS-PLAN §9 M3): Claude transcript turns become `usage` rows of the bus
//! agent that produced them, read incrementally from a per-file byte cursor, together with
//! their narrative rows (§7).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use axon_core::ingest::claude::{parse_main_jsonl, parse_subagent_jsonl, SubagentMeta};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::store::now_ms;
use crate::transcript;

/// The transcript that holds `agent`'s own turns: a subagent's sidechain file beside the
/// session transcript when Claude wrote one, else the transcript the hook named.
fn transcript_of(transcript: &Path, child: Option<&str>) -> PathBuf {
    if let Some(child) = child {
        let sidechain = transcript
            .with_extension("")
            .join("subagents")
            .join(format!("agent-{child}.jsonl"));
        if sidechain.is_file() {
            return sidechain;
        }
    }
    transcript.to_owned()
}

/// Ingest the turns `agent` added to its transcript since the last call. `child` is the
/// Claude agent_id for a subagent, None for the session's main thread. A missing
/// transcript is not an error: the hook may run before Claude flushes it.
pub fn ingest_claude(
    conn: &Connection,
    agent: &str,
    child: Option<&str>,
    transcript: &Path,
) -> anyhow::Result<()> {
    let path = transcript_of(transcript, child);
    let mut file = match std::fs::File::open(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        other => other?,
    };
    let key = path.to_string_lossy();
    let cursor: i64 = conn
        .query_row(
            "SELECT byte_offset FROM ingest_cursors WHERE agent_id=?1 AND path=?2",
            params![agent, key],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    // A transcript shorter than the cursor was rewritten; read it again from the top.
    let start = if file.metadata()?.len() < cursor as u64 { 0 } else { cursor };
    file.seek(SeekFrom::Start(start as u64))?;
    let mut appended = Vec::new();
    file.read_to_end(&mut appended)?;
    // A line still being written is left for the next call.
    let Some(complete) = appended.iter().rposition(|&b| b == b'\n').map(|i| i + 1) else {
        return Ok(());
    };
    let text = String::from_utf8_lossy(&appended[..complete]);
    let end = start + complete as i64;
    let turns = match child {
        Some(_) => parse_subagent_jsonl(&text, &SubagentMeta::default()),
        None => parse_main_jsonl(&text),
    };
    for turn in turns.iter().filter(|t| t.agent_id.as_deref() == child) {
        let Some(ts) = epoch_ms(&turn.first_ts) else {
            continue;
        };
        conn.execute(
            "INSERT OR IGNORE INTO usage (agent_id,ts,model,input_tokens,output_tokens,
             cache_read_tokens,cache_write_tokens,cache_write_1h_tokens,cost_usd,source_offset,source_key)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                agent,
                ts,
                turn.model_raw,
                turn.tokens_in as i64,
                turn.tokens_out as i64,
                turn.cache_read as i64,
                turn.cache_write_5m as i64,
                turn.cache_write_1h as i64,
                turn.reported_cost_usd,
                end,
                turn.message_id
            ],
        )?;
    }
    store_narrative(conn, agent, child, &text)?;
    conn.execute(
        "INSERT INTO ingest_cursors (agent_id,path,byte_offset) VALUES (?1,?2,?3)
         ON CONFLICT(agent_id,path) DO UPDATE SET byte_offset=excluded.byte_offset",
        params![agent, key, end],
    )?;
    Ok(())
}

fn epoch_ms(iso: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// The live narrative of the same lines: records of `child` (the main thread when None).
fn store_narrative(
    conn: &Connection,
    agent: &str,
    child: Option<&str>,
    text: &str,
) -> rusqlite::Result<()> {
    for record in text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|record| record["agentId"].as_str() == child)
    {
        let ts = transcript::timestamp("claude", &record).unwrap_or_else(now_ms);
        transcript::store_rows(conn, agent, "claude", ts, &transcript::rows("claude", &record, None))?;
    }
    Ok(())
}
