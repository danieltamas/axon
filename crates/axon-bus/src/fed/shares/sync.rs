//! Getting share changes to the peer: one prompt push per change, and a full resend of every
//! share each time a peer (re)connects, so a frame lost to an outage or a restart still
//! converges. Every frame is idempotent at the receiver (it compares revisions).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use iroh::EndpointId;
use rusqlite::OptionalExtension;
use serde_json::Value;
use tokio::time::sleep;

use super::{apply_frame, frame_of, get};
use crate::fed::service::{FrameHandler, Handle};
use crate::store;

const PUSH_TRIES: u32 = 5;
const PUSH_RETRY: Duration = Duration::from_secs(1);
const CONNECT_POLL: Duration = Duration::from_secs(1);

/// Register the four share frames and start the resend-on-connect task.
pub fn install(handle: &Handle, db: &Path) {
    for kind in [
        "share_offer",
        "share_accept",
        "share_update",
        "share_remove",
    ] {
        handle.on_frame(kind, frame_handler(db.to_owned()));
    }
    tokio::spawn(resend_on_connect(handle.clone(), db.to_owned()));
}

fn frame_handler(db: PathBuf) -> FrameHandler {
    Arc::new(move |node: String, frame: Value| {
        let db = db.clone();
        Box::pin(async move {
            let applied = tokio::task::spawn_blocking(move || {
                apply_frame(&mut store::open(&db)?, &node, frame)
            })
            .await;
            match applied {
                Ok(Ok(reply)) => reply,
                other => {
                    if let Ok(Err(err)) = other {
                        eprintln!("axon-bus: a share frame could not be applied: {err:#}");
                    }
                    serde_json::json!({"type": "error", "reason": "unavailable"})
                }
            }
        })
    })
}

/// The peer a share belongs to and the frame that brings it up to date.
fn target(db: &Path, share_id: &str, update: bool) -> anyhow::Result<Option<(EndpointId, Value)>> {
    let conn = store::open(db)?;
    let Some(share) = get(&conn, share_id)? else {
        return Ok(None);
    };
    let peer: Option<(String, i64)> = conn
        .query_row(
            "SELECT node_id, generation FROM peers WHERE peer_id=?1 AND state='active'",
            [&share.peer_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(peer.and_then(|(node, generation)| {
        let frame = frame_of(&share, generation, update)?;
        Some((node.parse().ok()?, frame))
    }))
}

/// Send `share_id`'s current state to its peer. `update` picks `share_update` over
/// `share_accept` for an active share. Gives up after a few tries: the resend on the next
/// connect covers a longer outage. An answer of any kind ends it; a refusal is final.
pub async fn push(handle: Handle, db: PathBuf, share_id: String, update: bool) {
    for _ in 0..PUSH_TRIES {
        if handle.stopped() {
            return;
        }
        let aim = tokio::task::spawn_blocking({
            let (db, share_id) = (db.clone(), share_id.clone());
            move || target(&db, &share_id, update)
        })
        .await;
        let Ok(Ok(Some((node, frame)))) = aim else {
            return;
        };
        if handle.request(&node, &frame).await.is_ok() {
            return;
        }
        sleep(PUSH_RETRY).await;
    }
}

/// The shares of `peer_id` the peer may need to hear about again.
fn unsettled(db: &Path, peer_id: &str) -> anyhow::Result<Vec<String>> {
    let conn = store::open(db)?;
    let mut stmt = conn.prepare(
        "SELECT share_id FROM peer_shares WHERE peer_id=?1
         AND state IN ('offered_out','active','removed')",
    )?;
    let rows = stmt.query_map([peer_id], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

async fn resend_on_connect(handle: Handle, db: PathBuf) {
    let mut connected: HashSet<String> = HashSet::new();
    while !handle.stopped() {
        sleep(CONNECT_POLL).await;
        let now: HashSet<String> = handle.health()["peers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|peer| peer["state"] == "connected")
            .filter_map(|peer| peer["peer_id"].as_str().map(str::to_owned))
            .collect();
        for peer_id in now.difference(&connected) {
            let listed = tokio::task::spawn_blocking({
                let (db, peer_id) = (db.clone(), peer_id.clone());
                move || unsettled(&db, &peer_id)
            })
            .await;
            for share_id in listed.into_iter().flatten().flatten() {
                tokio::spawn(push(handle.clone(), db.clone(), share_id, false));
            }
        }
        connected = now;
    }
}
