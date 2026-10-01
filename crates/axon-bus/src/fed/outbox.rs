//! Delivering queued messages (docs/P2P-SPEC.md §8, Outbox): a task that sends each `queued`
//! row to its peer while the connection is up, retries transient failures with backoff under
//! the original `expires_at`, and ends a row on the peer's answer: `accepted` or `rejected`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use iroh::EndpointId;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tokio::time::sleep;

use super::audit::{self, Decision};
use super::{identity, now_ms};
use crate::fed::service::Handle;
use crate::store;

const TICK: Duration = Duration::from_millis(500);
/// Rows sent per tick.
const BATCH: i64 = 20;
const MAX_BACKOFF_MS: i64 = 10_000;

pub fn install(handle: &Handle, db: &Path) {
    tokio::spawn(run(handle.clone(), db.to_owned()));
}

struct Due {
    message_id: String,
    node: EndpointId,
    generation: i64,
    frame: Value,
    attempts: i64,
}

/// Record what happened to an outbound message, under the peer's fingerprint.
fn audit_outcome(
    conn: &Connection,
    node: &str,
    generation: i64,
    message_id: &str,
    decision: &str,
    reason: Option<&str>,
) -> anyhow::Result<()> {
    let fingerprint = node
        .parse::<EndpointId>()
        .map(|node| identity::fingerprint(&node))
        .ok();
    audit::record(
        conn,
        &Decision {
            peer_fingerprint: fingerprint.as_deref(),
            generation: Some(generation),
            share_id: None,
            message_id: Some(message_id),
            direction: "out",
            decision,
            reason,
        },
    )
}

/// Housekeeping, then the rows ready to send. A row ends here when it expired, or when its
/// peer was removed or paired again (a new generation is a new relationship).
fn due(conn: &Connection, now: i64) -> anyhow::Result<Vec<Due>> {
    let mut stmt = conn.prepare(
        "SELECT o.message_id, o.from_agent, o.generation, p.node_id FROM fed_outbox o
         JOIN peers p ON p.peer_id=o.peer_id WHERE o.state='queued' AND o.expires_at <= ?1",
    )?;
    let lapsed: Vec<(String, String, i64, String)> = stmt
        .query_map([now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    for (message_id, sender, generation, node) in lapsed {
        conn.execute(
            "UPDATE fed_outbox SET state='expired' WHERE message_id=?1",
            [&message_id],
        )?;
        audit_outcome(conn, &node, generation, &message_id, "expired", None)?;
        let text = format!("remote delivery of {message_id} expired");
        if let Err(err) = crate::msg::from_bus(conn, &sender, &sender, "sync", &text) {
            eprintln!("axon-bus: could not tell {sender} about an expired message: {err:#}");
        }
    }
    conn.execute(
        "UPDATE fed_outbox SET state='cancelled', last_error='peer_changed' WHERE state='queued'
           AND NOT EXISTS (SELECT 1 FROM peers p WHERE p.peer_id=fed_outbox.peer_id
                           AND p.generation=fed_outbox.generation
                           AND p.state IN ('active','paused'))",
        [],
    )?;
    let mut stmt = conn.prepare(
        "SELECT o.message_id, p.node_id, o.generation, o.envelope_json, o.attempts
         FROM fed_outbox o JOIN peers p ON p.peer_id=o.peer_id
         WHERE o.state='queued' AND p.state='active'
           AND (o.next_attempt_at IS NULL OR o.next_attempt_at <= ?1)
         ORDER BY o.created_at, o.rowid LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![now, BATCH], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })?;
    let mut ready = Vec::new();
    for row in rows {
        let (message_id, node, generation, envelope, attempts) = row?;
        let (Ok(node), Ok(mut frame)) = (node.parse(), serde_json::from_str::<Value>(&envelope))
        else {
            continue;
        };
        frame["type"] = json!("msg");
        frame["v"] = json!(1);
        frame["generation"] = json!(generation);
        ready.push(Due {
            message_id,
            node,
            generation,
            frame,
            attempts,
        });
    }
    Ok(ready)
}

/// What the peer answered, or why we are still waiting.
enum Verdict {
    Accepted,
    Rejected(String),
    Retry,
}

fn judge(answer: &anyhow::Result<Value>) -> Verdict {
    let Ok(reply) = answer else {
        return Verdict::Retry;
    };
    let reason = reply["reason"].as_str().unwrap_or("rejected").to_owned();
    match (reply["type"].as_str(), reply["status"].as_str()) {
        (Some("ack"), Some("accepted" | "duplicate")) => Verdict::Accepted,
        // A busy receiver says nothing about the message itself: try again later.
        (Some("ack"), Some("rejected")) if reason == "rate_limited" => Verdict::Retry,
        (Some("ack"), _) => Verdict::Rejected(reason),
        (Some("error"), _) if reason == "unavailable" => Verdict::Retry,
        _ => Verdict::Rejected(reason),
    }
}

fn settle(db: &Path, row: &Due, result: &Verdict) -> anyhow::Result<()> {
    let mut conn = store::open(db)?;
    let tx = store::write_tx(&mut conn)?;
    let node = row.node.to_string();
    let audit = |decision, reason| {
        audit_outcome(
            &tx,
            &node,
            row.generation,
            &row.message_id,
            decision,
            reason,
        )
    };
    match result {
        Verdict::Accepted => {
            let changed = tx.execute(
                "UPDATE fed_outbox SET state='accepted', last_error=NULL WHERE message_id=?1 AND state='queued'",
                [&row.message_id],
            )?;
            if changed > 0 {
                audit("accepted", None)?;
            }
        }
        Verdict::Rejected(reason) => {
            let changed = tx.execute(
                "UPDATE fed_outbox SET state='rejected', last_error=?2 WHERE message_id=?1 AND state='queued'",
                params![row.message_id, reason],
            )?;
            if changed > 0 {
                audit("rejected", Some(reason))?;
            }
        }
        Verdict::Retry => {
            let wait = (1000i64 << row.attempts.clamp(0, 4)).min(MAX_BACKOFF_MS);
            tx.execute(
                "UPDATE fed_outbox SET attempts=attempts+1, next_attempt_at=?2, last_error='unreachable'
                 WHERE message_id=?1 AND state='queued'",
                params![row.message_id, now_ms() + wait],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

async fn run(handle: Handle, db: PathBuf) {
    while !handle.stopped() {
        sleep(TICK).await;
        let ready = tokio::task::spawn_blocking({
            let db = db.clone();
            move || due(&store::open(&db)?, now_ms())
        })
        .await;
        let Ok(Ok(ready)) = ready else {
            continue;
        };
        for row in ready {
            let answer = handle.request(&row.node, &row.frame).await;
            let result = judge(&answer);
            let db = db.clone();
            let _ = tokio::task::spawn_blocking(move || settle(&db, &row, &result)).await;
        }
    }
}
