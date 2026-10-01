//! Shares (docs/P2P-SPEC.md §6): a project one owner maps to a peer, with a direction flag
//! each way. The peer never learns the local repo; it only sees the share id, the label and
//! the flags. Every change bumps `revision`, which also invalidates anything queued under
//! the older one.

mod inbound;
mod sync;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::process::{Command, Stdio};

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::{json, Value};

use super::random_id;

pub use inbound::apply_frame;
pub use sync::{install, push};

/// Most shares (any state) one peer may hold with us; bounds what a peer can make us store.
pub const MAX_PER_PEER: i64 = 50;

#[derive(Clone, Debug)]
pub struct Share {
    pub share_id: String,
    pub peer_id: String,
    pub label: String,
    pub local_repo: Option<String>,
    pub inbound: bool,
    pub outbound: bool,
    pub remote_inbound: bool,
    pub remote_outbound: bool,
    pub revision: i64,
    pub state: String,
    pub root_commit: Option<String>,
}

const COLUMNS: &str = "share_id,peer_id,label,local_repo,inbound,outbound,remote_inbound,
    remote_outbound,revision,state,root_commit";

impl Share {
    fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            share_id: r.get(0)?,
            peer_id: r.get(1)?,
            label: r.get(2)?,
            local_repo: r.get(3)?,
            inbound: r.get(4)?,
            outbound: r.get(5)?,
            remote_inbound: r.get(6)?,
            remote_outbound: r.get(7)?,
            revision: r.get(8)?,
            state: r.get(9)?,
            root_commit: r.get(10)?,
        })
    }

    /// The share as `GET /api/fed` lists it (§10).
    pub fn to_json(&self) -> Value {
        json!({
            "share_id": self.share_id, "label": self.label, "local_repo": self.local_repo,
            "state": self.state, "inbound": self.inbound, "outbound": self.outbound,
            "remote_inbound": self.remote_inbound, "remote_outbound": self.remote_outbound,
        })
    }
}

pub fn get(conn: &Connection, share_id: &str) -> rusqlite::Result<Option<Share>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM peer_shares WHERE share_id=?1"),
        [share_id],
        Share::from_row,
    )
    .optional()
}

/// The shares of one peer that are not removed, oldest first.
pub fn of_peer(conn: &Connection, peer_id: &str) -> rusqlite::Result<Vec<Share>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM peer_shares WHERE peer_id=?1 AND state<>'removed' ORDER BY rowid"
    ))?;
    let rows = stmt.query_map([peer_id], Share::from_row)?;
    rows.collect()
}

/// A share label travels to the peer and into agents' contexts: one short plain line.
pub fn label_ok(label: &str) -> bool {
    (1..=64).contains(&label.chars().count())
        && label.trim() == label
        && label
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
}

/// The main repository `path` belongs to, as `repo_of` resolves it (worktrees included);
/// `None` when it is not a repository.
pub fn resolve_repo(path: &str) -> Option<String> {
    let path = Path::new(path);
    path.is_absolute()
        .then(|| crate::snapshot::repo_of(path))
        .flatten()
        .and_then(|repo| repo.to_str().map(str::to_owned))
}

/// The first commit of `repo`: only a hint for the other owner to find the same project.
fn root_commit(repo: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", repo, "rev-list", "--max-parents=0", "HEAD"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let first = String::from_utf8(out.stdout)
        .ok()?
        .lines()
        .next()?
        .to_owned();
    (first.len() >= 7 && first.bytes().all(|c| c.is_ascii_hexdigit())).then_some(first)
}

