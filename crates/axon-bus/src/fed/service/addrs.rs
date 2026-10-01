//! `peer_addrs` (docs/P2P-SPEC.md §5): the direct addresses an authenticated connection from
//! a peer actually arrived on or went out over, kept so peers re-find each other after a
//! restart without a relay. Never an address the peer claims about itself.

use std::net::SocketAddr;
use std::path::Path;

use iroh::endpoint::Connection;
use iroh::{EndpointAddr, EndpointId, TransportAddr};
use rusqlite::{params, Connection as Db};

use super::Shared;
use crate::fed::now_ms;
use crate::store;

/// The direct addresses of the paths `conn` is using.
fn observed(conn: &Connection) -> Vec<SocketAddr> {
    let mut addrs: Vec<SocketAddr> = conn
        .paths()
        .iter()
        .filter_map(|path| match path.remote_addr() {
            TransportAddr::Ip(addr) => Some(*addr),
            _ => None,
        })
        .collect();
    addrs.sort();
    addrs.dedup();
    addrs
}

/// Record the addresses of an active or paused peer; a peer that is gone or still pairing
/// gets no row.
fn save(db_path: &Path, node: &str, addrs: &[SocketAddr]) -> anyhow::Result<()> {
    let json = serde_json::to_string(&addrs.iter().map(ToString::to_string).collect::<Vec<_>>())?;
    store::open(db_path)?.execute(
        "INSERT INTO peer_addrs (peer_id, addrs_json, seen_at)
         SELECT peer_id, ?2, ?3 FROM peers WHERE node_id=?1 AND state IN ('active','paused')
         ON CONFLICT(peer_id) DO UPDATE SET addrs_json=excluded.addrs_json, seen_at=excluded.seen_at",
        params![node, json, now_ms()],
    )?;
    Ok(())
}

/// What was recorded for active and paused peers, ready for the endpoint's address book.
pub(super) fn load(conn: &Db) -> rusqlite::Result<Vec<EndpointAddr>> {
    let mut stmt = conn.prepare(
        "SELECT p.node_id, a.addrs_json FROM peer_addrs a JOIN peers p USING (peer_id)
         WHERE p.state IN ('active','paused')",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut found = Vec::new();
    for row in rows {
        let (node, json) = row?;
        let Ok(node) = node.parse::<EndpointId>() else {
            continue;
        };
        let addrs: Vec<SocketAddr> = serde_json::from_str::<Vec<String>>(&json)
            .unwrap_or_default()
            .iter()
            .filter_map(|a| a.parse().ok())
            .collect();
        if !addrs.is_empty() {
            found.push(EndpointAddr::from_parts(
                node,
                addrs.into_iter().map(TransportAddr::Ip),
            ));
        }
    }
    Ok(found)
}

impl Shared {
    /// Remember where an authenticated connection to `node` runs, when that changed.
    pub(in crate::fed) fn note_paths(&self, node: &EndpointId, conn: &Connection) {
        let addrs = observed(conn);
        if addrs.is_empty() {
            return;
        }
        {
            let mut seen = super::locked(&self.observed);
            if seen.get(node) == Some(&addrs) {
                return;
            }
            seen.insert(*node, addrs.clone());
        }
        let (db_path, node) = (self.db_path.clone(), node.to_string());
        tokio::task::spawn_blocking(move || {
            if let Err(err) = save(&db_path, &node, &addrs) {
                eprintln!("axon-bus: could not record a peer address: {err:#}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(conn: &Db, id: &str, state: &str) -> EndpointId {
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).unwrap();
        let node = iroh::SecretKey::from_bytes(&seed).public();
        conn.execute(
            "INSERT INTO peers (peer_id,node_id,label,generation,state,paired_at) VALUES (?1,?2,?1,1,?3,1)",
            params![id, node.to_string(), state],
        )
        .unwrap();
        node
    }

    #[test]
    fn addresses_are_kept_for_live_peers_only_and_deleted_when_the_peer_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("axon.db");
        let conn = store::init(&db).unwrap();
        let live = peer(&conn, "live", "active");
        let pending = peer(&conn, "pending", "pending_confirm");
        let addrs: Vec<SocketAddr> = vec!["127.0.0.1:4000".parse().unwrap()];
        save(&db, &live.to_string(), &addrs).unwrap();
        save(&db, &pending.to_string(), &addrs).unwrap();

        let loaded = load(&conn).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, live);
        assert_eq!(loaded[0].ip_addrs().copied().collect::<Vec<_>>(), addrs);

        conn.execute("UPDATE peers SET state='removed' WHERE peer_id='live'", [])
            .unwrap();
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM peer_addrs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "removal deletes the row");
        assert!(load(&conn).unwrap().is_empty());
    }
}
