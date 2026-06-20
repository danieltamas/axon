//! Codex session parser (DESIGN.md §6.2) — **verified against real `~/.codex` logs**.
//!
//! Verified shape (NOT the spec's recon):
//! - Each line is `{timestamp: ISO-8601, type, payload}`. Types: `session_meta`,
//!   `turn_context`, `event_msg`, `response_item`, `compacted`.
//! - **Token usage is session-CUMULATIVE**, reported in `event_msg`/`token_count`
//!   `payload.info.total_token_usage` (grows monotonically). Naive summing inflates wildly.
//!   `token_count` carries no `turn_id`.
//! - The turn key is `turn_context.payload.turn_id` (+ `model`). So we attribute per turn by
//!   taking the cumulative DELTA between consecutive `turn_context` boundaries.
//! - LOC comes from `event_msg`/`patch_apply_end` (which DOES carry `turn_id`).

use std::collections::HashMap;

use serde_json::Value;

use super::RawTurn;
use crate::model::Harness;

#[derive(Default, Clone, Copy)]
struct Cum {
    input: u64,
    cached: u64,
    output: u64,
}

struct TurnAcc {
    turn_id: String,
    model: String,
    chatgpt_plan_type: Option<String>,
    first_ts: String,
    last_ts: String,
}

/// Parse one Codex session `.jsonl`. `fallback_session` is used if `session_meta.id` is absent
/// (e.g. the file stem). Emits one [`RawTurn`] per turn that consumed tokens or edited code.
pub fn parse_session(content: &str, fallback_session: &str) -> Vec<RawTurn> {
    let mut session_id = fallback_session.to_string();
    let mut cwd = String::new();

    let mut latest = Cum::default(); // most recent cumulative usage seen
    let mut prev = Cum::default(); // cumulative at the previous turn boundary
    let mut cur: Option<TurnAcc> = None;
    let mut loc: HashMap<String, (u32, u32, bool)> = HashMap::new();
    let mut chatgpt_plan_type: Option<String> = None;
    let mut out: Vec<RawTurn> = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
        let ts = v
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let payload = v.get("payload").unwrap_or(&Value::Null);

        match kind {
            "session_meta" => {
                if let Some(id) = payload.get("id").and_then(Value::as_str) {
                    session_id = id.to_string();
                }
                if let Some(c) = payload.get("cwd").and_then(Value::as_str) {
                    cwd = c.to_string();
                }
            }
            "turn_context" => {
                // Close the previous turn before opening this one.
                if let Some(t) = cur.take() {
                    if let Some(rt) = emit(&t, prev, latest, &session_id, &cwd, &loc) {
                        out.push(rt);
                    }
                    prev = latest;
                }
                cur = Some(TurnAcc {
                    turn_id: payload
                        .get("turn_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    model: payload
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or("codex")
                        .to_string(),
                    chatgpt_plan_type: chatgpt_plan_type.clone(),
                    first_ts: ts.to_string(),
                    last_ts: ts.to_string(),
                });
            }
            "event_msg" => match payload.get("type").and_then(Value::as_str) {
                Some("token_count") => {
                    if let Some(c) = cumulative(payload) {
                        latest = c;
                        chatgpt_plan_type = payload
                            .get("rate_limits")
                            .and_then(|r| r.get("plan_type"))
                            .and_then(Value::as_str)
                            .map(|s| s.to_string());
                        if let Some(t) = cur.as_mut() {
                            if chatgpt_plan_type.is_some() {
                                t.chatgpt_plan_type = chatgpt_plan_type.clone();
                            }
                            t.last_ts = ts.to_string();
                        }
                    }
                }
                Some("patch_apply_end") => accumulate_loc(payload, &mut loc),
                _ => {}
            },
            _ => {}
        }
    }
    // Close the final open turn.
    if let Some(t) = cur.take() {
        if let Some(rt) = emit(&t, prev, latest, &session_id, &cwd, &loc) {
            out.push(rt);
        }
    }
    out
}

/// Extract the cumulative usage triple from a `token_count` payload (None if absent/null).
fn cumulative(payload: &Value) -> Option<Cum> {
    let t = payload.get("info")?.get("total_token_usage")?;
    let g = |k: &str| t.get(k).and_then(Value::as_u64).unwrap_or(0);
    Some(Cum {
        input: g("input_tokens"),
        cached: g("cached_input_tokens"),
        output: g("output_tokens"),
    })
}

