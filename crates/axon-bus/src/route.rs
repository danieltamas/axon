//! Message edges (BUS-PLAN §3): parent ↔ child, root ↔ root over an accepted `link`, and
//! thread-scoped temporary `grant`s. A send is allowed only along an edge; otherwise the
//! caller gets the relay route to take.
//!
//! Task routing (BUS-PLAN §4): `routes.toml` rules pick a lane, the remaining budget may
//! step it down one lane, and an advisor's proposal is only logged beside it.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;

use anyhow::{bail, Context};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

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
/// A grant only shortcuts a relay route that already exists, on a thread `from` takes part
/// in; it never connects trees that no link joins.
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
    if route(conn, from, to)?.is_none() {
        bail!("no route joins {from} and {to}; their roots must link first");
    }
    let in_thread: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE thread=?1 AND (from_id=?2 OR to_id=?2))",
        params![thread, from],
        |r| r.get(0),
    )?;
    if !in_thread {
        bail!("{from} has no message on thread {thread}; grant a thread it takes part in");
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
    if from == to {
        return Ok(format!("{from} cannot message itself"));
    }
    Ok(match route(conn, from, to)? {
        Some(path) => format!(
            "{from} has no edge to {to}; relay along the route {} (send to {} and ask it to forward)",
            path.join(" -> "),
            path[1]
        ),
        None => format!("{from} has no edge to {to}, and no route connects them"),
    })
}

/// `routes.toml`: lanes, most capable first, and the lane each role starts in.
#[derive(Deserialize)]
struct Routes {
    lanes: Vec<Lane>,
    #[serde(default)]
    roles: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Lane {
    name: String,
    harness: String,
    model: String,
    effort: String,
}

/// `$XDG_CONFIG_HOME/axon/routes.toml`, else `~/.config/axon/routes.toml`.
fn routes_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| axon_core::home().join(".config"))
        .join("axon/routes.toml")
}

fn load_routes() -> anyhow::Result<Routes> {
    let path = routes_path();
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let routes: Routes =
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    if routes.lanes.is_empty() {
        bail!("{} defines no [[lanes]]", path.display());
    }
    Ok(routes)
}

/// The median USD cost of the finished agents that ran as `role` on this lane's model and
/// effort. Per role x model x effort, never role alone, so a new model is judged on its own
/// history (§4). None without priced history.
fn typical_cost(conn: &Connection, role: Option<&str>, lane: &Lane) -> anyhow::Result<Option<f64>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM agents WHERE status='closed' AND role IS ?1 AND model=?2 AND effort=?3
         ORDER BY id",
    )?;
    let ids: Vec<String> = stmt
        .query_map(params![role, lane.model, lane.effort], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut costs = Vec::new();
    for id in ids {
        costs.extend(crate::budget::agent_cost(conn, &id)?);
    }
    costs.sort_by(f64::total_cmp);
    let mid = costs.len() / 2;
    Ok(match costs.len() {
        0 => None,
        n if n % 2 == 1 => Some(costs[mid]),
        _ => Some((costs[mid - 1] + costs[mid]) / 2.0),
    })
}

/// Answer `route`: {harness, model, effort, reason}, the same for the same inputs and
/// history. Logs the answer and the advisor's shadow proposal; the proposal never changes
/// the answer. Call inside a write transaction.
pub fn route_task(
    conn: &Connection,
    task: &str,
    role: Option<&str>,
    budget_usd: Option<f64>,
    advice: Option<&Value>,
) -> anyhow::Result<String> {
    let routes = load_routes()?;
    let lane_index = |name: &str| routes.lanes.iter().position(|lane| lane.name == name);
    let (mut index, mut reason) = match role.map(|role| (role, routes.roles.get(role))) {
        Some((role, Some(name))) => {
            let index = lane_index(name).with_context(|| {
                format!("role {role} maps to lane {name}, which routes.toml does not define")
            })?;
            (index, format!("role {role} maps to lane {name}"))
        }
        Some((role, None)) => (
            0,
            format!(
                "role {role} has no lane; first lane {}",
                routes.lanes[0].name
            ),
        ),
        None => (0, format!("no role; first lane {}", routes.lanes[0].name)),
    };
    if let Some(budget) = budget_usd {
        let lane = &routes.lanes[index];
        if let Some(typical) = typical_cost(conn, role, lane)? {
            if typical > budget {
                let shortfall = format!(
                    "; budget ${budget:.2} cannot cover lane {}'s median ${typical:.2} for {} x {} x {}",
                    lane.name,
                    role.unwrap_or("no role"),
                    lane.model,
                    lane.effort
                );
                reason.push_str(&shortfall);
                match routes.lanes.get(index + 1) {
                    Some(cheaper) => {
                        reason.push_str(&format!(", stepped down to lane {}", cheaper.name));
                        index += 1;
                    }
                    None => reason.push_str(", and no cheaper lane exists"),
                }
            }
        }
    }
    let lane = &routes.lanes[index];
    let answer = json!({"harness": lane.harness, "model": lane.model, "effort": lane.effort,
        "reason": reason})
    .to_string();
    conn.execute(
        "INSERT INTO routing_decisions (ts,role,task_hash,rule_json,advisor_json) VALUES (?1,?2,?3,?4,?5)",
        params![
            now_ms(),
            role,
            blake3::hash(task.as_bytes()).to_hex().as_str(),
            answer,
            advice.map(Value::to_string)
        ],
    )?;
    append_event(conn, crate::msg::BUS, "route", role.unwrap_or("-"), &answer)?;
    Ok(answer)
}

/// `0.7usd`, `$0.7` or `0.7`: remaining budget in USD.
pub fn parse_usd(text: &str) -> Result<f64, String> {
    let number = text.strip_suffix("usd").unwrap_or(text);
    let number = number.strip_prefix('$').unwrap_or(number);
    match number.parse::<f64>() {
        Ok(usd) if usd.is_finite() && usd >= 0.0 => Ok(usd),
        _ => Err(format!("invalid budget {text}; use e.g. 0.5usd")),
    }
}
