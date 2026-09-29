//! Path claims per checkout (BUS-PLAN §2): a directory claims its subtree, and a path
//! held by another live agent in the same checkout cannot be claimed.

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context};
use rusqlite::{params, Connection};
use serde_json::json;

use crate::store::{append_event, now_ms};

pub struct Conflict {
    pub path: String,
    pub owner: String,
    pub task: Option<String>,
}

/// The checkout that holds `dir`: the nearest ancestor with a `.git` entry (a worktree
/// has a `.git` file), else `dir` itself.
pub fn checkout_of(dir: &Path) -> PathBuf {
    dir.ancestors()
        .find(|d| d.join(".git").exists())
        .unwrap_or(dir)
        .to_path_buf()
}

/// `path` relative to `checkout`, lexically normalized; "." is the whole checkout.
fn relative(checkout: &Path, cwd: &Path, path: &str) -> anyhow::Result<String> {
    let mut normalized = PathBuf::new();
    for part in cwd.join(path).components() {
        match part {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    let inside = normalized
        .strip_prefix(checkout)
        .with_context(|| format!("{path} is outside the checkout {}", checkout.display()))?;
    let text = inside.to_str().context("claim path is not UTF-8")?;
    Ok(if text.is_empty() { "." } else { text }.to_owned())
}

fn overlaps(a: &str, b: &str) -> bool {
    let within = |inner: &str, outer: &str| {
        outer == "." || inner == outer || inner.starts_with(&format!("{outer}/"))
    };
    within(a, b) || within(b, a)
}

/// Claim `paths` for `agent`. Returns the conflicts instead of claiming anything when a
/// live agent already holds an overlapping path. Call inside a write transaction.
pub fn claim(
    conn: &Connection,
    agent: &str,
    cwd: &Path,
    task: Option<&str>,
    paths: &[String],
) -> anyhow::Result<Vec<Conflict>> {
    if crate::registry::root_of(conn, agent)?.is_none() {
        bail!("agent {agent} is not registered");
    }
    let checkout = checkout_of(cwd);
    let checkout_str = checkout.to_str().context("checkout path is not UTF-8")?;
    let wanted = paths
        .iter()
        .map(|p| relative(&checkout, cwd, p))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut stmt = conn.prepare(
        "SELECT c.path, c.agent_id, c.task FROM claims c JOIN agents a ON a.id=c.agent_id
         WHERE c.checkout=?1 AND c.agent_id<>?2 AND a.status IN ('active','idle')",
    )?;
    let held = stmt
        .query_map(params![checkout_str, agent], |r| {
            Ok(Conflict {
                path: r.get(0)?,
                owner: r.get(1)?,
                task: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let conflicts: Vec<Conflict> = held
        .into_iter()
        .filter(|c| wanted.iter().any(|w| overlaps(w, &c.path)))
        .collect();
    if !conflicts.is_empty() {
        return Ok(conflicts);
    }
    for path in &wanted {
        conn.execute(
            "INSERT INTO claims (agent_id,checkout,path,task,created_at) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(agent_id,checkout,path) DO UPDATE SET task=excluded.task",
            params![agent, checkout_str, path, task, now_ms()],
        )?;
    }
    let payload = json!({"checkout": checkout_str, "paths": wanted, "task": task});
    append_event(conn, agent, "claim", checkout_str, &payload.to_string())?;
    Ok(Vec::new())
}

/// Drop `agent`'s claims in the checkout of `cwd`, or all of them when `paths` is empty.
pub fn release(conn: &Connection, agent: &str, cwd: &Path, paths: &[String]) -> anyhow::Result<()> {
    if paths.is_empty() {
        return release_all(conn, agent);
    }
    let checkout = checkout_of(cwd);
    let checkout_str = checkout.to_str().context("checkout path is not UTF-8")?;
    for path in paths {
        conn.execute(
            "DELETE FROM claims WHERE agent_id=?1 AND checkout=?2 AND path=?3",
            params![agent, checkout_str, relative(&checkout, cwd, path)?],
        )?;
    }
    let payload = json!({"checkout": checkout_str, "paths": paths});
    append_event(conn, agent, "release", checkout_str, &payload.to_string())
}

pub fn release_all(conn: &Connection, agent: &str) -> anyhow::Result<()> {
    if conn.execute("DELETE FROM claims WHERE agent_id=?1", [agent])? > 0 {
        append_event(conn, agent, "release", agent, "{}")?;
    }
    Ok(())
}

/// Every claim, as JSON objects, oldest first.
pub fn list(conn: &Connection) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT agent_id,checkout,path,task,created_at FROM claims ORDER BY created_at, path",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(
            json!({"agent_id": r.get::<_, String>(0)?, "checkout": r.get::<_, String>(1)?,
            "path": r.get::<_, String>(2)?, "task": r.get::<_, Option<String>>(3)?,
            "created_at": r.get::<_, i64>(4)?}),
        )
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_overlaps_its_subtree_only() {
        assert!(overlaps("src", "src/main.rs"));
        assert!(overlaps("src/main.rs", "src"));
        assert!(overlaps(".", "docs"));
        assert!(!overlaps("src", "src2/main.rs"));
    }

    #[test]
    fn paths_normalize_relative_to_the_checkout() {
        let checkout = Path::new("/repo");
        let cwd = Path::new("/repo/crates");
        assert_eq!(relative(checkout, cwd, "../src/").unwrap(), "src");
        assert_eq!(relative(checkout, cwd, "..").unwrap(), ".");
        assert!(relative(checkout, cwd, "../../etc").is_err());
    }
}
