//! Keeping the service's view of the peers table current: who may connect, whom to dial,
//! and what to show of each peer, re-read whenever the database changes.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use iroh::EndpointId;
use tokio::task::{AbortHandle, JoinSet};

use super::{locked, Access, Shared, PUSH_EVERY};
use crate::fed::health::PeerHealth;
use crate::fed::{identity, transport};
use crate::store;

struct PeerRow {
    peer_id: String,
    node_id: String,
    label: String,
    generation: i64,
    state: String,
    /// The peer paused us: it refuses our connection on purpose, so we stop dialing it.
    remote_paused: bool,
}

fn load_peers(db_path: &Path) -> anyhow::Result<Vec<PeerRow>> {
    let conn = store::open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT peer_id, node_id, label, generation, state, remote_paused FROM peers
         WHERE state <> 'removed'",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(PeerRow {
            peer_id: r.get(0)?,
            node_id: r.get(1)?,
            label: r.get(2)?,
            generation: r.get(3)?,
            state: r.get(4)?,
            remote_paused: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The dial task of each peer being dialed. Dropping this aborts every one of them, so a
/// stopped service leaves none behind holding it alive.
#[derive(Default)]
struct Dialers {
    tasks: JoinSet<()>,
    running: HashMap<EndpointId, AbortHandle>,
}

impl Dialers {
    fn start(&mut self, shared: &Arc<Shared>, node: EndpointId) {
        self.running
            .entry(node)
            .or_insert_with(|| self.tasks.spawn(transport::dial_loop(shared.clone(), node)));
    }

    fn keep_only(&mut self, shared: &Shared, wanted: impl Fn(&EndpointId) -> bool) {
        self.running.retain(|node, task| {
            let keep = wanted(node);
            if !keep {
                task.abort();
                shared.drop_connection(node);
            }
            keep
        });
    }
}

/// Re-read the peers table: refresh the gate, the health entries, and start or stop the
/// dial task of each peer as it becomes live or stops being so. False when the table could
/// not be read; the gate then admits nobody and every connection is closed, because the
/// old map may still allow a peer the owner just paused or removed.
async fn sync_peers(shared: &Arc<Shared>, dialers: &mut Dialers) -> bool {
    let db_path = shared.db_path.clone();
    let rows = match tokio::task::spawn_blocking(move || load_peers(&db_path)).await {
        Ok(Ok(rows)) => rows,
        Ok(Err(err)) => {
            eprintln!("axon-bus: federation could not read peers, denying all: {err:#}");
            deny_all(shared, dialers);
            return false;
        }
        Err(_) => return true,
    };
    let mut access = HashMap::new();
    let mut paused_by_peer = Vec::new();
    for row in rows {
        let Ok(node) = row.node_id.parse::<EndpointId>() else {
            eprintln!(
                "axon-bus: peer {} has an unreadable node id; ignored",
                row.peer_id
            );
            continue;
        };
        {
            let mut health = locked(&shared.health);
            let entry = health.entry(node).or_insert_with(|| {
                let print = identity::fingerprint(&node);
                PeerHealth::new(row.peer_id.clone(), row.label.clone(), print)
            });
            entry.label.clone_from(&row.label);
            entry.stored_state.clone_from(&row.state);
            if row.remote_paused {
                // An expected silence, not a fault: show it as offline, with no error.
                (
                    entry.last_error,
                    entry.next_retry_at,
                    entry.last_response_at,
                ) = (None, None, None);
                paused_by_peer.push(node);
            }
        }
        access.insert(
            node,
            Access {
                peer_id: row.peer_id,
                generation: row.generation,
                state: row.state,
            },
        );
    }
    locked(&shared.health).retain(|node, _| access.contains_key(node));
    for (node, entry) in &access {
        if entry.is_live() && !paused_by_peer.contains(node) {
            dialers.start(shared, *node);
        }
    }
    dialers.keep_only(shared, |node| {
        access.get(node).is_some_and(Access::is_live) && !paused_by_peer.contains(node)
    });
    // A peer that became active may have been seen while still pairing, when no address is kept:
    // forget what was seen so the next connection records it.
    locked(&shared.observed).clear();
    *shared.access.write().unwrap_or_else(|p| p.into_inner()) = access;
    shared.changed.send_modify(|version| *version += 1);
    true
}

fn deny_all(shared: &Arc<Shared>, dialers: &mut Dialers) {
    *shared.access.write().unwrap_or_else(|p| p.into_inner()) = HashMap::new();
    dialers.keep_only(shared, |_| false);
    let connected: Vec<EndpointId> = locked(&shared.connections).keys().copied().collect();
    for node in connected {
        shared.drop_connection(&node);
    }
}

pub(super) async fn manage(shared: Arc<Shared>) {
    let mut dialers = Dialers::default();
    let mut push = tokio::time::interval(PUSH_EVERY);
    let mut unread = false;
    loop {
        tokio::select! {
            _ = shared.reload.notified() => unread = !sync_peers(&shared, &mut dialers).await,
            _ = push.tick() => {
                // A failed read is retried on the next tick, not only on the next change.
                if unread {
                    unread = !sync_peers(&shared, &mut dialers).await;
                }
                shared.notify_if_live();
            }
        }
    }
}
