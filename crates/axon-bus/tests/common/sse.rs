//! Bounded standard-library SSE decoder, including HTTP chunk framing.
use super::{response_headers, Bus, Server};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

pub struct Events {
    reader: BufReader<TcpStream>,
    chunked: bool,
    decoded: String,
}
impl Events {
    pub fn open(bus: &Bus, server: &Server) -> Self {
        let mut socket = server.connect(bus.remaining().min(Duration::from_secs(7)));
        write!(socket, "GET /api/stream HTTP/1.1\r\nHost: {}\r\nCookie: {}\r\nAccept: text/event-stream\r\n\r\n",
            server.address, server.cookie).unwrap();
        let mut reader = BufReader::new(socket);
        let (status, headers) = response_headers(&mut reader);
        assert_eq!(status, 200);
        assert!(headers["content-type"].starts_with("text/event-stream"));
        assert_eq!(headers["cache-control"], "no-store");
        Self {
            reader,
            chunked: headers
                .get("transfer-encoding")
                .is_some_and(|v| v.eq_ignore_ascii_case("chunked")),
            decoded: String::new(),
        }
    }
    pub fn fed(&mut self, bus: &Bus, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout.min(bus.remaining());
        loop {
            while let Some(end) = self.decoded.find("\n\n") {
                let event = self.decoded[..end].to_owned();
                self.decoded.drain(..end + 2);
                if event.lines().any(|l| l == "event: fed") {
                    let body = event
                        .lines()
                        .filter_map(|l| l.strip_prefix("data:").map(str::trim_start))
                        .collect::<Vec<_>>()
                        .join("\n");
                    return serde_json::from_str(&body).unwrap();
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "no fed SSE before deadline");
            self.reader
                .get_ref()
                .set_read_timeout(Some(remaining))
                .unwrap();
            if self.chunked {
                let mut line = String::new();
                self.reader.read_line(&mut line).unwrap();
                let n = usize::from_str_radix(line.trim().split(';').next().unwrap(), 16).unwrap();
                assert!(n > 0 && n <= 1024 * 1024, "bounded nonempty event chunk");
                let mut bytes = vec![0; n];
                self.reader.read_exact(&mut bytes).unwrap();
                let mut crlf = [0; 2];
                self.reader.read_exact(&mut crlf).unwrap();
                assert_eq!(crlf, *b"\r\n");
                self.decoded.push_str(std::str::from_utf8(&bytes).unwrap());
            } else {
                let mut line = String::new();
                assert!(self.reader.read_line(&mut line).unwrap() > 0);
                self.decoded.push_str(&line);
            }
            self.decoded = self.decoded.replace("\r\n", "\n");
        }
    }
}
