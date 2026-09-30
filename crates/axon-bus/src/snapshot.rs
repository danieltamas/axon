//! The live tree (BUS-PLAN §7): repo → harness → root agent → subagents. A root groups
//! under the main repository of its cwd (a worktree under its main repo); subagents nest
//! by parent and never by repo, carrying a badge when they work in another repo.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::store::now_ms;
use crate::transcript;

/// Rows of narrative per agent in a snapshot; the page asks for no more.
const NARRATIVE_ROWS: i64 = 200;

/// A finished tree stays visible this long after its last member closed.
const CLOSED_VISIBLE_MS: i64 = 15 * 60 * 1000;

#[derive(Clone, Default)]
pub struct Checkout {
    /// The main repository: the parent of the git common dir.
    repo: Option<PathBuf>,
    branch: Option<String>,
}

/// Resolve a cwd to its main repository and branch by reading `.git` directly, so a
/// snapshot never spawns `git`.
fn checkout(cwd: &Path) -> Checkout {
    let Ok(cwd) = cwd.canonicalize() else {
        return Checkout::default();
    };
    let top = crate::claims::checkout_of(&cwd);
    let dot_git = top.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        // A worktree's `.git` file points at `<common>/worktrees/<name>`.
        let Some(pointer) = std::fs::read_to_string(&dot_git).ok() else {
            return Checkout::default();
        };
        let Some(target) = pointer.trim().strip_prefix("gitdir:") else {
            return Checkout::default();
        };
        top.join(target.trim())
    };
    let common = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(relative) => git_dir.join(relative.trim()),
        Err(_) => git_dir.clone(),
    };
    let branch = std::fs::read_to_string(git_dir.join("HEAD"))
        .ok()
        .and_then(|head| {
            head.trim()
                .strip_prefix("ref: refs/heads/")
                .map(str::to_owned)
        });
    Checkout {
        repo: common
            .canonicalize()
            .ok()
            .and_then(|c| c.parent().map(Path::to_path_buf)),
        branch,
    }
}

struct Node {
    id: String,
    harness: String,
    parent_id: Option<String>,
    model: Option<String>,
    role: Option<String>,
    mission: Option<String>,
    status: String,
    checkout: Checkout,
}

/// The whole tree as the page renders it. `cache` keeps cwd lookups across snapshots.
pub fn build(conn: &Connection, cache: &mut HashMap<String, Checkout>) -> anyhow::Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT a.id,a.harness,a.parent_id,a.model,a.role,a.mission,a.status,a.cwd FROM agents a
         WHERE a.root_id IN (SELECT root_id FROM agents
                             WHERE status<>'closed' OR ended_at>?1 OR ended_at IS NULL)
         ORDER BY a.started_at, a.id",
    )?;
    let nodes: Vec<Node> = stmt
        .query_map([now_ms() - CLOSED_VISIBLE_MS], |r| {
            let cwd: Option<String> = r.get(7)?;
            Ok((
                Node {
                    id: r.get(0)?,
                    harness: r.get(1)?,
                    parent_id: r.get(2)?,
                    model: r.get(3)?,
                    role: r.get(4)?,
                    mission: r.get(5)?,
                    status: r.get(6)?,
                    checkout: Checkout::default(),
                },
                cwd,
            ))
        })?
        .map(|row| {
            row.map(|(mut node, cwd)| {
                if let Some(cwd) = cwd {
                    node.checkout = cache
                        .entry(cwd.clone())
                        .or_insert_with(|| checkout(Path::new(&cwd)))
                        .clone();
                }
                node
            })
        })
        .collect::<rusqlite::Result<_>>()?;
    let tokens = token_totals(conn)?;
    let content = transcript::content_enabled(conn)?;
    let mut children: HashMap<&str, Vec<&Node>> = HashMap::new();
    for node in nodes.iter().filter(|n| n.parent_id.is_some()) {
        children
            .entry(node.parent_id.as_deref().unwrap_or_default())
            .or_default()
            .push(node);
    }
    // Some repos sort first by path; "no repo" (None) goes last.
    let mut groups: BTreeMap<(bool, Option<PathBuf>), BTreeMap<String, Vec<Value>>> =
        BTreeMap::new();
    for root in nodes.iter().filter(|n| n.parent_id.is_none()) {
        let repo = root.checkout.repo.clone();
        let tree = render(conn, root, repo.as_deref(), &children, &tokens, content)?;
        groups
            .entry((repo.is_none(), repo))
            .or_default()
            .entry(root.harness.clone())
            .or_default()
            .push(tree);
    }
    let repos: Vec<Value> = groups
        .into_iter()
        .map(|((_, repo), harnesses)| {
            let name = repo.as_deref().and_then(Path::file_name).map_or_else(
                || "no repo".to_owned(),
                |n| n.to_string_lossy().into_owned(),
            );
            let harnesses: Vec<Value> = harnesses
                .into_iter()
                .map(|(harness, roots)| json!({"harness": harness, "roots": roots}))
                .collect();
            json!({"repo": repo, "name": name, "harnesses": harnesses})
        })
        .collect();
    Ok(json!({"repos": repos}))
}

