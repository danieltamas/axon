//! How long federation keeps what it wrote (docs/P2P-SPEC.md §12): nothing a peer or a
//! stale session leaves behind grows without bound. A sweep runs at start and then hourly.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection};
use tokio::time::sleep;

use super::service::Handle;
use super::{now_ms, shares};
use crate::store;

const SWEEP_EVERY: Duration = Duration::from_secs(3600);
const DAY_MS: i64 = 24 * 3600 * 1000;
/// Delivered, ended and rejected messages (both directions) stay this long. The health
/// counters read them, so this is also how far back they count.
const MESSAGES_KEPT_MS: i64 = 90 * DAY_MS;
const AUDIT_KEPT_MS: i64 = 365 * DAY_MS;
/// An invite is useless once expired; its row stays a week for the owner to see why.
const INVITES_KEPT_MS: i64 = 7 * DAY_MS;

pub fn install(handle: &Handle, db: &Path) {
    tokio::spawn(run(handle.clone(), db.to_owned()));
}

async fn run(handle: Handle, db: PathBuf) {
    while !handle.stopped() {
        let db = db.clone();
        let swept = tokio::task::spawn_blocking(move || sweep(&store::open(&db)?, now_ms())).await;
        if let Ok(Err(err)) = swept {
            eprintln!("axon-bus: federation retention failed: {err:#}");
        }
        sleep(SWEEP_EVERY).await;
    }
}

/// Delete what is past its retention, in one transaction. A removed share's row is a
/// tombstone that keeps stale frames from reviving it; only the newest `TOMBSTONES_KEPT` per
/// peer stay, and a peer holding `MAX_TOMBSTONES` cannot offer again until a sweep trims them.
pub fn sweep(conn: &Connection, now: i64) -> anyhow::Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let swept = sweep_in(conn, now);
    match swept {
        Ok(()) => Ok(conn.execute_batch("COMMIT")?),
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(err)
        }
    }
}

fn sweep_in(conn: &Connection, now: i64) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM peer_shares WHERE rowid IN (
           SELECT rowid FROM (SELECT rowid, row_number() OVER
                                (PARTITION BY peer_id ORDER BY rowid DESC) AS newest
                              FROM peer_shares WHERE state='removed')
           WHERE newest > ?1)",
        [shares::TOMBSTONES_KEPT],
    )?;
    conn.execute(
        "DELETE FROM fed_sessions WHERE share_id NOT IN (SELECT share_id FROM peer_shares)",
        [],
    )?;
    conn.execute(
        "DELETE FROM fed_outbox WHERE state<>'queued' AND created_at < ?1",
        [now - MESSAGES_KEPT_MS],
    )?;
    conn.execute(
        "DELETE FROM fed_inbox WHERE accepted_at < ?1",
        [now - MESSAGES_KEPT_MS],
    )?;
    conn.execute("DELETE FROM fed_audit WHERE ts < ?1", [now - AUDIT_KEPT_MS])?;
    conn.execute(
        "DELETE FROM peer_invites WHERE expires_at < ?1",
        params![now - INVITES_KEPT_MS],
    )?;
    conn.execute(
        "DELETE FROM dashboard_sessions WHERE expires_at <= ?1",
        [now],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
