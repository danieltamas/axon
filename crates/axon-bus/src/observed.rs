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
use crate::tail::{Said, Tails};

/// Ingested sessions older than this are not matched to an open process.
const LOOKBACK_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// A session whose last turn is this recent is working; older, it is idle.
const ACTIVE_MS: i64 = 2 * 60 * 1000;
/// Subagents shown under a session: those that ran within this window.
const SUBAGENT_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
/// A process writes its session from its start on; a resumed session began earlier.
const START_SLACK_MS: i64 = 60 * 1000;
/// A session's activity profile covers this many hours, the current one last.
const HOURS: usize = 24;
const HOUR_MS: i64 = 3600 * 1000;
/// Skills listed per session, most used first.
const SKILLS: usize = 6;
/// Axon writes on every agent turn; the ingested sessions are re-read at most this often.
const REFRESH_EVERY: Duration = Duration::from_secs(5);

/// Keeps the ingested sessions between snapshots, re-read on a slow clock.
#[derive(Default)]
pub struct Observer {
    refreshed: Option<Instant>,
    ingested: HashMap<String, Ingested>,
    tails: Tails,
    /// The session each process (pid, start) was last matched to, so a match the evidence
    /// can no longer separate is kept rather than re-guessed.
    matched: HashMap<(i64, i64), String>,
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
    first_ts: i64,
    last_ts: i64,
}

/// One ingested session: its projects (cwds it worked in), main agent and subagents.
#[derive(Default)]
struct Ingested {
    harness: String,
    projects: HashSet<String>,
    main: Usage,
    subagents: Vec<(String, Usage)>,
    activity: Activity,
}

/// What a session did over the last `HOURS`: turns per hour, lines it changed, skills.
#[derive(Default)]
struct Activity {
    hours: [u32; HOURS],
    turns: u32,
    lines_added: i64,
    lines_removed: i64,
    skills: HashMap<String, u32>,
}

impl Observer {
    pub fn roots(
        &mut self,
        conn: &Connection,
        sessions: &[Session],
        repo_of: &mut Locate<'_>,
        content: bool,
    ) -> anyhow::Result<Vec<Root>> {
        if self
            .refreshed
            .map_or(true, |at| at.elapsed() >= REFRESH_EVERY)
        {
            self.ingested = ingested(conn)?;
            activity(conn, &mut self.ingested)?;
            self.refreshed = Some(Instant::now());
        }
        roots(
            conn,
            sessions,
            &self.ingested,
            &mut self.tails,
            &mut self.matched,
            repo_of,
            content,
        )
    }
}

fn roots(
    conn: &Connection,
    sessions: &[Session],
    ingested: &HashMap<String, Ingested>,
    tails: &mut Tails,
    matched: &mut HashMap<(i64, i64), String>,
    repo_of: &mut Locate<'_>,
    content: bool,
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
    let open: Vec<&Session> = sessions
        .iter()
        .filter(|s| !registered.contains(&s.pid))
        .collect();
    let assigned = assign(&open, ingested, matched);
    let now = now_ms();
    let mut roots = Vec::new();
    for session in open {
        let found = assigned
            .get(&session.pid)
            .and_then(|id| ingested.get_key_value(*id));
        let (repo, branch) = repo_of(&session.cwd);
        let said = found.and_then(|(id, _)| tails.read(session.harness, id, content));
        let tree = render(session, found.map(|(_, i)| i), said, branch, now);
        roots.push(Root {
            repo,
            harness: session.harness.to_owned(),
            tree,
        });
    }
    Ok(roots)
}