fn token_totals(conn: &Connection) -> rusqlite::Result<HashMap<String, i64>> {
    let mut stmt = conn.prepare(
        "SELECT agent_id, sum(input_tokens+output_tokens+cache_read_tokens+cache_write_tokens
                              +cache_write_1h_tokens) FROM usage GROUP BY agent_id",
    )?;
    let totals = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    totals.collect()
}

fn render(
    conn: &Connection,
    node: &Node,
    group_repo: Option<&Path>,
    children: &HashMap<&str, Vec<&Node>>,
    tokens: &HashMap<String, i64>,
    content: bool,
) -> anyhow::Result<Value> {
    let kids = children
        .get(node.id.as_str())
        .map_or(&[][..], Vec::as_slice);
    let kids: Vec<Value> = kids
        .iter()
        .map(|child| render(conn, child, group_repo, children, tokens, content))
        .collect::<anyhow::Result<_>>()?;
    let repo = node.checkout.repo.as_deref();
    let repo_badge = repo.filter(|r| Some(*r) != group_repo);
    Ok(json!({
        "id": node.id,
        "harness": node.harness,
        "model": node.model,
        "role": node.role,
        "mission": node.mission,
        "status": node.status,
        "repo": repo,
        "repo_badge": repo_badge,
        "branch": node.checkout.branch,
        "tokens": tokens.get(&node.id).copied().unwrap_or(0),
        "narrative": narrative(conn, &node.id, content)?,
        "children": kids,
    }))
}

/// The agent's latest narrative, with each run of consecutive tool calls collapsed into
/// one `tool_run` chip.
/// Without content capture, text stored while it was on is withheld too (§7 privacy).
fn narrative(conn: &Connection, agent: &str, content: bool) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT kind,source,CASE WHEN ?4 THEN text END,recorded,tokens,tool_name,
         CASE WHEN ?4 THEN tool_detail END,failed,ts FROM
         (SELECT * FROM narrative WHERE agent_id=?1 AND ts>=?3 ORDER BY id DESC LIMIT ?2) ORDER BY id",
    )?;
    let mut rows = stmt.query(rusqlite::params![
        agent,
        NARRATIVE_ROWS,
        now_ms() - transcript::RETENTION_MS,
        content
    ])?;
    let mut out: Vec<Value> = Vec::new();
    while let Some(r) = rows.next()? {
        let kind: String = r.get(0)?;
        let source: String = r.get(1)?;
        let text: Option<String> = r.get(2)?;
        let ts: i64 = r.get(8)?;
        if kind == "tool" {
            let tool = json!({"name": r.get::<_, String>(5)?, "detail": r.get::<_, Option<String>>(6)?,
                "failed": r.get::<_, bool>(7)?});
            match out.last_mut().filter(|last| last["kind"] == "tool_run") {
                Some(run) => {
                    if let Some(tools) = run["tools"].as_array_mut() {
                        tools.push(tool);
                        run["count"] = json!(tools.len());
                    }
                }
                None => out.push(json!({"kind": "tool_run", "collapsed": true, "count": 1,
                    "tools": [tool], "source": source, "ts": ts})),
            }
            continue;
        }
        let mut row = json!({"kind": kind, "text": text, "source": source, "ts": ts});
        if kind == "reasoning" {
            let recorded: bool = r.get(3)?;
            row["recorded"] = json!(recorded);
            row["tokens"] = json!(r.get::<_, Option<i64>>(4)?);
            if !recorded {
                row["label"] = json!(format!("reasoning — not recorded by {source}"));
            }
        }
        out.push(row);
    }
    Ok(out)
}
