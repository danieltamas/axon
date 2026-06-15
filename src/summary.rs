//! The `--scan-only` JSON summary (DESIGN.md §14 M1): headline totals plus per-model and
//! per-agent breakdowns, the unpriced-model list (surfaced loudly, never a silent €0), and
//! the attribution honesty gauge (§15.3) — the share of output tokens we could **not**
//! attribute to a named agent.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::model::Event;
use crate::rtk::RtkSavings;

#[derive(Debug, Clone, Serialize)]
pub struct ModelStat {
    pub model: String,
    pub events: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_eur: f64,
    pub unpriced: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentStat {
    pub agent: String,
    pub is_subagent: bool,
    pub events: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_eur: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct HarnessStat {
    pub harness: String,
    pub events: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cost_eur: f64,
}

/// One row of the live feed — a real recent turn, trimmed to what the dashboard shows
/// (no project/path, so nothing identifying leaks into the feed).
#[derive(Debug, Clone, Serialize)]
pub struct RecentEvent {
    pub ts: i64,
    pub harness: String,
    pub agent: String,
    pub model: String,
    pub tokens_out: u64,
    pub cost_eur: f64,
    pub duration_ms: Option<u64>,
}

/// The `n` most recent events (newest first) as feed rows. Cheap: collect + sort by ts.
pub fn recent_events<'a>(
    events: impl IntoIterator<Item = &'a Event>,
    n: usize,
) -> Vec<RecentEvent> {
    let mut v: Vec<&Event> = events.into_iter().collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.ts)); // newest first
    v.into_iter()
        .take(n)
        .map(|e| RecentEvent {
            ts: e.ts,
            harness: e.harness.as_str().to_string(),
            agent: e.agent.clone(),
            model: e.model.clone(),
            tokens_out: e.tokens_out,
            cost_eur: e.cost_eur,
            duration_ms: e.duration_ms,
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub events: u64,
    pub sessions: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub loc_added: u64,
    pub loc_removed: u64,
    pub cost_eur: f64,
    /// Canonical ids of models missing from the pricing map (their cost is a floor).
    pub unpriced_models: Vec<String>,
    /// Honesty gauge (§15.3): % of output tokens attributed to `unknown-sub`/`unknown`.
    pub unattributed_token_pct: f64,
    pub by_model: Vec<ModelStat>,
    pub by_agent: Vec<AgentStat>,
    pub by_harness: Vec<HarnessStat>,
    /// Optional RTK (token-saver) analytics; `None` if rtk is not installed.
    pub rtk: Option<RtkSavings>,
    /// Spend in the current local day / week / month (display currency).
    pub today_cost_eur: f64,
    pub week_cost_eur: f64,
    pub month_cost_eur: f64,
    /// Optional soft budget caps from `config.toml`.
    pub budget_day_eur: Option<f64>,
    pub budget_week_eur: Option<f64>,
    pub budget_month_eur: Option<f64>,
    /// Newest turns for the live feed (filled by the server per request; empty in `--scan-only`).
    pub recent: Vec<RecentEvent>,
}

/// Aggregate any set of events (a slice, or a filtered iterator for a time range).
pub fn build_summary<'a>(events: impl IntoIterator<Item = &'a Event>) -> Summary {
    let mut s = Summary {
        events: 0,
        sessions: 0,
        tokens_in: 0,
        tokens_out: 0,
        cache_read: 0,
        cache_write_5m: 0,
        cache_write_1h: 0,
        loc_added: 0,
        loc_removed: 0,
        cost_eur: 0.0,
        unpriced_models: Vec::new(),
        unattributed_token_pct: 0.0,
        by_model: Vec::new(),
        by_agent: Vec::new(),
        by_harness: Vec::new(),
        rtk: None,
        today_cost_eur: 0.0,
        week_cost_eur: 0.0,
        month_cost_eur: 0.0,
        budget_day_eur: None,
        budget_week_eur: None,
        budget_month_eur: None,
        recent: Vec::new(),
    };

    let mut sessions = std::collections::BTreeSet::new();
    let mut models: BTreeMap<String, ModelStat> = BTreeMap::new();
    let mut agents: BTreeMap<String, AgentStat> = BTreeMap::new();
    let mut harnesses: BTreeMap<String, HarnessStat> = BTreeMap::new();
    let mut unattributed_out: u64 = 0;

    for e in events {
        s.events += 1;
        s.tokens_in += e.tokens_in;
        s.tokens_out += e.tokens_out;
        s.cache_read += e.cache_read;
        s.cache_write_5m += e.cache_write_5m;
        s.cache_write_1h += e.cache_write_1h;
        s.loc_added += e.loc_added as u64;
        s.loc_removed += e.loc_removed as u64;
        s.cost_eur += e.cost_eur;
        sessions.insert(e.session_id.clone());

        if matches!(e.agent.as_str(), "unknown-sub" | "unknown") {
            unattributed_out += e.tokens_out;
        }

        let m = models.entry(e.model.clone()).or_insert_with(|| ModelStat {
            model: e.model.clone(),
            events: 0,
            tokens_in: 0,
            tokens_out: 0,
            cost_eur: 0.0,
            unpriced: e.unpriced,
        });
        m.events += 1;
        m.tokens_in += e.tokens_in;
        m.tokens_out += e.tokens_out;
        m.cost_eur += e.cost_eur;
        m.unpriced |= e.unpriced;

        let a = agents.entry(e.agent.clone()).or_insert_with(|| AgentStat {
            agent: e.agent.clone(),
            is_subagent: e.is_subagent,
            events: 0,
            tokens_in: 0,
            tokens_out: 0,
            cost_eur: 0.0,
        });
        a.events += 1;
        a.tokens_in += e.tokens_in;
        a.tokens_out += e.tokens_out;
        a.cost_eur += e.cost_eur;

        let h = harnesses
            .entry(e.harness.as_str().to_string())
            .or_insert_with(|| HarnessStat {
                harness: e.harness.as_str().to_string(),
                events: 0,
                tokens_in: 0,
                tokens_out: 0,
                cost_eur: 0.0,
            });
        h.events += 1;
        h.tokens_in += e.tokens_in;
        h.tokens_out += e.tokens_out;
        h.cost_eur += e.cost_eur;
    }

    s.sessions = sessions.len() as u64;
    s.unpriced_models = models
        .values()
        .filter(|m| m.unpriced)
        .map(|m| m.model.clone())
        .collect();
    s.unattributed_token_pct = if s.tokens_out == 0 {
        0.0
    } else {
        100.0 * unattributed_out as f64 / s.tokens_out as f64
    };

    // Rank breakdowns by cost desc, then tokens_out desc, for a stable, useful order.
    s.by_model = models.into_values().collect();
    s.by_model.sort_by(|a, b| {
        b.cost_eur
            .total_cmp(&a.cost_eur)
            .then(b.tokens_out.cmp(&a.tokens_out))
            .then(a.model.cmp(&b.model))
    });
    s.by_agent = agents.into_values().collect();
    s.by_agent.sort_by(|a, b| {
        b.cost_eur
            .total_cmp(&a.cost_eur)
            .then(b.tokens_out.cmp(&a.tokens_out))
            .then(a.agent.cmp(&b.agent))
    });
    s.by_harness = harnesses.into_values().collect();
    s.by_harness.sort_by(|a, b| {
        b.cost_eur
            .total_cmp(&a.cost_eur)
            .then(a.harness.cmp(&b.harness))
    });

    s
}

/// Sum the cost of events at or after `since_ms` — used for today / this-week spend.
pub fn windowed_cost(events: &[Event], since_ms: i64) -> f64 {
    events
        .iter()
        .filter(|e| e.ts >= since_ms)
        .map(|e| e.cost_eur)
        .sum()
}
