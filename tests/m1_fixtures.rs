//! M1 acceptance gates (DESIGN.md §14/§15 + `tests/fixtures/README.md`).
//!
//! These run the real parser/normalizer/store against the committed, real-shaped fixtures.
//! They are the contract M1 must satisfy before anything else is built on top.

use std::path::Path;

use axon::ingest::claude::{self, SubagentMeta};
use axon::model::Event;
use axon::normalize;
use axon::pricing::Pricing;
use axon::store::Store;
use axon::summary::build_summary;

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn main_event() -> Event {
    let turns = claude::parse_main_jsonl(&fixture("claude_main.jsonl"));
    assert_eq!(
        turns.len(),
        1,
        "two same-message.id lines must collapse to ONE turn"
    );
    normalize::to_event(&turns[0], &Pricing::bundled()).unwrap()
}

fn subagent_event() -> Event {
    let meta = SubagentMeta::from_json_str(&fixture("claude_subagent.meta.json")).unwrap();
    let turns = claude::parse_subagent_jsonl(&fixture("claude_subagent.jsonl"), &meta);
    assert_eq!(
        turns.len(),
        1,
        "two same-message.id sub-agent lines must collapse to ONE turn"
    );
    normalize::to_event(&turns[0], &Pricing::bundled()).unwrap()
}

/// Gate 1 — collapse / no double-count / ISO ts / LOC, on the main thread.
#[test]
fn gate1_main_collapse() {
    let e = main_event();
    assert_eq!(
        e.tokens_out, 10498,
        "output_tokens taken ONCE (not 2x = 20996)"
    );
    assert_eq!(e.tokens_in, 11341);
    assert_eq!(e.cache_write_1h, 29928);
    assert_eq!(e.cache_write_5m, 0);
    assert_eq!(e.model, "claude-opus-4.8", "raw id canonicalized");
    assert_eq!(e.agent, "main");
    assert!(!e.is_subagent);
    assert!(e.ts > 0, "ISO-8601 parsed to epoch ms");
    assert_eq!(e.loc_added, 2, "\"a\" -> \"a\\nb\\nc\" adds 2 lines");
    assert_eq!(e.loc_removed, 0);
    assert!(!e.loc_failed);
}

/// Gate 2 (M1 GATE) — exact sub-agent attribution via meta.agentType.
#[test]
fn gate2_subagent_attribution() {
    let meta = SubagentMeta::from_json_str(&fixture("claude_subagent.meta.json")).unwrap();
    assert_eq!(meta.agent_type, "security");
    assert_eq!(
        meta.tool_use_id, "toolu_B",
        "joins to the parent Agent spawn"
    );

    let e = subagent_event();
    assert_eq!(e.tokens_out, 850);
    assert_eq!(e.tokens_in, 5000);
    assert_eq!(e.model, "claude-sonnet-4.6");
    assert_eq!(e.agent, "security", "attributed to agentType, NOT 'main'");
    assert!(e.is_subagent);
}

/// Gate 3 — main and sub-agent token totals are disjoint (no double-count between them).
#[test]
fn gate3_disjoint_session_totals() {
    let events = vec![main_event(), subagent_event()];
    let s = build_summary(&events);

    assert_eq!(s.events, 2);
    assert_eq!(s.sessions, 1, "same sessionId");
    assert_eq!(s.tokens_out, 10498 + 850);
    assert_eq!(s.by_agent.len(), 2);
    assert!(s
        .by_agent
        .iter()
        .any(|a| a.agent == "main" && a.tokens_out == 10498));
    assert!(s
        .by_agent
        .iter()
        .any(|a| a.agent == "security" && a.tokens_out == 850));
    assert_eq!(
        s.unattributed_token_pct, 0.0,
        "everything attributed to a named agent"
    );
}

/// Gate 4 — idempotency: re-scanning the same logs yields the identical row set.
#[test]
fn gate4_idempotent_upsert() {
    let events = vec![main_event(), subagent_event()];
    let mut store = Store::open(":memory:").unwrap();

    store.upsert_all(&events).unwrap();
    let first = store.all_events().unwrap();

    store.upsert_all(&events).unwrap(); // re-scan
    let second = store.all_events().unwrap();

    assert_eq!(store.count().unwrap(), 2, "no duplicate rows on re-scan");
    assert_eq!(first, second, "rows are byte-identical across scans");
    assert_ne!(
        first[0].id, first[1].id,
        "main and sub-agent have distinct ids"
    );
}

/// Gate 5 — cost engine: known model (placeholder 0.0 rates) is priced; unknown is flagged.
#[test]
fn gate5_cost_priced_vs_unpriced() {
    let e = main_event();
    assert!(
        e.cost_eur > 0.0,
        "opus-4.8 is priced → positive cost, never a silent €0 (got {})",
        e.cost_eur
    );
    assert!(
        !e.unpriced,
        "model present in the map is priced, never silently unpriced"
    );

    // Same turn, but an unknown model id must surface loudly as unpriced.
    let turns = claude::parse_main_jsonl(&fixture("claude_main.jsonl"));
    let mut t = turns[0].clone();
    t.model_raw = "totally-unknown-model".to_string();
    let e2 = normalize::to_event(&t, &Pricing::bundled()).unwrap();
    assert!(e2.unpriced);
}

/// Collapse must UNION content across the lines sharing a message.id, not pick one line.
/// Line 1 carries an Edit (→ LOC); line 2 carries a Skill (→ skills). Both must survive.
#[test]
fn collapse_unions_content_across_lines() {
    let jsonl = concat!(
        r#"{"type":"assistant","timestamp":"2026-06-14T10:00:00.000Z","sessionId":"s","cwd":"/r","uuid":"u1","message":{"id":"m1","model":"claude-opus-4-8","content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"old_string":"a","new_string":"a\nb\nc"}}],"usage":{"input_tokens":1,"output_tokens":2}}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-06-14T10:00:05.000Z","sessionId":"s","cwd":"/r","uuid":"u2","message":{"id":"m1","model":"claude-opus-4-8","content":[{"type":"tool_use","id":"t2","name":"Skill","input":{"skill":"verify"}}],"usage":{"input_tokens":1,"output_tokens":2}}}"#,
    );
    let turns = claude::parse_main_jsonl(jsonl);
    assert_eq!(turns.len(), 1);
    let t = &turns[0];
    assert_eq!(t.loc_added, 2, "Edit from line 1 survives the union");
    assert_eq!(
        t.skills,
        vec!["verify".to_string()],
        "Skill from line 2 survives the union"
    );
    assert_eq!(
        t.tokens_out, 2,
        "usage taken once, not summed across the two lines"
    );
}

/// A numeric timestamp must be rejected (DESIGN.md §15.1) — sources emit ISO strings only.
#[test]
fn numeric_timestamp_rejected() {
    let jsonl = r#"{"type":"assistant","timestamp":"1718360478279","sessionId":"s","cwd":"/r","uuid":"u1","message":{"id":"m1","model":"claude-opus-4-8","content":[],"usage":{"input_tokens":1,"output_tokens":2}}}"#;
    let turns = claude::parse_main_jsonl(jsonl);
    assert_eq!(turns.len(), 1);
    assert!(normalize::to_event(&turns[0], &Pricing::bundled()).is_err());
}
