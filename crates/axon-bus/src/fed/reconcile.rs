//! Keeping both sides' view of a pairing's pause state right (docs/P2P-SPEC.md §9). Every
//! pause, resume and removal bumps the pairing's own `lifecycle_seq`; a side applies what it
//! hears only if the sequence is newer than the last it applied, so a late notice cannot undo
//! a later one. The sides also trade their current state whenever they connect, so a notice
//! that was lost never leaves a pairing stuck.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

/// Our current state toward `node`, as the `state` notice that carries it: `(generation, frame)`.
pub fn hello(db: &Path, node: &str) -> anyhow::Result<Option<(i64, Value)>> {
    let conn = crate::store::open(db)?;
    Ok(own_state(&conn, node)?.map(|(generation, state)| {
        (
            generation,
            json!({"type": "notice", "v": 1, "generation": generation, "what": "state",
                   "paused": state["paused"], "seq": state["seq"]}),
        )
    }))
}

/// `(generation, {paused, seq})` of the live pairing with `node`.
fn own_state(conn: &Connection, node: &str) -> rusqlite::Result<Option<(i64, Value)>> {
    conn.query_row(
        "SELECT generation, state, lifecycle_seq FROM peers
         WHERE node_id=?1 AND state IN ('active','paused')",
        [node],
        |r| {
            let (generation, state, seq): (i64, String, i64) = (r.get(0)?, r.get(1)?, r.get(2)?);
            Ok((generation, json!({"paused": state == "paused", "seq": seq})))
        },
    )
    .optional()
}

/// Apply what `node` says about its side. True when the stored view changed.
pub fn learn(
    conn: &Connection,
    node: &str,
    generation: i64,
    paused: bool,
    seq: i64,
) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "UPDATE peers SET remote_paused=?3, remote_lifecycle_seq=?4
         WHERE node_id=?1 AND generation=?2 AND state IN ('active','paused')
           AND remote_lifecycle_seq < ?4",
        params![node, generation, paused, seq],
    )?;
    Ok(changed > 0)
}

/// `learn` from the `lifecycle` object an acknowledgement carries, if it has one.
pub fn learn_reply(
    conn: &Connection,
    node: &str,
    generation: i64,
    reply: &Value,
) -> rusqlite::Result<bool> {
    let state = &reply["lifecycle"];
    match (state["paused"].as_bool(), state["seq"].as_i64()) {
        (Some(paused), Some(seq)) => learn(conn, node, generation, paused, seq),
        _ => Ok(false),
    }
}

/// The acknowledgement of a lifecycle notice: accepted, with our own state for the sender to learn.
pub fn acknowledge(conn: &Connection, node: &str) -> rusqlite::Result<Value> {
    Ok(match own_state(conn, node)? {
        Some((_, state)) => json!({"type": "ack", "status": "accepted", "lifecycle": state}),
        None => json!({"type": "error", "reason": "unknown_peer"}),
    })
}

#[cfg(test)]
mod tests;
