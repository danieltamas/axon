//! M2 acceptance gates (DESIGN.md §14): Codex + OpenCode parse → normalize → cost, against
//! committed real-shaped fixtures.

use std::path::Path;

use axon::ingest::{codex, opencode};
use axon::model::{Harness, PricingKind};
use axon::normalize;
use axon::pricing::Pricing;
use serde_json::Value;

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Codex usage is session-cumulative; per-turn attribution must use the delta between
/// `turn_context` boundaries — never the raw cumulative totals.
#[test]
fn codex_cumulative_delta_priced() {
    let turns = codex::parse_session(&fixture("codex.jsonl"), "fallback");
    assert_eq!(turns.len(), 2, "two turn_context boundaries → two turns");
    let p = Pricing::bundled();

    let e1 = normalize::to_event(&turns[0], &p).unwrap();
    assert_eq!(e1.harness, Harness::Codex);
    assert_eq!(e1.agent, "codex");
    assert_eq!(e1.model, "gpt-5.5");
    assert_eq!(e1.session_id, "sess-codex-fixture");
    assert_eq!(e1.tokens_in, 800, "1000 input − 200 cached");
    assert_eq!(e1.cache_read, 200);
    assert_eq!(e1.tokens_out, 500);
    assert_eq!(e1.loc_added, 2, "unified diff +b +c");
    assert!(e1.ts > 0, "ISO timestamp parsed");
    assert!(!e1.unpriced, "gpt-5.5 is priced");
    assert!(e1.cost_eur > 0.0);
    assert_eq!(turns[0].chatgpt_plan_type.as_deref(), Some("prolite"));

    let e2 = normalize::to_event(&turns[1], &p).unwrap();
    assert_eq!(e2.tokens_in, 2500, "delta 3000 input − 500 cached");
    assert_eq!(e2.cache_read, 500);
    assert_eq!(e2.tokens_out, 700);
    assert_ne!(e1.id, e2.id, "per-turn ids are distinct");
}

#[test]
fn codex_chatgpt_gpt_5_4_uses_credits_not_unpriced() {
    let turns = codex::parse_session(&fixture("codex_gpt54_chatgpt.jsonl"), "fallback");
    let e = normalize::to_event(&turns[0], &Pricing::bundled()).unwrap();
    assert_eq!(e.pricing_kind, PricingKind::ChatgptIncluded);
    assert_eq!(e.cost_eur, 0.0);
    assert!(!e.unpriced);
    let credits = e.cost_credits.expect("credits");
    let expected = (800.0 * 62.5 + 200.0 * 6.25 + 500.0 * 375.0) / 1_000_000.0;
    assert!((credits - expected).abs() < 1e-9, "got {credits}");
}

#[test]
fn codex_chatgpt_spark_is_preview_not_unpriced() {
    let jsonl = [
        r#"{"timestamp":"2026-06-03T09:00:00.000Z","type":"session_meta","payload":{"id":"sess-spark","cwd":"/abs/repo"}}"#,
        r#"{"timestamp":"2026-06-03T09:00:01.000Z","type":"turn_context","payload":{"turn_id":"turn-1","model":"gpt-5.3-codex-spark"}}"#,
        r#"{"timestamp":"2026-06-03T09:00:06.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1200,"cached_input_tokens":100,"output_tokens":300}},"rate_limits":{"plan_type":"prolite"}}}"#,
    ]
    .join("\n");
    let turns = codex::parse_session(&jsonl, "fallback");
    let e = normalize::to_event(&turns[0], &Pricing::bundled()).unwrap();
    assert_eq!(e.pricing_kind, PricingKind::ChatgptPreview);
    assert_eq!(e.cost_eur, 0.0);
    assert_eq!(e.cost_credits, None);
    assert!(!e.unpriced);
}

#[test]
fn codex_api_gpt_5_4_uses_api_money() {
    let turns = codex::parse_session(&fixture("codex_gpt54_api.jsonl"), "fallback");
    let e = normalize::to_event(&turns[0], &Pricing::bundled()).unwrap();
    assert_eq!(e.pricing_kind, PricingKind::ApiMoney);
    assert!(e.cost_eur > 0.0);
    assert_eq!(e.cost_credits, None);
    assert!(!e.unpriced);
}

#[test]
fn codex_unknown_plan_type_gpt_5_4_falls_back_to_api_money() {
    let turns = codex::parse_session(&fixture("codex_gpt54_unknown_plan.jsonl"), "fallback");
    let e = normalize::to_event(&turns[0], &Pricing::bundled()).unwrap();
    assert_eq!(e.pricing_kind, PricingKind::ApiMoney);
    assert!(e.cost_eur > 0.0);
    assert_eq!(e.cost_credits, None);
    assert!(!e.unpriced);
}

/// OpenCode records its own exact cost — Axon uses it (× fx) instead of the rate table.
#[test]
fn opencode_uses_reported_cost() {
    let data: Value = serde_json::from_str(&fixture("opencode_message.json")).unwrap();
    let t = opencode::turn_from_data("msg_fix", "ses_fix", 0, &data).unwrap();
    let p = Pricing::bundled();
    let e = normalize::to_event(&t, &p).unwrap();

    assert_eq!(e.harness, Harness::OpenCode);
    assert_eq!(e.agent, "build");
    assert_eq!(e.model, "gpt-5.5");
    assert_eq!(e.tokens_in, 700);
    assert_eq!(e.tokens_out, 200, "output 150 + reasoning 50");
    assert!(!e.unpriced, "harness-reported cost is priced");
    assert!(
        (e.cost_eur - 0.0123 * 0.92).abs() < 1e-9,
        "reported 0.0123 USD × 0.92 fx, got {}",
        e.cost_eur
    );
    assert!(e.ts > 0, "epoch-ms → ISO → epoch-ms round-trips");
}
