//! Contract decisions
//! - `init` creates/migrates $XDG_DATA_HOME/axon/axon.db; hooks never create an absent hub.
//! - `register --id ID --harness H --session ID --cwd PATH [--parent ID]` creates an idle
//!   node; root_id is the root's id (including on roots). Harness tags use CLI spellings.
//! - Hook-created root ids equal session_id; Claude/Codex child ids equal agent_id;
//!   Hermes child ids equal child_session_id. Hook timestamps are receipt-time epoch ms.
//! - Successful commands/hooks exit 0; an allowing hook has empty stdout; denials exit 0
//!   with the harness JSON in the brief. CLI validation errors exit 2 with stderr.
//! - `send --from ID --to ID --kind KIND --body TEXT [--thread ID]` returns JSON
//!   containing `id` and `thread`. Peer body length counts Unicode scalar values.
//! - `link --from ROOT --to ROOT` proposes; `accept --from ROOT --to ROOT` reciprocates.
//! - SQLite usage follows §2 (cache_write_tokens means 5-minute writes); timestamps and
//!   edge expires_at are epoch ms. Seeding uses the binary-created schema, never a mock DB.

#![allow(dead_code)] // Each integration-test crate uses a different subset of helpers.

pub mod delivery;
pub mod faults;
pub mod fed;
mod http;
pub mod invite;
pub mod limits;
pub mod pairing_checks;
pub mod settings;
pub mod share_checks;
pub mod sse;

#[allow(unused_imports)] // Each integration-test crate uses a different subset of helpers.
pub use http::{response_headers, Response, Server};

use assert_cmd::Command;
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

pub const HARNESSES: [&str; 4] = ["claude", "codex", "opencode", "hermes"];

pub struct Bus {
    _temp: TempDir,
    pub root: PathBuf,
    deadline: Instant,
}

impl Bus {
    pub fn new() -> Self {
        Self::with_limit(Duration::from_secs(4))
    }

    pub fn with_limit(limit: Duration) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        // Git and slash-separated fixture paths need a non-verbatim drive path on Windows.
        #[cfg(windows)]
        let root = {
            let path = root.to_str().unwrap();
            PathBuf::from(
                path.strip_prefix(r"\\?\")
                    .filter(|path| path.as_bytes().get(1) == Some(&b':'))
                    .unwrap_or(path),
            )
        };
        for dir in ["home", "data", "config", "cache", "work", "tmp"] {
            fs::create_dir(root.join(dir)).unwrap();
        }
        Self {
            _temp: temp,
            root,
            deadline: Instant::now() + limit,
        }
    }

