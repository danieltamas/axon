//! Pairing on `axon/fed/1`: the `confirmed` and `notice` frames, their delivery, and the
//! upkeep task that times out stale pairings.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iroh::EndpointId;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::sleep;

use super::admit::pair_handler;
use super::{expire_stale, CONFIRM_WINDOW_MS, NOTICE_TRIES, RETRY_EVERY, SWEEP_EVERY};
use crate::fed::service::{FrameHandler, Handle, Link};
use crate::fed::{lifecycle, now_ms, reconcile};
use crate::store;

/// Tell the other side a pairing was removed, retrying while the connection comes up, then
/// let the service drop the peer. The service is reloaded only afterwards: reloading closes
/// the connection the notice needs.
pub async fn notify_removed(handle: Handle, node_id: String, generation: i64) {
    notify(&handle, &node_id, generation, "removed", 0).await;
    handle.reload();
}

/// Tell the other side the pairing is usable again, so it stops showing it paused. It
/// retries like `notify_removed`: a peer that never hears this keeps refusing to send.
pub async fn notify_resumed(handle: Handle, node_id: String, generation: i64, seq: i64) {
    notify(&handle, &node_id, generation, "resumed", seq).await;
}

/// Deliver a lifecycle notice (`paused`, `resumed`, `removed`) until the peer acknowledges
/// it. A connection failure and an error answer (a busy peer says `rate_limited`) both retry;
/// only an answer that says the pairing is gone ends it early. The acknowledgement carries
/// the peer's own state, which is learned on the spot.
pub async fn notify(handle: &Handle, node_id: &str, generation: i64, what: &str, seq: i64) {
    let Ok(node) = node_id.parse::<EndpointId>() else {
        return;
    };
    let frame = json!({"type": "notice", "v": 1, "generation": generation, "what": what,
                       "seq": seq});
    for _ in 0..NOTICE_TRIES {
        if handle.stopped() {
            return;
        }
        match handle.request(&node, &frame).await {
            Ok(reply) if reply["type"] == "ack" => {
                let db = handle.db_path().to_owned();
                let (node_id, reply) = (node_id.to_owned(), reply);
                let _ = tokio::task::spawn_blocking(move || {
                    reconcile::learn_reply(&store::open(&db)?, &node_id, generation, &reply)
                        .map_err(anyhow::Error::from)
                })
                .await;
                return;
            }
            Ok(reply)
                if matches!(
                    reply["reason"].as_str(),
                    Some("unknown_peer" | "stale_generation")
                ) =>
            {
                return
            }
            _ => sleep(RETRY_EVERY).await,
        }
    }
}

/// Send our `confirmed` frame until the other side acknowledges it. It ends when the peer is
/// no longer pairing, the window closes, or the service stops.
pub async fn send_confirmed(handle: Handle, db: PathBuf, peer_id: String) {
    while !handle.stopped() {
        let target = tokio::task::spawn_blocking({
            let (db, peer_id) = (db.clone(), peer_id.clone());
            move || confirmed_target(&db, &peer_id)
        })
        .await;
        let Ok(Ok(Some((node_id, generation)))) = target else {
            return;
        };
        let Ok(node) = node_id.parse::<EndpointId>() else {
            return;
        };
        let frame = json!({"type": "confirmed", "v": 1, "generation": generation});
        if let Ok(reply) = handle.request(&node, &frame).await {
            if reply["type"] == "ack" {
                return;
            }
        }
        sleep(RETRY_EVERY).await;
    }
}

