//! Delivering queued messages (docs/P2P-SPEC.md §8, Outbox): a task that sends each `queued`
//! row to its peer while the connection is up, retries transient failures with backoff under
//! the original `expires_at`, and ends a row on the peer's answer: `accepted` or `rejected`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iroh::EndpointId;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::sleep;

use super::audit::{self, Decision};
use super::{identity, now_ms, shares};
use crate::fed::service::Handle;
use crate::store;

const TICK: Duration = Duration::from_millis(500);
/// Rows sent per tick.
const BATCH: i64 = 20;
/// Rows of one peer in a batch, so a peer with a deep backlog leaves room for the others.
const PER_PEER: i64 = 4;
const MAX_BACKOFF_MS: i64 = 10_000;

/// Start the outbox. The handle's `shutdown` joins it, so a replacement service never runs
/// beside the old outbox.
pub fn install(handle: &Handle, db: &Path) {
    handle.attach_outbox(tokio::spawn(run(handle.clone(), db.to_owned())));
}

struct Due {
    message_id: String,
    peer_id: String,
    share_id: String,
    sender: String,
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

/// Tell the sending agent what became of its message; a failure here must not undo the outcome.
fn tell_sender(conn: &Connection, sender: &str, text: &str) {
    if let Err(err) = crate::msg::from_bus(conn, sender, sender, "sync", text) {
        eprintln!("axon-bus: could not tell {sender} about an undelivered message: {err:#}");
    }
}

/// End the rows that can no longer be sent: expired ones, and those whose peer was removed or
/// paired again (a new generation is a new relationship). Expiry, audit, sender notice and
/// cancellation commit together or not at all. A row of a peer in `sending` (node ids) is not
/// expired: its answer may be on the way, and an acceptance must not lose to the clock.
fn housekeeping(conn: &mut Connection, now: i64, sending: &[String]) -> anyhow::Result<()> {
    let conn = store::write_tx(conn)?;
    let mut stmt = conn.prepare(
        "SELECT o.message_id, o.from_agent, o.generation, p.node_id FROM fed_outbox o
         JOIN peers p ON p.peer_id=o.peer_id WHERE o.state='queued' AND o.expires_at <= ?1
           AND p.node_id NOT IN (SELECT value FROM json_each(?2))",
    )?;
    let skip = serde_json::to_string(sending)?;
    let lapsed: Vec<(String, String, i64, String)> = stmt
        .query_map(params![now, skip], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    for (message_id, sender, generation, node) in lapsed {
        conn.execute(
            "UPDATE fed_outbox SET state='expired' WHERE message_id=?1",
            [&message_id],
        )?;
        audit_outcome(&conn, &node, generation, &message_id, "expired", None)?;
        tell_sender(
            &conn,
            &sender,
            &format!("remote delivery of {message_id} expired"),
        );
    }
    conn.execute(
        "UPDATE fed_outbox SET state='cancelled', last_error='peer_changed' WHERE state='queued'
           AND NOT EXISTS (SELECT 1 FROM peers p WHERE p.peer_id=fed_outbox.peer_id
                           AND p.generation=fed_outbox.generation
                           AND p.state IN ('active','paused'))",
        [],
    )?;
    conn.commit()?;
    Ok(())
}

/// How long a failed send keeps the message, and its peer, out of the next batches.
fn retry_wait_ms(attempts: i64) -> i64 {
    (1000i64 << attempts.clamp(0, 4)).min(MAX_BACKOFF_MS)
}

/// The rows ready to send, at most `PER_PEER` per peer and none for the peers in `skipped`
/// (node ids: failed last send, or a send still in flight). Each peer's oldest row comes before
/// any peer's second, so a full batch spreads across peers instead of favouring the busiest.
fn due(conn: &mut Connection, now: i64, skipped: &[String]) -> anyhow::Result<Vec<Due>> {
    housekeeping(conn, now, skipped)?;
    let mut stmt = conn.prepare(
        "SELECT message_id, peer_id, share_id, from_agent, node_id, generation, envelope_json, attempts
         FROM (SELECT o.message_id, o.peer_id, o.share_id, o.from_agent, p.node_id, o.generation,
                      o.envelope_json, o.attempts, o.created_at, o.rowid AS seq,
                      ROW_NUMBER() OVER (PARTITION BY o.peer_id ORDER BY o.created_at, o.rowid) AS n
               FROM fed_outbox o JOIN peers p ON p.peer_id=o.peer_id
               WHERE o.state='queued' AND p.state='active' AND p.remote_paused=0
                 AND (o.next_attempt_at IS NULL OR o.next_attempt_at <= ?1)
                 AND p.node_id NOT IN (SELECT value FROM json_each(?4)))
         WHERE n <= ?3 ORDER BY n, created_at, seq LIMIT ?2",
    )?;
    let skip = serde_json::to_string(skipped)?;
    let rows = stmt.query_map(params![now, BATCH, PER_PEER, skip], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, i64>(7)?,
        ))
    })?;
    let mut ready = Vec::new();
    for row in rows {
        let (message_id, peer_id, share_id, sender, node, generation, envelope, attempts) = row?;
        let (Ok(node), Ok(mut frame)) = (node.parse(), serde_json::from_str::<Value>(&envelope))
        else {
            continue;
        };
        frame["type"] = json!("msg");
        frame["v"] = json!(1);
        frame["generation"] = json!(generation);
        ready.push(Due {
            message_id,
            peer_id,
            share_id,
            sender,
            node,
            generation,
            frame,
            attempts,
        });
    }
    Ok(ready)
}

