//! Contract decisions
//! - routes.toml lives at $XDG_CONFIG_HOME/axon/routes.toml. Ordered [[lanes]] contain
//!   name,harness,model,effort (most capable first); [roles] maps a role to a lane name.
//! - `route TASK --role ROLE [--budget Nusd]` treats budget as remaining USD and returns
//!   only {harness,model,effort,reason}; no timestamps/ids. A clamp reason contains `budget`
//!   and names the original and selected lanes. No clock or random input changes an answer.
//!   Each invocation appends an audit event with verb=route.
//! - `register` also accepts --role, --model, --effort, persisting agents.effort. Routing
//!   history uses completed agents' summed usage per role x model x effort, then its median.
//! - `spawn --virtual --register-only --parent ID --panel H:M,... --judge H:M TASK`
//!   registers without launching processes or calling models and prints {id}. The virtual
//!   harness is `virtual`, with one child per panel entry (role=panel) and one role=judge.
//! - `route ... --advisor-response PATH` reads a previously captured proposal JSON in
//!   lieu of outbound advice. routing_decisions(id,rule_json,advisor_json) stores the rule
//!   and optional shadow; advice cannot change any byte of the normal route output.

mod common;
use common::*;
use serde_json::{json, Value};
use std::fs;

fn routes(bus: &Bus) {
    let path = bus.root.join("config/axon");
    fs::create_dir_all(&path).unwrap();
    fs::write(
        path.join("routes.toml"),
        r#"
[[lanes]]
name = "standard"
harness = "claude"
model = "claude-sonnet-4.6"
effort = "medium"
[[lanes]]
name = "economy"
harness = "claude"
model = "claude-haiku-4.5"
effort = "low"
[[lanes]]
name = "local"
harness = "opencode"
model = "local:fixture"
effort = "low"
[roles]
coder = "standard"
reviewer = "standard"
"#,
    )
    .unwrap();
}

fn history(bus: &Bus, id: &str, role: &str, model: &str, effort: &str, tokens: i64) {
    bus.ok(&[
        "register",
        "--id",
        id,
        "--harness",
        "claude",
        "--session",
        id,
        "--cwd",
        bus.root.join("work").to_str().unwrap(),
        "--role",
        role,
        "--model",
        model,
        "--effort",
        effort,
    ]);
    // Two usage rows per completed task ensure the median is per task, not per row.
    bus.usage(id, model, [tokens / 2, 0, 0, 0], now_ms() - 10_000, 1);
    bus.usage(
        id,
        model,
        [tokens - tokens / 2, 0, 0, 0],
        now_ms() - 9_000,
        2,
    );
    bus.db()
        .execute(
            "UPDATE agents SET status='closed',ended_at=?1 WHERE id=?2",
            rusqlite::params![now_ms() - 8_000, id],
        )
        .unwrap();
}

fn mixed_history(bus: &Bus) {
    // Correct median: $0.60; mean: $10.30. Unrelated histories must not contaminate it.
    for (i, tokens) in [100_000, 200_000, 10_000_000].into_iter().enumerate() {
        history(
            bus,
            &format!("new-{i}"),
            "coder",
            "claude-sonnet-4.6",
            "medium",
            tokens,
        );
    }
    for (i, tokens) in [2_000_000, 3_000_000, 4_000_000].into_iter().enumerate() {
        history(
            bus,
            &format!("old-{i}"),
            "coder",
            "claude-opus-4.8",
            "medium",
            tokens,
        );
        history(
            bus,
            &format!("effort-{i}"),
            "coder",
            "claude-sonnet-4.6",
            "high",
            tokens,
        );
        history(
            bus,
            &format!("role-{i}"),
            "reviewer",
            "claude-sonnet-4.6",
            "medium",
            tokens,
        );
    }
    history(bus, "cheap", "coder", "claude-haiku-4.5", "low", 100_000);
}

