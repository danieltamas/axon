//! The task list the dashboard and `axon bus tasks` show (BUS-PLAN §3c D–E): tasks with a
//! turn in the range, summing only those turns, newest first. Costs come from the turns as
//! ingest priced them, so the tasks of a range add up to that range's usage total.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use rusqlite::{params, Connection};
use serde_json::{json, Value};

/// The most tasks one list holds.
const LIST_MAX: usize = 200;

#[derive(Default)]
struct Totals {
    measured: f64,
    credits: f64,
    turns: i64,
    agents: BTreeSet<String>,
    harnesses: BTreeSet<String>,
    tools_unpriced: BTreeSet<String>,
}

struct Task {
    id: String,
    kind: String,
    key: Option<String>,
    name: String,
    session: Option<String>,
    opened_at: i64,
    closed_at: Option<i64>,
}

/// Whether a turn's project lies in `repo`: its repository, worktrees resolving to the main
/// one, or by path when the project is no longer on disk.
fn in_repo(project: &str, repo: &str, cache: &mut HashMap<String, bool>) -> bool {
    *cache.entry(project.to_owned()).or_insert_with(|| {
        match crate::snapshot::repo_of(Path::new(project)) {
            Some(found) => found == Path::new(repo),
            None => project == repo || project.starts_with(&format!("{repo}/")),
        }
    })
}

/// `range` is a dashboard range (`today`, `7d`, `30d`, `all`); `repo` keeps only turns run
/// inside that repository. The newest tasks are kept, or with `by_cost` the costliest.
pub fn list(
    conn: &Connection,
    range: &str,
    repo: Option<&str>,
    by_cost: bool,
) -> rusqlite::Result<Vec<Value>> {
    super::ensure(conn)?;
    let since = axon_core::range_start_ms(range);
    let now = crate::store::now_ms();
    let mut stmt = conn.prepare(
        "SELECT tt.task_id, u.cost_eur, u.cost_credits, coalesce(tt.agent_id, u.agent), u.harness,
                u.tools_unpriced, u.project
         FROM task_turns tt JOIN usage_events u ON u.id=tt.event_id WHERE u.ts>=?1",
    )?;
    let mut totals: HashMap<String, Totals> = HashMap::new();
    let mut repos = HashMap::new();
    let mut rows = stmt.query([since])?;
    while let Some(r) = rows.next()? {
        let project: String = r.get(6)?;
        if repo.is_some_and(|repo| !in_repo(&project, repo, &mut repos)) {
            continue;
        }
        let total = totals.entry(r.get(0)?).or_default();
        total.measured += r.get::<_, f64>(1)?;
        total.credits += r.get::<_, Option<f64>>(2)?.unwrap_or(0.0);
        total.turns += 1;
        total.agents.insert(r.get(3)?);
        total.harnesses.insert(r.get(4)?);
        let tools: Vec<String> = serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default();
        total.tools_unpriced.extend(tools);
    }
    drop(rows);

    let mut stmt =
        conn.prepare("SELECT id,kind,key,name,session_id,opened_at,closed_at FROM tasks")?;
    let mut tasks = stmt
        .query_map([], |r| {
            Ok(Task {
                id: r.get(0)?,
                kind: r.get(1)?,
                key: r.get(2)?,
                name: r.get(3)?,
                session: r.get(4)?,
                opened_at: r.get(5)?,
                closed_at: r.get(6)?,
            })
        })?
        .filter_map(Result::ok)
        // An open declared task is listed before its first turn, but only over all time.
        .filter(|t| {
            totals.contains_key(&t.id)
                || (range == "all"
                    && repo.is_none()
                    && t.kind == "declared"
                    && t.closed_at.is_none())
        })
        .collect::<Vec<_>>();
    let cost = |t: &Task| totals.get(&t.id).map_or(0.0, |total| total.measured);
    if by_cost {
        tasks.sort_by(|a, b| cost(b).total_cmp(&cost(a)).then_with(|| a.id.cmp(&b.id)));
        tasks.truncate(LIST_MAX);
    }
    tasks.sort_by(|a, b| b.opened_at.cmp(&a.opened_at).then_with(|| a.id.cmp(&b.id)));
    tasks.truncate(LIST_MAX);
    // Names kept from prompts are shown only while content capture is on.
    let capture = crate::transcript::content_enabled(conn)?;

    let mut out = Vec::with_capacity(tasks.len());
    for task in tasks {
        let total = totals.remove(&task.id).unwrap_or_default();
        let human_wait_ms = human_wait(conn, &task, now)?;
        out.push(json!({
            "id": task.id,
            "kind": task.kind,
            "key": task.key,
            "name": if task.kind == "request" && !capture {
                super::time_name(task.opened_at)
            } else {
                task.name
            },
            "opened_at": task.opened_at,
            "closed_at": task.closed_at,
            "elapsed_ms": task.closed_at.unwrap_or(now) - task.opened_at,
            "cost": {
                "measured": total.measured,
                // No compute rate is configured for local models yet, so nothing is estimated.
                "estimated": 0.0,
                "credits": total.credits,
                "tools_unpriced": total.tools_unpriced,
            },
            "turns": total.turns,
            "agents": total.agents,
            "harnesses": total.harnesses,
            // Errored turns are not tracked yet, so retries cost nothing here.
            "retries_cost": 0.0,
            "human_wait_ms": human_wait_ms,
        }));
    }
    Ok(out)
}

/// How long the task waited on the operator. A request: from its last turn to the next
/// prompt. A declared task: the gap before each prompt in the taker's session while open.
fn human_wait(conn: &Connection, task: &Task, now: i64) -> rusqlite::Result<i64> {
    let mut stmt = conn.prepare_cached(
        "SELECT u.ts FROM usage_events u JOIN task_turns tt ON tt.event_id=u.id
         WHERE tt.task_id=?1 ORDER BY u.ts",
    )?;
    let turns = stmt
        .query_map([&task.id], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // The task's last turn at or after its opening and before `prompt`.
    let last_before = |prompt: i64| {
        let end = turns.partition_point(|ts| *ts < prompt);
        turns[..end]
            .last()
            .copied()
            .filter(|ts| *ts >= task.opened_at)
    };
    if task.kind == "request" {
        let Some(closed) = task.closed_at else {
            return Ok(0);
        };
        return Ok(last_before(closed).map_or(0, |last| closed - last));
    }
    let Some(session) = &task.session else {
        return Ok(0);
    };
    let mut stmt = conn.prepare_cached(
        "SELECT opened_at FROM tasks WHERE kind='request' AND session_id=?1
         AND opened_at>?2 AND opened_at<?3",
    )?;
    let prompts = stmt
        .query_map(
            params![session, task.opened_at, task.closed_at.unwrap_or(now)],
            |r| r.get::<_, i64>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(prompts
        .into_iter()
        .filter_map(|prompt| last_before(prompt).map(|last| prompt - last))
        .sum())
}
