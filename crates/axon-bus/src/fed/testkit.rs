//! Shared set-up of the federation unit tests: a database with two active peers, real git
//! repositories, and registered agents.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rusqlite::{params, Connection};

use crate::store;

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub conn: Connection,
}

/// The node id of the peer made from `seed`.
pub fn node(seed: u8) -> String {
    iroh::SecretKey::from_bytes(&[seed; 32])
        .public()
        .to_string()
}

/// A database with active peers `p1` (seed 1) and `p2` (seed 2), both at generation 7, and
/// federation switched on. Foreign keys are off, as in the receiver: a remote principal is
/// no `agents` row.
pub fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let conn = store::init(&dir.path().join("axon.db")).unwrap();
    store::put_setting(&conn, "fed_enabled", Some("1")).unwrap();
    for (id, seed) in [("p1", 1u8), ("p2", 2)] {
        conn.execute(
            "INSERT INTO peers (peer_id,node_id,label,generation,state,paired_at)
             VALUES (?1,?2,?1,7,'active',1)",
            params![id, node(seed)],
        )
        .unwrap();
    }
    Fixture { dir, conn }
}

/// A git repository `name` under `parent`, with one commit; its canonical path.
pub fn repo(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    std::fs::create_dir(&path).unwrap();
    for args in [
        &["init", "--initial-branch=main"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--allow-empty",
            "-m",
            "x",
        ],
    ] {
        let status = Command::new("git")
            .args(args)
            .current_dir(&path)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    }
    // Spelled as the bus keys repos: on Windows without the `\\?\` prefix.
    crate::snapshot::resolve(&path).unwrap()
}

/// Register `agent` working in `cwd`.
pub fn agent(conn: &Connection, id: &str, cwd: &Path, status: &str) {
    conn.execute(
        "INSERT INTO agents (id,harness,session_id,root_id,cwd,status,started_at,last_seen_at)
         VALUES (?1,'claude',?1,?1,?2,?3,1,1)",
        params![id, cwd.to_str().unwrap(), status],
    )
    .unwrap();
}

/// An `active` share of `peer` on `local_repo`, open in every direction.
pub fn share(conn: &Connection, id: &str, peer: &str, local_repo: &Path, revision: i64) {
    conn.execute(
        "INSERT INTO peer_shares (share_id,peer_id,label,local_repo,inbound,outbound,
           remote_inbound,remote_outbound,revision,state)
         VALUES (?1,?2,'project',?3,1,1,1,1,?4,'active')",
        params![id, peer, local_repo.to_str().unwrap(), revision],
    )
    .unwrap();
}
