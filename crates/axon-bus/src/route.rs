//! Message edges (BUS-PLAN §3): parent ↔ child, root ↔ root over an accepted `link`, and
//! thread-scoped temporary `grant`s. A send is allowed only along an edge; otherwise the
//! caller gets the relay route to take.

use std::collections::{HashMap, VecDeque};

use anyhow::bail;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;

use crate::store::{append_event, now_ms};

fn parent_of(conn: &Connection, id: &str) -> anyhow::Result<Option<Option<String>>> {
    Ok(conn
        .query_row("SELECT parent_id FROM agents WHERE id=?1", [id], |r| {
            r.get::<_, Option<String>>(0)
        })
        .optional()?)
}

/// Fails unless `id` is a registered root.
fn require_root(conn: &Connection, id: &str) -> anyhow::Result<()> {
    match parent_of(conn, id)? {
        None => bail!("agent {id} is not registered"),
        Some(Some(_)) => bail!("{id} is not a root; only roots link"),
        Some(None) => Ok(()),
    }
}

fn has_edge(conn: &Connection, from: &str, to: &str, kind: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM edges WHERE from_id=?1 AND to_id=?2 AND kind=?3)",
        params![from, to, kind],
        |r| r.get(0),
    )
}

/// Propose a root ↔ root link; it opens once the other root accepts.
pub fn link(conn: &Connection, from: &str, to: &str) -> anyhow::Result<()> {
    require_root(conn, from)?;
    require_root(conn, to)?;
    if !has_edge(conn, from, to, "link")? {
        conn.execute(
            "INSERT INTO edges (from_id,to_id,kind) VALUES (?1,?2,'link')",
            params![from, to],
        )?;
        append_event(conn, from, "link", to, &json!({"to": to}).to_string())?;
    }
    Ok(())
}

/// `from` accepts the link that `to` proposed to it.
pub fn accept(conn: &Connection, from: &str, to: &str) -> anyhow::Result<()> {
    require_root(conn, from)?;
    if !has_edge(conn, to, from, "link")? {
        bail!("{to} has not proposed a link to {from}");
    }
    if !has_edge(conn, from, to, "link")? {
        conn.execute(
            "INSERT INTO edges (from_id,to_id,kind) VALUES (?1,?2,'link')",
            params![from, to],
        )?;
        append_event(conn, from, "accept", to, &json!({"to": to}).to_string())?;
    }
    Ok(())
}

/// Open a direct edge between `from` and `to`, both ways, for one thread until `ttl_ms`.
pub fn grant(
    conn: &Connection,
    from: &str,
    to: &str,
    thread: &str,
    ttl_ms: i64,
) -> anyhow::Result<()> {
    for id in [from, to] {
        if parent_of(conn, id)?.is_none() {
            bail!("agent {id} is not registered");
        }
    }
    let expires_at = now_ms() + ttl_ms;
    conn.execute(
        "INSERT INTO edges (from_id,to_id,kind,thread,expires_at) VALUES (?1,?2,'grant',?3,?4)",
        params![from, to, thread, expires_at],
    )?;
    let payload = json!({"to": to, "thread": thread, "expires_at": expires_at});
    append_event(conn, from, "grant", to, &payload.to_string())
}

/// Whether `from` may message `to` directly, on `thread` when one is given.
pub fn allowed(
    conn: &Connection,
    from: &str,
    to: &str,
    thread: Option<&str>,
) -> anyhow::Result<bool> {
    let (Some(from_parent), Some(to_parent)) = (parent_of(conn, from)?, parent_of(conn, to)?)
    else {
        return Ok(false);
    };
    if from_parent.as_deref() == Some(to) || to_parent.as_deref() == Some(from) {
        return Ok(true);
    }
    if has_edge(conn, from, to, "link")? && has_edge(conn, to, from, "link")? {
        return Ok(true);
    }
    let Some(thread) = thread else {
        return Ok(false);
    };
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM edges WHERE kind='grant' AND thread=?3 AND expires_at>?4
         AND ((from_id=?1 AND to_id=?2) OR (from_id=?2 AND to_id=?1)))",
        params![from, to, thread, now_ms()],
        |r| r.get(0),
    )?)
}

/// The shortest relay path from `from` to `to` over tree edges and accepted links.
pub fn route(conn: &Connection, from: &str, to: &str) -> anyhow::Result<Option<Vec<String>>> {
    let mut adjacent: HashMap<String, Vec<String>> = HashMap::new();
    let mut connect = |a: String, b: String| {
        adjacent.entry(a.clone()).or_default().push(b.clone());
        adjacent.entry(b).or_default().push(a);
    };
    let mut tree = conn.prepare(
        "SELECT id,parent_id FROM agents WHERE parent_id IS NOT NULL AND status<>'closed'",
    )?;
    for pair in tree.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
        let (child, parent) = pair?;
        connect(child, parent);
    }
    let mut links = conn.prepare(
        "SELECT a.from_id,a.to_id FROM edges a JOIN edges b
         ON b.from_id=a.to_id AND b.to_id=a.from_id AND b.kind='link'
         WHERE a.kind='link' AND a.from_id<a.to_id",
    )?;
    for pair in links.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
        let (a, b) = pair?;
        connect(a, b);
    }
    let mut previous: HashMap<&str, &str> = HashMap::new();
    let mut queue = VecDeque::from([from]);
    while let Some(node) = queue.pop_front() {
        if node == to {
            let mut path = vec![to.to_owned()];
            let mut at = to;
            while let Some(step) = previous.get(at) {
                path.push((*step).to_owned());
                at = step;
            }
            path.reverse();
            return Ok(Some(path));
        }
        for next in adjacent.get(node).into_iter().flatten() {
            if next != from && !previous.contains_key(next.as_str()) {
                previous.insert(next, node);
                queue.push_back(next);
            }
        }
    }
    Ok(None)
}

/// The refusal text for a non-edge send, naming the route to relay through.
pub fn refusal(conn: &Connection, from: &str, to: &str) -> anyhow::Result<String> {
    Ok(match route(conn, from, to)? {
        Some(path) => format!(
            "{from} has no edge to {to}; relay along the route {} (send to {} and ask it to forward)",
            path.join(" -> "),
            path[1]
        ),
        None => format!("{from} has no edge to {to}, and no route connects them"),
    })
}