/// What the authorization state says about a queued message right now.
enum Gate {
    Send,
    /// Not sendable yet, but not dead either (the peer is paused).
    Wait,
    End(&'static str),
}

/// Whether the share, peer and sender still allow this message, read in the caller's
/// transaction. A share revision bumped since queueing is re-stamped, not a reason to drop.
fn gate(conn: &Connection, row: &mut Due) -> anyhow::Result<Gate> {
    if !super::enabled(conn) {
        return Ok(Gate::Wait);
    }
    let peer: Option<(String, i64)> = conn
        .query_row(
            "SELECT CASE WHEN remote_paused THEN 'paused' ELSE state END, generation
             FROM peers WHERE peer_id=?1",
            [&row.peer_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match peer {
        Some((state, generation)) if generation == row.generation && state == "paused" => {
            return Ok(Gate::Wait)
        }
        Some((state, generation)) if generation == row.generation && state == "active" => {}
        _ => return Ok(Gate::End("peer_changed")),
    }
    let Some(share) = shares::get(conn, &row.share_id)?
        .filter(|s| s.state == "active" && s.peer_id == row.peer_id)
    else {
        return Ok(Gate::End("unshared"));
    };
    let member = match &share.local_repo {
        Some(repo) => shares::is_member(conn, &row.sender, repo)?,
        None => false,
    };
    if !member {
        return Ok(Gate::End("not_a_member"));
    }
    if !share.outbound {
        return Ok(Gate::End("outbound_off"));
    }
    if !share.remote_inbound {
        return Ok(Gate::End("remote_inbound_off"));
    }
    if row.frame["revision"].as_i64() != Some(share.revision) {
        conn.execute(
            "UPDATE fed_outbox SET revision=?2, envelope_json=json_set(envelope_json,'$.revision',?2)
             WHERE message_id=?1",
            params![row.message_id, share.revision],
        )?;
        row.frame["revision"] = json!(share.revision);
    }
    Ok(Gate::Send)
}

/// Re-check, in one write transaction right before the transmit, that the message is still
/// authorized (docs/P2P-SPEC.md §8); a revocation committed first ends the row here, one
/// committed later finds the message already sent. False means do not send now.
fn clear_to_send(db: &Path, row: &mut Due) -> anyhow::Result<bool> {
    let mut conn = store::open(db)?;
    let tx = store::write_tx(&mut conn)?;
    let expires_at: Option<i64> = tx
        .query_row(
            "SELECT expires_at FROM fed_outbox WHERE message_id=?1 AND state='queued'",
            [&row.message_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(expires_at) = expires_at else {
        return Ok(false);
    };
    let ended = if expires_at <= now_ms() {
        Some(("expired", None))
    } else {
        match gate(&tx, row)? {
            Gate::Send => None,
            Gate::Wait => return Ok(false),
            Gate::End(reason) => Some(("cancelled", Some(reason))),
        }
    };
    let Some((state, reason)) = ended else {
        tx.commit()?;
        return Ok(true);
    };
    tx.execute(
        "UPDATE fed_outbox SET state=?2, last_error=?3 WHERE message_id=?1",
        params![row.message_id, state, reason],
    )?;
    audit_outcome(
        &tx,
        &row.node.to_string(),
        row.generation,
        &row.message_id,
        state,
        reason,
    )?;
    let why = reason.map(|r| format!(": {r}")).unwrap_or_default();
    tell_sender(
        &tx,
        &row.sender,
        &format!("remote delivery of {} {state}{why}", row.message_id),
    );
    tx.commit()?;
    Ok(false)
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
        (Some("error"), _) if matches!(reason.as_str(), "unavailable" | "rate_limited") => {
            Verdict::Retry
        }
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
                tell_sender(
                    &tx,
                    &row.sender,
                    &format!("remote delivery of {} rejected: {reason}", row.message_id),
                );
            }
        }
        Verdict::Retry => {
            let wait = retry_wait_ms(row.attempts);
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

/// Send one peer's rows in order. Stops at the first transport failure and returns when the
/// peer may be tried again; the remaining rows stay queued.
/// Database work is never abandoned mid-way; only the wait for the peer's answer ends when
/// `stop` fires, so a joined task leaves no blocking job behind.
async fn send_to_peer(
    handle: Handle,
    db: PathBuf,
    rows: Vec<Due>,
    mut stop: watch::Receiver<bool>,
) -> Option<Instant> {
    for mut row in rows {
        if *stop.borrow() {
            return None;
        }
        // Shared with nothing but a revocation: held from the re-check to the write, not
        // through the peer's answer, so a stalled peer never holds it.
        let transmitting = handle.transmit_gate().read_owned().await;
        let cleared = tokio::task::spawn_blocking({
            let db = db.clone();
            move || clear_to_send(&db, &mut row).map(|ok| (ok, row))
        })
        .await;
        let Ok(Ok((true, row))) = cleared else {
            continue;
        };
        let answer = tokio::select! {
            answer = handle.request_then(&row.node, &row.frame, move || drop(transmitting)) => answer,
            _ = stop.wait_for(|stopped| *stopped) => return None,
        };
        let result = judge(&answer);
        let failed = answer.is_err();
        let wait = Duration::from_millis(retry_wait_ms(row.attempts) as u64);
        let db = db.clone();
        let _ = tokio::task::spawn_blocking(move || settle(&db, &row, &result)).await;
        if failed {
            return Some(Instant::now() + wait);
        }
    }
    None
}

async fn run(handle: Handle, db: PathBuf) {
    // Peers whose last send failed, and until when they are left alone.
    let mut backed_off: HashMap<EndpointId, Instant> = HashMap::new();
    // One task per peer with rows in flight: peers never wait on each other's answers.
    let mut sending: JoinSet<(EndpointId, Option<Instant>)> = JoinSet::new();
    let mut busy: HashSet<EndpointId> = HashSet::new();
    let (stop, stopped) = watch::channel(false);
    while !handle.stopped() {
        sleep(TICK).await;
        while let Some(finished) = sending.try_join_next() {
            if let Ok((node, retry_at)) = finished {
                busy.remove(&node);
                if let Some(until) = retry_at {
                    backed_off.insert(node, until);
                }
            }
        }
        backed_off.retain(|_, until| *until > Instant::now());
        let ready = tokio::task::spawn_blocking({
            let db = db.clone();
            let skip: Vec<String> = backed_off
                .keys()
                .chain(&busy)
                .map(ToString::to_string)
                .collect();
            move || due(&mut store::open(&db)?, now_ms(), &skip)
        })
        .await;
        let Ok(Ok(ready)) = ready else {
            continue;
        };
        let mut by_peer: Vec<(EndpointId, Vec<Due>)> = Vec::new();
        for row in ready {
            match by_peer.iter_mut().find(|(node, _)| *node == row.node) {
                Some((_, rows)) => rows.push(row),
                None => by_peer.push((row.node, vec![row])),
            }
        }
        for (node, rows) in by_peer {
            busy.insert(node);
            let (handle, db) = (handle.clone(), db.clone());
            let stopped = stopped.clone();
            sending.spawn(async move { (node, send_to_peer(handle, db, rows, stopped).await) });
        }
    }
    let _ = stop.send(true);
    while sending.join_next().await.is_some() {}
}

#[cfg(test)]
mod tests;