    pub fn remaining(&self) -> Duration {
        let left = self.deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "acceptance test exceeded its total time budget"
        );
        left
    }

    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }
    pub fn db_path(&self) -> PathBuf {
        self.root.join("data/axon/axon.db")
    }

    pub fn process(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin!("axon-bus"));
        self.isolate(&mut cmd);
        cmd
    }

    pub fn isolate(&self, cmd: &mut std::process::Command) {
        cmd.env_clear()
            .current_dir(self.root.join("work"))
            .env("HOME", self.home())
            .env("USERPROFILE", self.home())
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_STATE_HOME", self.root.join("data/state"))
            .env("APPDATA", self.root.join("config"))
            .env("LOCALAPPDATA", self.root.join("data"))
            .env("CODEX_HOME", self.home().join(".codex"))
            .env("CLAUDE_CONFIG_DIR", self.home().join(".claude"))
            .env("HERMES_HOME", self.home().join(".hermes"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("TMP", self.root.join("tmp"))
            .env("TEMP", self.root.join("tmp"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("empty-gitconfig"))
            .env("AXON_FED_RELAY", "disabled")
            .env("AXON_FED_BIND", "127.0.0.1:0")
            .env("NO_COLOR", "1")
            .env("TZ", "UTC")
            .env("LANG", "C");
        // Required to locate git and OS runtime libraries; no credentials/config overrides survive.
        for key in ["PATH", "SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
    }

    pub fn cmd(&self) -> Command {
        let mut cmd = Command::from(self.process());
        cmd.timeout(self.remaining());
        cmd
    }

    pub fn ok(&self, args: &[&str]) -> Output {
        self.cmd()
            .args(args)
            .assert()
            .success()
            .get_output()
            .clone()
    }

    pub fn json(&self, args: &[&str]) -> Value {
        parse_json(&self.ok(args).stdout)
    }

    pub fn init(&self) {
        self.ok(&["init"]);
        assert!(
            self.db_path().is_file(),
            "init must create the shared Axon database"
        );
    }

    pub fn db(&self) -> Connection {
        let conn = Connection::open_with_flags(self.db_path(), OpenFlags::SQLITE_OPEN_READ_WRITE)
            .expect("the binary, not the test, must create the database");
        conn.busy_timeout(Duration::from_millis(150)).unwrap();
        conn
    }

    pub fn register(&self, id: &str, harness: &str, parent: Option<&str>) {
        self.register_at(id, harness, parent, &self.root.join("work"));
    }

    pub fn register_at(&self, id: &str, harness: &str, parent: Option<&str>, cwd: &Path) {
        let mut args = vec![
            "register",
            "--id",
            id,
            "--harness",
            harness,
            "--session",
            id,
            "--cwd",
            cwd.to_str().unwrap(),
        ];
        if let Some(parent) = parent {
            args.extend(["--parent", parent]);
        }
        self.ok(&args);
    }

    pub fn fixture(&self, harness: &str, file: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/hooks")
            .join(harness)
            .join(format!("{file}.json"));
        let mut payload: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        self.rehome(&mut payload);
        payload
    }

    fn rehome(&self, value: &mut Value) {
        match value {
            Value::String(s) => {
                for (prefix, replacement) in [
                    ("/Users/user", self.home()),
                    ("/tmp/axon-fixture", self.root.join("fixture")),
                ] {
                    if let Some(rest) = s.strip_prefix(prefix) {
                        *s = format!("{}{rest}", replacement.display());
                        break;
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|v| self.rehome(v)),
            Value::Object(items) => items.values_mut().for_each(|v| self.rehome(v)),
            _ => {}
        }
    }

    pub fn hook(&self, harness: &str, event: &str, payload: &Value) -> Output {
        self.cmd()
            .args(["hook", harness, event])
            .write_stdin(payload.to_string())
            .assert()
            .success()
            .get_output()
            .clone()
    }

    pub fn replay_hook(&self, harness: &str, file: &str) -> Output {
        self.hook(
            harness,
            file.trim_end_matches(".child"),
            &self.fixture(harness, file),
        )
    }

    pub fn pre(&self, harness: &str, id: &str) -> Value {
        let mut payload = self.fixture(harness, pre_event(harness));
        if harness == "opencode" {
            payload["input"]["sessionID"] = json!(id);
        } else {
            payload["session_id"] = json!(id);
        }
        payload
    }

    pub fn gate(&self, harness: &str, id: &str) -> Output {
        self.hook(harness, pre_event(harness), &self.pre(harness, id))
    }

    pub fn agent(&self, id: &str) -> Value {
        self.db()
            .query_row(
                "SELECT id,harness,session_id,parent_id,root_id,status,model FROM agents WHERE id=?1",
                [id],
                |r| {
                    Ok(json!({"id":r.get::<_,String>(0)?,"harness":r.get::<_,String>(1)?,
                "session_id":r.get::<_,String>(2)?,"parent_id":r.get::<_,Option<String>>(3)?,
                "root_id":r.get::<_,String>(4)?,"status":r.get::<_,String>(5)?,
                "model":r.get::<_,Option<String>>(6)?}))
                },
            )
            .unwrap()
    }

    pub fn count(&self, sql: &str) -> i64 {
        self.db().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    pub fn send(&self, from: &str, to: &str, kind: &str, body: &str) -> Value {
        self.json(&[
            "send", "--from", from, "--to", to, "--kind", kind, "--body", body,
        ])
    }

    pub fn linked_roots(&self) {
        for id in ["orch1", "orch2"] {
            self.register(id, "claude", None);
        }
        self.register("sub1", "claude", Some("orch1"));
        self.register("sub2", "claude", Some("orch2"));
        self.ok(&["link", "--from", "orch1", "--to", "orch2"]);
        self.ok(&["accept", "--from", "orch2", "--to", "orch1"]);
    }

    pub fn usage(&self, agent: &str, model: &str, buckets: [i64; 4], ts: i64, offset: i64) {
        self.db()
            .execute(
                "INSERT INTO usage (agent_id,ts,model,input_tokens,output_tokens,cache_read_tokens,
             cache_write_tokens,cost_usd,source_offset) VALUES (?1,?2,?3,?4,?5,?6,?7,NULL,?8)",
                params![agent, ts, model, buckets[0], buckets[1], buckets[2], buckets[3], offset],
            )
            .unwrap();
    }

    pub fn spawn(&self, args: &[&str]) -> Running {
        Running(
            self.process()
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        )
    }
}

pub struct Running(pub Child);
impl Running {
    pub fn finish(&mut self, timeout: Duration) -> Output {
        use std::io::Read;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut pipe) = self.0.stdout.take() {
                    pipe.read_to_end(&mut stdout).unwrap();
                }
                if let Some(mut pipe) = self.0.stderr.take() {
                    pipe.read_to_end(&mut stderr).unwrap();
                }
                return Output {
                    status,
                    stdout,
                    stderr,
                };
            }
            assert!(
                Instant::now() < deadline,
                "child process did not exit within {timeout:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
pub fn parse_json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|e| panic!("invalid JSON: {e}: {}", String::from_utf8_lossy(bytes)))
}
pub fn pre_event(harness: &str) -> &str {
    match harness {
        "hermes" => "pre_tool_call",
        "opencode" => "tool.execute.before",
        _ => "PreToolUse",
    }
}
pub fn assert_allowed(output: &Output) {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "allow must be silent: {output:?}");
}
pub fn deny_reason(harness: &str, output: &Output) -> String {
    assert!(
        output.status.success(),
        "hook denial is a protocol reply, not a process failure"
    );
    let reply = parse_json(&output.stdout);
    let reason = match harness {
        "claude" | "codex" => {
            assert_eq!(reply["hookSpecificOutput"]["hookEventName"], "PreToolUse");
            assert_eq!(reply["hookSpecificOutput"]["permissionDecision"], "deny");
            reply["hookSpecificOutput"]["permissionDecisionReason"].as_str()
        }
        "hermes" => {
            assert_eq!(reply["decision"], "block");
            reply["reason"].as_str()
        }
        "opencode" => {
            assert_eq!(reply["decision"], "deny");
            reply["reason"].as_str()
        }
        _ => panic!("unexpected harness"),
    }
    .expect("denial must explain why");
    assert!(!reason.is_empty());
    reason.to_owned()
}
pub fn assert_route(text: &str, nodes: &[&str]) {
    let normalized = text.replace('→', "->");
    assert!(
        normalized.contains(&nodes.join(" -> ")),
        "route must be ordered and explicit: {text}"
    );
}
pub fn write_jsonl(path: &Path, records: &[Value]) {
    let mut file = fs::File::create(path).unwrap();
    for record in records {
        writeln!(file, "{record}").unwrap();
    }
}