/// Where `confirmed` should go: a peer we confirmed that is still within its window.
fn confirmed_target(db: &Path, peer_id: &str) -> anyhow::Result<Option<(String, i64)>> {
    let conn = store::open(db)?;
    Ok(conn
        .query_row(
            "SELECT node_id, generation FROM peers
             WHERE peer_id=?1 AND state IN ('pending_confirm','active')
               AND local_confirmed_at IS NOT NULL AND paired_at > ?2 - ?3",
            params![peer_id, now_ms(), CONFIRM_WINDOW_MS],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairingFrame {
    #[serde(rename = "type")]
    _type: String,
    v: i64,
    generation: i64,
    /// Present on `notice` only.
    what: Option<String>,
    /// The sender's lifecycle sequence, on `paused`, `resumed` and `state` notices.
    seq: Option<i64>,
    /// The sender's pause state, on `state` notices.
    paused: Option<bool>,
}

fn error(reason: &str) -> Value {
    json!({"type": "error", "reason": reason})
}

/// The other side confirmed: record it, and become `active` if we confirmed too.
fn remote_confirmed(conn: &mut Connection, node: &str, generation: i64) -> anyhow::Result<Value> {
    let now = now_ms();
    let tx = store::write_tx(conn)?;
    expire_stale(&tx, now)?;
    let peer: Option<(String, i64, String)> = tx
        .query_row(
            "SELECT peer_id, generation, state FROM peers WHERE node_id=?1 AND state<>'removed'",
            [node],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let reply = match peer {
        Some((_, current, _)) if current != generation => error("stale_generation"),
        Some((peer_id, _, state)) if state == "pending_confirm" => {
            tx.execute(
                "UPDATE peers SET remote_confirmed_at=COALESCE(remote_confirmed_at, ?1),
                 state=CASE WHEN local_confirmed_at IS NOT NULL THEN 'active' ELSE state END
                 WHERE peer_id=?2",
                params![now, peer_id],
            )?;
            json!({"type": "ack", "status": "accepted"})
        }
        Some((_, _, state)) if state == "active" => json!({"type": "ack", "status": "duplicate"}),
        _ => error("not_pending"),
    };
    tx.commit()?;
    Ok(reply)
}

/// The other side rejected or abandoned the pairing while it was pending, or removed it
/// after it was live: either way it is over, and a live one ends exactly as if the owner had
/// removed it, recording `remote_removed` so the UI can say who ended it.
fn remote_removed(conn: &mut Connection, node: &str, generation: i64) -> anyhow::Result<Value> {
    let changed = conn.execute(
        "UPDATE peers SET state='removed', removed_at=?1, removed_reason='remote_rejected'
         WHERE node_id=?2 AND generation=?3 AND state='pending_confirm'",
        params![now_ms(), node, generation],
    )?;
    if changed > 0 {
        return Ok(json!({"type": "ack", "status": "accepted"}));
    }
    let live: Option<String> = conn
        .query_row(
            "SELECT peer_id FROM peers WHERE node_id=?1 AND generation=?2
             AND state IN ('active','paused')",
            params![node, generation],
            |r| r.get(0),
        )
        .optional()?;
    let Some(peer_id) = live else {
        return Ok(error("not_pending"));
    };
    lifecycle::remove_because(conn, &peer_id, "remote_removed")?;
    Ok(json!({"type": "ack", "status": "accepted"}))
}

/// The other side paused, resumed or told us its state: remember it, so sends are refused
/// with `peer_paused` and the UI says who paused, unless a newer sequence was already applied.
/// Our own state does not change; the answer carries it back.
fn set_remote_paused(
    conn: &mut Connection,
    node: &str,
    frame: &PairingFrame,
) -> anyhow::Result<Value> {
    let paused = match frame.what.as_deref() {
        Some("paused") => true,
        Some("resumed") => false,
        _ => frame.paused.unwrap_or(false),
    };
    let Some(seq) = frame.seq else {
        return Ok(error("bad_frame"));
    };
    reconcile::learn(conn, node, frame.generation, paused, seq)?;
    Ok(reconcile::acknowledge(conn, node)?)
}

/// What a handler does to the database for one parsed frame: `(connection, sender's node id,
/// frame)`, answering the response frame.
type Apply = fn(&mut Connection, &str, &PairingFrame) -> anyhow::Result<Value>;

fn frame_handler(db: PathBuf, link: Link, apply: Apply) -> FrameHandler {
    Arc::new(move |node: String, frame: Value| {
        let (db, link) = (db.clone(), link.clone());
        Box::pin(async move {
            let Ok(frame) = serde_json::from_value::<PairingFrame>(frame) else {
                return error("bad_frame");
            };
            if frame.v != 1 {
                return error("unsupported_version");
            }
            let reply = tokio::task::spawn_blocking(move || {
                apply(&mut crate::fed::open_durable(&db)?, &node, &frame)
            })
            .await;
            link.reload();
            match reply {
                Ok(Ok(reply)) => reply,
                Ok(Err(err)) => {
                    eprintln!("axon-bus: a pairing frame could not be applied: {err:#}");
                    error("unavailable")
                }
                Err(_) => error("unavailable"),
            }
        })
    })
}

/// Wire pairing into a started service: the `axon/pair/1` handler, the `confirmed` and
/// `notice` frames, and a task that times out stale pairings and resumes unsent confirmations.
pub fn install(handle: &Handle, db: &Path) {
    let link = handle.link();
    handle.on_pair(pair_handler(db.to_owned(), link.clone()));
    handle.on_frame(
        "confirmed",
        frame_handler(db.to_owned(), link.clone(), |conn, node, frame| {
            remote_confirmed(conn, node, frame.generation)
        }),
    );
    handle.on_frame(
        "notice",
        frame_handler(db.to_owned(), link, |conn, node, frame| {
            match frame.what.as_deref() {
                Some("removed") => remote_removed(conn, node, frame.generation),
                Some("paused" | "resumed" | "state") => set_remote_paused(conn, node, frame),
                _ => Ok(error("unsupported_notice")),
            }
        }),
    );
    tokio::spawn(maintain(handle.clone(), db.to_owned()));
}

async fn maintain(handle: Handle, db: PathBuf) {
    let mut first = true;
    while !handle.stopped() {
        let upkeep_db = db.clone();
        let upkeep = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let conn = store::open(&upkeep_db)?;
            Ok((expire_stale(&conn, now_ms())?, unsent_confirmations(&conn)?))
        })
        .await;
        match upkeep {
            Ok(Ok((expired, unsent))) => {
                if expired > 0 {
                    handle.reload();
                }
                if first {
                    for peer_id in unsent {
                        tokio::spawn(send_confirmed(handle.clone(), db.clone(), peer_id));
                    }
                }
            }
            Ok(Err(err)) => eprintln!("axon-bus: pairing upkeep failed: {err:#}"),
            Err(_) => {}
        }
        first = false;
        sleep(SWEEP_EVERY).await;
    }
}

/// Pairings we confirmed before a restart whose window is still open; the frame may not have
/// reached the other side, whether or not we are already `active` (the ack is not stored).
/// `send_confirmed` ends on the first ack, and an already-active peer answers `duplicate`.
fn unsent_confirmations(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT peer_id FROM peers WHERE state IN ('pending_confirm','active')
           AND local_confirmed_at IS NOT NULL AND paired_at > ?1 - ?2",
    )?;
    let rows = stmt.query_map(params![now_ms(), CONFIRM_WINDOW_MS], |r| r.get(0))?;
    rows.collect()
}

#[cfg(test)]
mod tests;
