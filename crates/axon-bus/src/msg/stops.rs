//! Stops: a peer's or a budget's order to halt an agent and the subtree it waits on, held
//! until its turn ends (or its budget is raised), with the doorbell file as the fallback a
//! hook reads when the hub is unreadable.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

use super::{stop_reason, BUDGET_THREAD};
use crate::doorbell;
use crate::store::now_ms;

/// `agent` and its ancestors up to its root, as a `line(id)` CTE bound to ?1.
const LINEAGE: &str = "WITH RECURSIVE line(id) AS (SELECT ?1 UNION
    SELECT a.parent_id FROM agents a JOIN line ON a.id=line.id WHERE a.parent_id IS NOT NULL)";

/// `agent` and everything spawned under it.
pub(super) fn subtree(conn: &Connection, agent: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE sub(id) AS (SELECT ?1 UNION SELECT a.id FROM agents a JOIN sub ON a.parent_id=sub.id)
         SELECT id FROM sub",
    )?;
    let ids = stmt.query_map([agent], |r| r.get(0))?;
    ids.collect()
}

/// The oldest stop that holds `agent`: addressed to it or to an ancestor, whose turn has
/// not ended on it yet. Stopping an orchestrator stops the workers it is waiting on.
pub fn pending_stop(conn: &Connection, agent: &str) -> anyhow::Result<Option<String>> {
    let stop: Option<(String, String)> = conn
        .query_row(
            &format!(
                "{LINEAGE} SELECT from_id,body FROM messages WHERE to_id IN (SELECT id FROM line)
                 AND kind='stop' AND acked_at IS NULL ORDER BY rowid LIMIT 1"
            ),
            [agent],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((from, body)) = stop else {
        return Ok(None);
    };
    conn.execute(
        "UPDATE messages SET delivered_at=coalesce(delivered_at, ?2)
         WHERE to_id=?1 AND kind='stop' AND acked_at IS NULL",
        params![agent, now_ms()],
    )?;
    Ok(Some(stop_reason(&from, &body)))
}

/// A peer's stop is honoured once the agent's turn ends. A budget stop from the bus holds
/// until the budget is raised (`clear_bus_stops`).
pub fn ack_stops(conn: &Connection, agent: &str) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE messages SET acked_at=?2
         WHERE to_id=?1 AND kind='stop' AND acked_at IS NULL AND thread NOT LIKE ?3",
        params![agent, now_ms(), format!("{BUDGET_THREAD}%")],
    )?;
    Ok(())
}

/// A closed agent runs no more tool calls, so every stop on it, budget stops included,
/// is done; its doorbell can then go.
pub fn ack_stops_on_close(conn: &Connection, agent: &str) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE messages SET acked_at=?2 WHERE to_id=?1 AND kind='stop' AND acked_at IS NULL",
        params![agent, now_ms()],
    )?;
    Ok(())
}

/// Lift the stops that `scope`'s budget put on `agent`; stops from other budgets hold.
pub fn clear_bus_stops(conn: &Connection, scope: &str, agent: &str) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE messages SET acked_at=?2
         WHERE to_id=?1 AND kind='stop' AND acked_at IS NULL AND thread=?3",
        params![agent, now_ms(), format!("{BUDGET_THREAD}{scope}")],
    )?;
    Ok(())
}

/// Remove `agent`'s doorbell once no stop is pending. Call after the transaction that
/// acked its stops commits, so a rolled-back ack never loses the fallback.
pub fn silence_doorbell(conn: &Connection, agent: &str) -> anyhow::Result<()> {
    let Some(db) = conn.path() else {
        return Ok(());
    };
    // The agent's stops held its subtree too; each member keeps its doorbell only while
    // some stop still holds it.
    for member in subtree(conn, agent)? {
        if pending_stop_row(conn, &member)? {
            continue;
        }
        for name in doorbell_names(conn, &member)? {
            doorbell::clear(Path::new(db), &name)?;
        }
    }
    Ok(())
}

fn pending_stop_row(conn: &Connection, agent: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        &format!(
            "{LINEAGE} SELECT EXISTS(SELECT 1 FROM messages WHERE to_id IN (SELECT id FROM line)
             AND kind='stop' AND acked_at IS NULL)"
        ),
        [agent],
        |r| r.get(0),
    )
}

/// The names a hook may know `agent` by when the hub is unreadable: its id and, for a
/// root registered under another id, its session id.
pub(super) fn doorbell_names(conn: &Connection, agent: &str) -> rusqlite::Result<Vec<String>> {
    let alias: Option<String> = conn
        .query_row(
            "SELECT session_id FROM agents WHERE id=?1 AND parent_id IS NULL AND session_id<>id",
            [agent],
            |r| r.get(0),
        )
        .optional()?;
    Ok(std::iter::once(agent.to_owned()).chain(alias).collect())
}
