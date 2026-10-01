//! Asking peers for their sessions: every active share is re-read every 30 s, and at once
//! when its revision changes. What the answer no longer lists is dropped, and so is any row
//! a peer has not confirmed for 60 s.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::EndpointId;
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::sleep;

use super::{answer, PAGE, PEER_CAP};
use crate::fed::now_ms;
use crate::fed::service::{FrameHandler, Handle};
use crate::store;

const REFRESH_EVERY: Duration = Duration::from_secs(30);
/// A refresh that failed (the peer is away, or its share has not caught up) is retried soon.
const RETRY_AFTER: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_secs(1);
const STALE_AFTER_MS: i64 = 60_000;
/// Pages that fit the per-peer cap; a peer that offers more is cut off here.
const MAX_PAGES: usize = PEER_CAP.div_ceil(PAGE);

/// Register the `discovery` frame and start the refresh task.
pub fn install(handle: &Handle, db: &Path) {
    handle.on_frame("discovery", answer_handler(db.to_owned()));
    tokio::spawn(refresh_loop(handle.clone(), db.to_owned()));
}

fn answer_handler(db: PathBuf) -> FrameHandler {
    Arc::new(move |node: String, frame: Value| {
        let db = db.clone();
        Box::pin(async move {
            match tokio::task::spawn_blocking(move || answer(&mut store::open(&db)?, &node, frame))
                .await
            {
                Ok(Ok(reply)) => reply,
                other => {
                    if let Ok(Err(err)) = other {
                        eprintln!("axon-bus: discovery could not answer: {err:#}");
                    }
                    json!({"type": "error", "reason": "unavailable"})
                }
            }
        })
    })
}

/// One session as a peer lists it; checked field by field because the peer is not trusted.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Listed {
    session: String,
    label: String,
    availability: String,
}

impl Listed {
    fn valid(&self) -> bool {
        let plain = |s: &str, max: usize| {
            (1..=max).contains(&s.len())
                && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        };
        self.session.len() == 12
            && self
                .session
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && plain(&self.label, 40)
            && matches!(self.availability.as_str(), "active" | "idle")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    #[serde(rename = "type")]
    _type: String,
    sessions: Vec<Listed>,
    next_page: Option<usize>,
}

#[derive(Clone)]
struct Target {
    share_id: String,
    revision: i64,
    peer_id: String,
    node: EndpointId,
    generation: i64,
}

/// Active shares with an active peer, as of now.
fn due_targets(conn: &Connection) -> anyhow::Result<Vec<Target>> {
    let mut stmt = conn.prepare(
        "SELECT s.share_id, s.revision, s.peer_id, p.node_id, p.generation
         FROM peer_shares s JOIN peers p ON p.peer_id=s.peer_id
         WHERE s.state='active' AND p.state='active' AND s.local_repo IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get::<_, String>(3)?,
            r.get(4)?,
        ))
    })?;
    let mut targets = Vec::new();
    for row in rows {
        let (share_id, revision, peer_id, node, generation) = row?;
        if let Ok(node) = node.parse() {
            targets.push(Target {
                share_id,
                revision,
                peer_id,
                node,
                generation,
            });
        }
    }
    Ok(targets)
}

/// Rows a peer has not confirmed within a minute: it is gone or has stopped answering.
pub fn drop_stale(conn: &Connection, now: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM fed_remote_sessions WHERE seen_at < ?1",
        [now - STALE_AFTER_MS],
    )
}

/// Replace what one share lists with `listed`, keeping the peer under its cap. Nothing is
/// written when the share stopped being active while we asked.
fn replace(db: &Path, target: &Target, listed: &[Listed]) -> anyhow::Result<()> {
    let mut conn = store::open(db)?;
    let tx = store::write_tx(&mut conn)?;
    let active: bool = tx.query_row(
        "SELECT count(*) > 0 FROM peer_shares WHERE share_id=?1 AND state='active'",
        [&target.share_id],
        |r| r.get(0),
    )?;
    tx.execute(
        "DELETE FROM fed_remote_sessions WHERE peer_id=?1 AND share_id=?2",
        params![target.peer_id, target.share_id],
    )?;
    if active {
        let others: usize = tx.query_row(
            "SELECT count(*) FROM fed_remote_sessions WHERE peer_id=?1",
            [&target.peer_id],
            |r| r.get(0),
        )?;
        let now = now_ms();
        for item in listed.iter().take(PEER_CAP.saturating_sub(others)) {
            tx.execute(
                "INSERT OR REPLACE INTO fed_remote_sessions
                 (peer_id, share_id, session, label, availability, seen_at) VALUES (?1,?2,?3,?4,?5,?6)",
                params![target.peer_id, target.share_id, item.session, item.label, item.availability, now],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Read every page of one share from its peer and store the result.
async fn refresh(handle: &Handle, db: &Path, target: &Target) -> anyhow::Result<()> {
    let mut listed = Vec::new();
    let mut page = 0;
    loop {
        let frame = json!({"type": "discovery", "v": 1, "generation": target.generation,
                           "share_id": target.share_id, "revision": target.revision, "page": page});
        let reply = handle.request(&target.node, &frame).await?;
        if reply["type"] == "error" {
            anyhow::bail!("{}", reply["reason"].as_str().unwrap_or("error"));
        }
        let answer: Answer = serde_json::from_value(reply)?;
        anyhow::ensure!(
            answer.sessions.len() <= PAGE && answer.sessions.iter().all(Listed::valid),
            "invalid session list"
        );
        listed.extend(answer.sessions);
        match answer.next_page {
            Some(next) if next == page + 1 && next < MAX_PAGES => page = next,
            Some(_) => break,
            None => break,
        }
    }
    let (db, owned) = (db.to_owned(), target.clone());
    tokio::task::spawn_blocking(move || replace(&db, &owned, &listed)).await?
}

async fn refresh_loop(handle: Handle, db: PathBuf) {
    // Per share: the revision and time of the last good refresh, and when to retry a bad one.
    let mut done: HashMap<String, (i64, Instant)> = HashMap::new();
    let mut retry: HashMap<String, Instant> = HashMap::new();
    while !handle.stopped() {
        sleep(POLL).await;
        let targets = tokio::task::spawn_blocking({
            let db = db.clone();
            move || -> anyhow::Result<Vec<Target>> {
                let conn = store::open(&db)?;
                drop_stale(&conn, now_ms())?;
                due_targets(&conn)
            }
        })
        .await;
        let Ok(Ok(targets)) = targets else {
            continue;
        };
        done.retain(|id, _| targets.iter().any(|t| &t.share_id == id));
        for target in targets {
            let fresh = done.get(&target.share_id).is_some_and(|(revision, at)| {
                *revision == target.revision && at.elapsed() < REFRESH_EVERY
            });
            let waiting = retry
                .get(&target.share_id)
                .is_some_and(|at| Instant::now() < *at);
            if fresh || waiting {
                continue;
            }
            match refresh(&handle, &db, &target).await {
                Ok(()) => {
                    done.insert(target.share_id.clone(), (target.revision, Instant::now()));
                    retry.remove(&target.share_id);
                }
                Err(_) => {
                    retry.insert(target.share_id.clone(), Instant::now() + RETRY_AFTER);
                }
            }
        }
    }
}
