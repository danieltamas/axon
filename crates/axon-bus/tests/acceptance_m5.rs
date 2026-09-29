//! Contract decisions
//! - `serve --port 0 --ready-file PATH [--content]` binds an ephemeral loopback port and
//!   atomically writes {url,token} after readiness; token changes on every boot.
//! - POST /api/msg accepts {from_id,to_id,kind,body}, requires X-Axon-Token plus exact
//!   Origin=url, and returns 201 JSON {id,thread}. Denied requests must not insert messages.
//! - Snapshot: {repos:[{repo:MAIN_PATH|null,name,harnesses:[{harness,roots:[NODE]}]}]}.
//!   NODE has id,children,repo,repo_badge,branch,narrative. No-repo name is `no repo`;
//!   paths are canonical absolute paths. Cross-repo children have repo_badge=their repo.
//! - SSE sends `event: snapshot` with the complete snapshot JSON as data for updates.
//! - `replay FILE --speed 50x` consumes JSONL {harness,session_id,record,reasoning_tokens?};
//!   records use native transcript shapes, and missing reasoning may carry a token count.
//!   OpenCode records combine its stored message `info` with ordered `parts`; Hermes
//!   records are assistant messages with content/reasoning_content/tool_calls.
//! - Narrative inputs below are synthetic examples of §7 wire shapes, not M0 captures.
//!   Claude progress uses thinking.display=updates (also valid with mode=between_tools).
//! - Narrative rows: assistant/progress have kind,text,source; reasoning additionally has
//!   recorded,tokens, with text=null and label=`reasoning — not recorded by H` if opaque.
//!   Consecutive tools form kind=tool_run,collapsed=true,count=N,tools=[{name,...}].
//! - Content-off replay retains structure but neither stores nor returns narrative text.

mod common;
use common::*;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

fn server(content: bool) -> (Bus, Server) {
    let bus = Bus::new();
    bus.init();
    let server = Server::start(&bus, content);
    (bus, server)
}
fn find_node<'a>(value: &'a Value, id: &str) -> Option<&'a Value> {
    match value {
        Value::Object(object) => {
            if object.get("id").and_then(Value::as_str) == Some(id) {
                return Some(value);
            }
            object.values().find_map(|v| find_node(v, id))
        }
        Value::Array(array) => array.iter().find_map(|v| find_node(v, id)),
        _ => None,
    }
}
fn node<'a>(snapshot: &'a Value, id: &str) -> &'a Value {
    find_node(snapshot, id).unwrap_or_else(|| panic!("missing node {id}: {snapshot}"))
}

// BUS-PLAN §7 Security: non-loopback Host values cannot reach the API through DNS rebinding.
#[test]
fn non_loopback_host_gets_403_while_loopback_host_succeeds() {
    let (bus, server) = server(false);
    for host in [
        "attacker.invalid",
        "127.0.0.1.attacker.invalid",
        "localhost.attacker.invalid",
    ] {
        assert_eq!(
            server
                .request(&bus, "GET", "/api/snapshot", &[("Host", host)], "")
                .status,
            403
        );
    }
    assert_eq!(
        server.request(&bus, "GET", "/api/snapshot", &[], "").status,
        200
    );
}

