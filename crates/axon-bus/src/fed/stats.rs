//! The database's half of a peer's health (docs/P2P-SPEC.md §10): what waits in the outbox and
//! the lifetime counters. Counters count unique messages, so a retry never adds to one.

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::identity;
use super::pairing::PeerRow;

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
                "rejected": r.get::<_, i64>(3)?,
                "cancelled": r.get::<_, i64>(4)?,
            }))
        },
    )
}