fn accumulate_loc(payload: &Value, loc: &mut HashMap<String, (u32, u32, bool)>) {
    let success = payload
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let turn_id = payload
        .get("turn_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let entry = loc.entry(turn_id).or_default();
    if !success {
        entry.2 = true; // loc_failed
        return;
    }
    if let Some(changes) = payload.get("changes").and_then(Value::as_object) {
        for change in changes.values() {
            if let Some(diff) = change.get("unified_diff").and_then(Value::as_str) {
                let (a, r) = count_diff(diff);
                entry.0 += a;
                entry.1 += r;
            }
        }
    }
}

/// Count added/removed lines in a unified diff (ignoring the `+++`/`---`/`@@` headers).
fn count_diff(diff: &str) -> (u32, u32) {
    let mut added = 0;
    let mut removed = 0;
    for l in diff.lines() {
        if l.starts_with("+++") || l.starts_with("---") {
            continue;
        }
        match l.as_bytes().first() {
            Some(b'+') => added += 1,
            Some(b'-') => removed += 1,
            _ => {}
        }
    }
    (added, removed)
}

fn emit(
    t: &TurnAcc,
    prev: Cum,
    latest: Cum,
    session_id: &str,
    cwd: &str,
    loc: &HashMap<String, (u32, u32, bool)>,
) -> Option<RawTurn> {
    // Per-turn usage = cumulative delta across this turn's boundary (saturating).
    let d_input = latest.input.saturating_sub(prev.input);
    let d_cached = latest.cached.saturating_sub(prev.cached);
    let d_output = latest.output.saturating_sub(prev.output);
    let tokens_in = d_input.saturating_sub(d_cached); // input is reported inclusive of cached
    let (la, lr, lf) = loc.get(&t.turn_id).copied().unwrap_or_default();

    // Skip turns that neither consumed tokens nor edited code.
    if tokens_in == 0 && d_output == 0 && d_cached == 0 && la == 0 && lr == 0 {
        return None;
    }

    Some(RawTurn {
        harness: Harness::Codex,
        session_id: session_id.to_string(),
        agent_id: None,
        message_id: t.turn_id.clone(),
        model_raw: t.model.clone(),
        is_subagent: false,
        agent: "codex".to_string(),
        project: cwd.to_string(),
        first_ts: t.first_ts.clone(),
        last_ts: t.last_ts.clone(),
        tokens_in,
        tokens_out: d_output,
        cache_read: d_cached,
        cache_write_5m: 0,
        cache_write_1h: 0,
        loc_added: la,
        loc_removed: lr,
        loc_failed: lf,
        skills: Vec::new(),
        chatgpt_plan_type: t.chatgpt_plan_type.clone(),
        reported_cost_usd: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cumulative_delta_attributes_per_turn() {
        // Two turns; token_count is cumulative (10k then 30k). Turn 1 = 10k, turn 2 = 20k.
        let jsonl = [
            r#"{"timestamp":"2026-06-03T09:00:00.000Z","type":"session_meta","payload":{"id":"sess-X","cwd":"/r"}}"#,
            r#"{"timestamp":"2026-06-03T09:00:01.000Z","type":"turn_context","payload":{"turn_id":"t1","model":"gpt-5.5"}}"#,
            r#"{"timestamp":"2026-06-03T09:00:02.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":8000,"cached_input_tokens":1000,"output_tokens":2000}},"rate_limits":{"plan_type":"prolite"}}}"#,
            r#"{"timestamp":"2026-06-03T09:01:00.000Z","type":"turn_context","payload":{"turn_id":"t2","model":"gpt-5.5"}}"#,
            r#"{"timestamp":"2026-06-03T09:01:02.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":24000,"cached_input_tokens":3000,"output_tokens":6000}}}}"#,
        ]
        .join("\n");

        let turns = parse_session(&jsonl, "fallback");
        assert_eq!(turns.len(), 2);

        // Turn 1: input 8000 (incl 1000 cached) → tokens_in 7000, cache_read 1000, out 2000.
        assert_eq!(turns[0].message_id, "t1");
        assert_eq!(turns[0].session_id, "sess-X");
        assert_eq!(turns[0].agent, "codex");
        assert_eq!(turns[0].tokens_in, 7000);
        assert_eq!(turns[0].cache_read, 1000);
        assert_eq!(turns[0].tokens_out, 2000);
        assert_eq!(turns[0].chatgpt_plan_type.as_deref(), Some("prolite"));

        // Turn 2: delta input 16000 (cached +2000) → in 14000, cache_read 2000, out 4000.
        assert_eq!(turns[1].message_id, "t2");
        assert_eq!(turns[1].tokens_in, 14000);
        assert_eq!(turns[1].cache_read, 2000);
        assert_eq!(turns[1].tokens_out, 4000);
        assert_eq!(turns[1].chatgpt_plan_type.as_deref(), Some("prolite"));
    }

    #[test]
    fn counts_unified_diff_loc() {
        assert_eq!(count_diff("@@ -1 +1,3 @@\n a\n+b\n+c\n-x"), (2, 1));
    }
}
