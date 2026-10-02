//! Pairing (docs/P2P-SPEC.md §4): admitting a joiner on `axon/pair/1`, the two-sided
//! pair-code confirmation on `axon/fed/1`, and the background work that finishes it or
//! times it out. A pending peer holds no authority: it has no shares.

mod admit;
mod join;
mod wire;

use std::time::Duration;

use iroh::EndpointId;
use rusqlite::{params, Connection, OptionalExtension};

use super::{identity, now_ms, random_id};
use crate::store;

pub use join::{join, JoinError};
pub use wire::{install, notify, notify_removed, notify_resumed, send_confirmed};

/// A pairing not confirmed on both sides within this long is removed.
pub const CONFIRM_WINDOW_MS: i64 = 600_000;
pub(super) const RETRY_EVERY: Duration = Duration::from_secs(1);
pub(super) const SWEEP_EVERY: Duration = Duration::from_secs(5);
/// How long a `removed` notice keeps trying while the connection comes up.
pub(super) const NOTICE_TRIES: u32 = 30;
/// Generations are small counters; a huge proposal is a hostile one.
pub(super) const MAX_GENERATION: i64 = 1 << 40;

/// Letters, digits, `.`, `_`, `-`: 1 to 32 of them.
pub fn label_ok(label: &str) -> bool {
    (1..=32).contains(&label.len())
        && label
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// One `peers` row, as pairing and `GET /api/fed` need it.
#[derive(Clone)]
pub struct PeerRow {
    pub peer_id: String,
    pub node_id: String,
    pub label: String,
    pub generation: i64,
    pub state: String,
    pub last_error: Option<String>,
    pub removed_reason: Option<String>,
    pub remote_paused: bool,
}

pub fn all_peers(conn: &Connection) -> rusqlite::Result<Vec<PeerRow>> {
    let mut stmt = conn.prepare(
        "SELECT peer_id, node_id, label, generation, state, last_error, removed_reason, remote_paused
         FROM peers
         ORDER BY paired_at, peer_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(PeerRow {
            peer_id: r.get(0)?,
            node_id: r.get(1)?,
            label: r.get(2)?,
            generation: r.get(3)?,
            state: r.get(4)?,
            last_error: r.get(5)?,
            removed_reason: r.get(6)?,
            remote_paused: r.get(7)?,
        })
    })?;
    rows.collect()
}

/// The pair code both screens show, once both node ids are known.
pub fn pair_code(own: &EndpointId, node_id: &str) -> Option<String> {
    Some(identity::pair_code(own, &node_id.parse().ok()?))
}

pub(super) fn history_generation(conn: &Connection, node: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(generation), 0) FROM peers WHERE node_id=?1",
        [node],
        |r| r.get(0),
    )
}

pub(super) fn insert_pending(
    conn: &Connection,
    node: &str,
    label: &str,
    generation: i64,
) -> anyhow::Result<usize> {
    // OR IGNORE: the live-node index refuses a second pairing of the same node, which is
    // how two simultaneous retries of one join collapse into one row. The live-label index
    // refuses a label in use too; the caller finds out by not seeing its row.
    Ok(conn.execute(
        "INSERT OR IGNORE INTO peers (peer_id, node_id, label, generation, state, paired_at)
         VALUES (?1, ?2, ?3, ?4, 'pending_confirm', ?5)",
        params![random_id()?, node, label, generation, now_ms()],
    )?)
}

pub(super) fn pending_by_node(conn: &Connection, node: &str) -> rusqlite::Result<Option<PeerRow>> {
    Ok(all_peers(conn)?
        .into_iter()
        .find(|p| p.node_id == node && p.state == "pending_confirm"))
}

/// Remove peers whose confirmation window has closed; how many went.
pub(super) fn expire_stale(conn: &Connection, now: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE peers SET state='removed', removed_at=?1, removed_reason='confirm_timeout'
         WHERE state='pending_confirm' AND paired_at <= ?1 - ?2",
        params![now, CONFIRM_WINDOW_MS],
    )
}

/// Remove a peer that is still pending: `(node_id, generation)` of the one removed.
fn remove_pending(
    conn: &Connection,
    peer_id: &str,
    reason: &str,
) -> rusqlite::Result<Option<(String, i64)>> {
    let target: Option<(String, i64)> = conn
        .query_row(
            "SELECT node_id, generation FROM peers WHERE peer_id=?1 AND state='pending_confirm'",
            [peer_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if target.is_some() {
        conn.execute(
            "UPDATE peers SET state='removed', removed_at=?1, removed_reason=?2 WHERE peer_id=?3",
            params![now_ms(), reason, peer_id],
        )?;
    }
    Ok(target)
}

// ---- confirmation ----------------------------------------------------------------------

pub enum Confirm {
    UnknownPeer,
    NotPending,
    /// Wrong code: the peer is removed; tell the other side.
    Mismatch {
        node_id: String,
        generation: i64,
    },
    /// Our side is confirmed; `active` once the other side's frame has also arrived.
    Confirmed,
}

fn digits(code: &str) -> String {
    code.chars().filter(char::is_ascii_digit).collect()
}

pub fn confirm(
    conn: &mut Connection,
    own: &EndpointId,
    peer_id: &str,
    pair_code: &str,
) -> anyhow::Result<Confirm> {
    let now = now_ms();
    let tx = store::write_tx(conn)?;
    expire_stale(&tx, now)?;
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT node_id, state FROM peers WHERE peer_id=?1",
            [peer_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let outcome = match row {
        None => Confirm::UnknownPeer,
        Some((_, state)) if state != "pending_confirm" => Confirm::NotPending,
        Some((node_id, _)) => {
            let expected = self::pair_code(own, &node_id).map(|code| digits(&code));
            if expected.is_some_and(|code| code == digits(pair_code)) {
                tx.execute(
                    "UPDATE peers SET local_confirmed_at=COALESCE(local_confirmed_at, ?1),
                     state=CASE WHEN remote_confirmed_at IS NOT NULL THEN 'active' ELSE state END
                     WHERE peer_id=?2",
                    params![now, peer_id],
                )?;
                Confirm::Confirmed
            } else {
                let (node_id, generation) = remove_pending(&tx, peer_id, "pair_code_mismatch")?
                    .ok_or_else(|| anyhow::anyhow!("pending peer vanished inside a transaction"))?;
                Confirm::Mismatch {
                    node_id,
                    generation,
                }
            }
        }
    };
    tx.commit()?;
    Ok(outcome)
}

/// The owner rejected the fingerprint: remove the pending peer. `None` when it is not pending.
pub fn reject(conn: &Connection, peer_id: &str) -> rusqlite::Result<Option<(String, i64)>> {
    remove_pending(conn, peer_id, "rejected")
}
