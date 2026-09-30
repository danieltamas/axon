//! Budgets (BUS-PLAN §6): a ceiling on a tree (set on its root) or on one agent. The gate
//! warns the root at 80%, stops every member at 100%, and fails closed on stale usage.

use anyhow::{bail, Context};
use axon_core::model::canonicalize_model;
use axon_core::pricing::{Buckets, Pricing};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::msg;
use crate::store::{append_event, now_ms};

/// Usage older than this on an active budgeted tree is stale (§6).
const STALE_AFTER_MS: i64 = 120_000;

struct Budget {
    scope_id: String,
    kind: String,
    tokens_max: Option<i64>,
    usd_max: Option<f64>,
    state: String,
    stale_warned_at: Option<i64>,
}

struct Totals {
    tokens: i64,
    /// None when any usage row has no price: an unknown model is not free.
    cost_usd: Option<f64>,
    newest_ts: Option<i64>,
}

/// `50Ktok`, `2Mtok`, `1500`: a token count with an optional K/M multiplier.
pub fn parse_tokens(text: &str) -> anyhow::Result<i64> {
    let number = text.strip_suffix("tok").unwrap_or(text);
    let (digits, multiplier) = match number.chars().last() {
        Some('K' | 'k') => (&number[..number.len() - 1], 1_000),
        Some('M' | 'm') => (&number[..number.len() - 1], 1_000_000),
        _ => (number, 1),
    };
    let count: i64 = digits
        .parse()
        .with_context(|| format!("invalid token ceiling {text}; use e.g. 50Ktok or 2Mtok"))?;
    Ok(count * multiplier)
}

/// Set (or replace) the ceiling on `scope`: the whole tree when it is a root, else the
/// agent alone. Replacing it re-arms the gate and lifts this budget's stops. Returns the
/// members whose doorbells to silence once the transaction commits.
pub fn set(
    conn: &Connection,
    scope: &str,
    tokens_max: Option<i64>,
    usd_max: Option<f64>,
) -> anyhow::Result<Vec<String>> {
    let parent: Option<String> = conn
        .query_row("SELECT parent_id FROM agents WHERE id=?1", [scope], |r| {
            r.get(0)
        })
        .optional()?
        .with_context(|| format!("agent {scope} is not registered"))?;
    if tokens_max.is_none() && usd_max.is_none() {
        bail!("a budget needs a token ceiling or --usd");
    }
    let kind = if parent.is_none() { "tree" } else { "agent" };
    conn.execute(
        "INSERT INTO budgets (scope_id,kind,tokens_max,usd_max,state) VALUES (?1,?2,?3,?4,'ok')
         ON CONFLICT(scope_id) DO UPDATE SET kind=excluded.kind, tokens_max=excluded.tokens_max,
         usd_max=excluded.usd_max, state='ok', stale_warned_at=NULL",
        params![scope, kind, tokens_max, usd_max],
    )?;
    let members = members(conn, scope, kind)?;
    for member in &members {
        msg::clear_bus_stops(conn, scope, member)?;
    }
    let payload = json!({"kind": kind, "tokens_max": tokens_max, "usd_max": usd_max});
    append_event(conn, scope, "budget", scope, &payload.to_string())?;
    Ok(members)
}

fn load(conn: &Connection, scope: &str) -> rusqlite::Result<Option<Budget>> {
    conn.query_row(
        "SELECT scope_id,kind,tokens_max,usd_max,state,stale_warned_at FROM budgets WHERE scope_id=?1",
        [scope],
        |r| {
            Ok(Budget {
                scope_id: r.get(0)?,
                kind: r.get(1)?,
                tokens_max: r.get(2)?,
                usd_max: r.get(3)?,
                state: r.get(4)?,
                stale_warned_at: r.get(5)?,
            })
        },
    )
    .optional()
}

fn members(conn: &Connection, scope: &str, kind: &str) -> rusqlite::Result<Vec<String>> {
    if kind == "agent" {
        return Ok(vec![scope.to_owned()]);
    }
    let mut stmt =
        conn.prepare("SELECT id FROM agents WHERE root_id=?1 ORDER BY started_at, id")?;
    let ids = stmt.query_map([scope], |r| r.get(0))?;
    ids.collect()
}

