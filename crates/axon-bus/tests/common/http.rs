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
    pub token: String,
}
pub struct Response {
    pub status: u16,
    pub headers: std::collections::BTreeMap<String, String>,
    pub body: Vec<u8>,
}
impl Server {
    pub fn start(bus: &Bus, content: bool) -> Self {
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
        if content {
            args.push("--content");
        } else {
            args.push("--no-content");
        }
        let mut process = bus.spawn(&args);
        let deadline = Instant::now() + bus.remaining().min(Duration::from_millis(1500));
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
        let token = info["token"].as_str().unwrap().to_owned();
        assert!(!token.is_empty(), "POST token must not be empty");
        Self {
            process,
            address,
            url,
            token,
        }
    }

    pub fn connect(&self, timeout: Duration) -> std::net::TcpStream {
        let socket = std::net::TcpStream::connect_timeout(&self.address, timeout).unwrap();
        socket.set_read_timeout(Some(timeout)).unwrap();
        socket.set_write_timeout(Some(timeout)).unwrap();
        socket
    }

    pub fn request(
        &self,
        bus: &Bus,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Response {
        use std::io::{BufRead, BufReader, Read};
        let mut socket = self.connect(bus.remaining().min(Duration::from_millis(500)));
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
