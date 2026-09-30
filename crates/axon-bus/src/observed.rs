//! Sessions open on this machine that never registered on the bus (BUS-PLAN §0, graceful
//! absence): each harness process becomes a root, matched to the session Axon ingested
//! from its working directory for the model, tokens and subagents. Observed nodes carry
//! `observed: true`; they take no messages, budgets or stops until hooks are installed.
//!
//! Only shown when Axon has ingested turns within the lookback, so a bus without Axon
//! lists exactly its registered agents.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use axon_core::model::canonicalize_model;
use axon_core::pricing::Buckets;
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::memory::Session;
use crate::store::now_ms;

/// Ingested sessions older than this are not matched to an open process.
const LOOKBACK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// A session whose last turn is this recent is working; older, it is idle.
const ACTIVE_MS: i64 = 2 * 60 * 1000;
/// Subagents shown under a session: those that ran within this window.
const SUBAGENT_WINDOW_MS: i64 = 60 * 60 * 1000;
/// A process writes its session from its start on; a resumed session began earlier.
const START_SLACK_MS: i64 = 60 * 1000;
/// Axon writes on every agent turn; the ingested sessions are re-read at most this often.
const REFRESH_EVERY: Duration = Duration::from_secs(5);

/// Keeps the ingested sessions between snapshots, re-read on a slow clock.
#[derive(Default)]
pub struct Observer {
    refreshed: Option<Instant>,
    ingested: HashMap<String, Ingested>,
}

/// Resolves a working directory to its main repository and branch.
pub type Locate<'a> = dyn FnMut(&Path) -> (Option<PathBuf>, Option<String>) + 'a;

/// One observed root: the main repository it groups under, its harness, and its tree.
pub struct Root {
    pub repo: Option<PathBuf>,
    pub harness: String,
    pub tree: Value,
}

#[derive(Default)]
struct Usage {
    model: String,
    buckets: Buckets,
    cost: Option<f64>,
    last_ts: i64,
}

/// One ingested session: its projects (cwds it worked in), main agent and subagents.
#[derive(Default)]
struct Ingested {
    harness: String,
    projects: HashSet<String>,
    main: Usage,
    subagents: Vec<(String, Usage)>,
}

impl Observer {
    pub fn roots(
        &mut self,
        conn: &Connection,
        sessions: &[Session],
        repo_of: &mut Locate<'_>,
    ) -> anyhow::Result<Vec<Root>> {
        if self.refreshed.map_or(true, |at| at.elapsed() >= REFRESH_EVERY) {
            self.ingested = ingested(conn)?;
            self.refreshed = Some(Instant::now());
        }
        roots(conn, sessions, &self.ingested, repo_of)
    }
}

