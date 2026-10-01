//! The live tree (BUS-PLAN §7): repo → harness → root agent → subagents. A root groups
//! under the main repository of its cwd (a worktree under its main repo); subagents nest
//! by parent and never by repo, carrying a badge when they work in another repo.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::memory::Sampler;
use crate::observed::Observer;
use crate::store::now_ms;
use crate::{budget, chatter, transcript};

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
    let Some(cwd) = resolve(cwd) else {
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
        repo: resolve(&common).and_then(|c| c.parent().map(Path::to_path_buf)),
        branch,
    }
}

/// `path` with links resolved, spelled as harnesses report cwds: on Windows without the
/// `\\?\` prefix `canonicalize` puts on drive paths, so repo keys match session paths.
fn resolve(path: &Path) -> Option<PathBuf> {
    let resolved = path.canonicalize().ok()?;
    #[cfg(windows)]
    if let Some(plain) = resolved
        .to_str()
        .and_then(|p| p.strip_prefix(r"\\?\"))
        .filter(|p| p.as_bytes().get(1) == Some(&b':'))
    {
        return Some(PathBuf::from(plain));
    }
    Some(resolved)
}

struct Node {
    id: String,
    harness: String,
    parent_id: Option<String>,
    model: Option<String>,
    role: Option<String>,
    mission: Option<String>,
    status: String,
    pid: Option<i64>,
    checkout: Checkout,
}

/// The whole tree as the page renders it. `cache` keeps cwd lookups across snapshots;
/// `memory` holds the sampled resident memory and the open harness sessions, which
/// `observer` matches to what Axon ingested.
pub fn build(
    conn: &Connection,
    cache: &mut HashMap<String, Checkout>,
    memory: &Sampler,
    observer: &mut Observer,
    allow_content: bool,
) -> anyhow::Result<Value> {
    let rss = memory.rss();
    let mut stmt = conn.prepare(
        "SELECT a.id,a.harness,a.parent_id,a.model,a.role,a.mission,a.status,a.cwd,a.pid FROM agents a
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
                    pid: r.get(8)?,
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
    let facts = Facts {
        tokens: token_totals(conn)?,
        costs: budget::agent_costs(conn)?,
        gauges: budget::gauges(conn)?,
        // This server's own option bounds the shared lease another server may be renewing.
        content: allow_content && transcript::content_enabled(conn)?,
        rss,
    };
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
        let tree = render(conn, root, None, repo.as_deref(), &children, &facts)?;
        groups
            .entry((repo.is_none(), repo))
            .or_default()
            .entry(root.harness.clone())
            .or_default()
            .push(tree);
    }
    let mut repo_of = |cwd: &Path| {
        let found = cache
            .entry(cwd.to_string_lossy().into_owned())
            .or_insert_with(|| checkout(cwd));
        (found.repo.clone(), found.branch.clone())
    };
    for root in observer.roots(conn, memory.sessions(), &mut repo_of, facts.content)? {
        groups
            .entry((root.repo.is_none(), root.repo))
            .or_default()
            .entry(root.harness)
            .or_default()
            .push(root.tree);
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
    Ok(json!({
        "repos": repos,
        "messages": chatter::messages(conn, facts.content)?,
        "links": chatter::links(conn)?,
        "content": facts.content,
        "currency": currency(),
    }))
}

/// Costs in the snapshot are USD; the page shows them in the operator's display currency.
fn currency() -> Value {
    let pricing = axon_core::pricing::Pricing::bundled();
    let code = pricing
        .display_currency
        .clone()
        .unwrap_or_else(|| "USD".into());
    json!({"code": code, "per_usd": pricing.fx()})
}

fn token_totals(conn: &Connection) -> rusqlite::Result<HashMap<String, i64>> {
    let mut stmt = conn.prepare(
        "SELECT agent_id, sum(input_tokens+output_tokens+cache_read_tokens+cache_write_tokens
                              +cache_write_1h_tokens) FROM usage GROUP BY agent_id",
    )?;
    let totals = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    totals.collect()
}

/// Per-agent numbers gathered once per snapshot.
struct Facts<'a> {
    tokens: HashMap<String, i64>,
    costs: HashMap<String, Option<f64>>,
    gauges: HashMap<String, Value>,
    content: bool,
    rss: &'a HashMap<i64, u64>,
}

fn render(
    conn: &Connection,
    node: &Node,
    parent_pid: Option<i64>,
    group_repo: Option<&Path>,
    children: &HashMap<&str, Vec<&Node>>,
    facts: &Facts,
) -> anyhow::Result<Value> {
    let kids = children
        .get(node.id.as_str())
        .map_or(&[][..], Vec::as_slice);
    let kids: Vec<Value> = kids
        .iter()
        .map(|child| render(conn, child, node.pid, group_repo, children, facts))
        .collect::<anyhow::Result<_>>()?;
    let repo = node.checkout.repo.as_deref();
    let repo_badge = repo.filter(|r| Some(*r) != group_repo);
    // A subagent inside its parent's process (Claude's) shares that memory; only the
    // process owner reports it, so sums never count a process twice.
    let shares_process = node.pid.is_some() && node.pid == parent_pid;
    let rss = node
        .pid
        .filter(|_| !shares_process)
        .and_then(|pid| facts.rss.get(&pid));
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
        "tokens": facts.tokens.get(&node.id).copied().unwrap_or(0),
        "cost_usd": facts.costs.get(&node.id).copied().flatten(),
        "unpriced": facts.costs.get(&node.id).is_some_and(Option::is_none),
        "budget": facts.gauges.get(&node.id),
        "rss": rss,
        "shares_process": shares_process,
        "narrative": narrative(conn, &node.id, facts.content)?,
        "children": kids,
    }))
}

/// One narrative row as the page shows it. Stored rows and observed transcripts both
/// come through `push_line`, so both collapse tool runs and label reasoning alike.
pub(crate) struct Line<'a> {
    pub kind: &'a str,
    pub source: &'a str,
    pub text: Option<String>,
    pub recorded: Option<bool>,
    pub tokens: Option<i64>,
    /// Name, detail and whether it failed, for a tool call.
    pub tool: Option<(String, Option<String>, bool)>,
    pub ts: i64,
}

pub(crate) fn push_line(out: &mut Vec<Value>, line: Line) {
    let Line {
        kind,
        source,
        text,
        recorded,
        tokens,
        tool,
        ts,
    } = line;
    if let Some((name, detail, failed)) = tool {
        let tool = json!({"name": name, "detail": detail, "failed": failed});
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
        return;
    }
    let mut row = json!({"kind": kind, "text": text, "source": source, "ts": ts});
    if kind == "reasoning" {
        let recorded = recorded.unwrap_or(false);
        row["recorded"] = json!(recorded);
        row["tokens"] = json!(tokens);
        if !recorded {
            row["label"] = json!(format!("reasoning — not recorded by {source}"));
        }
    }
    out.push(row);
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
        let tool = if kind == "tool" {
            Some((r.get(5)?, r.get(6)?, r.get(7)?))
        } else {
            None
        };
        push_line(
            &mut out,
            Line {
                kind: &kind,
                source: &r.get::<_, String>(1)?,
                text: r.get(2)?,
                recorded: r.get(3)?,
                tokens: r.get(4)?,
                tool,
                ts: r.get(8)?,
            },
        );
    }
    Ok(out)
}
