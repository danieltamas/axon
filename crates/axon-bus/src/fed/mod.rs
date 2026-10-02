//! Federation between Axon installs (docs/P2P-SPEC.md): identity, the transport service,
//! and its health. Off unless the owner turns it on; nothing here runs otherwise.

pub mod api;
mod audit;
pub mod codec;
pub mod delivery;
pub mod discovery;
mod envelope;
mod health;
pub mod identity;
mod invite;
mod lifecycle;
mod lock;
mod outbox;
pub mod pairing;
mod rate;
mod receive;
mod reconcile;
pub mod remote;
mod retention;
pub mod service;
pub mod shares;
mod stats;
#[cfg(test)]
mod testkit;
mod transport;

use std::path::Path;

use anyhow::Context;
use rusqlite::Connection;

use crate::store::setting;

/// ALPN of the one-time pairing exchange.
pub const PAIR_ALPN: &[u8] = b"axon/pair/1";
/// ALPN of everything after pairing.
pub const FED_ALPN: &[u8] = b"axon/fed/1";

const SETTING_ENABLED: &str = "fed_enabled";
const SETTING_RELAY: &str = "fed_relay";

/// A connection for a handler whose commit precedes a wire ack: the peer's acknowledgement is
/// a promise, so the change must survive power loss (`synchronous=FULL`, unlike the hub's NORMAL).
pub fn open_durable(db: &std::path::Path) -> anyhow::Result<rusqlite::Connection> {
    let conn = crate::store::open(db)?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    Ok(conn)
}

/// Federation is on only when the owner switched it on; absent or unreadable means off.
pub fn enabled(conn: &Connection) -> bool {
    matches!(setting(conn, SETTING_ENABLED), Ok(Some(v)) if v == "1" || v == "true")
}

/// `"default"` or a relay URL, as stored by Settings.
pub fn relay_setting(conn: &Connection) -> String {
    setting(conn, SETTING_RELAY)
        .ok()
        .flatten()
        .unwrap_or_else(|| "default".to_owned())
}

/// Switch federation on or off. Turning it on creates the identity key the first time; it
/// refuses when a key that peers depend on is gone (identity::load_or_create).
pub fn enable(data_dir: &Path, conn: &Connection, on: bool) -> anyhow::Result<()> {
    if on {
        identity::load_or_create(data_dir, live_peer_count(conn)? > 0)?;
    }
    conn.execute(
        "INSERT INTO settings (key,value) VALUES (?1,?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        rusqlite::params![SETTING_ENABLED, if on { "1" } else { "0" }],
    )
    .context("store the federation switch")?;
    Ok(())
}

/// Store the relay Settings chose: `"default"` or a relay URL. A running service applies it
/// on its next start.
pub fn set_relay(conn: &Connection, relay: &str) -> rusqlite::Result<()> {
    crate::store::put_setting(conn, SETTING_RELAY, Some(relay))
}

/// Peers that are not removed: the ones whose pinned key must still exist.
pub fn live_peer_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT count(*) FROM peers WHERE state<>'removed'",
        [],
        |r| r.get(0),
    )
}

/// The clock federation uses, in ms since the epoch. Debug builds honour
/// `AXON_TEST_NOW_OFFSET_MS` so tests can move time without waiting.
pub fn now_ms() -> i64 {
    crate::store::now_ms()
        + seam("AXON_TEST_NOW_OFFSET_MS")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
}

/// Wire a started service to everything that speaks on `axon/fed/1`: pairing, shares,
/// discovery, and the delivery of messages both ways.
pub fn install(handle: &service::Handle, db: &Path) {
    pairing::install(handle, db);
    shares::install(handle, db);
    discovery::install(handle, db);
    receive::install(handle, db);
    outbox::install(handle, db);
    retention::install(handle, db);
}

/// 16 random bytes as 32 hex characters: the id of an invite or a peer row.
fn random_id() -> anyhow::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow::anyhow!("no OS randomness: {e}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// A test seam (docs/P2P-SPEC.md §0): read only in debug builds, ignored in release.
pub(crate) fn seam(name: &str) -> Option<String> {
    if cfg!(debug_assertions) {
        std::env::var(name).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_by_default_and_the_key_appears_only_when_turned_on() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("axon.db");
        let conn = crate::store::init(&db).unwrap();
        assert!(!enabled(&conn));
        assert_eq!(relay_setting(&conn), "default");
        assert!(
            !identity::fed_dir(dir.path()).exists(),
            "off writes nothing"
        );

        enable(dir.path(), &conn, true).unwrap();
        assert!(enabled(&conn));
        let key = identity::fed_dir(dir.path()).join(identity::KEY_FILE);
        assert!(key.exists());

        enable(dir.path(), &conn, false).unwrap();
        assert!(!enabled(&conn));
        assert!(key.exists(), "turning it off keeps the identity");
    }

    #[test]
    fn enabling_refuses_when_the_key_peers_depend_on_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::store::init(&dir.path().join("axon.db")).unwrap();
        conn.execute(
            "INSERT INTO peers (peer_id,node_id,label,generation,state,paired_at)
             VALUES ('p','n','alice',1,'active',1)",
            [],
        )
        .unwrap();
        assert!(enable(dir.path(), &conn, true).is_err());
        assert!(!enabled(&conn));
    }

    #[test]
    fn a_connection_that_precedes_an_ack_syncs_every_commit() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("axon.db");
        drop(crate::store::init(&db).unwrap());
        let sync = |conn: &rusqlite::Connection| -> i64 {
            conn.query_row("PRAGMA synchronous", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(sync(&crate::store::open(&db).unwrap()), 1, "NORMAL");
        assert_eq!(sync(&open_durable(&db).unwrap()), 2, "FULL");
    }
}
