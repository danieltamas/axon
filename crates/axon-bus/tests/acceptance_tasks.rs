//! Frozen BUS-PLAN §3b–3c B–E, independent of the task implementation.
//! All writes are isolated. Usage enters through real transcript ingest, never task/usage inserts.
mod common;
use common::fed::{api, eventually, repo};
use common::{now_ms, parse_json, write_jsonl, Bus, Running, Server};
use rusqlite::params;
use serde_json::{json, Value};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;
struct Fixture {
    server: Server, // Stop the server before dropping its sandbox, including on panic.
    bus: Bus,
    cwd: PathBuf,
}
impl Fixture {
    fn new(content: bool) -> Self {
        let bus = Bus::with_limit(Duration::from_secs(90));
        bus.init();
        let cwd = repo(&bus, "task-project");
        bus.register_at("root", "claude", None, &cwd);
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        let mut command = root_command(&bus);
        command.args([
            "--port",
            &address.port().to_string(),
            "--no-open",
            "--no-hooks",
        ]);
        if !content {
            command.arg("--no-content");
        }
        drop(reservation);
        let mut server = Server {
            process: Running(
                command
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("build root axon first"),
            ),
            address,
            url: format!("http://{address}"),
            cookie: String::new(),
            token: String::new(),
        };
        eventually(&bus, "root server listening", || {
            assert!(
                server.process.0.try_wait().unwrap().is_none(),
                "root server exited"
            );
            TcpStream::connect_timeout(&address, Duration::from_millis(50)).is_ok()
        });
        (server.cookie, server.token) = server.login(&bus);
        api(
            &bus,
            &server,
            "PUT",
            "/api/settings/capture",
            json!({"enabled":content,"narrative_days":365}),
        );
        Self { server, bus, cwd }
    }
    fn scan(&self) {
        assert_cmd::Command::from(root_command(&self.bus))
            .args(["--scan-only", "--no-hooks"])
            .timeout(self.bus.remaining())
            .assert()
            .success();
    }
    fn ledger(&self, actor: &str, operation: &str, key: &str) {
        self.bus
            .cmd()
            .current_dir(&self.cwd)
            .args([operation, "--agent", actor, "--key", key])
            .assert()
            .success();
    }
    fn path(&self, actor: &str) -> PathBuf {
        let base = self.bus.home().join(".claude/projects/task-fixture");
        if actor == "child" || actor == "grandchild" {
            base.join(format!("root/subagents/agent-{actor}.jsonl"))
        } else {
            base.join(format!("{actor}.jsonl"))
        }
    }
    fn append(&self, actor: &str, mut row: Value) {
        row["cwd"] = json!(self.cwd);
        row["sessionId"] = json!(if actor == "child" || actor == "grandchild" {
            "root"
        } else {
            actor
        });
        let path = self.path(actor);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{row}").unwrap();
    }
    fn prompt(&self, actor: &str, ts: i64, content: Value) {
        self.append(
            actor,
            json!({"type":"user","uuid":format!("prompt-{ts}"),
            "timestamp":iso(ts),"message":{"role":"user","content":content}}),
        );
    }
    fn turn(&self, actor: &str, ts: i64, marker: u64) {
        let mut row = completion(ts, marker);
        if actor == "child" || actor == "grandchild" {
            row["isSidechain"] = json!(true);
            row["agentId"] = json!(actor);
        }
        self.append(actor, row);
    }
    fn child(&self, id: &str, parent: &str) {
        self.bus.ok(&[
            "register",
            "--id",
            id,
            "--harness",
            "claude",
            "--session",
            "root",
            "--parent",
            parent,
            "--cwd",
            self.cwd.to_str().unwrap(),
        ]);
    }
    fn tasks(&self, range: &str) -> Vec<Value> {
        let response = self.server.request(
            &self.bus,
            "GET",
            &format!("/api/tasks?range={range}"),
            &[],
            "",
        );
        assert_eq!(
            response.status,
            200,
            "§3c D: GET /api/tasks: {}",
            String::from_utf8_lossy(&response.body)
        );
        let body = parse_json(&response.body);
        let rows = body["tasks"]
            .as_array()
            .expect("§3c D: {tasks:[...]}")
            .clone();
        for row in &rows {
            for field in [
                "id",
                "kind",
                "key",
                "name",
                "opened_at",
                "closed_at",
                "cost",
                "turns",
                "agents",
                "harnesses",
                "retries_cost",
                "human_wait_ms",
                "elapsed_ms",
            ] {
                assert!(row.get(field).is_some(), "§3c D: missing {field}: {row}");
            }
            for field in ["measured", "estimated", "credits", "tools_unpriced"] {
                assert!(
                    row["cost"].get(field).is_some(),
                    "§3c D: missing cost.{field}: {row}"
                );
            }
            assert!(row["kind"] == "declared" || row["kind"] == "request");
        }
        assert!(rows.len() <= 200, "§3c D: at most 200 tasks");
        assert!(rows.windows(2).all(|w| w[0]["opened_at"].as_i64().unwrap() >= w[1]["opened_at"].as_i64().unwrap()), "§3c D: newest first");
        rows
    }
    fn assigned(&self, marker: u64) -> Value {
        let rows = self.tasks("all");
        let db = self.bus.db();
        let id: String = db.query_row(
            "SELECT tt.task_id FROM task_turns tt JOIN usage_events u ON u.id=tt.event_id WHERE u.tokens_out=?1", [marker], |r| r.get(0))
            .expect("§3c B: turn assigned in task_turns");
        rows.into_iter()
            .find(|t| t["id"] == id)
            .expect("assigned task appears in API")
    }
    fn summary(&self, range: &str, count: u64) -> Value {
        let mut value = Value::Null;
        eventually(&self.bus, "summary contains ingested fixture", || {
            let response = self.server.request(
                &self.bus,
                "GET",
                &format!("/api/summary?range={range}"),
                &[],
                "",
            );
            assert_eq!(response.status, 200);
            value = parse_json(&response.body);
            value["events"] == count
        });
        value
    }
}
fn root_command(bus: &Bus) -> Command {
    let binary = PathBuf::from(assert_cmd::cargo::cargo_bin!("axon-bus"))
        .with_file_name(format!("axon{}", std::env::consts::EXE_SUFFIX));
    let mut cmd = Command::new(binary);
    bus.isolate(&mut cmd);
    cmd
}
fn iso(ts: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts)
        .unwrap()
        .to_rfc3339()
}
fn completion(ts: i64, marker: u64) -> Value {
    let mut row: Value = serde_json::from_str(
        include_str!("../../../tests/fixtures/claude_main.jsonl")
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    row["timestamp"] = json!(iso(ts));
    row["uuid"] = json!(format!("turn-{marker}"));
    row["message"]["id"] = json!(format!("message-{marker}"));
    row["message"]["content"] = json!([{"type":"text","text":"fixture completion"}]);
    row["message"]["usage"] = json!({"input_tokens":1000,"output_tokens":marker,
        "cache_read_input_tokens":0,"cache_creation_input_tokens":0});
    row
}
fn cents(value: f64) -> i64 {
    (value * 100.0).round() as i64
}
fn money(task: &Value, field: &str) -> f64 {
    task["cost"][field].as_f64().expect("numeric task cost")
}
fn assignments(f: &Fixture) -> Vec<(String, String)> {
    let db = f.bus.db();
    let mut query = db
        .prepare("SELECT event_id,task_id FROM task_turns ORDER BY event_id")
        .unwrap();
    query
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}
#[test]
fn open_take_owns_turn_and_done_closes_task_without_reassigning_history() {
    let f = Fixture::new(false);
    f.ledger("root", "take", "issue:owned");
    let t = now_ms();
    f.prompt("root", t, json!("work"));
    f.turn("root", t + 1, 11);
    f.scan();
    let first = f.assigned(11);
    assert_eq!(first["kind"], "declared");
    assert_eq!(first["key"], "issue:owned");
    assert!(first["closed_at"].is_null());
    f.ledger("root", "done", "issue:owned");
    f.turn("root", now_ms(), 12);
    f.scan();
    let closed = f.assigned(11);
    assert_eq!(closed["id"], first["id"]);
    assert!(closed["closed_at"].is_i64());
    assert_eq!(f.assigned(12)["kind"], "request");
}
#[test]
fn descendants_at_two_depths_inherit_take_but_own_take_has_priority() {
    let f = Fixture::new(false);
    f.child("child", "root");
    f.child("grandchild", "child");
    f.ledger("root", "take", "issue:parent");
    let t = now_ms();
    f.prompt("root", t, json!("delegate"));
    f.turn("child", t + 1, 21);
    f.turn("grandchild", t + 2, 22);
    f.scan();
    for marker in [21, 22] {
        assert_eq!(f.assigned(marker)["key"], "issue:parent");
    }
    f.ledger("child", "take", "issue:child");
    f.turn("child", now_ms(), 23);
    f.scan();
    assert_eq!(f.assigned(23)["key"], "issue:child");
}
fn delegation(kind: &str, end: &str) {
    let f = Fixture::new(false);
    f.bus.register_at("recipient", "claude", None, &f.cwd);
    f.bus.ok(&["link", "--from", "root", "--to", "recipient"]);
    f.bus.ok(&["accept", "--from", "recipient", "--to", "root"]);
    f.ledger("root", "take", "issue:delegated");
    let message = f.bus.send("root", "recipient", kind, "please review");
    let t = now_ms();
    f.prompt("recipient", t, json!("review"));
    let mut delivery = f.bus.fixture("claude", "PostToolUse");
    delivery["session_id"] = json!("recipient");
    delivery["cwd"] = json!(f.cwd);
    delivery["transcript_path"] = json!(f.path("recipient"));
    let delivered = f.bus.hook("claude", "PostToolUse", &delivery);
    assert!(
        String::from_utf8(delivered.stdout)
            .unwrap()
            .contains("please review"),
        "handoff/question actually delivered"
    );
    f.turn("recipient", now_ms(), 31);
    f.scan();
    assert_eq!(f.assigned(31)["key"], "issue:delegated");
    f.bus.ok(&[
        "send",
        "--from",
        "recipient",
        "--to",
        "root",
        "--kind",
        end,
        "--thread",
        "unrelated-thread",
        "--body",
        "another conversation",
    ]);
    f.turn("recipient", now_ms(), 32);
    f.scan();
    assert_eq!(
        f.assigned(32)["key"],
        "issue:delegated",
        "only the delegated thread ends assignment"
    );
    f.bus.ok(&[
        "send",
        "--from",
        "recipient",
        "--to",
        "root",
        "--kind",
        end,
        "--thread",
        message["thread"].as_str().unwrap(),
        "--body",
        "finished",
    ]);
    f.turn("recipient", now_ms(), 33);
    f.scan();
    assert_eq!(f.assigned(33)["kind"], "request");
    assert_eq!(
        f.assigned(31)["key"],
        "issue:delegated",
        "old assignments are immutable"
    );
}
#[test]
fn handoff_recipient_belongs_to_task_until_answer_in_that_thread() {
    delegation("handoff", "answer");
}
#[test]
fn question_recipient_belongs_to_task_until_ack_in_that_thread() {
    delegation("question", "ack");
}
#[test]
fn claude_latest_operator_prompt_owns_turns_and_tool_results_do_not_open_requests() {
    let f = Fixture::new(false);
    f.child("child", "root");
    let t = now_ms() - 10_000;
    f.prompt("root", t, json!("first"));
    f.turn("root", t + 1000, 41);
    f.prompt(
        "root",
        t + 2000,
        json!([{"type":"tool_result","tool_use_id":"toolu-fixture","content":"output"}]),
    );
    f.turn("root", t + 3000, 42);
    f.turn("child", t + 4000, 43);
    f.prompt("root", t + 5000, json!([{"type":"text","text":"second"}]));
    f.turn("root", t + 6000, 44);
    f.turn("child", t + 7000, 45);
    f.scan();
    let first = f.assigned(41);
    let second = f.assigned(44);
    assert_eq!(first["kind"], "request");
    assert_eq!(second["kind"], "request");
    assert_ne!(first["id"], second["id"]);
    for marker in [42, 43] {
        assert_eq!(f.assigned(marker)["id"], first["id"]);
    }
    assert_eq!(f.assigned(45)["id"], second["id"]);
    assert_eq!(first["closed_at"], t + 5000);
    assert_eq!(first["elapsed_ms"], 5000);
}
fn codex(f: &Fixture, ts: i64, prompts: usize) {
    f.bus.register_at("codex-root", "codex", None, &f.cwd);
    let template: Vec<Value> = include_str!("../../../tests/fixtures/codex_gpt54_chatgpt.jsonl")
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let mut meta = template[0].clone();
    meta["payload"]["id"] = json!("codex-root");
    meta["payload"]["cwd"] = json!(f.cwd);
    meta["timestamp"] = json!(iso(ts));
    let mut rows = vec![meta];
    for i in 0..prompts {
        rows.push(
            json!({"timestamp":iso(ts + i as i64 * 1000),"type":"event_msg",
            "payload":{"type":"user_message","message":format!("request {i}")}}),
        );
        let mut context = template[1].clone();
        context["timestamp"] = json!(iso(ts + i as i64 * 1000 + 1));
        context["payload"]["turn_id"] = json!(format!("turn-{i}"));
        rows.push(context);
        let mut usage = template[2].clone();
        usage["timestamp"] = json!(iso(ts + i as i64 * 1000 + 2));
        usage["payload"]["info"]["total_token_usage"] = json!({"input_tokens":1000*(i+1),"cached_input_tokens":200*(i+1),"output_tokens":500*(i+1)});
        rows.push(usage);
    }
    let path = f
        .bus
        .home()
        .join(".codex/sessions/2026/10/03/rollout-fixture.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_jsonl(&path, &rows);
}
#[test]
fn codex_user_messages_open_distinct_requests() {
    let f = Fixture::new(false);
    codex(&f, now_ms() - 5000, 2);
    f.scan();
    let rows = f.tasks("all");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|t| t["kind"] == "request"));
    assert_ne!(rows[0]["id"], rows[1]["id"]);
    assert_eq!(assignments(&f).len(), 2);
}
#[test]
fn most_recent_open_take_wins_and_rescanning_cannot_move_old_turns() {
    let f = Fixture::new(false);
    f.ledger("root", "take", "issue:older");
    let t = now_ms();
    f.prompt("root", t, json!("work"));
    f.turn("root", t + 1, 51);
    f.scan();
    assert_eq!(f.assigned(51)["key"], "issue:older");
    f.ledger("root", "take", "issue:newer");
    f.turn("root", now_ms(), 52);
    f.scan();
    assert_eq!(f.assigned(52)["key"], "issue:newer");
    let before = assignments(&f);
    f.bus
        .db()
        .execute("DELETE FROM scanned_sources", [])
        .unwrap();
    f.scan();
    assert_eq!(
        assignments(&f),
        before,
        "§3c B: assignment never changes on re-ingest"
    );
}
#[test]
fn request_ids_and_assignments_survive_actual_reingest_and_clean_rebuild() {
    let f = Fixture::new(false);
    let t = now_ms() - 5000;
    for (i, marker) in [61, 62].into_iter().enumerate() {
        f.prompt("root", t + i as i64 * 1000, json!("request"));
        f.turn("root", t + i as i64 * 1000 + 1, marker);
    }
    f.scan();
    assert_eq!(f.tasks("all").len(), 2);
    let before = assignments(&f);
    f.bus
        .db()
        .execute("DELETE FROM scanned_sources", [])
        .unwrap();
    f.scan();
    assert_eq!(assignments(&f), before);
    f.bus.db().execute_batch("BEGIN; DELETE FROM task_turns; DELETE FROM tasks; DELETE FROM usage_events; DELETE FROM scanned_sources; COMMIT;").unwrap();
    f.scan();
    assert_eq!(
        assignments(&f),
        before,
        "§3c B: ids derive from session and prompt timestamp"
    );
}
#[test]
fn every_turn_has_one_task_and_costs_conserve_for_all_supported_ranges() {
    let f = Fixture::new(false);
    let now = now_ms();
    // Two turns straddle range boundaries inside the SAME request; task filtering cannot
    // simply include/exclude whole tasks by opened_at.
    f.prompt("root", now - 41 * 86_400_000, json!("long task"));
    for (days, marker) in [(40, 71), (10, 72), (2, 73), (0, 74)] {
        f.turn("root", now - days * 86_400_000, marker);
    }
    f.scan();
    f.tasks("all");
    assert_eq!(f.bus.count("SELECT count(*) FROM usage_events"), 4);
    assert_eq!(f.bus.count("SELECT count(*) FROM usage_events u LEFT JOIN task_turns t ON t.event_id=u.id WHERE t.task_id IS NULL"), 0);
    assert_eq!(f.bus.count("SELECT count(*) FROM (SELECT event_id FROM task_turns GROUP BY event_id HAVING count(*)<>1)"), 0);
    assert_eq!(assignments(&f).len(), 4);
    for (range, count) in [("all", 4), ("30d", 3), ("7d", 2), ("today", 1)] {
        let tasks = f.tasks(range);
        let summary = f.summary(range, count);
        let measured: f64 = tasks.iter().map(|t| money(t, "measured")).sum();
        let cutoff = match range {
            "30d" => now - 30 * 86_400_000,
            "7d" => now - 7 * 86_400_000,
            "today" => chrono::Utc::now()
                .date_naive()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis(),
            _ => 0,
        };
        let turn_cost: f64 = f
            .bus
            .db()
            .query_row(
                "SELECT sum(cost_eur) FROM usage_events WHERE ts>=?1",
                [cutoff],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            cents(measured),
            cents(turn_cost),
            "§3c E: task sum equals sqlite turn sum for {range}"
        );
        assert_eq!(
            cents(measured),
            cents(summary["cost_eur"].as_f64().unwrap()),
            "§3c E: cost conserved for {range}"
        );
        assert!(measured > 0.0, "positive priced fixture, not empty totals");
    }
}
fn prices(f: &Fixture) {
    let path = f.bus.root.join("config/axon/pricing.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "display_currency='EUR'\nfx_to_display=0.5\n[models.'claude-opus-4.8']\ninput=2.0\noutput=11.0\ncache_read=0.25\ncache_write_5m=2.5\ncache_write_1h=4.0\n[models.'gpt-5.4']\ninput=2.5\noutput=15.0\ncache_read=0.25\ncache_write_5m=0.0\ncache_write_1h=0.0\nchatgpt_credit_input=62.5\nchatgpt_credit_output=375.0\nchatgpt_credit_cache_read=6.25\n[tools]\n").unwrap();
}
#[test]
fn cache_write_five_minute_and_one_hour_terms_are_priced_separately() {
    let f = Fixture::new(false);
    prices(&f);
    let t = now_ms();
    f.prompt("root", t, json!("price"));
    let mut row = completion(t + 1, 5_000_000);
    row["message"]["usage"] = json!({"input_tokens":1_000_000,"output_tokens":5_000_000,
        "cache_read_input_tokens":2_000_000,"cache_creation_input_tokens":7_000_000,
        "cache_creation":{"ephemeral_5m_input_tokens":3_000_000,"ephemeral_1h_input_tokens":4_000_000}});
    f.append("root", row);
    f.scan();
    let task = f.assigned(5_000_000);
    // (1*2 + 2*.25 + 3*2.5 + 4*4 + 5*11) * .5 = EUR 40.50.
    assert!(
        (money(&task, "measured") - 40.5).abs() < 1e-9,
        "separate cache terms and FX: {task}"
    );
}
#[test]
fn subscription_credits_remain_separate_from_euros() {
    let f = Fixture::new(false);
    prices(&f);
    codex(&f, now_ms() - 5000, 1);
    f.scan();
    let tasks = f.tasks("all");
    assert_eq!(tasks.len(), 1);
    let task = &tasks[0];
    assert_eq!(money(task, "measured"), 0.0);
    assert_eq!(money(task, "estimated"), 0.0);
    // Explicit fixture prices: uncached=800, cached=200, output=500; FX never applies to credits.
    let expected = (800.0 * 62.5 + 200.0 * 6.25 + 500.0 * 375.0) / 1_000_000.0;
    assert!(
        (money(task, "credits") - expected).abs() < 1e-9,
        "credits retained: {task}"
    );
}
#[test]
fn mcp_tool_absent_from_pricing_is_explicitly_unpriced() {
    let f = Fixture::new(false);
    prices(&f);
    let t = now_ms();
    f.prompt("root", t, json!("tool"));
    let tool = "mcp__fixture__unlisted_lookup";
    let mut row = completion(t + 1, 81);
    row["message"]["content"] =
        json!([{"type":"tool_use","id":"tool-fixture","name":tool,"input":{}}]);
    f.append("root", row);
    f.scan();
    let task = f.assigned(81);
    fn names(value: &Value, name: &str) -> bool {
        match value {
            Value::String(s) => s == name,
            Value::Array(a) => a.iter().any(|v| names(v, name)),
            Value::Object(o) => o.contains_key(name) || o.values().any(|v| names(v, name)),
            _ => false,
        }
    }
    assert!(
        names(&task["cost"]["tools_unpriced"], tool),
        "§3c C: missing unpriced MCP tool: {task}"
    );
    assert!(
        (money(&task, "measured") - (1000.0 * 2.0 + 81.0 * 11.0) / 1_000_000.0 * 0.5).abs() < 1e-9
    );
}
#[test]
fn local_compute_with_nonzero_duration_is_zero_without_opt_in_rate() {
    let f = Fixture::new(false);
    let t = now_ms() - 5000;
    f.bus.register_at("local", "opencode", None, &f.cwd);
    let mut row: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/opencode_message.json"
    ))
    .unwrap();
    row["modelID"] = json!("ollama/qwen3.6-deepcoder");
    row["providerID"] = json!("ollama");
    row["cost"] = json!(0);
    row["time"] = json!({"created":t,"completed":t+3000});
    row["path"] = json!({"cwd":f.cwd,"root":f.cwd});
    row["tokens"]["output"] = json!(91);
    row["tokens"]["reasoning"] = json!(0);
    let source = f.bus.root.join("data/opencode/opencode.db");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    let db = rusqlite::Connection::open(source).unwrap();
    db.execute_batch("CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT)").unwrap();
    db.execute(
        "INSERT INTO message VALUES (?1,?2,?3,?4)",
        params!["local-turn", "local", t, row.to_string()],
    )
    .unwrap();
    drop(db);
    f.scan();
    let task = f.assigned(91);
    let duration: i64 = f
        .bus
        .db()
        .query_row(
            "SELECT duration_ms FROM usage_events WHERE tokens_out=91",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(duration, 3000, "fixture has measurable compute time");
    assert_eq!(money(&task, "measured"), 0.0);
    assert_eq!(money(&task, "estimated"), 0.0);
}
#[test]
fn capture_off_request_name_is_time_only() {
    let f = Fixture::new(false);
    let t = now_ms() - 1000;
    f.prompt("root", t, json!("private operator prompt"));
    f.turn("root", t + 1, 101);
    f.scan();
    let expected = format!(
        "request at {}",
        chrono::DateTime::from_timestamp_millis(t)
            .unwrap()
            .format("%H:%M")
    );
    assert_eq!(f.assigned(101)["name"], expected);
}
#[test]
fn capture_on_request_name_redacts_secret_and_limits_to_eighty_characters() {
    let f = Fixture::new(true);
    let t = now_ms() - 5000;
    let secret = "sk-ant-api03-0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijk";
    f.prompt("root", t, json!(format!("Review {secret} now")));
    f.turn("root", t + 1, 111);
    f.prompt("root", t + 2000, json!("z".repeat(120)));
    f.turn("root", t + 2001, 112);
    f.scan();
    let task = f.assigned(111);
    let name = task["name"].as_str().unwrap();
    assert!(
        name.starts_with("Review "),
        "capture-on keeps safe prompt text: {name}"
    );
    assert!(
        !name.contains(secret) && !name.contains("0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
        "secret leaked: {name}"
    );
    assert!(name.chars().count() <= 80);
    assert_eq!(f.assigned(112)["name"], "z".repeat(80));
}
#[test]
fn api_requires_owner_session() {
    let f = Fixture::new(false);
    let response = f.server.raw_request(&f.bus, "GET", "/api/tasks", &[], "");
    assert_eq!(
        response.status,
        401,
        "§3c D: owner required, body={}",
        String::from_utf8_lossy(&response.body)
    );
}
#[test]
fn cli_and_api_return_same_newest_two_hundred_tasks_with_documented_shape() {
    let f = Fixture::new(false);
    let t = now_ms() - 500_000;
    for i in 0..205 {
        f.prompt("root", t + i * 1000, json!(format!("request {i}")));
        f.turn("root", t + i * 1000 + 1, 1000 + i as u64);
    }
    f.scan();
    let output = assert_cmd::Command::from(root_command(&f.bus))
        .args(["bus", "tasks", "--json"])
        .timeout(f.bus.remaining())
        .assert()
        .success()
        .get_output()
        .clone();
    let cli = parse_json(&output.stdout);
    let api = f.tasks("all");
    // §3c D says "same list"; it does not specify a CLI envelope.
    let rows = cli
        .as_array()
        .or_else(|| cli["tasks"].as_array())
        .expect("CLI JSON task list");
    assert_eq!(rows.len(), 200);
    assert_eq!(api.len(), 200);
    let ids = |rows: &[Value]| rows.iter().map(|r| r["id"].clone()).collect::<Vec<_>>();
    assert_eq!(ids(rows), ids(&api));
    assert_eq!(api[0]["opened_at"], t + 204_000);
    assert_eq!(api[199]["opened_at"], t + 5000);
    for (cli, http) in rows.iter().zip(&api) {
        assert!(
            cli.get("elapsed_ms").is_some(),
            "CLI includes elapsed_ms even for an open task"
        );
        for field in [
            "kind",
            "key",
            "name",
            "opened_at",
            "closed_at",
            "cost",
            "turns",
            "agents",
            "harnesses",
            "retries_cost",
            "human_wait_ms",
        ] {
            assert!(cli.get(field).is_some(), "CLI missing {field}");
            assert_eq!(cli[field], http[field], "same CLI/API field {field}");
        }
    }
}
