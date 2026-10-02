//! The owner guard over real HTTP: the cookie alone is refused on every route, the stream
//! ends when its session is revoked, and the token is read from the query on the stream only.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::{serve, session, store};

struct Served {
    port: u16,
    db: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

async fn served() -> Served {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("hub.db");
    store::init(&db).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (router, _federation) = serve::router(&db, port, true).unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await });
    Served {
        port,
        db,
        _dir: dir,
    }
}

impl Served {
    fn login(&self) -> session::Granted {
        let conn = store::open(&self.db).unwrap();
        let nonce = session::issue_nonce(&conn).unwrap();
        session::exchange(&conn, &nonce).unwrap().unwrap()
    }

    async fn request(&self, method: &str, path: &str, extra: &[String]) -> TcpStream {
        let mut socket = TcpStream::connect(("127.0.0.1", self.port)).await.unwrap();
        let origin = format!("http://127.0.0.1:{}", self.port);
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n",
            self.port
        );
        head.push_str(&format!("Origin: {origin}\r\nContent-Length: 0\r\n"));
        for line in extra {
            head.push_str(&format!("{line}\r\n"));
        }
        head.push_str("\r\n");
        socket.write_all(head.as_bytes()).await.unwrap();
        socket
    }

    async fn status(&self, method: &str, path: &str, extra: &[String]) -> u16 {
        let mut socket = self.request(method, path, extra).await;
        let mut buffer = [0u8; 64];
        let read = socket.read(&mut buffer).await.unwrap();
        String::from_utf8_lossy(&buffer[..read])
            .split(' ')
            .nth(1)
            .unwrap()
            .parse()
            .unwrap()
    }
}

fn cookie(granted: &session::Granted) -> String {
    format!("Cookie: {}={}", session::COOKIE, granted.secret)
}

fn token(granted: &session::Granted) -> String {
    format!("{}: {}", session::TOKEN_HEADER, granted.token)
}

#[tokio::test]
async fn the_cookie_alone_is_refused_on_every_api_route() {
    let server = served().await;
    let granted = server.login();
    for path in [
        "/api/health",
        "/api/snapshot",
        "/api/fed",
        "/api/settings",
        "/api/stream",
    ] {
        assert_eq!(
            server.status("GET", path, &[cookie(&granted)]).await,
            401,
            "{path}"
        );
        assert_eq!(
            server
                .status("GET", path, &[cookie(&granted), token(&server.login())])
                .await,
            401,
            "{path}: another session's token"
        );
    }
    let both = [cookie(&granted), token(&granted)];
    assert_eq!(server.status("GET", "/api/health", &both).await, 200);
    assert_eq!(server.status("GET", "/api/fed", &both).await, 200);
}

#[tokio::test]
async fn the_token_rides_in_the_query_only_on_the_stream() {
    let server = served().await;
    let granted = server.login();
    let cookie = [cookie(&granted)];
    let path = format!("/api/health?t={}", granted.token);
    assert_eq!(server.status("GET", &path, &cookie).await, 401);
    let stream = format!("/api/stream?t={}", granted.token);
    assert_eq!(server.status("GET", &stream, &cookie).await, 200);
}

#[tokio::test]
async fn signing_a_session_out_ends_its_open_stream() {
    let server = served().await;
    let granted = server.login();
    let path = format!("/api/stream?t={}", granted.token);
    let mut socket = server
        .request(
            "GET",
            &path,
            &[cookie(&granted), "Connection: close".to_owned()],
        )
        .await;
    let mut seen = String::new();
    let mut buffer = [0u8; 4096];
    while !seen.contains("event: snapshot") {
        let read = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buffer))
            .await
            .expect("the stream opens")
            .unwrap();
        assert!(read > 0, "stream closed before its first event");
        seen.push_str(&String::from_utf8_lossy(&buffer[..read]));
    }
    let other = server.login();
    session::revoke_others(&store::open(&server.db).unwrap(), &other.secret).unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match socket.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "the stream outlived its session");
}