// BUS-PLAN §4 and §9 M4: identical routing inputs give byte-identical, logged rule answers.
#[test]
fn repeated_route_input_produces_byte_identical_output() {
    let bus = Bus::new();
    bus.init();
    routes(&bus);
    let args = ["route", "Implement the parser", "--role", "coder"];
    let first = bus.ok(&args);
    let second = bus.ok(&args);
    assert_eq!(first.stdout, second.stdout);
    let answer = parse_json(&first.stdout);
    assert_eq!(answer["harness"], "claude");
    assert_eq!(answer["model"], "claude-sonnet-4.6");
    assert_eq!(answer["effort"], "medium");
    assert!(answer["reason"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(
        bus.count("SELECT count(*) FROM events WHERE verb='route'"),
        2
    );
}

// BUS-PLAN §4: role x model x effort and the median decide affordability, not old-model averages.
#[test]
fn affordable_new_model_uses_its_own_role_model_effort_median() {
    let bus = Bus::new();
    bus.init();
    routes(&bus);
    mixed_history(&bus);
    let answer = bus.json(&[
        "route",
        "Implement the parser",
        "--role",
        "coder",
        "--budget",
        "0.7usd",
    ]);
    assert_eq!(answer["model"], "claude-sonnet-4.6");
    assert_eq!(answer["effort"], "medium");
}

// BUS-PLAN §4 and §9 M4: insufficient remaining budget steps down exactly one lane with a reason.
#[test]
fn budget_clamp_steps_down_one_lane_and_explains_the_change() {
    let bus = Bus::new();
    bus.init();
    routes(&bus);
    mixed_history(&bus);
    let answer = bus.json(&[
        "route",
        "Implement the parser",
        "--role",
        "coder",
        "--budget",
        "0.5usd",
    ]);
    assert_eq!(answer["harness"], "claude");
    assert_eq!(answer["model"], "claude-haiku-4.5");
    assert_eq!(answer["effort"], "low");
    let reason = answer["reason"].as_str().unwrap().to_lowercase();
    for word in ["budget", "standard", "economy"] {
        assert!(reason.contains(word), "{reason}");
    }
}

// BUS-PLAN §5 and §9 M4: a virtual agent is one addressable node containing panel and judge children.
#[test]
fn virtual_spawn_registers_one_addressable_node_with_panel_and_judge_children() {
    let bus = Bus::new();
    bus.init();
    bus.register("parent", "claude", None);
    let spawned = bus.json(&[
        "spawn",
        "--virtual",
        "--register-only",
        "--parent",
        "parent",
        "--panel",
        "claude:opus,codex:gpt-5.5,opencode:gemini-3.1-pro",
        "--judge",
        "codex:gpt-5.5",
        "Review this design",
    ]);
    let id = spawned["id"].as_str().unwrap();
    let node = bus.agent(id);
    assert_eq!(node["parent_id"], "parent");
    assert_eq!(node["root_id"], "parent");
    assert_eq!(node["harness"], "virtual");
    let db = bus.db();
    let children: Vec<(String, String, String, String)> = db
        .prepare(
            "SELECT id,harness,role,root_id FROM agents WHERE parent_id=?1 ORDER BY role,harness",
        )
        .unwrap()
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(children.len(), 4);
    let mut panel: Vec<_> = children
        .iter()
        .filter(|c| c.2 == "panel")
        .map(|c| c.1.as_str())
        .collect();
    panel.sort();
    assert_eq!(panel, vec!["claude", "codex", "opencode"]);
    assert_eq!(
        children
            .iter()
            .filter(|c| c.2 == "judge" && c.1 == "codex")
            .count(),
        1
    );
    assert!(children.iter().all(|c| c.3 == "parent"));
    assert_eq!(
        bus.count("SELECT count(*) FROM agents WHERE parent_id='parent'"),
        1
    );
    assert_eq!(bus.count("SELECT count(*) FROM agents"), 6);
    let message = bus.send("parent", id, "question", "one public address");
    let target: String = db
        .query_row(
            "SELECT to_id FROM messages WHERE id=?1",
            [message["id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(target, id);
    let output = bus
        .cmd()
        .args([
            "send",
            "--from",
            "parent",
            "--to",
            &children[0].0,
            "--kind",
            "redirect",
            "--body",
            "cannot skip the virtual node",
        ])
        .assert()
        .code(3)
        .get_output()
        .clone();
    assert_route(
        &String::from_utf8(output.stderr).unwrap(),
        &["parent", id, &children[0].0],
    );
}

// BUS-PLAN §4: advisor proposals remain logged shadows and never override a deterministic rule.
#[test]
fn conflicting_advisor_is_logged_as_a_shadow_without_changing_the_answer() {
    let bus = Bus::new();
    bus.init();
    routes(&bus);
    let args = ["route", "Implement the parser", "--role", "coder"];
    let baseline = bus.ok(&args);
    let proposal = json!({"harness":"opencode","model":"local:fixture","effort":"low",
        "reason":"offline advisor proposes the opposite lane","probability":0.99});
    let file = bus.root.join("advisor.json");
    fs::write(&file, proposal.to_string()).unwrap();
    let mut advised = args.to_vec();
    advised.extend(["--advisor-response", file.to_str().unwrap()]);
    assert_eq!(bus.ok(&advised).stdout, baseline.stdout);
    let (rule,shadow): (String,String) = bus.db().query_row(
        "SELECT rule_json,advisor_json FROM routing_decisions WHERE advisor_json IS NOT NULL ORDER BY id DESC LIMIT 1",
        [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&rule).unwrap(),
        parse_json(&baseline.stdout)
    );
    assert_eq!(serde_json::from_str::<Value>(&shadow).unwrap(), proposal);
}
