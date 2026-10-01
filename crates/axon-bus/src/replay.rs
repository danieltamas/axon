//! `replay FILE --speed 50x` (BUS-PLAN §8): drive the registry and the narrative from a
//! corpus of native transcript records, paced by their own timestamps. The one mechanism
//! behind the demo, the design fixture and visual regression.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context};
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::Value;

use crate::registry::{self, Agent, Status};
use crate::store::{self, now_ms};
use crate::transcript;
use crate::Harness;

/// The longest pause between two records, however far apart their timestamps are.
const MAX_PAUSE: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
struct Line {
    harness: String,
    session_id: String,
    record: Value,
    reasoning_tokens: Option<i64>,
}

/// `50x` or `50`: how many times faster than recorded.
pub fn parse_speed(text: &str) -> Result<f64, String> {
    match text.strip_suffix('x').unwrap_or(text).parse::<f64>() {
        Ok(speed) if speed.is_finite() && speed > 0.0 => Ok(speed),
        _ => Err(format!("invalid speed {text}; use e.g. 50x")),
    }
}

pub fn run(db: &Path, file: &Path, speed: f64) -> anyhow::Result<()> {
    let corpus =
        std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let mut conn = store::open(db).context("no hub; run `axon-bus init`")?;
    let mut previous_ts: Option<i64> = None;
    for (number, text) in corpus
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let line: Line = serde_json::from_str(text)
            .with_context(|| format!("{}:{}: not a replay record", file.display(), number + 1))?;
        if !Harness::ALL.iter().any(|h| h.as_str() == line.harness) {
            bail!(
                "{}:{}: unknown harness {}",
                file.display(),
                number + 1,
                line.harness
            );
        }
        let recorded_ts = transcript::timestamp(&line.harness, &line.record);
        if let (Some(before), Some(now)) = (previous_ts, recorded_ts) {
            let gap = Duration::from_millis((now - before).max(0) as u64).div_f64(speed);
            std::thread::sleep(gap.min(MAX_PAUSE));
        }
        previous_ts = recorded_ts.or(previous_ts);
        let tx = store::write_tx(&mut conn)?;
        let agent = agent_for(&tx, &line)?;
        let rows = transcript::rows(&line.harness, &line.record, line.reasoning_tokens);
        transcript::store_rows(
            &tx,
            &agent,
            &line.harness,
            recorded_ts.unwrap_or_else(now_ms),
            &rows,
        )?;
        tx.commit()?;
    }
    transcript::expire(&conn)?;
    Ok(())
}

/// The agent a record belongs to: a registered Claude subagent, else the session's
/// agent, registered as a root on first sight.
fn agent_for(conn: &Connection, line: &Line) -> anyhow::Result<String> {
    if let Some(child) = line.record["agentId"].as_str() {
        if registry::root_of(conn, child)?.is_some() {
            return Ok(child.to_owned());
        }
    }
    let known: Option<String> = conn
        .query_row(
            "SELECT id FROM agents WHERE session_id=?1 ORDER BY parent_id IS NOT NULL, started_at LIMIT 1",
            [&line.session_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = known {
        return Ok(id);
    }
    let agent = Agent {
        id: &line.session_id,
        harness: &line.harness,
        session_id: &line.session_id,
        ..Default::default()
    };
    registry::upsert(conn, &agent, Status::Idle)?;
    Ok(line.session_id.clone())
}