/// Usage over the scope's members, priced per model from axon-core's bundled USD rates.
/// With `priced` false the cost is left unknown, sparing the hook path the rate table.
fn totals(conn: &Connection, budget_scope: &str, kind: &str, priced: bool) -> anyhow::Result<Totals> {
    let member_filter = if kind == "agent" {
        "u.agent_id=?1"
    } else {
        "u.agent_id IN (SELECT id FROM agents WHERE root_id=?1)"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT u.model, sum(u.input_tokens), sum(u.output_tokens), sum(u.cache_read_tokens),
                sum(u.cache_write_tokens), sum(u.cache_write_1h_tokens),
                sum(u.cost_usd), count(u.cost_usd), count(*), max(u.ts)
         FROM usage u WHERE {member_filter} GROUP BY u.model"
    ))?;
    // Budgets are in USD, so the display-currency FX is not applied.
    let pricing = priced.then(|| {
        let mut pricing = Pricing::bundled();
        pricing.fx_to_display = None;
        pricing
    });
    let mut totals = Totals {
        tokens: 0,
        cost_usd: priced.then_some(0.0),
        newest_ts: None,
    };
    let mut rows = stmt.query([budget_scope])?;
    while let Some(r) = rows.next()? {
        let model: String = r.get(0)?;
        let bucket = |i| r.get::<_, i64>(i).map(|v| v.max(0) as u64);
        let buckets = Buckets {
            input: bucket(1)?,
            output: bucket(2)?,
            cache_read: bucket(3)?,
            cache_write_5m: bucket(4)?,
            cache_write_1h: bucket(5)?,
        };
        totals.tokens += (buckets.input
            + buckets.output
            + buckets.cache_read
            + buckets.cache_write_5m
            + buckets.cache_write_1h) as i64;
        let reported: f64 = r.get::<_, Option<f64>>(6)?.unwrap_or(0.0);
        let (reported_rows, all_rows): (i64, i64) = (r.get(7)?, r.get(8)?);
        let row_cost = if reported_rows == all_rows {
            Some(reported)
        } else if let Some(pricing) = &pricing {
            // Rows without a reported cost are priced from their buckets; a model without
            // rates makes the whole total unknown. Mixed groups are not expected per model.
            let (cost, unpriced) = pricing.cost(&canonicalize_model(&model), &buckets);
            (!unpriced).then_some(cost)
        } else {
            None
        };
        totals.cost_usd = totals.cost_usd.zip(row_cost).map(|(a, b)| a + b);
        totals.newest_ts = totals.newest_ts.max(r.get(9)?);
    }
    Ok(totals)
}

/// What one agent's usage cost in USD; None when it has no usage or any of it is unpriced.
pub fn agent_cost(conn: &Connection, agent: &str) -> anyhow::Result<Option<f64>> {
    let totals = totals(conn, agent, "agent", true)?;
    Ok(totals.cost_usd.filter(|_| totals.tokens > 0))
}

/// `budget show`: the scope's ceiling, totals and state.
pub fn show(conn: &Connection, scope: &str) -> anyhow::Result<Value> {
    let budget = load(conn, scope)?.with_context(|| format!("{scope} has no budget"))?;
    let totals = totals(conn, scope, &budget.kind, true)?;
    Ok(json!({
        "scope_id": budget.scope_id,
        "kind": budget.kind,
        "tokens": totals.tokens,
        "tokens_max": budget.tokens_max,
        "cost_usd": totals.cost_usd,
        "usd_max": budget.usd_max,
        "unpriced": totals.cost_usd.is_none(),
        "state": budget.state,
    }))
}

/// Fraction of the ceiling used; the USD ceiling counts only when the cost is known.
fn used(budget: &Budget, totals: &Totals) -> f64 {
    let by_tokens = budget
        .tokens_max
        .map_or(0.0, |max| totals.tokens as f64 / max as f64);
    let by_usd = budget
        .usd_max
        .zip(totals.cost_usd)
        .map_or(0.0, |(max, cost)| cost / max);
    by_tokens.max(by_usd)
}

/// The gate for one tool call of `agent`: a denial reason, or None to allow. Checks the
/// agent's own budget and its tree's. Call inside the hook's write transaction.
pub fn check(conn: &Connection, agent: &str) -> anyhow::Result<Option<String>> {
    let root: String = conn.query_row("SELECT root_id FROM agents WHERE id=?1", [agent], |r| {
        r.get(0)
    })?;
    for scope in [agent, root.as_str()] {
        if let Some(budget) = load(conn, scope)? {
            if let Some(reason) = check_scope(conn, &budget, &root)? {
                return Ok(Some(reason));
            }
        }
        if scope == root {
            break;
        }
    }
    Ok(None)
}