// BUS-PLAN §7 Security: POST requires both exact same-origin and the current per-boot token.
#[test]
fn cross_origin_missing_origin_missing_token_and_wrong_token_posts_get_403() {
    let (bus, server) = server(false);
    bus.register("root", "claude", None);
    bus.register("child", "codex", Some("root"));
    let body = json!({"from_id":"root","to_id":"child","kind":"redirect","body":"http marker"})
        .to_string();
    let before = bus.count("SELECT count(*) FROM messages");
    let cases = vec![
        vec![
            ("Origin", "https://attacker.invalid"),
            ("X-Axon-Token", server.token.as_str()),
        ],
        vec![("X-Axon-Token", server.token.as_str())],
        vec![("Origin", server.url.as_str())],
        vec![
            ("Origin", server.url.as_str()),
            ("X-Axon-Token", "wrong-token"),
        ],
    ];
    for mut headers in cases {
        headers.push(("Content-Type", "application/json"));
        assert_eq!(
            server
                .request(&bus, "POST", "/api/msg", &headers, &body)
                .status,
            403
        );
        assert_eq!(bus.count("SELECT count(*) FROM messages"), before);
    }
    let accepted = server.request(
        &bus,
        "POST",
        "/api/msg",
        &[
            ("Origin", &server.url),
            ("X-Axon-Token", &server.token),
            ("Content-Type", "application/json"),
        ],
        &body,
    );
    assert_eq!(accepted.status, 201);
    let id = parse_json(&accepted.body)["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let stored: String = bus
        .db()
        .query_row("SELECT body FROM messages WHERE id=?1", [id], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, "http marker");
    assert_eq!(bus.count("SELECT count(*) FROM messages"), before + 1);
}

// BUS-PLAN §7 Security: the dashboard has a strict self-only CSP with no inline scripts.
#[test]
fn dashboard_csp_is_present_self_only_and_has_no_unsafe_inline() {
    let (bus, server) = server(false);
    let response = server.request(&bus, "GET", "/", &[], "");
    assert_eq!(response.status, 200);
    let csp = response
        .headers
        .get("content-security-policy")
        .expect("missing CSP");
    let directives: Vec<_> = csp.split(';').map(str::trim).collect();
    assert!(directives.contains(&"default-src 'self'"), "{csp}");
    let script = directives.iter().find(|d| d.starts_with("script-src "));
    if let Some(script) = script {
        assert_eq!(*script, "script-src 'self'");
    }
    assert!(
        !csp.contains("unsafe-inline") && !csp.contains("unsafe-eval"),
        "{csp}"
    );
    assert!(!csp.contains('*'), "{csp}");
}

fn git(bus: &Bus, cwd: &Path, args: &[&str]) {
    let mut cmd = std::process::Command::new("git");
    bus.isolate(&mut cmd);
    cmd.current_dir(cwd)
        .args([
            "-c",
            "user.name=Acceptance Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args);
    assert_cmd::Command::from(cmd)
        .timeout(bus.remaining())
        .assert()
        .success();
}

// BUS-PLAN §7 Live tree: worktrees group under main repos and children never split from their parents.
#[test]
fn snapshot_groups_repo_harness_root_children_with_worktrees_and_cross_repo_badges() {
    let (bus, server) = server(false);
    let main = bus.root.join("main");
    let other = bus.root.join("other");
    let worktree = bus.root.join("worktree");
    let outside = bus.root.join("outside");
    for path in [&main, &other, &outside] {
        std::fs::create_dir(path).unwrap();
    }
    for path in [&main, &other] {
        git(&bus, path, &["init", "--initial-branch=main"]);
        git(&bus, path, &["commit", "--allow-empty", "-m", "fixture"]);
    }
    git(
        &bus,
        &main,
        &[
            "worktree",
            "add",
            "-b",
            "feature-fixture",
            worktree.to_str().unwrap(),
        ],
    );
    bus.register_at("orchestrator", "claude", None, &main);
    bus.register_at("cross-repo-child", "claude", Some("orchestrator"), &other);
    bus.register_at("worktree-root", "codex", None, &worktree);
    bus.register_at("other-root", "claude", None, &other);
    bus.register_at("outside-root", "hermes", None, &outside);
    let snapshot = server.snapshot(&bus);
    let repos = snapshot["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 3);
    let main_group = repos.iter().find(|r| r["repo"] == json!(main)).unwrap();
    let harnesses = main_group["harnesses"].as_array().unwrap();
    assert_eq!(harnesses.len(), 2);
    let claude = harnesses.iter().find(|h| h["harness"] == "claude").unwrap();
    assert_eq!(claude["roots"].as_array().unwrap().len(), 1);
    assert_eq!(claude["roots"][0]["id"], "orchestrator");
    let children = claude["roots"][0]["children"].as_array().unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0]["id"], "cross-repo-child");
    assert_eq!(children[0]["repo_badge"], json!(other));
    let codex = harnesses.iter().find(|h| h["harness"] == "codex").unwrap();
    assert_eq!(codex["roots"].as_array().unwrap().len(), 1);
    assert_eq!(codex["roots"][0]["id"], "worktree-root");
    assert_eq!(codex["roots"][0]["branch"], "feature-fixture");
    assert_eq!(codex["roots"][0]["repo"], json!(main));
    let other_group = repos.iter().find(|r| r["repo"] == json!(other)).unwrap();
    assert!(find_node(other_group, "cross-repo-child").is_none());
    assert_eq!(other_group["harnesses"].as_array().unwrap().len(), 1);
    assert_eq!(
        other_group["harnesses"][0]["roots"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(other_group["harnesses"][0]["roots"][0]["id"], "other-root");
    let no_repo = repos.iter().find(|r| r["repo"].is_null()).unwrap();
    assert_eq!(no_repo["name"], "no repo");
    assert_eq!(no_repo["harnesses"][0]["harness"], "hermes");
    assert_eq!(no_repo["harnesses"][0]["roots"][0]["id"], "outside-root");
}

// BUS-PLAN §7 and §9 M5: a live SSE subscription observes a new registration within one second.
#[test]
fn new_registration_reaches_the_existing_sse_stream_within_one_second() {
    let (bus, server) = server(false);
    let mut socket = server.connect(Duration::from_millis(900));
    write!(
        socket,
        "GET /api/stream HTTP/1.1\r\nHost: {}\r\nAccept: text/event-stream\r\n\r\n",
        server.address
    )
    .unwrap();
    let mut reader = BufReader::new(socket);
    let (status, headers) = response_headers(&mut reader);
    assert_eq!(status, 200);
    assert!(headers["content-type"].starts_with("text/event-stream"));
    let start = Instant::now();
    bus.register("sse-new-root", "claude", None);
    let mut decoded = String::new();
    loop {
        let remaining = Duration::from_secs(1).saturating_sub(start.elapsed());
        assert!(
            !remaining.is_zero(),
            "new registration was not streamed within one second"
        );
        reader.get_ref().set_read_timeout(Some(remaining)).unwrap();
        if headers
            .get("transfer-encoding")
            .is_some_and(|s| s.eq_ignore_ascii_case("chunked"))
        {
            let mut size = String::new();
            reader.read_line(&mut size).unwrap();
            let size = usize::from_str_radix(size.trim().split(';').next().unwrap(), 16).unwrap();
            assert_ne!(size, 0, "SSE stream ended before registration");
            let mut bytes = vec![0; size];
            reader.read_exact(&mut bytes).unwrap();
            decoded.push_str(&String::from_utf8(bytes).unwrap());
            let mut crlf = [0; 2];
            reader.read_exact(&mut crlf).unwrap();
            assert_eq!(&crlf, b"\r\n");
        } else {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            decoded.push_str(&line);
        }
        while let Some(end) = decoded.find("\n\n") {
            let event = decoded[..end].to_owned();
            decoded.drain(..end + 2);
            let data: String = event
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(str::trim)
                .collect();
            if data.is_empty() {
                continue;
            }
            let snapshot: Value = serde_json::from_str(&data).unwrap();
            if find_node(&snapshot, "sse-new-root").is_some() {
                assert!(event.lines().any(|l| l.trim() == "event: snapshot"));
                assert!(start.elapsed() < Duration::from_secs(1));
                return;
            }
        }
    }
}

fn narrative_records(harness: &str, timestamp: &str, timestamp_ms: i64) -> Vec<Value> {
    let text = format!("{harness} assistant marker");
    let reasoning = format!("{harness} reasoning marker");
    let raw = match harness {
        "claude" => vec![json!({"type":"assistant","timestamp":timestamp,
            "sessionId":harness,"uuid":"narrative-claude","message":{"id":"msg-narrative","model":"claude-sonnet-4-6",
            "content":[{"type":"text","text":text},{"type":"thinking","thinking":reasoning,"signature":"opaque-readable"},
                {"type":"thinking","thinking":"","signature":"opaque-private"},
                {"type":"thinking","thinking":"claude progress marker","display":"updates","mode":"between_tools"},
                {"type":"tool_use","id":"r1","name":"Read","input":{"file_path":"a.rs"}},
                {"type":"tool_use","id":"r2","name":"Read","input":{"file_path":"b.rs"}}],
            "usage":{"input_tokens":20,"output_tokens":10}}})],
        "codex" => vec![
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}}),
            json!({"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":reasoning}],"encrypted_content":"opaque-summary"}}),
            json!({"type":"response_item","payload":{"type":"reasoning","summary":[],"encrypted_content":"opaque-private"}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"r1","name":"exec_command","arguments":"{\"cmd\":\"cat a.rs\"}"}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"r2","name":"exec_command","arguments":"{\"cmd\":\"cat b.rs\"}"}}),
        ],
        "opencode" => vec![
            json!({"info":{"id":"msg-narrative","sessionID":harness,"role":"assistant","modelID":"gpt-5.5","providerID":"openai",
            "time":{"created":timestamp_ms},"tokens":{"input":20,"output":10,"reasoning":7,"cache":{"read":0,"write":0}}},
            "parts":[{"id":"p1","type":"text","text":text},{"id":"p2","type":"reasoning","text":reasoning},
                {"id":"p3","type":"reasoning","text":""},
                {"id":"p4","type":"tool","callID":"r1","tool":"read","state":{"status":"completed","input":{"filePath":"a.rs"},"output":"file a"}},
                {"id":"p5","type":"tool","callID":"r2","tool":"read","state":{"status":"completed","input":{"filePath":"b.rs"},"output":"file b"}}]}),
        ],
        "hermes" => vec![
            json!({"role":"assistant","content":text,"reasoning_content":reasoning}),
            json!({"role":"assistant","content":null,"reasoning_content":"","tool_calls":[
                {"id":"r1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.rs\"}"}},
                {"id":"r2","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"b.rs\"}"}}]}),
        ],
        _ => unreachable!(),
    };
    raw.into_iter()
        .map(|record| json!({"harness":harness,"session_id":harness,"record":record,"reasoning_tokens":7}))
        .collect()
}