/// Whether `agent` belongs to a share mapped to `local_repo` right now: it is registered
/// and not closed, and the repository of its reported cwd is that repo. The cwd is resolved
/// on the spot, never from a cache, and one that does not resolve belongs to nothing.
pub fn is_member(conn: &Connection, agent: &str, local_repo: &str) -> rusqlite::Result<bool> {
    let cwd: Option<String> = conn
        .query_row(
            "SELECT cwd FROM agents WHERE id=?1 AND status<>'closed'",
            [agent],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(cwd
        .and_then(|cwd| resolve_repo(&cwd))
        .is_some_and(|repo| repo == local_repo))
}

/// Why a local share change was not made.
#[derive(Debug, PartialEq)]
pub enum Fail {
    UnknownPeer,
    PeerNotActive,
    UnknownShare,
    /// The share is not in a state the change applies to.
    WrongState,
    /// The repo does not resolve, or the label is not allowed: the field.
    Invalid(&'static str),
    /// This peer already has a live share for this repo.
    AlreadyShared,
    TooMany,
}

impl From<rusqlite::Error> for Fail {
    fn from(err: rusqlite::Error) -> Self {
        eprintln!("axon-bus: share change failed: {err}");
        Self::WrongState
    }
}

/// Offer `local_repo` to `peer_id`: a new `offered_out` share at revision 1.
pub fn offer(
    tx: &Transaction,
    peer_id: &str,
    local_repo: &str,
    label: &str,
    inbound: bool,
    outbound: bool,
) -> Result<Share, Fail> {
    let state: Option<String> = tx
        .query_row("SELECT state FROM peers WHERE peer_id=?1", [peer_id], |r| {
            r.get(0)
        })
        .optional()?;
    match state.as_deref() {
        None => return Err(Fail::UnknownPeer),
        Some("active") => {}
        Some(_) => return Err(Fail::PeerNotActive),
    }
    if !label_ok(label) {
        return Err(Fail::Invalid("label"));
    }
    let repo = resolve_repo(local_repo).ok_or(Fail::Invalid("local_repo"))?;
    let (taken, held): (i64, i64) = tx.query_row(
        "SELECT coalesce(sum(local_repo=?2 AND state<>'removed'),0), count(*)
         FROM peer_shares WHERE peer_id=?1 AND state<>'removed'",
        params![peer_id, repo],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if taken > 0 {
        return Err(Fail::AlreadyShared);
    }
    if held >= MAX_PER_PEER {
        return Err(Fail::TooMany);
    }
    let share_id = random_id().map_err(|_| Fail::WrongState)?;
    tx.execute(
        "INSERT INTO peer_shares (share_id,peer_id,label,local_repo,inbound,outbound,revision,state,root_commit)
         VALUES (?1,?2,?3,?4,?5,?6,1,'offered_out',?7)",
        params![share_id, peer_id, label, repo, inbound, outbound, root_commit(&repo)],
    )?;
    get(tx, &share_id)?.ok_or(Fail::UnknownShare)
}

/// Map an offered share to a local repo and turn it on: `active`, one revision on.
pub fn accept(
    tx: &Transaction,
    share_id: &str,
    local_repo: &str,
    inbound: bool,
    outbound: bool,
) -> Result<Share, Fail> {
    let share = get(tx, share_id)?.ok_or(Fail::UnknownShare)?;
    if share.state != "offered_in" {
        return Err(Fail::WrongState);
    }
    let repo = resolve_repo(local_repo).ok_or(Fail::Invalid("local_repo"))?;
    tx.execute(
        "UPDATE peer_shares SET local_repo=?2, inbound=?3, outbound=?4, state='active',
         revision=revision+1 WHERE share_id=?1",
        params![share_id, repo, inbound, outbound],
    )?;
    get(tx, share_id)?.ok_or(Fail::UnknownShare)
}

/// Change the local flags of a share, bumping its revision.
pub fn change(
    tx: &Transaction,
    share_id: &str,
    inbound: bool,
    outbound: bool,
) -> Result<Share, Fail> {
    let share = get(tx, share_id)?.ok_or(Fail::UnknownShare)?;
    if !matches!(share.state.as_str(), "active" | "offered_out") {
        return Err(Fail::WrongState);
    }
    tx.execute(
        "UPDATE peer_shares SET inbound=?2, outbound=?3, revision=revision+1 WHERE share_id=?1",
        params![share_id, inbound, outbound],
    )?;
    get(tx, share_id)?.ok_or(Fail::UnknownShare)
}

/// Unshare locally: `removed`, one revision on.
pub fn remove(tx: &Transaction, share_id: &str) -> Result<Share, Fail> {
    let share = get(tx, share_id)?.ok_or(Fail::UnknownShare)?;
    if share.state == "removed" {
        return Err(Fail::WrongState);
    }
    close(tx, &share, share.revision + 1)?;
    get(tx, share_id)?.ok_or(Fail::UnknownShare)
}

/// What an unshare does to everything that depended on the share, in the caller's
/// transaction: the share is `removed` at `revision`, its discovered sessions go, queued
/// outbound messages are cancelled, and inbound messages not yet delivered are deleted.
/// An agent is in at most one live share per peer (a share is a repo, and a repo is offered
/// once), so the sessions of this share name exactly the recipients it served.
fn close(tx: &Connection, share: &Share, revision: i64) -> rusqlite::Result<()> {
    let id = &share.share_id;
    tx.execute(
        "UPDATE peer_shares SET state='removed', revision=?2 WHERE share_id=?1",
        params![id, revision],
    )?;
    tx.execute("DELETE FROM fed_remote_sessions WHERE share_id=?1", [id])?;
    tx.execute(
        "UPDATE fed_outbox SET state='cancelled', last_error='unshared'
         WHERE share_id=?1 AND state='queued'",
        [id],
    )?;
    tx.execute(
        "DELETE FROM messages WHERE delivered_at IS NULL
           AND id IN (SELECT local_message_id FROM fed_inbox WHERE peer_id=?2)
           AND to_id IN (SELECT agent_id FROM fed_sessions WHERE share_id=?1)",
        params![id, share.peer_id],
    )?;
    Ok(())
}

/// The frame that brings the peer up to date with `share`, or `None` when the share needs
/// none. `update` picks `share_update` over `share_accept` for an active share.
pub fn frame_of(share: &Share, generation: i64, update: bool) -> Option<Value> {
    let flags = json!({"inbound": share.inbound, "outbound": share.outbound});
    let (kind, mut frame) = match (share.state.as_str(), update) {
        ("offered_out", _) => {
            let mut offer = flags;
            offer["label"] = json!(share.label);
            offer["root_commit"] = json!(share.root_commit);
            ("share_offer", offer)
        }
        ("active", true) => ("share_update", flags),
        ("active", false) => ("share_accept", flags),
        ("removed", _) => ("share_remove", json!({})),
        _ => return None,
    };
    frame["type"] = json!(kind);
    frame["v"] = json!(1);
    frame["generation"] = json!(generation);
    frame["share_id"] = json!(share.share_id);
    frame["revision"] = json!(share.revision);
    Some(frame)
}
