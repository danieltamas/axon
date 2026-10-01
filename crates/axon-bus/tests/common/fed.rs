//! P2P-SPEC §0: real isolated services, HTTP policy changes, read-only SQL assertions.
use super::{parse_json, Bus, Response, Server};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub fn db(bus: &Bus) -> Connection {
    let conn =
        Connection::open_with_flags(bus.db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    conn.busy_timeout(Duration::from_millis(150)).unwrap();
    conn
}
pub fn count(bus: &Bus, sql: &str) -> i64 {
    db(bus).query_row(sql, [], |r| r.get(0)).unwrap()
}
pub fn text(bus: &Bus, sql: &str, id: &str) -> String {
    db(bus).query_row(sql, [id], |r| r.get(0)).unwrap()
}
pub fn request(bus: &Bus, server: &Server, method: &str, path: &str, body: Value) -> Response {
    let encoded = if body.is_null() || body.as_object().is_some_and(|object| object.is_empty()) {
        String::new()
    } else {
        body.to_string()
    };
    server.request(
        bus,
        method,
        path,
        &[
            ("Origin", &server.url),
            ("Content-Type", "application/json"),
        ],
        &encoded,
    )
}
pub fn api(bus: &Bus, server: &Server, method: &str, path: &str, body: Value) -> Value {
    let response = request(bus, server, method, path, body);
    assert!(
        (200..300).contains(&response.status),
        "{method} {path}: {} {}",
        response.status,
        String::from_utf8_lossy(&response.body)
    );
    assert_eq!(
        response.headers.get("cache-control").map(String::as_str),
        Some("no-store")
    );
    if response.body.is_empty() {
        assert!(
            !path.starts_with("/api/settings/"),
            "Settings writes return the full body"
        );
        Value::Null
    } else {
        let value = parse_json(&response.body);
        if path.starts_with("/api/settings/") {
            for section in [
                "capture",
                "usage",
                "budgets",
                "hooks",
                "storage",
                "federation",
            ] {
                assert!(
                    value.get(section).is_some(),
                    "Settings response missing {section}"
                );
            }
        }
        value
    }
}
pub fn health(bus: &Bus, server: &Server) -> Value {
    let response = server.request(bus, "GET", "/api/fed", &[], "");
    assert_eq!(response.status, 200);
    let value = parse_json(&response.body);
    if value["enabled"] == true {
        assert!(!value["node_id"].as_str().unwrap().is_empty());
        let groups: Vec<_> = value["fingerprint"].as_str().unwrap().split(' ').collect();
        assert_eq!(groups.len(), 4);
        assert!(groups
            .iter()
            .all(|group| group.len() == 4 && group.bytes().all(|c| c.is_ascii_hexdigit())));
    }
    value
}
pub fn eventually(bus: &Bus, label: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + bus.remaining();
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out: {label}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
pub fn peer(bus: &Bus, server: &Server, id: &str) -> Value {
    health(bus, server)["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["peer_id"] == id)
        .expect("peer in health")
        .clone()
}
pub fn live_id(bus: &Bus) -> String {
    text(
        bus,
        "SELECT peer_id FROM peers WHERE state<>'removed' AND node_id<>?1",
        "",
    )
}
pub fn git(bus: &Bus, cwd: &Path, args: &[&str]) {
    let mut command = std::process::Command::new("git");
    bus.isolate(&mut command);
    command
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
        .args(args);
    assert_cmd::Command::from(command)
        .timeout(bus.remaining())
        .assert()
        .success();
}
pub fn repo(bus: &Bus, name: &str) -> PathBuf {
    let path = bus.root.join(name);
    std::fs::create_dir(&path).unwrap();
    git(bus, &path, &["init", "--initial-branch=main"]);
    git(bus, &path, &["commit", "--allow-empty", "-m", name]);
    path.canonicalize().unwrap()
}

pub struct Pair {
    // Services drop before their isolated directories (including on assertion failure).
    pub sa: Server,
    pub sb: Server,
    pub a: Bus,
    pub b: Bus,
    pub pa: String,
    pub pb: String,
    pub ra: PathBuf,
    pub rb: PathBuf,
}
impl Pair {
    pub fn pending(limit: Duration) -> Self {
        let a = Bus::with_limit(limit);
        let b = Bus::with_limit(limit);
        a.init();
        b.init();
        let ra = repo(&a, "project-a");
        let rb = repo(&b, "project-b");
        a.register_at("agent-a", "claude", None, &ra);
        b.register_at("agent-b", "codex", None, &rb);
        let sa = Server::start(&a, true);
        let sb = Server::start(&b, true);
        for (bus, server) in [(&a, &sa), (&b, &sb)] {
            api(
                bus,
                server,
                "PUT",
                "/api/settings/federation",
                json!({"enabled":true}),
            );
        }
        let invitation = api(&a, &sa, "POST", "/api/fed/invites", json!({}));
        api(
            &b,
            &sb,
            "POST",
            "/api/fed/join",
            json!({"invite":invitation["invite"],"label":"alice"}),
        );
        eventually(&a, "pending inviter", || {
            count(
                &a,
                "SELECT count(*) FROM peers WHERE state='pending_confirm'",
            ) == 1
        });
        eventually(&b, "pending joiner", || {
            count(
                &b,
                "SELECT count(*) FROM peers WHERE state='pending_confirm'",
            ) == 1
        });
        let pa = live_id(&a);
        let pb = live_id(&b);
        Self {
            sa,
            sb,
            a,
            b,
            pa,
            pb,
            ra,
            rb,
        }
    }
    pub fn paired(limit: Duration) -> Self {
        let pair = Self::pending(limit);
        pair.confirm();
        pair
    }
    pub fn confirm(&self) {
        // §4 requires both screens to show the code; §10 omits that field in its example.
        // Freeze pair_code on each pending peer in GET /api/fed (see coverage notes).
        let code = peer(&self.a, &self.sa, &self.pa)["pair_code"]
            .as_str()
            .expect("pending peer must expose pair_code to Settings")
            .to_owned();
        let groups: Vec<_> = code.split(' ').collect();
        assert_eq!(groups.len(), 6);
        assert!(groups
            .iter()
            .all(|s| s.len() == 4 && s.bytes().all(|c| c.is_ascii_digit())));
        assert_eq!(peer(&self.b, &self.sb, &self.pb)["pair_code"], code);
        api(
            &self.a,
            &self.sa,
            "POST",
            &format!("/api/fed/peers/{}/confirm", self.pa),
            json!({"pair_code":code}),
        );
        assert_eq!(
            text(
                &self.a,
                "SELECT state FROM peers WHERE peer_id=?1",
                &self.pa
            ),
            "pending_confirm"
        );
        api(
            &self.b,
            &self.sb,
            "POST",
            &format!("/api/fed/peers/{}/confirm", self.pb),
            json!({"pair_code":code}),
        );
        self.connected();
    }
    pub fn connected(&self) {
        for (bus, server, id) in [(&self.a, &self.sa, &self.pa), (&self.b, &self.sb, &self.pb)] {
            eventually(bus, "mutually confirmed connection", || {
                peer(bus, server, id)["state"] == "connected"
            });
            assert_eq!(
                text(bus, "SELECT state FROM peers WHERE peer_id=?1", id),
                "active"
            );
            assert_eq!(
                peer(bus, server, id)["path"],
                "direct",
                "relays are disabled"
            );
        }
    }
    pub fn share(&self, a_in: bool, a_out: bool, b_in: bool, b_out: bool) -> String {
        api(
            &self.a,
            &self.sa,
            "POST",
            &format!("/api/fed/peers/{}/shares", self.pa),
            json!({"local_repo":self.ra,"label":"project","inbound":a_in,"outbound":a_out}),
        );
        let share = text(
            &self.a,
            "SELECT share_id FROM peer_shares WHERE peer_id=?1 AND state='offered_out'",
            &self.pa,
        );
        eventually(&self.b, "offer delivered", || {
            count(
                &self.b,
                "SELECT count(*) FROM peer_shares WHERE state='offered_in'",
            ) > 0
        });
        assert_eq!(
            count(
                &self.b,
                "SELECT count(*) FROM peer_shares WHERE state='offered_in' AND local_repo IS NULL"
            ),
            1
        );
        api(
            &self.b,
            &self.sb,
            "POST",
            &format!("/api/fed/shares/{share}/accept"),
            json!({"local_repo":self.rb,"inbound":b_in,"outbound":b_out}),
        );
        for bus in [&self.a, &self.b] {
            eventually(bus, "active share", || {
                text(
                    bus,
                    "SELECT state FROM peer_shares WHERE share_id=?1",
                    &share,
                ) == "active"
            });
        }
        share
    }
    pub fn target(&self, from_a: bool, agent: &str) -> String {
        let (sender, receiver, server, peer_id) = if from_a {
            (&self.a, &self.b, &self.sa, &self.pa)
        } else {
            (&self.b, &self.a, &self.sb, &self.pb)
        };
        eventually(sender, "discovery", || {
            count(sender, "SELECT count(*) FROM fed_remote_sessions") > 0
        });
        let session = text(
            receiver,
            "SELECT session FROM fed_sessions WHERE agent_id=?1",
            agent,
        );
        assert_eq!(session.len(), 12);
        assert!(session
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        let label = peer(sender, server, peer_id)["label"]
            .as_str()
            .unwrap()
            .to_owned();
        format!("peer:{label}/{session}")
    }
}
pub fn queued(bus: &Bus, from: &str, to: &str, kind: &str, body: &str) -> String {
    queued_extra(bus, from, to, kind, body, &[], 0)
}
pub fn queued_extra(
    bus: &Bus,
    from: &str,
    to: &str,
    kind: &str,
    body: &str,
    extra: &[&str],
    offset: i64,
) -> String {
    let output = bus
        .cmd()
        .env("AXON_TEST_NOW_OFFSET_MS", offset.to_string())
        .args([
            "send", "--from", from, "--to", to, "--kind", kind, "--body", body,
        ])
        .args(extra)
        .assert()
        .success()
        .get_output()
        .clone();
    let line = String::from_utf8(output.stdout).unwrap();
    let suffix = format!(" to {to} (delivers when connected; expires in 24h)\n");
    let id = line
        .strip_prefix("queued ")
        .and_then(|s| s.strip_suffix(&suffix))
        .expect("§7 exact queued line");
    uuid(id);
    id.to_owned()
}
pub fn uuid(id: &str) {
    assert_eq!(id.len(), 36);
    for (i, c) in id.bytes().enumerate() {
        assert!(if [8, 13, 18, 23].contains(&i) {
            c == b'-'
        } else {
            c.is_ascii_hexdigit()
        });
    }
}
pub fn refused(bus: &Bus, from: &str, to: &str, kind: &str, body: &str, reason: &str) {
    let before = count(bus, "SELECT count(*) FROM fed_outbox");
    let output = bus
        .cmd()
        .args([
            "send", "--from", from, "--to", to, "--kind", kind, "--body", body,
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("refused: {reason}\n")
    );
    assert_eq!(count(bus, "SELECT count(*) FROM fed_outbox"), before);
}
pub fn received(bus: &Bus, message: &str) -> String {
    eventually(bus, "committed inbox", || {
        db(bus)
            .query_row(
                "SELECT count(*) FROM fed_inbox WHERE message_id=?1",
                [message],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    });
    text(
        bus,
        "SELECT local_message_id FROM fed_inbox WHERE message_id=?1",
        message,
    )
}
pub fn hook_context(bus: &Bus, harness: &str, agent: &str, cwd: &Path) -> String {
    let mut payload = bus.fixture(harness, "PostToolUse");
    payload["session_id"] = json!(agent);
    payload["cwd"] = json!(cwd);
    let output = bus.hook(harness, "PostToolUse", &payload);
    if output.stdout.is_empty() {
        return String::new();
    }
    let value = parse_json(&output.stdout);
    value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or("")
        .to_owned()
}

#[cfg(unix)]
pub struct Stopped(u32);
#[cfg(unix)]
impl Stopped {
    pub fn new(server: &Server) -> Self {
        let stopped = Self(server.process.0.id());
        signal(stopped.0, "-STOP");
        stopped
    }
}
#[cfg(unix)]
impl Drop for Stopped {
    fn drop(&mut self) {
        let _ = std::process::Command::new("/bin/kill")
            .env_clear()
            .args(["-CONT", &self.0.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}
#[cfg(unix)]
fn signal(pid: u32, signal: &str) {
    // No shell and no inherited user configuration; the PID belongs to this harness.
    assert!(std::process::Command::new("/bin/kill")
        .env_clear()
        .args([signal, &pid.to_string()])
        .status()
        .unwrap()
        .success());
}
