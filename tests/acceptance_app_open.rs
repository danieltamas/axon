//! P2P-SPEC §1, Login link: an authenticated installed app may take over browser opening.
//! Real root binary and HTTP; only the external browser opener is replaced.
//! The pinned webbrowser crate uses Launch Services on macOS, bypassing PATH. Browser
//! cases run on Linux; enabling them on macOS requires a PATH-observable opener first.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Sandbox {
    root: PathBuf,
    deadline: Instant,
}
struct Server {
    child: Child,
    address: SocketAddr,
    stdout: PathBuf,
    stderr: PathBuf,
    login_url: String,
}
struct Owner {
    cookie: String,
    token: String,
}
struct Response {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
impl Server {
    fn assert_running(&mut self) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "server exited:\n{}\n{}",
            fs::read_to_string(&self.stdout).unwrap(),
            fs::read_to_string(&self.stderr).unwrap()
        );
    }
    fn origin(&self) -> String {
        format!("http://{}", self.address)
    }
}
impl Sandbox {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "axon-app-open-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        for dir in [
            "home", "data", "config", "cache", "state", "runtime", "tmp", "bin",
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::set_permissions(root.join("runtime"), fs::Permissions::from_mode(0o700)).unwrap();
        for opener in ["open", "xdg-open"] {
            let path = root.join("bin").join(opener);
            fs::write(
                &path,
                "#!/bin/sh\nprintf '%s\\0' \"$@\" >> \"$AXON_OPENER_LOG\"\nprintf '\\n' >> \"$AXON_OPENER_LOG\"\n",
            ).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
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
            "app-open acceptance exceeded its 45 s budget"
        );
        left
    }
    fn pause(&self) {
        std::thread::sleep(self.remaining().min(Duration::from_millis(10)));
    }
    fn start(&self, no_open: bool) -> Server {
        let reservation = TcpListener::bind("127.0.0.1:0")
            .expect("acceptance requires permission to bind loopback TCP ports");
        let address = reservation.local_addr().unwrap();
        let stdout = self.root.join(format!("server-{}.stdout", address.port()));
        let stderr = self.root.join(format!("server-{}.stderr", address.port()));
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_axon"));
        cmd.env_clear()
            .current_dir(&self.root)
            .env("HOME", self.root.join("home"))
            .env("USERPROFILE", self.root.join("home"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_RUNTIME_DIR", self.root.join("runtime"))
            .env("APPDATA", self.root.join("config"))
            .env("LOCALAPPDATA", self.root.join("data"))
            .env("CODEX_HOME", self.root.join("home/.codex"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("home/.claude"))
            .env("HERMES_HOME", self.root.join("home/.hermes"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("TMP", self.root.join("tmp"))
            .env("TEMP", self.root.join("tmp"))
            // No inherited browser, RTK, harness, proxy or credential configuration.
            .env("PATH", self.root.join("bin"))
            .env("AXON_OPENER_LOG", self.root.join("opener.argv"))
            .env("AXON_FED_RELAY", "disabled")
            .env("AXON_FED_BIND", "127.0.0.1:0")
            .env("TZ", "UTC")
            .env("LANG", "C")
            .args(["--port", &address.port().to_string(), "--no-hooks"])
            .stdin(Stdio::null())
            .stdout(fs::File::create(&stdout).unwrap())
            .stderr(fs::File::create(&stderr).unwrap());
        if no_open {
            cmd.arg("--no-open");
        }
        drop(reservation);
        let mut server = Server {
            child: cmd.spawn().unwrap(),
            address,
            stdout,
            stderr,
            login_url: String::new(),
        };
        loop {
            server.assert_running();
            if TcpStream::connect_timeout(&address, self.remaining().min(Duration::from_millis(50)))
                .is_ok()
            {
                break;
            }
            self.pause();
        }
        // A bound socket alone is too early: wait for the server to serve a real response.
        assert_eq!(self.http(&server, "GET", "/", &[], "").status, 200);
        let output = fs::read_to_string(&server.stdout).unwrap();
        server.login_url = output
            .lines()
            .find_map(|line| line.strip_prefix("Dashboard: "))
            .expect("every start must print its Dashboard login line")
            .to_owned();
        let prefix = format!("{}/#login=", server.origin());
        let nonce = server
            .login_url
            .strip_prefix(&prefix)
            .expect("loopback login URL");
        assert_eq!(nonce.len(), 43, "32 bytes encoded as unpadded base64url");
        assert!(nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        server
    }
    fn http(
        &self,
        server: &Server,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Response {
        let timeout = self.remaining().min(Duration::from_secs(5));
        let mut socket = TcpStream::connect_timeout(&server.address, timeout).unwrap();
        socket.set_read_timeout(Some(timeout)).unwrap();
        socket.set_write_timeout(Some(timeout)).unwrap();
        write!(
            socket,
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n",
            server.address,
            body.len()
        )
        .unwrap();
        for (name, value) in headers {
            write!(socket, "{name}: {value}\r\n").unwrap();
        }
        write!(socket, "\r\n{body}").unwrap();
        let mut reader = BufReader::new(socket);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let status = line
            .split_whitespace()
            .nth(1)
            .expect("HTTP status")
            .parse()
            .unwrap();
        let mut headers = BTreeMap::new();
        loop {
            line.clear();
            assert!(reader.read_line(&mut line).unwrap() > 0, "EOF in headers");
            if line == "\r\n" {
                break;
            }
            let (name, value) = line.split_once(':').unwrap();
            headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
        }
        let mut body = Vec::new();
        if headers
            .get("transfer-encoding")
            .is_some_and(|v| v.eq_ignore_ascii_case("chunked"))
        {
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                let count =
                    usize::from_str_radix(line.trim().split(';').next().unwrap(), 16).unwrap();
                if count == 0 {
                    break;
                }
                let start = body.len();
                body.resize(start + count, 0);
                reader.read_exact(&mut body[start..]).unwrap();
                let mut crlf = [0; 2];
                reader.read_exact(&mut crlf).unwrap();
                assert_eq!(crlf, *b"\r\n");
            }
        } else {
            reader.read_to_end(&mut body).unwrap();
        }
        Response {
            status,
            headers,
            body,
        }
    }
    fn login(&self, server: &Server) -> Owner {
        let nonce = server.login_url.split_once("#login=").unwrap().1;
        let response = self.http(
            server,
            "POST",
            "/api/session",
            &[
                ("Origin", &server.origin()),
                ("Content-Type", "application/json"),
            ],
            &json!({"nonce": nonce}).to_string(),
        );
        assert_eq!(response.status, 200, "nonce exchange: {:?}", response.body);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        Owner {
            cookie: response.headers["set-cookie"]
                .split(';')
                .next()
                .unwrap()
                .to_owned(),
            token: body["token"]
                .as_str()
                .expect("owner session token")
                .to_owned(),
        }
    }
    fn report_app(&self, server: &Server, owner: &Owner) {
        let response = self.http(
            server,
            "POST",
            "/api/app",
            &[
                ("Origin", &server.origin()),
                ("Cookie", &owner.cookie),
                ("x-axon-session", &owner.token),
            ],
            "",
        );
        assert!(
            (200..300).contains(&response.status),
            "authenticated app report: HTTP {} {:?}",
            response.status,
            response.body
        );
    }
    fn assert_owner_status(&self, server: &Server, owner: &Owner, expected: u16) {
        let response = self.http(
            server,
            "GET",
            "/api/settings",
            &[("Cookie", &owner.cookie), ("x-axon-session", &owner.token)],
            "",
        );
        assert_eq!(
            response.status, expected,
            "persisted owner session: {:?}",
            response.body
        );
    }
    fn assert_opened(&self, server: &mut Server) {
        let expected = format!("{}\0\n", server.login_url);
        loop {
            server.assert_running();
            match fs::read(self.root.join("opener.argv")) {
                Ok(bytes) if bytes.ends_with(b"\n") => {
                    assert_eq!(
                        bytes,
                        expected.as_bytes(),
                        "opener argv must be exactly the printed login URL"
                    );
                    return;
                }
                Ok(_) => {}
                Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::NotFound),
            }
            self.pause();
        }
    }
    fn clear_opener(&self) {
        fs::remove_file(self.root.join("opener.argv")).unwrap();
    }
    fn assert_not_opened(&self, server: &mut Server) {
        // HTTP readiness crosses startup; also allow an asynchronously spawned opener to run.
        let until = Instant::now() + Duration::from_millis(500);
        loop {
            server.assert_running();
            assert!(
                !self.root.join("opener.argv").exists(),
                "installed app with a live session must suppress browser opening"
            );
            if Instant::now() >= until {
                break;
            }
            self.pause();
        }
    }
}

#[test]
fn app_report_requires_both_owner_cookie_and_token() {
    let sandbox = Sandbox::new();
    let server = sandbox.start(true);
    let owner = sandbox.login(&server);
    let origin = server.origin();
    for credentials in [
        vec![],
        vec![("Cookie", owner.cookie.as_str())],
        vec![("x-axon-session", owner.token.as_str())],
        vec![
            ("Cookie", owner.cookie.as_str()),
            ("x-axon-session", "invalid"),
        ],
        vec![
            ("Cookie", "axon_session=invalid"),
            ("x-axon-session", owner.token.as_str()),
        ],
    ] {
        let mut headers = vec![("Origin", origin.as_str())];
        headers.extend(credentials);
        assert_eq!(
            sandbox
                .http(&server, "POST", "/api/app", &headers, "")
                .status,
            401,
            "a matching Origin cannot replace either owner credential"
        );
    }
    sandbox.report_app(&server, &owner);
}

#[test]
fn app_report_requires_matching_origin_even_with_owner_session() {
    let sandbox = Sandbox::new();
    let server = sandbox.start(true);
    let owner = sandbox.login(&server);
    for origin in [
        None,
        Some("https://attacker.invalid"),
        Some("http://127.0.0.1:0"),
    ] {
        let mut headers = vec![
            ("Cookie", owner.cookie.as_str()),
            ("x-axon-session", owner.token.as_str()),
        ];
        if let Some(origin) = origin {
            headers.push(("Origin", origin));
        }
        let response = sandbox.http(&server, "POST", "/api/app", &headers, "");
        assert!(
            (400..500).contains(&response.status),
            "missing/mismatched Origin must be rejected, got HTTP {}",
            response.status
        );
    }
    sandbox.report_app(&server, &owner);
}

#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "macOS webbrowser uses Launch Services, so a PATH open stub cannot observe or contain it"
)]
fn fresh_home_opens_browser_with_printed_login_url() {
    let sandbox = Sandbox::new();
    let mut server = sandbox.start(false);
    sandbox.assert_opened(&mut server);
    sandbox.login(&server); // The printed nonce must actually be usable over HTTP.
}

#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "macOS webbrowser uses Launch Services, so a PATH open stub cannot observe or contain it"
)]
fn reported_app_and_live_session_suppress_browser_after_restart() {
    let sandbox = Sandbox::new();
    let mut first = sandbox.start(false);
    sandbox.assert_opened(&mut first); // Positive control: this very stub can observe opening.
    let owner = sandbox.login(&first);
    sandbox.report_app(&first, &owner);
    let first_nonce = first.login_url.split_once("#login=").unwrap().1.to_owned();
    drop(first);
    sandbox.clear_opener();

    let mut second = sandbox.start(false);
    assert_ne!(
        second.login_url.split_once("#login=").unwrap().1,
        first_nonce,
        "restart issues a fresh nonce"
    );
    sandbox.assert_owner_status(&second, &owner, 200);
    sandbox.assert_not_opened(&mut second);
    sandbox.login(&second); // Dashboard line still gives a usable login while the app takes over.
}

