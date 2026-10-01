//! A peer's `msg` frame (docs/P2P-SPEC.md §8): the ten checks, in order. The first failure
//! rejects and nothing is stored; the commit of the message is durable before `accepted` is
//! sent. This is the receiving side's gate, and it trusts nothing the sender checked.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use iroh::EndpointId;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

use super::audit::{self, Decision};
use super::envelope::{self, CLOCK_SKEW_MS, KINDS, LIFETIME_MS};
use super::service::{FrameHandler, Handle};
use super::{identity, now_ms, random_id, shares};
use crate::session::sha256_hex;
use crate::store;

/// Unanswered inbound messages one agent may hold.
const PENDING_PER_RECIPIENT: i64 = 100;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Msg {
    #[serde(rename = "type")]
    _type: String,
    v: i64,
    generation: i64,
    message_id: String,
    share_id: String,
    revision: i64,
    from_session: String,
    to_session: String,
    kind: String,
    body: String,
    thread: String,
    reply_to: Option<String>,
    refs: Vec<String>,
    created_at: i64,
    expires_at: i64,
}

impl Msg {
    /// What makes two frames with one `message_id` the same message.
    fn content_hash(&self) -> String {
        let parts = json!([
            self.share_id,
            self.from_session,
            self.to_session,
            self.kind,
            self.body,
            self.thread,
            self.reply_to,
            self.refs,
            self.created_at,
            self.expires_at
        ]);
        sha256_hex(parts.to_string().as_bytes())
    }
}

/// A token bucket: `burst` at most, refilled at `per_second`.
struct Bucket {
    tokens: f64,
    at: Instant,
}

/// Per-peer and per-recipient request rates (§8, check 8), kept in memory.
#[derive(Default)]
pub struct Limits {
    peers: Mutex<HashMap<String, Bucket>>,
    recipients: Mutex<HashMap<String, Bucket>>,
}

fn take(buckets: &Mutex<HashMap<String, Bucket>>, key: &str, per_second: f64, burst: f64) -> bool {
    let mut buckets = buckets.lock().unwrap_or_else(|p| p.into_inner());
    let now = Instant::now();
    let bucket = buckets.entry(key.to_owned()).or_insert(Bucket {
        tokens: burst,
        at: now,
    });
    bucket.tokens =
        (bucket.tokens + now.duration_since(bucket.at).as_secs_f64() * per_second).min(burst);
    bucket.at = now;
    let allowed = bucket.tokens >= 1.0;
    if allowed {
        bucket.tokens -= 1.0;
    }
    allowed
}

impl Limits {
    fn allow(&self, peer_id: &str, recipient: &str) -> bool {
        take(&self.peers, peer_id, 10.0, 20.0) && take(&self.recipients, recipient, 2.0, 5.0)
    }
}

pub fn install(handle: &Handle, db: &Path) {
    handle.on_frame("msg", handler(db.to_owned(), Arc::new(Limits::default())));
}

fn handler(db: PathBuf, limits: Arc<Limits>) -> FrameHandler {
    Arc::new(move |node: String, frame: Value| {
        let (db, limits) = (db.clone(), limits.clone());
        Box::pin(async move {
            let Ok(msg) = serde_json::from_value::<Msg>(frame) else {
                return json!({"type": "error", "reason": "bad_frame"});
            };
            let done = tokio::task::spawn_blocking(move || {
                receive(&mut store::open(&db)?, &limits, &node, msg)
            })
            .await;
            match done {
                Ok(Ok(reply)) => reply,
                other => {
                    if let Ok(Err(err)) = other {
                        eprintln!("axon-bus: a remote message could not be handled: {err:#}");
                    }
                    json!({"type": "error", "reason": "unavailable"})
                }
            }
        })
    })
}

fn ack(status: &str, reason: Option<&str>) -> Value {
    match reason {
        Some(reason) => json!({"type": "ack", "status": status, "reason": reason}),
        None => json!({"type": "ack", "status": status}),
    }
}

