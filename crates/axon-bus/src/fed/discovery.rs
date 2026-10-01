//! Discovery (docs/P2P-SPEC.md §7): which of the other owner's agents can be written to. An
//! agent crosses only as an opaque session id, a `<harness>-<4 chars>` label and whether it
//! is active; nothing about where it works or what it does.

mod refresh;

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

use super::shares::{self, get, Share};
use crate::store;

pub use refresh::{drop_stale, install};

/// Sessions in one `discovery` answer.
pub const PAGE: usize = 100;
/// Sessions listed for one peer, whatever its shares.
pub const PEER_CAP: usize = 1000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ask {
    #[serde(rename = "type")]
    _type: String,
    v: i64,
    generation: i64,
    share_id: String,
    revision: i64,
    page: usize,
}

fn error(reason: &str) -> Value {
    json!({"type": "error", "reason": reason})
}

/// 12 characters of lowercase base32: 60 random bits.
fn random_session() -> anyhow::Result<String> {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut bytes = [0u8; 12];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow::anyhow!("no OS randomness: {e}"))?;
    Ok(bytes
        .iter()
        .map(|b| char::from(ALPHABET[usize::from(b & 31)]))
        .collect())
}

/// The session id of `agent` in `share_id`: made once, then stable, and never handed to
/// another agent or share.
pub(super) fn session_for(
    conn: &Connection,
    agent: &str,
    share_id: &str,
) -> anyhow::Result<String> {
    loop {
        let known: Option<String> = conn
            .query_row(
                "SELECT session FROM fed_sessions WHERE agent_id=?1 AND share_id=?2",
                params![agent, share_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(session) = known {
            return Ok(session);
        }
        conn.execute(
            "INSERT OR IGNORE INTO fed_sessions (session, agent_id, share_id) VALUES (?1,?2,?3)",
            params![random_session()?, agent, share_id],
        )?;
    }
}

/// The agents of `share` right now, oldest first and at most `PEER_CAP`: registered, not
/// closed, and working in the share's repo. Each cwd is resolved on the spot.
fn members(conn: &Connection, share: &Share) -> rusqlite::Result<Vec<(String, String, String)>> {
    let Some(repo) = &share.local_repo else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT id, harness, status, cwd FROM agents
         WHERE status IN ('active','idle') AND cwd IS NOT NULL ORDER BY started_at, id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, String>(3)?))
    })?;
    let mut resolved: HashMap<String, bool> = HashMap::new();
    let mut found = Vec::new();
    for row in rows {
        let (id, harness, status, cwd) = row?;
        let inside = *resolved
            .entry(cwd.clone())
            .or_insert_with(|| shares::resolve_repo(&cwd).as_ref() == Some(repo));
        if inside {
            found.push((id, harness, status));
            if found.len() == PEER_CAP {
                break;
            }
        }
    }
    Ok(found)
}

/// Answer a peer's `discovery` frame with one page of the sessions its share can reach.
pub fn answer(conn: &mut Connection, node: &str, frame: Value) -> anyhow::Result<Value> {
    let Ok(ask) = serde_json::from_value::<Ask>(frame) else {
        return Ok(error("bad_frame"));
    };
    if ask.v != 1 {
        return Ok(error("unsupported_version"));
    }
    let tx = store::write_tx(conn)?;
    let peer: Option<(String, i64)> = tx
        .query_row(
            "SELECT peer_id, generation FROM peers WHERE node_id=?1 AND state='active'",
            [node],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((peer_id, generation)) = peer else {
        return Ok(error("not_a_peer"));
    };
    if generation != ask.generation {
        return Ok(error("stale_generation"));
    }
    let share = get(&tx, &ask.share_id)?
        .filter(|share| share.peer_id == peer_id && share.state == "active");
    let Some(share) = share else {
        return Ok(error("unknown_share"));
    };
    if share.revision != ask.revision {
        return Ok(error("stale_revision"));
    }
    let members = members(&tx, &share)?;
    let start = ask.page.saturating_mul(PAGE);
    let mut sessions = Vec::new();
    for (agent, harness, status) in members.iter().skip(start).take(PAGE) {
        let session = session_for(&tx, agent, &share.share_id)?;
        sessions.push(json!({
            "session": session,
            "label": format!("{harness}-{}", &session[..4]),
            "availability": if status == "active" { "active" } else { "idle" },
        }));
    }
    tx.commit()?;
    let next_page = (members.len() > start + PAGE).then_some(ask.page + 1);
    Ok(json!({"type": "sessions", "sessions": sessions, "next_page": next_page}))
}

/// The "Remote" blocks of `agent`'s roster: for each active share of its repo that it may
/// send over, the other owner's sessions as `peer:<label>/<session>` targets. `None` when
/// there is nothing to list.
pub fn remote_block(conn: &Connection, agent: &str) -> rusqlite::Result<Option<String>> {
    let cwd: Option<String> = conn
        .query_row(
            "SELECT cwd FROM agents WHERE id=?1 AND status<>'closed'",
            [agent],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let Some(repo) = cwd.and_then(|cwd| shares::resolve_repo(&cwd)) else {
        return Ok(None);
    };
    let mut shares = conn.prepare(
        "SELECT s.share_id, s.label, p.label, p.peer_id FROM peer_shares s
         JOIN peers p ON p.peer_id=s.peer_id
         WHERE s.state='active' AND s.outbound=1 AND p.state='active' AND s.local_repo=?1
         ORDER BY s.rowid",
    )?;
    let listed: Vec<(String, String, String, String)> = shares
        .query_map([&repo], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    let mut text = String::new();
    for (share_id, project, peer_label, peer_id) in listed {
        let mut stmt = conn.prepare(
            "SELECT session, label, availability FROM fed_remote_sessions
             WHERE peer_id=?1 AND share_id=?2 ORDER BY label, session",
        )?;
        let sessions: Vec<(String, String, String)> = stmt
            .query_map(params![peer_id, share_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<Result<_, _>>()?;
        if sessions.is_empty() {
            continue;
        }
        text.push_str(&format!(
            "Remote (another person's agents, on their machine; project {project}):\n"
        ));
        for (session, label, availability) in sessions {
            text.push_str(&format!(
                "  peer:{peer_label}/{session}  {label}  {availability}\n"
            ));
        }
    }
    Ok((!text.is_empty()).then_some(text))
}

#[cfg(test)]
mod tests;