fn reported_app_without_live_sessions_opens_browser(revoke: bool) {
    let sandbox = Sandbox::new();
    let mut first = sandbox.start(false);
    sandbox.assert_opened(&mut first);
    let owner = sandbox.login(&first);
    sandbox.report_app(&first, &owner);
    drop(first);
    sandbox.clear_opener();

    // No revoke-all HTTP endpoint exists. Alter only this stopped server's real database;
    // app presence was established by HTTP and is deliberately left intact.
    let db = rusqlite::Connection::open(sandbox.root.join("data/axon/axon.db")).unwrap();
    let changed = if revoke {
        db.execute("DELETE FROM dashboard_sessions", []).unwrap()
    } else {
        db.execute("UPDATE dashboard_sessions SET expires_at = ?1", [1_i64])
            .unwrap()
    };
    assert_eq!(
        changed, 1,
        "invalidate the sole session established through HTTP"
    );
    drop(db);

    let mut second = sandbox.start(false);
    sandbox.assert_opened(&mut second);
    sandbox.assert_owner_status(&second, &owner, 401);
    sandbox.login(&second);
}

#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "macOS webbrowser uses Launch Services, so a PATH open stub cannot observe or contain it"
)]
fn reported_app_with_all_sessions_revoked_opens_browser_after_restart() {
    reported_app_without_live_sessions_opens_browser(true);
}

#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "macOS webbrowser uses Launch Services, so a PATH open stub cannot observe or contain it"
)]
fn reported_app_with_all_sessions_expired_opens_browser_after_restart() {
    reported_app_without_live_sessions_opens_browser(false);
}