/// The peer a message arrived from.
struct Sender {
    peer_id: String,
    label: String,
}

/// Run the pipeline. `Ok` is the response frame; a rejection is a reply, not an error.
fn receive(conn: &mut Connection, limits: &Limits, node: &str, msg: Msg) -> anyhow::Result<Value> {
    // Durable before the acknowledgement leaves; hook transactions keep NORMAL (§5).
    conn.pragma_update(None, "synchronous", "FULL")?;
    let tx = store::write_tx(conn)?;
    let fingerprint = node
        .parse::<EndpointId>()
        .map(|id| identity::fingerprint(&id))?;
    let verdict = check(&tx, limits, node, &msg)?;
    let decision = |decision: &'static str, reason: Option<&'static str>| Decision {
        peer_fingerprint: Some(&fingerprint),
        generation: Some(msg.generation),
        share_id: Some(&msg.share_id),
        message_id: Some(&msg.message_id),
        direction: "in",
        decision,
        reason,
    };
    let reply = match verdict {
        Verdict::Reject(reason) => {
            audit::record(&tx, &decision("rejected", Some(reason)))?;
            ack("rejected", Some(reason))
        }
        Verdict::Duplicate => ack("duplicate", None),
        Verdict::Store { from, recipient } => {
            store_message(&tx, &from, &recipient, &msg)?;
            audit::record(&tx, &decision("accepted", None))?;
            ack("accepted", None)
        }
    };
    tx.commit()?;
    Ok(reply)
}

