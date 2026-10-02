//! The joiner's side of `axon/pair/1`: dial the inviter and record the pending pairing.

use std::path::Path;

use serde_json::json;

use super::{
    all_peers, history_generation, insert_pending, pending_by_node, PeerRow, MAX_GENERATION,
};
use crate::fed::invite::Joinable;
use crate::fed::service::Handle;
use crate::store;

pub enum JoinError {
    /// The blob names this very node.
    OwnInvite,
    LabelTaken,
    AlreadyPaired,
    /// The inviter refused: wrong, expired, consumed, cancelled or exhausted.
    Refused,
    /// The inviter could not be reached, or its key is not the one the invite pins.
    Unreachable,
    Unavailable,
}

/// Whatever fails inside (the database, the runtime) reads as `Unavailable`.
impl<E: Into<anyhow::Error>> From<E> for JoinError {
    fn from(_: E) -> Self {
        Self::Unavailable
    }
}

enum Local {
    /// A pairing with this node is already pending: a retry.
    Pending(PeerRow),
    Fresh {
        proposed_generation: i64,
    },
}

fn check_local(db: &Path, node: &str, label: &str) -> Result<Local, JoinError> {
    let conn = store::open(db)?;
    let peers = all_peers(&conn)?;
    if let Some(pending) = peers
        .iter()
        .find(|p| p.node_id == node && p.state == "pending_confirm")
    {
        return Ok(Local::Pending(pending.clone()));
    }
    let mut live = peers.iter().filter(|p| p.state != "removed");
    if live.clone().any(|p| p.node_id == node) {
        return Err(JoinError::AlreadyPaired);
    }
    if live.any(|p| p.label == label) {
        return Err(JoinError::LabelTaken);
    }
    Ok(Local::Fresh {
        proposed_generation: history_generation(&conn, node)? + 1,
    })
}

/// Store the pending row for an admitted pairing; the row that now exists for the node is
/// the answer, whether this call or a simultaneous retry wrote it.
fn record_joined(
    db: &Path,
    node: &str,
    label: &str,
    generation: i64,
) -> Result<PeerRow, JoinError> {
    let mut conn = store::open(db)?;
    let tx = store::write_tx(&mut conn)?;
    insert_pending(&tx, node, label, generation)?;
    let row = pending_by_node(&tx, node)?;
    let label_taken = row.is_none()
        && all_peers(&tx)?
            .iter()
            .any(|p| p.label == label && p.state != "removed");
    tx.commit()?;
    match row {
        Some(row) => Ok(row),
        None if label_taken => Err(JoinError::LabelTaken),
        None => Err(JoinError::Unavailable),
    }
}

/// Dial the inviter, prove the secret, and record the pending pairing. iroh authenticates
/// the inviter's key against the invite's pin before the secret is sent, so a wrong key
/// ends the dial with nothing disclosed.
pub async fn join(
    handle: &Handle,
    db: &Path,
    invite: Joinable,
    label: String,
) -> Result<PeerRow, JoinError> {
    let node = invite.addr.id;
    if node == handle.addr().id {
        return Err(JoinError::OwnInvite);
    }
    let (local_db, node_text, local_label) = (db.to_owned(), node.to_string(), label.clone());
    let local =
        tokio::task::spawn_blocking(move || check_local(&local_db, &node_text, &local_label))
            .await??;
    let proposed_generation = match local {
        Local::Pending(peer) => return Ok(peer),
        Local::Fresh {
            proposed_generation,
        } => proposed_generation,
    };
    let request = json!({
        "v": 1, "invite_id": invite.invite_id, "secret": invite.secret,
        "label": label, "generation": proposed_generation,
    });
    let reply = handle
        .pair_request(invite.addr.clone(), &request)
        .await
        .map_err(|err| {
            eprintln!("axon-bus: pairing could not reach the inviter: {err:#}");
            JoinError::Unreachable
        })?;
    let generation = match (reply["ok"].as_bool(), reply["error"].as_str()) {
        (Some(true), _) => reply["generation"]
            .as_i64()
            .filter(|g| (proposed_generation..=MAX_GENERATION).contains(g))
            .ok_or(JoinError::Unavailable)?,
        (_, Some("invalid_invite")) => return Err(JoinError::Refused),
        (_, Some("label_taken")) => return Err(JoinError::LabelTaken),
        _ => return Err(JoinError::Unavailable),
    };
    let (db, node_text) = (db.to_owned(), node.to_string());
    let peer =
        tokio::task::spawn_blocking(move || record_joined(&db, &node_text, &label, generation))
            .await??;
    handle.add_addr(invite.addr);
    Ok(peer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fed::testkit::{fixture, node};

    #[test]
    fn a_second_pairing_under_a_live_label_fails_as_label_taken() {
        let fx = fixture();
        let db = fx.dir.path().join("axon.db");
        assert!(record_joined(&db, &node(5), "shared-label", 1).is_ok());
        assert!(matches!(
            record_joined(&db, &node(6), "shared-label", 1),
            Err(JoinError::LabelTaken)
        ));
        assert!(
            record_joined(&db, &node(5), "shared-label", 1).is_ok(),
            "a retry of the same join still collapses into its row"
        );
    }
}
