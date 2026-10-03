//! The handled ledger over a share (P2P-SPEC §7b). Every POLL the local entries of each
//! share's repository that changed since the peer last confirmed are pushed as a `handled`
//! frame; the first send after a connect, a new share or a failure is a full send of the live
//! entries. Entries a peer sends are filed under the share's `local_repo` with `peer_id` set
//! and are never sent back.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::EndpointId;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::sleep;

use crate::fed::discovery::session_for;
use crate::fed::now_ms;
use crate::fed::service::{FrameHandler, Handle};
use crate::handled::{self, Entry};
use crate::store;

const POLL: Duration = Duration::from_secs(2);
/// A peer on an older version is asked again after this long (it may have upgraded).
const SKIP_FOR: Duration = Duration::from_secs(600);
const FRAME_MAX: usize = 200;
const LABEL_MAX: usize = 40;

pub fn install(handle: &Handle, db: &Path) {
    let me = handle.health()["node_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    handle.on_frame("handled", receive_handler(db.to_owned(), me));
    tokio::spawn(push_loop(handle.clone(), db.to_owned()));
}

#[derive(Clone)]
struct Target {
    share_id: String,
    peer_id: String,
    repo: String,
    node: EndpointId,
    generation: i64,
}

/// Shares this side sends its ledger over: active, outbound here and inbound there.
fn targets(conn: &Connection) -> anyhow::Result<Vec<Target>> {
    let mut stmt = conn.prepare(
        "SELECT s.share_id, s.peer_id, s.local_repo, p.node_id, p.generation
         FROM peer_shares s JOIN peers p ON p.peer_id=s.peer_id
         WHERE s.state='active' AND p.state='active' AND s.local_repo IS NOT NULL
           AND s.outbound=1 AND s.remote_inbound=1",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (share_id, peer_id, repo, node, generation) = row?;
        if let Ok(node) = node.parse() {
            out.push(Target {
                share_id,
                peer_id,
                repo,
                node,
                generation,
            });
        }
    }
    Ok(out)
}

/// The holder as the peer sees it: its discovery label (`<harness>-<4 chars>`), minting
/// the session discovery would, so the label never changes after it was first sent.
fn label(conn: &Connection, agent: Option<&str>, share_id: &str) -> anyhow::Result<String> {
    let Some(agent) = agent else {
        return Ok("agent".into());
    };
    let harness: Option<String> = conn
        .query_row("SELECT harness FROM agents WHERE id=?1", [agent], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(harness) = harness else {
        return Ok("agent".into());
    };
    let session = session_for(conn, agent, share_id)?;
    Ok(format!("{harness}-{}", &session[..4]))
}

/// Local entries of the target's repo changed after `since`, or every live one when `None`,
/// with the newest `changed_at` among them.
fn pending(
    conn: &Connection,
    target: &Target,
    since: Option<i64>,
    now: i64,
) -> anyhow::Result<(Vec<Value>, i64)> {
    let mut stmt = conn.prepare(
        "SELECT key, state, agent_id, note, at, expires_at, changed_at FROM handled
         WHERE repo=?1 AND peer_id IS NULL AND changed_at > ?2
           AND (?3 OR state='done' OR (state='taken' AND expires_at > ?4))
         ORDER BY changed_at",
    )?;
    let rows = stmt.query_map(
        params![target.repo, since.unwrap_or(i64::MIN), since.is_some(), now],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, i64>(6)?,
            ))
        },
    )?;
    let mut entries = Vec::new();
    let mut through = since.unwrap_or(0);
    for row in rows {
        let (key, state, agent, note, at, expires_at, changed) = row?;
        let label = label(conn, agent.as_deref(), &target.share_id)?;
        entries.push(
            json!({"key": key, "state": state, "label": label, "note": note,
                            "at": at, "expires_at": expires_at}),
        );
        through = through.max(changed);
    }
    Ok((entries, through))
}

/// Send what changed on every target. `sent` holds, per (share, generation), the newest
/// `changed_at` the peer confirmed; a missing cursor means a full send.
async fn push_loop(handle: Handle, db: PathBuf) {
    let mut sent: HashMap<(String, i64), i64> = HashMap::new();
    let mut skipped: HashMap<String, Instant> = HashMap::new();
    loop {
        sleep(POLL).await;
        let list = {
            let db = db.clone();
            tokio::task::spawn_blocking(move || targets(&store::open(&db)?)).await
        };
        let Ok(Ok(list)) = list else { continue };
        let live: HashSet<_> = list
            .iter()
            .map(|t| (t.share_id.clone(), t.generation))
            .collect();
        sent.retain(|k, _| live.contains(k));
        skipped.retain(|_, at| at.elapsed() < SKIP_FOR);
        for target in list {
            if skipped.contains_key(&target.peer_id) {
                continue;
            }
            let cursor = (target.share_id.clone(), target.generation);
            match push(&handle, &db, &target, sent.get(&cursor).copied()).await {
                Ok(Some(through)) => {
                    sent.insert(cursor, through);
                }
                Ok(None) => {
                    skipped.insert(target.peer_id.clone(), Instant::now());
                }
                Err(_) => {
                    // Not connected, or the peer refused: start over with a full send.
                    sent.remove(&cursor);
                }
            }
        }
    }
}

