//! Sending to another person's agent (docs/P2P-SPEC.md §7): `peer:<label>/<session>`. This is
//! the sender's side of the gate: it decides who may write to which remote session and what
//! may be written, then queues one row for the service to deliver (`outbox`). The receiver
//! decides again; nothing here is trusted over there.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;

use super::envelope::{self, KINDS, LIFETIME_MS};
use super::{discovery, enabled, now_ms, shares};
use crate::store;

pub const TARGET_PREFIX: &str = "peer:";

/// Queued rows one peer may hold, and their bytes; and the bytes across all peers.
const PEER_ROWS: i64 = 1000;
const PEER_BYTES: i64 = 8 << 20;
const TOTAL_BYTES: i64 = 32 << 20;

pub struct Request<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub kind: &'a str,
    pub body: &'a str,
    pub thread: Option<&'a str>,
    pub refs: &'a [String],
    /// The remote message an `answer` replies to.
    pub reply_to: Option<&'a str>,
}

pub enum Outcome {
    Queued {
        id: String,
        to: String,
    },
    /// One of the reasons of §7's table.
    Refused(&'static str),
}

/// Send to a remote target; `None` when `request.to` is not one and the local path applies.
pub fn send(conn: &mut Connection, request: &Request) -> anyhow::Result<Option<Outcome>> {
    send_via(conn, request, None)
}

/// What an answer to a received message is tied to: the peer by its immutable id (a label can
/// be changed, or reused by another peer after a removal) and the share the message came in
/// under. The discovery cache is only eventually consistent, so an answer never depends on it.
struct Pinned<'a> {
    peer_id: &'a str,
    share_id: &'a str,
}

fn send_via(
    conn: &mut Connection,
    request: &Request,
    pinned: Option<Pinned>,
) -> anyhow::Result<Option<Outcome>> {
    let Some(target) = request.to.strip_prefix(TARGET_PREFIX) else {
        return Ok(None);
    };
    Ok(Some(match enqueue(conn, request, target, pinned)? {
        Ok(id) => Outcome::Queued {
            id,
            to: request.to.to_owned(),
        },
        Err(reason) => Outcome::Refused(reason),
    }))
}

