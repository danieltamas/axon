use super::{parse_json, Bus, Running};
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::time::{Duration, Instant};

// HTTP is deliberately standard-library only; every socket targets the server's loopback address.
pub struct Server {
    pub process: Running,
    pub address: std::net::SocketAddr,
    pub url: String,
    pub cookie: String,
    pub token: String,
}
pub struct Response {
    pub status: u16,
    pub headers: std::collections::BTreeMap<String, String>,
    pub body: Vec<u8>,
}
impl Server {
    pub fn start(bus: &Bus, content: bool) -> Self {
        let mut server = Self::anonymous(bus, content, 0);
        (server.cookie, server.token) = server.login(bus);
        server
    }

    pub fn shifted(bus: &Bus, content: bool, offset_ms: i64) -> Self {
        let mut server = Self::anonymous(bus, content, offset_ms);
        (server.cookie, server.token) = server.login(bus);
        server
    }

    pub fn anonymous(bus: &Bus, content: bool, offset_ms: i64) -> Self {
        #[cfg(not(debug_assertions))]
        compile_error!(
            "federation test seams require debug binaries; release may dial public relays"
        );
        let ready = bus.root.join("ready.json");
        if ready.exists() {
            fs::remove_file(&ready).unwrap();
        }
        let mut args = vec![
            "serve",
            "--port",
            "0",
            "--ready-file",
            ready.to_str().unwrap(),
        ];
        if !content {
            args.push("--no-content");
        }
        let mut command = bus.process();
        command
            .args(&args)
            .env("AXON_TEST_NOW_OFFSET_MS", offset_ms.to_string())
            // A missing file adds zero; tests can advance this server without restarting it.
            .env("AXON_TEST_NOW_OFFSET_FILE", bus.root.join("now-offset-ms"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut process = Running(command.spawn().unwrap());
        // The test's own budget: a freshly linked binary is checked by macOS on its first
        // launch, which alone can take seconds under a full suite.
        let deadline = Instant::now() + bus.remaining();
        let info = loop {
            if let Ok(bytes) = fs::read(&ready) {
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    break value;
                }
            }
            assert!(
                process.0.try_wait().unwrap().is_none(),
                "serve exited before readiness"
            );
            assert!(Instant::now() < deadline, "serve never became ready");
            std::thread::sleep(Duration::from_millis(5));
        };
        let url = info["url"].as_str().unwrap().to_owned();
        let address: std::net::SocketAddr = url.strip_prefix("http://").unwrap().parse().unwrap();
        assert_eq!(
            address.ip(),
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        );
        assert_ne!(address.port(), 0);
        Self {
            process,
            address,
            url,
            cookie: String::new(),
            token: String::new(),
        }
    }

    // P2P-SPEC §1: neither binary has `open` in the pre-feature tree. Freeze the
    // serve alias as `axon-bus open --print`; no source module or root binary is linked.
    pub fn nonce(&self, bus: &Bus) -> String {
        let output = bus.ok(&["open", "--print"]);
        let line = String::from_utf8(output.stdout).unwrap();
        let prefix = format!("Dashboard: {}/#login=", self.url);
        let nonce = line
            .strip_prefix(&prefix)
            .expect("exact Dashboard login line")
            .strip_suffix('\n')
            .expect("newline-terminated Dashboard line");
        assert_eq!(nonce.len(), 43, "32 random bytes in unpadded base64url");
        assert!(nonce
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
        nonce.to_owned()
    }

    pub fn exchange(&self, bus: &Bus, nonce: &str) -> Response {
        self.raw_request(
            bus,
            "POST",
            "/api/session",
            &[("Origin", &self.url), ("Content-Type", "application/json")],
            &serde_json::json!({"nonce":nonce}).to_string(),
        )
    }

    pub fn login(&self, bus: &Bus) -> (String, String) {
        let response = self.exchange(bus, &self.nonce(bus));
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        let token = response.session_token();
        assert_eq!(
            response.headers.get("cache-control").map(String::as_str),
            Some("no-store")
        );
        let header = &response.headers["set-cookie"];
        let fields: Vec<_> = header.split(';').map(str::trim).collect();
        let cookie = fields[0];
        let secret = cookie
            .strip_prefix("axon_session=")
            .expect("session cookie name");
        assert_eq!(secret.len(), 43);
        assert!(secret
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
        for attribute in ["HttpOnly", "SameSite=Strict", "Path=/", "Max-Age=2592000"] {
            assert!(fields.contains(&attribute), "missing {attribute}: {header}");
        }
        (cookie.to_owned(), token)
    }

    pub fn request(
        &self,
        bus: &Bus,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Response {
        let mut authenticated = headers.to_vec();
        if !self.cookie.is_empty()
            && !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("cookie"))
        {
            authenticated.push(("Cookie", &self.cookie));
        }
        if !self.token.is_empty()
            && !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("x-axon-session"))
        {
            authenticated.push(("x-axon-session", &self.token));
        }
        let stream_path;
        let path = if path == "/api/stream" && !self.token.is_empty() {
            stream_path = format!("{path}?t={}", self.token);
            stream_path.as_str()
        } else {
            path
        };
        self.raw_request(bus, method, path, &authenticated, body)
    }

    pub fn connect(&self, timeout: Duration) -> std::net::TcpStream {
        let socket = std::net::TcpStream::connect_timeout(&self.address, timeout).unwrap();
        socket.set_read_timeout(Some(timeout)).unwrap();
        socket.set_write_timeout(Some(timeout)).unwrap();
        socket
    }

    pub fn raw_request(
        &self,
        bus: &Bus,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Response {
        use std::io::{BufRead, BufReader, Read};
        let mut socket = self.connect(bus.remaining());
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        );
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            request.push_str(&format!("Host: {}\r\n", self.address));
        }
        for (key, value) in headers {
            request.push_str(&format!("{key}: {value}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(body);
        socket.write_all(request.as_bytes()).unwrap();
        let mut reader = BufReader::new(socket);
        let (status, headers) = response_headers(&mut reader);
        let mut body = Vec::new();
        if headers
            .get("transfer-encoding")
            .is_some_and(|v| v.eq_ignore_ascii_case("chunked"))
        {
            loop {
                let mut line = String::new();
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
                assert_eq!(&crlf, b"\r\n");
            }
        } else if let Some(length) = headers.get("content-length") {
            body.resize(length.parse().unwrap(), 0);
            reader.read_exact(&mut body).unwrap();
        } else {
            reader.read_to_end(&mut body).unwrap();
        }
        Response {
            status,
            headers,
            body,
        }
    }

    pub fn snapshot(&self, bus: &Bus) -> Value {
        let response = self.request(bus, "GET", "/api/snapshot", &[], "");
        assert_eq!(response.status, 200);
        parse_json(&response.body)
    }
}
pub fn response_headers(
    reader: &mut impl std::io::BufRead,
) -> (u16, std::collections::BTreeMap<String, String>) {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    let mut headers = std::collections::BTreeMap::new();
    loop {
        line.clear();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "EOF in HTTP headers"
        );
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    (status, headers)
}

impl Response {
    pub fn session_token(&self) -> String {
        assert_eq!(
            self.status, 200,
            "C1: login returns 200 with a session token"
        );
        let value = parse_json(&self.body);
        let token = value["token"].as_str().expect("C1: token in login JSON");
        assert_eq!(token.len(), 43, "C1: 32 random bytes in unpadded base64url");
        assert!(token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
        token.to_owned()
    }
}