fn roots(
    conn: &Connection,
    sessions: &[Session],
    ingested: &HashMap<String, Ingested>,
    repo_of: &mut Locate<'_>,
) -> anyhow::Result<Vec<Root>> {
    // No turns ingested means Axon is not running on this database (a bus-only hub, a
    // test fixture): list exactly the registered agents.
    if ingested.is_empty() {
        return Ok(Vec::new());
    }
    let registered: HashSet<i64> = conn
        .prepare("SELECT pid FROM agents WHERE pid IS NOT NULL AND status<>'closed'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut taken: HashSet<&str> = HashSet::new();
    // Newest process first, so a restarted session in the same directory claims the
    // newest transcript and an older process keeps the one it has been writing.
    let mut open: Vec<&Session> = sessions
        .iter()
        .filter(|s| !registered.contains(&s.pid))
        .collect();
    open.sort_by_key(|s| std::cmp::Reverse(s.started_ms));
    let now = now_ms();
    let mut roots = Vec::new();
    for session in open {
        let cwd = session.cwd.to_string_lossy();
        let found = ingested
            .iter()
            .filter(|(id, i)| {
                !taken.contains(id.as_str())
                    && i.harness == session.harness
                    && i.main.last_ts >= session.started_ms - START_SLACK_MS
                    && i.projects.iter().any(|p| Path::new(p).starts_with(&*cwd))
            })
            .max_by_key(|(_, i)| i.main.last_ts);
        if let Some((id, _)) = found {
            taken.insert(id);
        }
        let (repo, branch) = repo_of(&session.cwd);
        let tree = render(session, found.map(|(_, i)| i), branch, now);
        roots.push(Root {
            repo,
            harness: session.harness.to_owned(),
            tree,
        });
    }
    Ok(roots)
}

fn render(session: &Session, found: Option<&Ingested>, branch: Option<String>, now: i64) -> Value {
    let id = format!("{}-{}", session.harness, session.pid);
    let status = |last: i64| {
        if now - last < ACTIVE_MS {
            "active"
        } else {
            "idle"
        }
    };
    let children: Vec<Value> = found
        .map(|i| {
            i.subagents
                .iter()
                .filter(|(_, u)| now - u.last_ts < SUBAGENT_WINDOW_MS)
                .map(|(agent, u)| {
                    json!({
                        "id": format!("{id}/{agent}"),
                        "harness": session.harness,
                        "model": u.model,
                        "role": agent,
                        "status": status(u.last_ts),
                        "tokens": total(&u.buckets),
                        "cost_usd": u.cost,
                        "unpriced": u.cost.is_none(),
                        "shares_process": true,
                        "observed": true,
                        "narrative": [],
                        "children": [],
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let main = found.map(|i| &i.main);
    json!({
        "id": id,
        "harness": session.harness,
        "model": main.map(|u| u.model.as_str()),
        "role": "session",
        "status": main.map_or("idle", |u| status(u.last_ts)),
        "repo": session.cwd,
        "branch": branch,
        "tokens": main.map_or(0, |u| total(&u.buckets)),
        "cost_usd": main.and_then(|u| u.cost),
        "unpriced": main.is_some_and(|u| u.cost.is_none()),
        "rss": session.rss,
        "pid": session.pid,
        "last_ts": main.map(|u| u.last_ts),
        "observed": true,
        "narrative": [],
        "children": children,
    })
}

/// Every session Axon ingested within the lookback, priced in USD.
fn ingested(conn: &Connection) -> anyhow::Result<HashMap<String, Ingested>> {
    let pricing = crate::budget::usd_pricing();
    let mut stmt = conn.prepare(
        "SELECT session_id, harness, project, is_subagent, agent, model,
                sum(tokens_in), sum(tokens_out), sum(cache_read), sum(cache_write_5m),
                sum(cache_write_1h), max(ts)
         FROM usage_events WHERE ts > ?1
         GROUP BY session_id, harness, project, is_subagent, agent, model
         ORDER BY max(ts)",
    )?;
    let mut rows = stmt.query([now_ms() - LOOKBACK_MS])?;
    let mut sessions: HashMap<String, Ingested> = HashMap::new();
    while let Some(r) = rows.next()? {
        let bucket = |i| r.get::<_, i64>(i).map(|v| v.max(0) as u64);
        let buckets = Buckets {
            input: bucket(6)?,
            output: bucket(7)?,
            cache_read: bucket(8)?,
            cache_write_5m: bucket(9)?,
            cache_write_1h: bucket(10)?,
        };
        let model: String = r.get(5)?;
        let (cost, unpriced) = pricing.cost(&canonicalize_model(&model), &buckets);
        let part = Usage {
            model,
            buckets,
            cost: (!unpriced).then_some(cost),
            last_ts: r.get(11)?,
        };
        let session = sessions.entry(r.get(0)?).or_default();
        session.harness = harness_name(&r.get::<_, String>(1)?).to_owned();
        session.projects.insert(r.get(2)?);
        if r.get::<_, i64>(3)? == 0 {
            merge(&mut session.main, part);
        } else {
            let agent: String = r.get(4)?;
            match session.subagents.iter_mut().find(|(a, _)| *a == agent) {
                Some((_, usage)) => merge(usage, part),
                None => session.subagents.push((agent, part)),
            }
        }
    }
    Ok(sessions)
}

/// Fold one model group into an agent's usage; rows arrive oldest first, so the latest
/// model wins.
fn merge(into: &mut Usage, part: Usage) {
    if into.last_ts == 0 {
        *into = part;
        return;
    }
    into.buckets.input += part.buckets.input;
    into.buckets.output += part.buckets.output;
    into.buckets.cache_read += part.buckets.cache_read;
    into.buckets.cache_write_5m += part.buckets.cache_write_5m;
    into.buckets.cache_write_1h += part.buckets.cache_write_1h;
    into.cost = into.cost.zip(part.cost).map(|(a, b)| a + b);
    if part.last_ts >= into.last_ts {
        into.model = part.model;
        into.last_ts = part.last_ts;
    }
}

fn total(b: &Buckets) -> u64 {
    b.input + b.output + b.cache_read + b.cache_write_5m + b.cache_write_1h
}

/// Axon names Claude Code `claude-code`; the bus calls it `claude`.
fn harness_name(axon: &str) -> &str {
    match axon {
        "claude-code" => "claude",
        other => other,
    }
}