fn replay_narrative(bus: &Bus, harness: &str) {
    bus.register(harness, harness, None);
    let path = bus.root.join(format!("{harness}-narrative.jsonl"));
    // Fresh fixture timestamps keep retention from making this test depend on the calendar.
    let timestamp: String = bus
        .db()
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| {
            r.get(0)
        })
        .unwrap();
    write_jsonl(&path, &narrative_records(harness, &timestamp, now_ms()));
    bus.ok(&["replay", path.to_str().unwrap(), "--speed", "50x"]);
}

fn assert_narrative(harness: &str) {
    let (bus, server) = server(true);
    replay_narrative(&bus, harness);
    let snapshot = server.snapshot(&bus);
    let rows = node(&snapshot, harness)["narrative"].as_array().unwrap();
    assert!(rows.iter().any(|r| r["kind"] == "assistant"
        && r["text"] == format!("{harness} assistant marker")
        && r["source"] == harness));
    assert!(rows.iter().any(|r| r["kind"] == "reasoning"
        && r["recorded"] == true
        && r["text"] == format!("{harness} reasoning marker")
        && r["source"] == harness));
    let missing = rows
        .iter()
        .find(|r| r["kind"] == "reasoning" && r["recorded"] == false)
        .expect("missing not-recorded row");
    assert!(missing.get("text").unwrap().is_null());
    assert_eq!(missing["tokens"], 7);
    assert_eq!(
        missing["label"],
        format!("reasoning — not recorded by {harness}")
    );
    assert_eq!(missing["source"], harness);
    let tool_rows: Vec<_> = rows.iter().filter(|r| r["kind"] == "tool_run").collect();
    assert_eq!(tool_rows.len(), 1);
    assert_eq!(tool_rows[0]["count"], 2);
    assert_eq!(tool_rows[0]["collapsed"], true);
    assert_eq!(tool_rows[0]["tools"].as_array().unwrap().len(), 2);
    assert!(!snapshot.to_string().contains("opaque-private"));
    if harness == "claude" {
        let progress: Vec<_> = rows
            .iter()
            .filter(|r| r["text"] == "claude progress marker")
            .collect();
        assert_eq!(progress.len(), 1);
        assert_eq!(progress[0]["kind"], "progress");
        assert_eq!(progress[0]["source"], "claude");
    }
}