/// One target's changes, in frames of at most FRAME_MAX. `Ok(None)`: the peer does not know
/// the frame.
async fn push(
    handle: &Handle,
    db: &Path,
    target: &Target,
    since: Option<i64>,
) -> anyhow::Result<Option<i64>> {
    let (entries, through) = {
        let db = db.to_owned();
        let target = target.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = store::open(&db)?;
            // Deferred: the poll takes the write lock only when a holder needs a session.
            let tx = conn.transaction()?;
            let found = pending(&tx, &target, since, now_ms())?;
            tx.commit()?;
            anyhow::Ok(found)
        })
        .await??
    };
    if entries.is_empty() && since.is_some() {
        return Ok(Some(through));
    }
    // An empty full send still confirms the peer understands the frame.
    let chunks: Vec<&[Value]> = if entries.is_empty() {
        vec![&[]]
    } else {
        entries.chunks(FRAME_MAX).collect()
    };
    for chunk in chunks {
        let frame = json!({"type": "handled", "share_id": target.share_id, "entries": chunk});
        let reply = handle.request(&target.node, &frame).await?;
        match reply["type"].as_str() {
            Some("handled_ack") => {}
            Some("error") if reply["reason"] == "unknown_frame" => return Ok(None),
            _ => anyhow::bail!("{}", reply["reason"].as_str().unwrap_or("bad reply")),
        }
    }
    Ok(Some(through))
}

/// One entry as a peer sends it; checked field by field because the peer is not trusted.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Sent {
    key: String,
    state: String,
    label: String,
    note: Option<String>,
    at: i64,
    expires_at: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    #[serde(rename = "type")]
    _type: String,
    share_id: String,
    entries: Vec<Sent>,
}

fn receive_handler(db: PathBuf, me: String) -> FrameHandler {
    Arc::new(move |node: String, frame: Value| {
        let db = db.clone();
        let me = me.clone();
        Box::pin(async move {
            let applied = tokio::task::spawn_blocking(move || {
                apply(&mut store::open(&db)?, &me, &node, frame)
            })
            .await;
            match applied {
                Ok(Ok(reply)) => reply,
                other => {
                    if let Ok(Err(err)) = other {
                        eprintln!("axon-bus: handled entries could not be applied: {err:#}");
                    }
                    json!({"type": "error", "reason": "unavailable"})
                }
            }
        })
    })
}

fn error(reason: &str) -> Value {
    json!({"type": "error", "reason": reason})
}

fn valid(sent: &Sent) -> bool {
    handled::key(&sent.key).is_ok()
        && matches!(sent.state.as_str(), "taken" | "done" | "free")
        && (1..=LABEL_MAX).contains(&sent.label.len())
        && sent
            .label
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        && handled::note(sent.note.as_deref()).is_ok()
        && sent.at > 0
        && (sent.state != "taken" || sent.expires_at.is_some_and(|e| e > sent.at))
}

/// Whether `incoming` (from `theirs`) beats the entry already holding the key (owned by
/// `owner`): done beats taken; otherwise the earlier `at`, a tie going to the lower node id.
fn wins(incoming: &Sent, theirs: &str, held: &Entry, owner: &str) -> bool {
    let rank = |state: &str| u8::from(state == "done");
    match rank(&incoming.state).cmp(&rank(&held.state)) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => (incoming.at, theirs) < (held.at, owner),
    }
}

fn apply(conn: &mut Connection, me: &str, node: &str, frame: Value) -> anyhow::Result<Value> {
    let Ok(frame) = serde_json::from_value::<Frame>(frame) else {
        return Ok(error("bad_frame"));
    };
    if frame.entries.len() > FRAME_MAX || !frame.entries.iter().all(valid) {
        return Ok(error("bad_frame"));
    }
    let tx = store::write_tx(conn)?;
    let peer: Option<(String, String)> = tx
        .query_row(
            "SELECT peer_id, label FROM peers WHERE node_id=?1 AND state='active'",
            [node],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((peer_id, peer_label)) = peer else {
        return Ok(error("not_a_peer"));
    };
    let repo: Option<String> = tx
        .query_row(
            "SELECT local_repo FROM peer_shares WHERE share_id=?1 AND peer_id=?2
             AND state='active' AND inbound=1 AND local_repo IS NOT NULL",
            [&frame.share_id, &peer_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(repo) = repo else {
        return Ok(error("unknown_share"));
    };
    let now = store::now_ms();
    for sent in frame.entries {
        let held = handled::get(&tx, &repo, &sent.key)?;
        let theirs = held
            .as_ref()
            .is_some_and(|h| h.peer_id.as_deref() == Some(&peer_id));
        if sent.state == "free" {
            if let (true, Some(mut entry)) = (theirs, held) {
                entry.state = "free".into();
                handled::put(&tx, &repo, &entry, now)?;
            }
            continue;
        }
        if let Some(held) = held.as_ref().filter(|_| !theirs) {
            if handled::holds(&tx, held, now)? {
                let owner = match &held.peer_id {
                    Some(other) => tx
                        .query_row("SELECT node_id FROM peers WHERE peer_id=?1", [other], |r| {
                            r.get::<_, String>(0)
                        })
                        .optional()?
                        .unwrap_or_default(),
                    None => me.to_owned(),
                };
                if !wins(&sent, node, held, &owner) {
                    continue;
                }
            }
        }
        let entry = Entry {
            key: sent.key,
            state: sent.state,
            holder: format!("peer:{peer_label}/{}", sent.label),
            agent_id: None,
            peer_id: Some(peer_id.clone()),
            note: sent.note,
            at: sent.at,
            expires_at: sent.expires_at,
        };
        handled::put(&tx, &repo, &entry, now)?;
    }
    tx.commit()?;
    Ok(json!({"type": "handled_ack"}))
}
