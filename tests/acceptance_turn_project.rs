//! Frozen BUS-PLAN §3c A. Expected identities come only from the spec.
//! Real repositories, a linked worktree, transcripts, CLI export and owner HTTP session.
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Sandbox {
    root: PathBuf,
    deadline: Instant,
}
struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
impl Sandbox {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "axon-turn-project-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        for dir in ["home", "data", "config", "cache", "tmp", "outside"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        Self {
            root: root.canonicalize().unwrap(),
            deadline: Instant::now() + Duration::from_secs(45),
        }
    }
    fn remaining(&self) -> Duration {
        let left = self.deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "turn-project acceptance exceeded its 45 s budget"
        );
        left
    }
    fn command(&self, binary: &str) -> Command {
        let mut cmd = Command::new(binary);
        cmd.env_clear()
            .current_dir(&self.root)
            .env("HOME", self.root.join("home"))
            .env("USERPROFILE", self.root.join("home"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_STATE_HOME", self.root.join("data/state"))
            .env("APPDATA", self.root.join("config"))
            .env("LOCALAPPDATA", self.root.join("data"))
            .env("CODEX_HOME", self.root.join("home/.codex"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("home/.claude"))
            .env("HERMES_HOME", self.root.join("home/.hermes"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("TMP", self.root.join("tmp"))
            .env("TEMP", self.root.join("tmp"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.root.join("empty-gitconfig"))
            .env("AXON_FED_RELAY", "disabled")
            .env("AXON_FED_BIND", "127.0.0.1:0")
            .env("TZ", "UTC")
            .env("LANG", "C");
        for key in ["PATH", "SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
        cmd
    }
    fn run(&self, cmd: &mut Command) -> String {
        let stdout = self.root.join("command.stdout");
        let stderr = self.root.join("command.stderr");
        let mut child = Running(
            cmd.stdin(Stdio::null())
                .stdout(std::fs::File::create(&stdout).unwrap())
                .stderr(std::fs::File::create(&stderr).unwrap())
                .spawn()
                .unwrap(),
        );
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                let out = std::fs::read_to_string(stdout).unwrap();
                assert!(
                    status.success(),
                    "{cmd:?}: {status}\n{out}\n{}",
                    std::fs::read_to_string(stderr).unwrap()
                );
                return out;
            }
            std::thread::sleep(self.remaining().min(Duration::from_millis(10)));
        }
    }
    fn axon(&self, args: &[&str]) -> String {
        self.run(self.command(env!("CARGO_BIN_EXE_axon")).args(args))
    }
    fn git(&self, cwd: &Path, args: &[&str]) {
        self.run(
            self.command("git")
                .current_dir(cwd)
                .args([
                    "-c",
                    "user.name=Acceptance Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=",
                ])
                .args(args),
        );
    }
    fn seed(&self) {
        let main = self.root.join("main-project");
        std::fs::create_dir(&main).unwrap();
        self.git(&main, &["init", "--initial-branch=main"]);
        self.git(&main, &["commit", "--allow-empty", "-m", "fixture"]);
        let linked = self.root.join("different-worktree-name");
        self.git(
            &main,
            &["worktree", "add", "-b", "fixture", linked.to_str().unwrap()],
        );
        for (session, cwd, marker) in [
            ("main-session-private", main.join("nested"), 101),
            ("work-session-private", linked.join("nested"), 102),
            ("away-session-private", self.root.join("outside"), 103),
        ] {
            std::fs::create_dir_all(&cwd).unwrap();
            let dir = self.root.join("home/.claude/projects/fixture");
            std::fs::create_dir_all(&dir).unwrap();
            let rows = include_str!("fixtures/claude_main.jsonl")
                .lines()
                .map(|line| {
                    let mut row: Value = serde_json::from_str(line).unwrap();
                    row["cwd"] = json!(cwd);
                    row["sessionId"] = json!(session);
                    row["message"]["id"] = json!(format!("message-{marker}"));
                    row["message"]["usage"]["output_tokens"] = json!(marker);
                    row.to_string()
                })
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";
            std::fs::write(dir.join(format!("{session}.jsonl")), rows).unwrap();
        }
    }
    fn server(&self) -> (Running, SocketAddr, String, String) {
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let mut process = Running(
            self.command(env!("CARGO_BIN_EXE_axon"))
                .args([
                    "--port",
                    &address.port().to_string(),
                    "--no-open",
                    "--no-hooks",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(std::fs::File::create(self.root.join("server.stderr")).unwrap())
                .spawn()
                .unwrap(),
        );
        loop {
            assert!(
                process.0.try_wait().unwrap().is_none(),
                "server exited: {}",
                std::fs::read_to_string(self.root.join("server.stderr")).unwrap()
            );
            if TcpStream::connect_timeout(&address, self.remaining().min(Duration::from_millis(50)))
                .is_ok()
            {
                break;
            }
            std::thread::sleep(self.remaining().min(Duration::from_millis(10)));
        }
        let login = self.axon(&["open", "--print"]);
        let nonce = login
            .trim()
            .split_once("#login=")
            .expect("owner login URL")
            .1;
        let (status, headers, body) = self.http(
            address,
            "POST",
            "/api/session",
            "",
            "",
            &json!({"nonce":nonce}).to_string(),
        );
        assert_eq!(status, 200, "owner session exchange");
        let cookie = headers["set-cookie"].split(';').next().unwrap().to_owned();
        (
            process,
            address,
            cookie,
            body["token"]
                .as_str()
                .expect("owner session token")
                .to_owned(),
        )
    }
    fn http(
        &self,
        address: SocketAddr,
        method: &str,
        path: &str,
        cookie: &str,
        token: &str,
        body: &str,
    ) -> (u16, BTreeMap<String, String>, Value) {
        let mut stream = TcpStream::connect_timeout(&address, self.remaining()).unwrap();
        stream.set_read_timeout(Some(self.remaining())).unwrap();
        stream.set_write_timeout(Some(self.remaining())).unwrap();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nCookie: {cookie}\r\nx-axon-session: {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut headers = BTreeMap::new();
        loop {
            line.clear();
            assert!(reader.read_line(&mut line).unwrap() > 0, "EOF in headers");
            if line == "\r\n" {
                break;
            }
            let (key, value) = line.split_once(':').unwrap();
            headers.insert(key.to_ascii_lowercase(), value.trim().to_owned());
        }
        let mut bytes = Vec::new();
        if headers
            .get("transfer-encoding")
            .is_some_and(|v| v == "chunked")
        {
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                let n = usize::from_str_radix(line.trim().split(';').next().unwrap(), 16).unwrap();
                if n == 0 {
                    break;
                }
                let start = bytes.len();
                bytes.resize(start + n, 0);
                reader.read_exact(&mut bytes[start..]).unwrap();
                let mut crlf = [0; 2];
                reader.read_exact(&mut crlf).unwrap();
                assert_eq!(crlf, *b"\r\n");
            }
        } else {
            reader.read_to_end(&mut bytes).unwrap();
        }
        let body = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| panic!("HTTP {status}: {}", String::from_utf8_lossy(&bytes)));
        (status, headers, body)
    }
}

fn check_feed(marker: u64, expected_repo: Value, session: &str) {
    let sandbox = Sandbox::new();
    sandbox.seed();
    let (_process, address, cookie, token) = sandbox.server();
    let summary = loop {
        let (status, _, summary) = sandbox.http(
            address,
            "GET",
            "/api/summary?range=all",
            &cookie,
            &token,
            "",
        );
        assert_eq!(status, 200);
        if summary["events"] == 3 {
            break summary;
        }
        std::thread::sleep(sandbox.remaining().min(Duration::from_millis(20)));
    };
    let recent = summary["recent"].as_array().expect("recent array");
    assert_eq!(recent.len(), 3, "real fixture turns ingested exactly once");
    let row = recent.iter().find(|r| r["tokens_out"] == marker).unwrap();
    assert!(
        row.get("repo").is_some(),
        "§3c A: recent turn is missing repo: {row}"
    );
    assert_eq!(row["repo"], expected_repo, "§3c A: repository resolution");
    assert_eq!(
        row["session"], session,
        "§3c A: first four session characters"
    );
}

#[test]
fn main_repository_turn_names_repo_and_session() {
    check_feed(101, json!("main-project"), "main");
}
#[test]
fn worktree_turn_names_main_repository_and_session() {
    check_feed(102, json!("main-project"), "work");
}
#[test]
fn outside_repository_turn_has_explicit_null_repo_and_session() {
    check_feed(103, Value::Null, "away");
}

#[test]
fn existing_scan_json_export_omits_repo_and_session() {
    // src/export is currently a placeholder; --scan-only is the existing JSON export.
    let sandbox = Sandbox::new();
    sandbox.seed();
    let output = sandbox.axon(&["--scan-only", "--no-hooks"]);
    let value: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["events"], 3, "nonempty export is required");
    fn no_identity(value: &Value) {
        match value {
            Value::Object(map) => {
                for key in ["repo", "session"] {
                    assert!(!map.contains_key(key), "export leaks {key}: {value}");
                }
                map.values().for_each(no_identity);
            }
            Value::Array(rows) => rows.iter().for_each(no_identity),
            _ => {}
        }
    }
    no_identity(&value);
    for secret in ["main-project", "different-worktree-name", "session-private"] {
        assert!(!output.contains(secret), "export leaks {secret}");
    }
}
