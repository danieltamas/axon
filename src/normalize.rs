//! Normalization: [`crate::ingest::RawTurn`] → [`Event`] (DESIGN.md §7).
//!
//! Responsibilities: parse the ISO-8601 timestamp to epoch-ms (reject numeric stamps),
//! canonicalize the model id, compute the idempotent `id` hash, derive `duration_ms`, and
//! attach the computed cost.

use anyhow::{anyhow, Context};
use chrono::DateTime;

use crate::ingest::RawTurn;
use crate::model::{canonicalize_model, Event, PricingKind};
use crate::pricing::{Buckets, Pricing};

/// Convert a parsed turn into a normalized, costed [`Event`].
///
/// Returns `Err` if the timestamp is not RFC3339 (e.g. a numeric epoch the source should
/// never emit) — callers count and skip these rather than store a wrong `ts`.
pub fn to_event(turn: &RawTurn, pricing: &Pricing) -> anyhow::Result<Event> {
    let first_ms = parse_iso_ms(&turn.first_ts)
        .with_context(|| format!("timestamp {:?} is not ISO-8601", turn.first_ts))?;
    let last_ms = parse_iso_ms(&turn.last_ts).unwrap_or(first_ms);
    let duration_ms = (last_ms > first_ms).then(|| (last_ms - first_ms) as u64);

    let model = canonicalize_model(&turn.model_raw);
    let buckets = Buckets {
        input: turn.tokens_in,
        output: turn.tokens_out,
        cache_read: turn.cache_read,
        cache_write_5m: turn.cache_write_5m,
        cache_write_1h: turn.cache_write_1h,
    };
    // Prefer a cost the source harness already computed (OpenCode/Kilo); otherwise compute
    // from the pricing table or classify special ChatGPT-plan Codex cases.
    let (cost_eur, cost_credits, pricing_kind, unpriced) = match turn.reported_cost_usd {
        Some(usd) => (usd * pricing.fx(), None, PricingKind::ReportedCost, false),
        None if pricing.is_local(&model) => (0.0, None, PricingKind::LocalFree, false),
        None if turn.chatgpt_plan_type.is_some() && model == "gpt-5.4" => (
            0.0,
            pricing.chatgpt_credits(&model, &buckets),
            PricingKind::ChatgptIncluded,
            false,
        ),
        None if turn.chatgpt_plan_type.is_some() && model == "gpt-5.3-codex-spark" => {
            (0.0, None, PricingKind::ChatgptPreview, false)
        }
        None => {
            let (cost_eur, unpriced) = pricing.cost(&model, &buckets);
            let kind = if unpriced {
                PricingKind::Unknown
            } else {
                PricingKind::ApiMoney
            };
            (cost_eur, None, kind, unpriced)
        }
    };

    Ok(Event {
        id: event_id(turn),
        ts: first_ms,
        harness: turn.harness,
        project: turn.project.clone(),
        agent: turn.agent.clone(),
        is_subagent: turn.is_subagent,
        model,
        session_id: turn.session_id.clone(),
        tokens_in: turn.tokens_in,
        tokens_out: turn.tokens_out,
        cache_read: turn.cache_read,
        cache_write_5m: turn.cache_write_5m,
        cache_write_1h: turn.cache_write_1h,
        duration_ms,
        loc_added: turn.loc_added,
        loc_removed: turn.loc_removed,
        loc_failed: turn.loc_failed,
        skills: turn.skills.clone(),
        cost_eur,
        cost_credits,
        pricing_kind,
        unpriced,
    })
}

/// Idempotent content key: `blake3(harness, session_id | agent_id, message_id)`.
/// Sub-agents key on their `agent_id` so their turns never collide with the parent's.
fn event_id(turn: &RawTurn) -> String {
    let scope = if turn.is_subagent {
        turn.agent_id.as_deref().unwrap_or(&turn.session_id)
    } else {
        &turn.session_id
    };
    let mut hasher = blake3::Hasher::new();
    hasher.update(turn.harness.as_str().as_bytes());
    hasher.update(b"\x1f");
    hasher.update(scope.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(turn.message_id.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Parse an RFC3339/ISO-8601 timestamp to epoch milliseconds.
fn parse_iso_ms(ts: &str) -> anyhow::Result<i64> {
    if ts.is_empty() {
        return Err(anyhow!("empty timestamp"));
    }
    // A bare number is NOT accepted: the sources emit ISO strings, and a numeric value here
    // would silently mean "already epoch" — exactly the bug §15.1 guards against.
    if ts.chars().all(|c| c.is_ascii_digit()) {
        return Err(anyhow!(
            "numeric timestamp {ts:?} rejected (expected ISO-8601)"
        ));
    }
    Ok(DateTime::parse_from_rfc3339(ts)?.timestamp_millis())
}