/// Which ingested session each open process is writing, on evidence only. A session that
/// began after a process started, and before any other candidate process did, can only be
/// that process's; once matched, a process drops out as an owner for the rest. A session
/// the evidence cannot place stays unmatched, unless it was this process's match before:
/// a wrong transcript under a session is worse than none.
fn assign<'a>(
    open: &[&Session],
    ingested: &'a HashMap<String, Ingested>,
    matched: &mut HashMap<(i64, i64), String>,
) -> HashMap<i64, &'a str> {
    let candidates: Vec<Vec<&'a str>> = open
        .iter()
        .map(|session| {
            let cwd = session.cwd.to_string_lossy();
            ingested
                .iter()
                .filter(|(_, i)| {
                    i.harness == session.harness
                        && i.main.last_ts >= session.started_ms - START_SLACK_MS
                        && i.projects.iter().any(|p| Path::new(p).starts_with(&*cwd))
                })
                .map(|(id, _)| id.as_str())
                .collect()
        })
        .collect();
    // The processes that could have written `id`: those that had started when it began,
    // or, for a session resumed from before any of them, every candidate process.
    let owners = |id: &str| -> Vec<usize> {
        let first = ingested[id].main.first_ts;
        let all: Vec<usize> = (0..open.len())
            .filter(|&p| candidates[p].contains(&id))
            .collect();
        let started: Vec<usize> = all
            .iter()
            .copied()
            .filter(|&p| open[p].started_ms - START_SLACK_MS <= first)
            .collect();
        if started.is_empty() {
            all
        } else {
            started
        }
    };
    let mut by_process: HashMap<usize, &'a str> = HashMap::new();
    let mut taken: HashSet<&str> = HashSet::new();
    loop {
        let mut progressed = false;
        for (p, mine) in candidates.iter().enumerate() {
            if by_process.contains_key(&p) {
                continue;
            }
            let only_mine = mine
                .iter()
                .copied()
                .filter(|id| !taken.contains(id))
                .filter(|id| {
                    owners(id)
                        .iter()
                        .all(|&q| q == p || by_process.contains_key(&q))
                })
                .max_by_key(|id| ingested[*id].main.last_ts);
            if let Some(id) = only_mine {
                by_process.insert(p, id);
                taken.insert(id);
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    for (p, session) in open.iter().enumerate() {
        if by_process.contains_key(&p) {
            continue;
        }
        let before = matched.get(&(session.pid, session.started_ms));
        if let Some(&id) = before.and_then(|id| candidates[p].iter().find(|c| **c == id.as_str())) {
            if taken.insert(id) {
                by_process.insert(p, id);
            }
        }
    }
    matched.clear();
    for (&p, &id) in &by_process {
        matched.insert((open[p].pid, open[p].started_ms), id.to_owned());
    }
    by_process
        .into_iter()
        .map(|(p, id)| (open[p].pid, id))
        .collect()
}

fn render(
    session: &Session,
    found: Option<&Ingested>,
    said: Option<Said<'_>>,
    branch: Option<String>,
    now: i64,
) -> Value {
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
    let written = said.as_ref().map_or(0, |s| s.written_ms);
    json!({
        "id": id,
        "harness": session.harness,
        "model": main.map(|u| u.model.as_str()),
        "role": "session",
        "mission": said.as_ref().and_then(|s| s.prompt),
        "status": main.map_or("idle", |u| status(u.last_ts.max(written))),
        "repo": session.cwd,
        "branch": branch,
        "tokens": main.map_or(0, |u| total(&u.buckets)),
        "cost_usd": main.and_then(|u| u.cost),
        "unpriced": main.is_some_and(|u| u.cost.is_none()),
        "rss": session.rss,
        "pid": session.pid,
        "last_ts": main.map(|u| u.last_ts.max(written)),
        "observed": true,
        "activity": found.map(|i| activity_json(&i.activity)),
        "narrative": said.map_or(&[][..], |s| s.rows),
        "children": children,
    })
}

fn activity_json(a: &Activity) -> Value {
    let mut skills: Vec<(&String, &u32)> = a.skills.iter().collect();
    skills.sort_by(|x, y| y.1.cmp(x.1).then_with(|| x.0.cmp(y.0)));
    let skills: Vec<Value> = skills
        .into_iter()
        .take(SKILLS)
        .map(|(name, count)| json!({"name": name, "count": count}))
        .collect();
    json!({
        "hours": a.hours,
        "turns": a.turns,
        "lines_added": a.lines_added,
        "lines_removed": a.lines_removed,
        "skills": skills,
    })
}

/// Fold the last `HOURS` of turns into each ingested session, in hours aligned to the
/// clock so a bar keeps its place between snapshots.
fn activity(conn: &Connection, sessions: &mut HashMap<String, Ingested>) -> anyhow::Result<()> {
    let since = (now_ms() / HOUR_MS - (HOURS as i64 - 1)) * HOUR_MS;
    let mut stmt = conn.prepare(
        "SELECT session_id, (ts - ?1) / ?2, count(*), sum(loc_added), sum(loc_removed)
         FROM usage_events WHERE ts >= ?1 GROUP BY 1, 2",
    )?;
    let mut rows = stmt.query(rusqlite::params![since, HOUR_MS])?;
    while let Some(r) = rows.next()? {
        let Some(session) = sessions.get_mut(&r.get::<_, String>(0)?) else {
            continue;
        };
        let hour = r.get::<_, i64>(1)?.clamp(0, HOURS as i64 - 1) as usize;
        let turns: u32 = r.get(2)?;
        let a = &mut session.activity;
        a.hours[hour] += turns;
        a.turns += turns;
        a.lines_added += r.get::<_, i64>(3)?;
        a.lines_removed += r.get::<_, i64>(4)?;
    }
    let mut stmt = conn.prepare(
        "SELECT session_id, skills, count(*) FROM usage_events
         WHERE ts >= ?1 AND skills NOT IN ('', '[]') GROUP BY 1, 2",
    )?;
    let mut rows = stmt.query([since])?;
    while let Some(r) = rows.next()? {
        let Some(session) = sessions.get_mut(&r.get::<_, String>(0)?) else {
            continue;
        };
        let count: u32 = r.get(2)?;
        let names: Vec<String> = serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or_default();
        for name in names {
            *session.activity.skills.entry(name).or_default() += count;
        }
    }
    Ok(())
}

/// Every session Axon ingested within the lookback, priced in USD.
fn ingested(conn: &Connection) -> anyhow::Result<HashMap<String, Ingested>> {
    let pricing = crate::budget::usd_pricing();
    let mut stmt = conn.prepare(
        "SELECT session_id, harness, project, is_subagent, agent, model,
                sum(tokens_in), sum(tokens_out), sum(cache_read), sum(cache_write_5m),
                sum(cache_write_1h), min(ts), max(ts)
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
            first_ts: r.get(11)?,
            last_ts: r.get(12)?,
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
    into.first_ts = into.first_ts.min(part.first_ts);
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