/// Answer a remote question: an `answer` to its sender, in the same thread, citing the
/// sender's own message id. `None` when `question` is not a remote message addressed to `from`.
pub fn reply(
    conn: &mut Connection,
    question: &str,
    from: &str,
    body: &str,
) -> anyhow::Result<Option<Outcome>> {
    let asked: Option<(String, String, String, String, Option<String>)> = conn
        .query_row(
            "SELECT m.from_id, m.thread, i.message_id, i.peer_id,
                    (SELECT a.share_id FROM fed_audit a
                     WHERE a.message_id=i.message_id AND a.generation=i.generation
                       AND a.direction='in' AND a.decision='accepted')
             FROM messages m
             JOIN fed_inbox i ON i.local_message_id=m.id
             WHERE m.id=?1 AND m.needs_reply=1 AND m.to_id=?2",
            params![question, from],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((asker, thread, remote_id, peer_id, share_id)) = asked else {
        return Ok(None);
    };
    let outcome = send_via(
        conn,
        &Request {
            from,
            to: &asker,
            kind: "answer",
            body,
            thread: Some(&thread),
            refs: &[],
            reply_to: Some(&remote_id),
        },
        Some(Pinned {
            peer_id: &peer_id,
            share_id: share_id.as_deref().unwrap_or(""),
        }),
    )?;
    if matches!(outcome, Some(Outcome::Queued { .. })) {
        conn.execute(
            "UPDATE messages SET acked_at=?2 WHERE id=?1",
            params![question, now_ms()],
        )?;
    }
    Ok(outcome)
}

/// The live row for a peer label: a removed peer's label may have been reused since.
fn peer_by_label(
    conn: &Connection,
    label: &str,
) -> rusqlite::Result<Option<(String, String, i64)>> {
    conn.query_row(
        "SELECT peer_id, state, generation FROM peers WHERE label=?1
         ORDER BY state='removed', paired_at DESC",
        [label],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()
}

/// The row of a peer id, in the shape `peer_by_label` returns.
fn peer_by_id(conn: &Connection, peer_id: &str) -> rusqlite::Result<Option<(String, String, i64)>> {
    conn.query_row(
        "SELECT peer_id, state, generation FROM peers WHERE peer_id=?1",
        [peer_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()
}

fn enqueue(
    conn: &mut Connection,
    request: &Request,
    target: &str,
    pinned: Option<Pinned>,
) -> anyhow::Result<Result<String, &'static str>> {
    if !enabled(conn) {
        return Ok(Err("federation_off"));
    }
    let (label, session) = target.split_once('/').unwrap_or((target, ""));
    let found = match &pinned {
        Some(pin) => peer_by_id(conn, pin.peer_id)?,
        None => peer_by_label(conn, label)?,
    };
    let (peer_id, generation) = match found {
        None => return Ok(Err("unknown_peer")),
        Some((_, state, _)) if state == "removed" => return Ok(Err("peer_removed")),
        Some((_, state, _)) if state == "paused" => return Ok(Err("peer_paused")),
        Some((_, state, _)) if state != "active" => return Ok(Err("unknown_session")),
        Some((peer_id, _, generation)) => (peer_id, generation),
    };
    let tx = store::write_tx(conn)?;
    let share_id: Option<String> = match &pinned {
        Some(pin) => Some(pin.share_id.to_owned()),
        None => tx
            .query_row(
                "SELECT share_id FROM fed_remote_sessions WHERE peer_id=?1 AND session=?2",
                params![peer_id, session],
                |r| r.get(0),
            )
            .optional()?,
    };
    let share = match share_id {
        Some(id) => shares::get(&tx, &id)?,
        None => None,
    };
    let Some(share) = share.filter(|s| s.state == "active" && s.peer_id == peer_id) else {
        return Ok(Err("unknown_session"));
    };
    let member = match &share.local_repo {
        Some(repo) => shares::is_member(&tx, request.from, repo)?,
        None => false,
    };
    if !member {
        return Ok(Err("not_a_member"));
    }
    if !share.outbound {
        return Ok(Err("outbound_off"));
    }
    if !share.remote_inbound {
        return Ok(Err("remote_inbound_off"));
    }
    if !KINDS.contains(&request.kind) {
        return Ok(Err("kind_not_allowed"));
    }
    // §7 names no reason for a malformed field; `too_long` is the closest of its list.
    let thread_fits = request.thread.is_none_or(envelope::thread_ok);
    if !envelope::body_ok(request.body) || !envelope::refs_ok(request.refs) || !thread_fits {
        return Ok(Err("too_long"));
    }
    let now = now_ms();
    let message_id = envelope::uuid_v4()?;
    let envelope = json!({
        "message_id": message_id, "share_id": share.share_id, "revision": share.revision,
        "from_session": discovery::session_for(&tx, request.from, &share.share_id)?,
        "to_session": session, "kind": request.kind, "body": request.body,
        "thread": request.thread.unwrap_or(&message_id), "reply_to": request.reply_to,
        "refs": request.refs, "created_at": now, "expires_at": now + LIFETIME_MS,
    })
    .to_string();
    let bytes = envelope.len() as i64;
    let (rows, peer_bytes): (i64, i64) = tx.query_row(
        "SELECT count(*), coalesce(sum(bytes),0) FROM fed_outbox WHERE peer_id=?1 AND state='queued'",
        [&peer_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let total_bytes: i64 = tx.query_row(
        "SELECT coalesce(sum(bytes),0) FROM fed_outbox WHERE state='queued'",
        [],
        |r| r.get(0),
    )?;
    if rows >= PEER_ROWS || peer_bytes + bytes > PEER_BYTES || total_bytes + bytes > TOTAL_BYTES {
        return Ok(Err("queue_full"));
    }
    tx.execute(
        "INSERT INTO fed_outbox (message_id,peer_id,generation,share_id,revision,from_agent,
           envelope_json,bytes,created_at,expires_at,state)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'queued')",
        params![
            message_id,
            peer_id,
            generation,
            share.share_id,
            share.revision,
            request.from,
            envelope,
            bytes,
            now,
            now + LIFETIME_MS
        ],
    )?;
    tx.commit()?;
    Ok(Ok(message_id))
}

#[cfg(test)]
mod tests;