fn check_scope(conn: &Connection, budget: &Budget, root: &str) -> anyhow::Result<Option<String>> {
    let scope = budget.scope_id.as_str();
    let totals = totals(conn, scope, &budget.kind, budget.usd_max.is_some())?;
    let used = used(budget, &totals);
    let spent = format!(
        "{} tokens{} of {}",
        totals.tokens,
        totals
            .cost_usd
            .map(|c| format!(" (${c:.2})"))
            .unwrap_or_default(),
        ceiling(budget)
    );
    let exhausted = format!("Budget of {scope} is exhausted: {spent}. Stop now.");
    if budget.state == "stopped" {
        return Ok(Some(exhausted));
    }
    // Unknown is not free: a USD-only ceiling over unpriced usage cannot be enforced.
    if budget.tokens_max.is_none() && totals.cost_usd.is_none() && totals.tokens > 0 {
        return Ok(Some(format!(
            "Budget of {scope} is USD-only, but usage from an unpriced model makes its cost \
             unknown ({} tokens); add a token ceiling with `axon-bus budget set {scope} <N>tok`.",
            totals.tokens
        )));
    }
    if used >= 1.0 {
        // The denial stands even if recording the stop fails (§2: this gate fails closed).
        if let Err(err) = stop_members(conn, budget, &totals, &spent) {
            eprintln!("axon-bus: budget of {scope} reached, but recording its stop failed: {err:#}");
        }
        return Ok(Some(exhausted));
    }
    if used >= 0.8 && budget.state == "ok" {
        set_state(conn, scope, "warned")?;
        msg::from_bus(
            conn,
            scope,
            root,
            "sync",
            &format!("budget of {scope} at 80%: {spent}"),
        )?;
    }
    let active: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agents WHERE root_id=?1 AND status='active')",
        [root],
        |r| r.get(0),
    )?;
    let stale = active
        && totals
            .newest_ts
            .is_some_and(|ts| now_ms() - ts > STALE_AFTER_MS);
    if !stale {
        if budget.stale_warned_at.is_some() {
            conn.execute(
                "UPDATE budgets SET stale_warned_at=NULL WHERE scope_id=?1",
                [scope],
            )?;
        }
        return Ok(None);
    }
    if budget.stale_warned_at.is_none() {
        conn.execute(
            "UPDATE budgets SET stale_warned_at=?2 WHERE scope_id=?1",
            params![scope, now_ms()],
        )?;
        msg::from_bus(
            conn,
            scope,
            root,
            "sync",
            &format!(
                "usage data for {scope} is stale (over 120 s old); spend may be unseen: {spent}"
            ),
        )?;
        return Ok(None);
    }
    // The 10% margin covers spend the bus cannot see while usage is stale (§6).
    if used >= 0.9 {
        return Ok(Some(format!(
            "Usage data for {scope} is stale and the last known total is at 90% of its budget \
             ({spent}); tool calls are held until fresh usage arrives."
        )));
    }
    Ok(None)
}

fn stop_members(conn: &Connection, budget: &Budget, totals: &Totals, spent: &str) -> anyhow::Result<()> {
    let scope = budget.scope_id.as_str();
    set_state(conn, scope, "stopped")?;
    let body = format!("budget of {scope} reached: {spent}");
    for member in members(conn, scope, &budget.kind)? {
        msg::from_bus(conn, scope, &member, "stop", &body)?;
    }
    append_event(conn, msg::BUS, "budget_stop", scope, &json!({"tokens": totals.tokens}).to_string())
}

fn ceiling(budget: &Budget) -> String {
    match (budget.tokens_max, budget.usd_max) {
        (Some(tokens), Some(usd)) => format!("{tokens} tokens / ${usd:.2}"),
        (Some(tokens), None) => format!("{tokens} tokens"),
        (None, Some(usd)) => format!("${usd:.2}"),
        (None, None) => "no ceiling".to_owned(),
    }
}

fn set_state(conn: &Connection, scope: &str, state: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE budgets SET state=?2 WHERE scope_id=?1",
        params![scope, state],
    )
}

#[cfg(test)]
mod tests {
    use super::parse_tokens;

    #[test]
    fn token_ceilings_parse_k_and_m() {
        assert_eq!(parse_tokens("50Ktok").unwrap(), 50_000);
        assert_eq!(parse_tokens("2Mtok").unwrap(), 2_000_000);
        assert_eq!(parse_tokens("1500").unwrap(), 1_500);
        assert!(parse_tokens("lots").is_err());
    }
}