enum Verdict {
    Reject(&'static str),
    Duplicate,
    Store { from: Sender, recipient: String },
}

fn check(tx: &Connection, limits: &Limits, node: &str, msg: &Msg) -> anyhow::Result<Verdict> {
    let reject = |reason| Ok(Verdict::Reject(reason));
    // 1. An active peer, in the generation we know.
    let peer: Option<(String, String, i64)> = tx
        .query_row(
            "SELECT peer_id, label, generation FROM peers WHERE node_id=?1 AND state='active'",
            [node],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((peer_id, label, generation)) = peer else {
        return reject("not_a_peer");
    };
    if msg.v != 1 || generation != msg.generation {
        return reject("stale_generation");
    }
    // 2. Field bounds.
    let session_ok = |s: &str| {
        s.len() == 12
            && s.bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    };
    let reply_ok = msg.reply_to.as_deref().is_none_or(envelope::is_uuid);
    if !envelope::is_uuid(&msg.message_id)
        || !envelope::body_ok(&msg.body)
        || !envelope::refs_ok(&msg.refs)
        || !envelope::thread_ok(&msg.thread)
        || !session_ok(&msg.from_session)
        || !session_ok(&msg.to_session)
        || !reply_ok
    {
        return reject("bad_message");
    }
    // 3. Only the four conversational kinds.
    if !KINDS.contains(&msg.kind.as_str()) {
        return reject("kind_not_allowed");
    }
    // 4. Time.
    let now = now_ms();
    if msg.created_at > now + CLOCK_SKEW_MS
        || msg.expires_at - msg.created_at > LIFETIME_MS
        || msg.expires_at <= msg.created_at
    {
        return reject("bad_time");
    }
    if msg.expires_at <= now {
        return reject("expired");
    }
    // 5. An active share of this peer, at the revision both sides agreed, open inbound.
    let share =
        shares::get(tx, &msg.share_id)?.filter(|s| s.peer_id == peer_id && s.state == "active");
    let Some(share) = share else {
        return reject("unknown_share");
    };
    if share.revision != msg.revision {
        return reject("stale_revision");
    }
    if !share.inbound {
        return reject("inbound_off");
    }
    // 6. A recipient that is still in the share's project and not closed.
    let recipient: Option<String> = tx
        .query_row(
            "SELECT agent_id FROM fed_sessions WHERE session=?1 AND share_id=?2",
            params![msg.to_session, share.share_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(recipient) = recipient else {
        return reject("unknown_session");
    };
    let member = match &share.local_repo {
        Some(repo) => shares::is_member(tx, &recipient, repo)?,
        None => false,
    };
    if !member {
        return reject("unknown_session");
    }
    // 7. An answer replies to something we sent that peer's recipient, in this generation.
    if msg.kind == "answer" && !answers_our_message(tx, &peer_id, generation, msg)? {
        return reject("bad_reply");
    }
    // 8. Rates, and how much is already waiting for the recipient.
    let pending: i64 = tx.query_row(
        "SELECT count(*) FROM messages WHERE to_id=?1 AND delivered_at IS NULL AND from_id LIKE 'peer:%'",
        [&recipient],
        |r| r.get(0),
    )?;
    // A full inbox does not clear by waiting, so unlike a rate limit the sender must not retry.
    if pending >= PENDING_PER_RECIPIENT {
        return reject("recipient_full");
    }
    if !limits.allow(&peer_id, &recipient) {
        return reject("rate_limited");
    }
    // 9. The same message again is acknowledged, not stored; the same id with other content
    // is a conflict.
    let seen: Option<String> = tx
        .query_row(
            "SELECT content_hash FROM fed_inbox WHERE peer_id=?1 AND generation=?2 AND message_id=?3",
            params![peer_id, generation, msg.message_id],
            |r| r.get(0),
        )
        .optional()?;
    match seen {
        Some(hash) if hash == msg.content_hash() => return Ok(Verdict::Duplicate),
        Some(_) => return reject("conflict"),
        None => {}
    }
    Ok(Verdict::Store {
        from: Sender { peer_id, label },
        recipient,
    })
}

/// Whether `msg.reply_to` is a message this side sent to exactly this peer, in this
/// generation, addressed to the session now answering.
fn answers_our_message(
    tx: &Connection,
    peer_id: &str,
    generation: i64,
    msg: &Msg,
) -> anyhow::Result<bool> {
    let Some(reply_to) = &msg.reply_to else {
        return Ok(false);
    };
    let envelope: Option<String> = tx
        .query_row(
            "SELECT envelope_json FROM fed_outbox WHERE message_id=?1 AND peer_id=?2
               AND generation=?3 AND state IN ('queued','accepted')",
            params![reply_to, peer_id, generation],
            |r| r.get(0),
        )
        .optional()?;
    let sent: Option<Value> = envelope.and_then(|json| serde_json::from_str(&json).ok());
    Ok(sent.is_some_and(|sent| sent["to_session"] == msg.from_session.as_str()))
}

/// Commit the message as an ordinary `messages` row from `peer:<label>/<session>`, with its
/// dedup record. The sender is frozen as it was at receipt.
fn store_message(tx: &Connection, from: &Sender, recipient: &str, msg: &Msg) -> anyhow::Result<()> {
    let local_id = format!("r-{}", random_id()?);
    let seq: i64 = tx.query_row(
        "SELECT coalesce(max(seq), 0) + 1 FROM messages WHERE thread=?1",
        [&msg.thread],
        |r| r.get(0),
    )?;
    let now = now_ms();
    tx.execute(
        "INSERT INTO messages (id,thread,seq,from_id,to_id,kind,body,refs_json,needs_reply,sent_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            local_id,
            msg.thread,
            seq,
            format!("peer:{}/{}", from.label, msg.from_session),
            recipient,
            msg.kind,
            msg.body,
            serde_json::to_string(&msg.refs)?,
            msg.kind == "question",
            now
        ],
    )?;
    tx.execute(
        "INSERT INTO fed_inbox (peer_id,generation,message_id,content_hash,local_message_id,accepted_at,expires_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            from.peer_id,
            msg.generation,
            msg.message_id,
            msg.content_hash(),
            local_id,
            now,
            msg.expires_at
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