// BUS-PLAN §7 Live narrative: Claude summaries, opaque reasoning and progress remain distinct.
#[test]
fn claude_narrative_has_text_summary_not_recorded_progress_and_collapsed_tools() {
    assert_narrative("claude");
}

// BUS-PLAN §7 Live narrative: Codex exposes plaintext summaries, never encrypted reasoning.
#[test]
fn codex_narrative_has_text_summary_not_recorded_and_collapsed_tools() {
    assert_narrative("codex");
}

// BUS-PLAN §7 Live narrative: OpenCode records readable reasoning and collapses consecutive tools.
#[test]
fn opencode_narrative_has_text_reasoning_not_recorded_and_collapsed_tools() {
    assert_narrative("opencode");
}

// BUS-PLAN §7 Live narrative: Hermes assistant and reasoning content use the same narrative contract.
#[test]
fn hermes_narrative_has_text_reasoning_not_recorded_and_collapsed_tools() {
    assert_narrative("hermes");
}

// BUS-PLAN §7 Privacy: without --content, text never leaves the tailer for snapshot or storage.
#[test]
fn content_off_keeps_structure_but_neither_exposes_nor_stores_assistant_text() {
    let (bus, server) = server(false);
    for harness in HARNESSES {
        replay_narrative(&bus, harness);
    }
    let snapshot = server.snapshot(&bus);
    let serialized = snapshot.to_string();
    for marker in [
        "assistant marker",
        "reasoning marker",
        "progress marker",
        "opaque-private",
    ] {
        assert!(!serialized.contains(marker), "content leaked: {marker}");
    }
    for harness in HARNESSES {
        assert!(node(&snapshot, harness)["narrative"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "tool_run"));
    }
    let db = bus.db();
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for table in tables {
        let mut query = db
            .prepare(&format!("SELECT * FROM \"{}\"", table.replace('"', "\"\"")))
            .unwrap();
        let columns = query.column_count();
        let mut rows = query.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for column in 0..columns {
                if let rusqlite::types::ValueRef::Text(bytes) = row.get_ref(column).unwrap() {
                    let text = String::from_utf8_lossy(bytes);
                    for marker in [
                        "assistant marker",
                        "reasoning marker",
                        "progress marker",
                        "opaque-private",
                    ] {
                        assert!(
                            !text.contains(marker),
                            "content stored in {table}: {marker}"
                        );
                    }
                }
            }
        }
    }
}

// BUS-PLAN §7 Security: a per-boot token cannot authorize a later server process.
#[test]
fn restarting_serve_rotates_the_token_and_rejects_the_previous_token() {
    let (bus, first) = server(false);
    let old_token = first.token.clone();
    drop(first);
    let second = Server::start(&bus, false);
    assert_ne!(second.token, old_token);
    let response = second.request(
        &bus,
        "POST",
        "/api/msg",
        &[
            ("Origin", &second.url),
            ("X-Axon-Token", &old_token),
            ("Content-Type", "application/json"),
        ],
        "{}",
    );
    assert_eq!(response.status, 403);
}
