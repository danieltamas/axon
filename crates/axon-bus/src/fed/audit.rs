//! The federation audit (docs/P2P-SPEC.md §5): every decision about a peer's message is a
//! `fed_audit` row and, in the same transaction, an `events` row chained like all others. Neither
//! ever holds a key, a secret or a message body.

use rusqlite::{params, Connection};
use serde_json::json;

use super::{now_ms, rate};
use crate::store;

pub struct Decision<'a> {
    pub peer_fingerprint: Option<&'a str>,
    pub generation: Option<i64>,
    pub share_id: Option<&'a str>,
    pub message_id: Option<&'a str>,
    pub direction: &'a str,
    /// `accepted`, `duplicate`, `rejected` or `expired`.
    pub decision: &'a str,
    pub reason: Option<&'a str>,
}

/// Record `decision` in the caller's transaction. A rejection past the per-minute budget of
/// its peer is dropped, so a peer that floods us cannot also flood the audit. Only inbound rejections spend it:
/// our own outbound rejections are bounded by the queue and must never lock a peer out.
pub fn record(conn: &Connection, d: &Decision) -> anyhow::Result<()> {
    let ts = now_ms();
    if d.decision == "rejected"
        && d.direction == "in"
        && !rate::may_audit_rejection(conn.path(), d.peer_fingerprint)
    {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO fed_audit (ts,peer_fingerprint,generation,share_id,message_id,direction,decision,reason)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            ts,
            d.peer_fingerprint,
            d.generation,
            d.share_id,
            d.message_id,
            d.direction,
            d.decision,
            d.reason
        ],
    )?;
    let row = json!({
        "seq": conn.last_insert_rowid(), "ts": ts, "peer_fingerprint": d.peer_fingerprint,
        "generation": d.generation, "share_id": d.share_id, "message_id": d.message_id,
        "direction": d.direction, "decision": d.decision, "reason": d.reason,
    });
    store::append_event(
        conn,
        "fed",
        d.decision,
        d.message_id.or(d.share_id).unwrap_or("-"),
        &row.to_string(),
    )
}
