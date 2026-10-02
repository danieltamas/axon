//! What the owner can do to a paired peer (docs/P2P-SPEC.md §9): pause, resume, remove and
//! relabel. Each is one transaction; telling the other side and the connections is the
//! caller's job, after the commit, and never waits for a peer that is away.

use iroh::EndpointId;
use rusqlite::{params, Connection, OptionalExtension};

use super::audit::{self, Decision};
use super::{identity, now_ms, shares};
use crate::store;

/// The peer a change applied to, as the notice to its other side needs it.
pub struct Peer {
    pub node_id: String,
    pub generation: i64,
    /// The pairing's lifecycle sequence after this change (see `reconcile`).
    pub seq: i64,
}

pub enum Change {
    /// No peer has this id, or it is already removed.
    Unknown,
    /// The peer is not in a state this change applies to.
    WrongState,
    /// A live peer already has this label.
    LabelTaken,
    Done(Peer),
}

fn find(conn: &Connection, peer_id: &str) -> rusqlite::Result<Option<(Peer, String)>> {
    conn.query_row(
        "SELECT node_id, generation, state, lifecycle_seq FROM peers WHERE peer_id=?1",
        [peer_id],
        |r| {
            Ok((
                Peer {
                    node_id: r.get(0)?,
                    generation: r.get(1)?,
                    seq: r.get(3)?,
                },
                r.get(2)?,
            ))
        },
    )
    .optional()
}

/// Record an owner decision about `peer` under its fingerprint, the identity that outlives a
/// label change.
fn audit_decision(conn: &Connection, peer: &Peer, decision: &str) -> anyhow::Result<()> {
    let fingerprint = peer
        .node_id
        .parse::<EndpointId>()
        .map(|node| identity::fingerprint(&node))
        .ok();
    audit::record(
        conn,
        &Decision {
            peer_fingerprint: fingerprint.as_deref(),
            generation: Some(peer.generation),
            share_id: None,
            message_id: None,
            direction: "local",
            decision,
            reason: None,
        },
    )
}

/// Move a peer between two stored states.
fn transition(
    conn: &mut Connection,
    peer_id: &str,
    from: &str,
    to: &str,
    decision: &str,
) -> anyhow::Result<Change> {
    let tx = store::write_tx(conn)?;
    let change = match find(&tx, peer_id)? {
        None => Change::Unknown,
        Some((_, state)) if state == "removed" => Change::Unknown,
        Some((_, state)) if state != from => Change::WrongState,
        Some((peer, _)) => {
            tx.execute(
                "UPDATE peers SET state=?2, lifecycle_seq=lifecycle_seq+1 WHERE peer_id=?1",
                params![peer_id, to],
            )?;
            audit_decision(&tx, &peer, decision)?;
            Change::Done(Peer {
                seq: peer.seq + 1,
                ..peer
            })
        }
    };
    tx.commit()?;
    Ok(change)
}

/// `active` to `paused`: nothing is sent, delivered or accepted until it is resumed.
pub fn pause(conn: &mut Connection, peer_id: &str) -> anyhow::Result<Change> {
    transition(conn, peer_id, "active", "paused", "paused")
}

/// `paused` to `active`.
pub fn resume(conn: &mut Connection, peer_id: &str) -> anyhow::Result<Change> {
    transition(conn, peer_id, "paused", "active", "resumed")
}

/// Remove a peer and everything it was granted, in one transaction: its shares end, queued
/// messages to it are cancelled, messages from it not yet delivered are deleted, and what it
/// told us about its sessions is forgotten. Delivered messages and the audit stay, keyed by
/// the peer's immutable identity. Removal is local authority: the peer is not asked.
pub fn remove(conn: &mut Connection, peer_id: &str) -> anyhow::Result<Change> {
    // "Forget" on a pairing the peer already ended: nothing is left to undo, the owner only
    // acknowledges it, and the page stops listing it.
    let forgotten = conn.execute(
        "UPDATE peers SET removed_reason='remote_removed_forgotten'
         WHERE peer_id=?1 AND state='removed' AND removed_reason='remote_removed'",
        [peer_id],
    )?;
    if forgotten > 0 {
        return Ok(find(conn, peer_id)?.map_or(Change::Unknown, |(peer, _)| Change::Done(peer)));
    }
    remove_because(conn, peer_id, "removed")
}

/// `remove`, recording why: `removed` when the owner did it, `remote_removed` when the peer's
/// notice did. The effects are the same, because a peer that ended the pairing is gone either way.
pub fn remove_because(
    conn: &mut Connection,
    peer_id: &str,
    reason: &str,
) -> anyhow::Result<Change> {
    let tx = store::write_tx(conn)?;
    let change = match find(&tx, peer_id)? {
        None => Change::Unknown,
        Some((_, state)) if state == "removed" => Change::Unknown,
        Some((peer, _)) => {
            tx.execute(
                "UPDATE peers SET state='removed', removed_at=?2, removed_reason=?3,
                 lifecycle_seq=lifecycle_seq+1 WHERE peer_id=?1",
                params![peer_id, now_ms(), reason],
            )?;
            for share in shares::of_peer(&tx, peer_id)? {
                shares::remove(&tx, &share.share_id)
                    .map_err(|err| anyhow::anyhow!("share of a removed peer: {err:?}"))?;
            }
            tx.execute(
                "UPDATE fed_outbox SET state='cancelled', last_error='peer_removed'
                 WHERE peer_id=?1 AND state='queued'",
                [peer_id],
            )?;
            tx.execute(
                "DELETE FROM messages WHERE delivered_at IS NULL
                   AND id IN (SELECT local_message_id FROM fed_inbox WHERE peer_id=?1)",
                [peer_id],
            )?;
            tx.execute(
                "DELETE FROM fed_remote_sessions WHERE peer_id=?1",
                [peer_id],
            )?;
            audit_decision(
                &tx,
                &peer,
                if reason == "removed" {
                    "removed"
                } else {
                    reason
                },
            )?;
            Change::Done(peer)
        }
    };
    tx.commit()?;
    Ok(change)
}

/// Change the display label. Identity, routing and audit never read it.
pub fn relabel(conn: &mut Connection, peer_id: &str, label: &str) -> anyhow::Result<Change> {
    let tx = store::write_tx(conn)?;
    let taken: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM peers WHERE label=?1 AND state<>'removed' AND peer_id<>?2)",
        params![label, peer_id],
        |r| r.get(0),
    )?;
    let change = match find(&tx, peer_id)? {
        None => Change::Unknown,
        Some((_, state)) if state == "removed" => Change::Unknown,
        Some(_) if taken => Change::LabelTaken,
        Some((peer, _)) => {
            tx.execute(
                "UPDATE peers SET label=?2 WHERE peer_id=?1",
                params![peer_id, label],
            )?;
            Change::Done(peer)
        }
    };
    tx.commit()?;
    Ok(change)
}

#[cfg(test)]
mod tests;
