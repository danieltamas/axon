//! The database's half of a peer's health (docs/P2P-SPEC.md §10): what waits in the outbox and
//! the lifetime counters. Counters count unique messages, so a retry never adds to one.

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::pairing::PeerRow;
use super::shares::Share;
use super::{discovery, identity, rate};

/// `{count, bytes, oldest_at}` of the messages still queued for `peer_id`.
pub fn queue(conn: &Connection, peer_id: &str) -> rusqlite::Result<Value> {
    conn.query_row(
        "SELECT count(*), coalesce(sum(bytes), 0), min(created_at) FROM fed_outbox
         WHERE peer_id=?1 AND state='queued'",
        [peer_id],
        |r| {
            Ok(json!({
                "count": r.get::<_, i64>(0)?,
                "bytes": r.get::<_, i64>(1)?,
                "oldest_at": r.get::<_, Option<i64>>(2)?,
            }))
        },
    )
}

/// Messages of this pairing by outcome. Inbound outcomes come from the audit under the peer's
/// fingerprint and generation, the identity that outlives a label change.
pub fn counters(conn: &Connection, peer: &PeerRow) -> rusqlite::Result<Value> {
    let fingerprint = peer
        .node_id
        .parse()
        .ok()
        .map(|node| identity::fingerprint(&node));
    conn.query_row(
        "SELECT
           (SELECT count(*) FROM fed_outbox WHERE peer_id=?1 AND state='accepted'),
           (SELECT count(*) FROM fed_inbox WHERE peer_id=?1 AND generation=?3),
           (SELECT count(*) FROM fed_outbox WHERE peer_id=?1 AND state='expired')
             + (SELECT count(DISTINCT message_id) FROM fed_audit WHERE peer_fingerprint IS ?2
                  AND generation=?3 AND direction='in' AND decision='expired'),
           (SELECT count(*) FROM fed_outbox WHERE peer_id=?1 AND state='rejected')
             + (SELECT count(DISTINCT message_id) FROM fed_audit WHERE peer_fingerprint IS ?2
                  AND generation=?3 AND direction='in' AND decision='rejected'),
           (SELECT count(*) FROM fed_outbox WHERE peer_id=?1 AND state='cancelled')",
        params![peer.peer_id, fingerprint, peer.generation],
        |r| {
            Ok(json!({
                "sent_accepted": r.get::<_, i64>(0)?,
                "received": r.get::<_, i64>(1)?,
                "expired": r.get::<_, i64>(2)?,
                "rejected": r.get::<_, i64>(3)? + fingerprint.as_deref().map_or(0, rate::refused) as i64,
                "cancelled": r.get::<_, i64>(4)?,
            }))
        },
    )
}

/// `{bytes_sent, bytes_received}` of this pairing: delivered envelopes out, message bodies in
/// (while they are kept, §12).
pub fn traffic(conn: &Connection, peer: &PeerRow) -> rusqlite::Result<Value> {
    conn.query_row(
        "SELECT
           (SELECT coalesce(sum(bytes), 0) FROM fed_outbox WHERE peer_id=?1 AND state='accepted'),
           (SELECT coalesce(sum(length(CAST(m.body AS BLOB))), 0) FROM fed_inbox i
              JOIN messages m ON m.id=i.local_message_id WHERE i.peer_id=?1 AND i.generation=?2)",
        params![peer.peer_id, peer.generation],
        |r| Ok(json!({"bytes_sent": r.get::<_, i64>(0)?, "bytes_received": r.get::<_, i64>(1)?})),
    )
}

/// Who one share connects: the peer's sessions by harness and availability, as discovery
/// last listed them, and the agents here that belong to the share.
pub fn reach(conn: &Connection, share: &Share) -> rusqlite::Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT label, availability FROM fed_remote_sessions WHERE peer_id=?1 AND share_id=?2",
    )?;
    let theirs: Vec<Value> = stmt
        .query_map(params![share.peer_id, share.share_id], |r| {
            let label: String = r.get(0)?;
            // The label is `<harness>-<4 chars>` (§7).
            let harness = label
                .rsplit_once('-')
                .map_or(label.as_str(), |(h, _)| h)
                .to_owned();
            Ok(json!({"harness": harness, "availability": r.get::<_, String>(1)?}))
        })?
        .collect::<Result<_, _>>()?;
    let mine: Vec<Value> = if share.state == "active" {
        discovery::members(conn, share)?
            .into_iter()
            .map(|(_, harness, status)| json!({"harness": harness, "availability": status}))
            .collect()
    } else {
        Vec::new()
    };
    Ok(json!({"theirs": theirs, "mine": mine}))
}
