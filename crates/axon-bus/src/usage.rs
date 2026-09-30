//! Usage ingest (BUS-PLAN §9 M3): Claude transcript turns become `usage` rows of the bus
//! agent that produced them, read incrementally from the last ingested byte offset.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use axon_core::ingest::claude::{parse_main_jsonl, parse_subagent_jsonl, SubagentMeta};
use axon_core::normalize::to_event;
use axon_core::pricing::Pricing;
use rusqlite::{params, Connection};

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
    let start: i64 = conn.query_row(
        "SELECT coalesce(max(source_offset),0) FROM usage WHERE agent_id=?1 AND source_key IS NOT NULL",
        [agent],
        |r| r.get(0),
    )?;
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
    let pricing = Pricing::bundled();
    for turn in turns.iter().filter(|t| t.agent_id.as_deref() == child) {
        let ts = to_event(turn, &pricing)?.ts;
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
    Ok(())
}
