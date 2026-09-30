//! `serve` (BUS-PLAN §7): the dashboard and its API on 127.0.0.1. A reader of the same
//! database plus a message sender; killing it loses nothing.
//!
//! Security: the Host header must name this loopback server (DNS rebinding), every POST
//! needs the per-boot token and this exact Origin, and the CSP allows self only. The page
//! hands its token to any local account, so `serve` is for single-user hosts (§7).

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::watch;

use crate::memory::Sampler;
use crate::observed::Observer;
use crate::{msg, snapshot, store, transcript};

const INDEX_HTML: &str = include_str!("../ui/index.html");
/// The page's static files, embedded: path, content type, body.
const ASSETS: [(&str, &str, &str); 14] = [
    ("/activity.js", "text/javascript", include_str!("../ui/activity.js")),
    ("/app.js", "text/javascript", include_str!("../ui/app.js")),
    ("/arcs.js", "text/javascript", include_str!("../ui/arcs.js")),
    ("/board.js", "text/javascript", include_str!("../ui/board.js")),
    ("/brain.js", "text/javascript", include_str!("../ui/brain.js")),
    ("/brain-math.js", "text/javascript", include_str!("../ui/brain-math.js")),
    ("/brain-render.js", "text/javascript", include_str!("../ui/brain-render.js")),
    ("/brain-worker.js", "text/javascript", include_str!("../ui/brain-worker.js")),
    ("/context.js", "text/javascript", include_str!("../ui/context.js")),
    ("/dom.js", "text/javascript", include_str!("../ui/dom.js")),
    ("/overview.js", "text/javascript", include_str!("../ui/overview.js")),
    ("/send.js", "text/javascript", include_str!("../ui/send.js")),
    ("/style.css", "text/css", include_str!("../ui/style.css")),
    ("/usage.js", "text/javascript", include_str!("../ui/usage.js")),
];

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// How often the database is checked for changes to stream (§9 M5: a register reaches the
/// page in under 1 s).
const POLL: Duration = Duration::from_millis(100);

/// How often the poller renews the capture lease and runs retention; well inside the
/// lease, so capture stays on while `serve --content` runs.
const RENEW_EVERY: Duration = Duration::from_secs(10);
/// How often the agents' process memory is re-read.
const SAMPLE_EVERY: Duration = Duration::from_secs(5);

struct App {
    db: PathBuf,
    port: u16,
    origin: String,
    token: String,
    snapshots: watch::Receiver<Arc<String>>,
}

pub fn run(db: &Path, port: u16, ready_file: Option<&Path>, content: bool) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
        let address: SocketAddr = listener.local_addr()?;
        let (router, token) = build(db, address.port(), content)?;
        let origin = format!("http://{address}");
        match ready_file {
            Some(path) => write_ready(path, &origin, &token)?,
            None => eprintln!("axon-bus: dashboard at {origin}"),
        }
        axum::serve(listener, router).await?;
        Ok(())
    })
}

/// The dashboard and its API, for a server another binary bound on 127.0.0.1:`port`.
pub fn router(db: &Path, port: u16, content: bool) -> anyhow::Result<Router> {
    Ok(build(db, port, content)?.0)
}

fn build(db: &Path, port: u16, content: bool) -> anyhow::Result<(Router, String)> {
    let conn = store::open(db).context("no hub; run `axon bus init`")?;
    transcript::set_capture(&conn, content)?;
    drop(conn);
    let token = boot_token()?;
    let app = Arc::new(App {
        db: db.to_owned(),
        port,
        origin: format!("http://127.0.0.1:{port}"),
        token: token.clone(),
        snapshots: watch_database(db.to_owned(), content)?,
    });
    let mut router = Router::new().route("/", get(index));
    for (path, content_type, body) in ASSETS {
        router = router.route(path, get(move || async move { asset(content_type, body) }));
    }
    let router = router
        .route("/api/snapshot", get(snapshot_json))
        .route("/api/stream", get(stream))
        .route("/api/msg", post(send))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app);
    Ok((router, token))
}

/// 256 bits from the OS, new on every boot, so a token never outlives its server.
fn boot_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow::anyhow!("no OS randomness: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// `{url, token}`, written whole (temp file + rename). The temp file is created fresh with
/// owner-only permissions, so a file or symlink planted at its name is refused.
fn write_ready(path: &Path, url: &str, token: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .with_context(|| format!("create {}", temp.display()))?;
    file.write_all(json!({"url": url, "token": token}).to_string().as_bytes())?;
    drop(file);
    std::fs::rename(&temp, path)?;
    Ok(())
}

/// One thread watches `PRAGMA data_version` and publishes a new snapshot when another
/// connection changed the database. It also renews the capture lease and runs retention.
fn watch_database(db: PathBuf, content: bool) -> anyhow::Result<watch::Receiver<Arc<String>>> {
    let conn = store::open(&db)?;
    let mut cache = HashMap::new();
    let mut memory = Sampler::new();
    let mut observer = Observer::default();
    memory.sample(&conn)?;
    let first = snapshot::build(&conn, &mut cache, &memory, &mut observer)?.to_string();
    let (sender, receiver) = watch::channel(Arc::new(first));
    std::thread::spawn(move || {
        let mut seen: Option<i64> = None;
        let mut renewed_at: Option<Instant> = None;
        let mut sampled_at = Instant::now();
        loop {
            if renewed_at.map_or(true, |at| at.elapsed() >= RENEW_EVERY) {
                let upkeep = transcript::set_capture(&conn, content)
                    .and_then(|()| transcript::expire_if_due(&conn));
                if let Err(err) = upkeep {
                    eprintln!("axon-bus: capture lease or retention failed: {err}");
                }
                renewed_at = Some(Instant::now());
            }
            std::thread::sleep(POLL);
            let version = conn.query_row("PRAGMA data_version", [], |r| r.get(0)).ok();
            // Memory, open sessions and observed activity move without touching the
            // database, so they rebuild on their own slower clock.
            let mut sampled = false;
            if sampled_at.elapsed() >= SAMPLE_EVERY {
                sampled_at = Instant::now();
                sampled = true;
                if let Err(err) = memory.sample(&conn) {
                    eprintln!("axon-bus: memory sample failed: {err:#}");
                }
            }
            if version == seen && !sampled {
                continue;
            }
            seen = version;
            match snapshot::build(&conn, &mut cache, &memory, &mut observer) {
                Ok(tree) => {
                    let tree = tree.to_string();
                    sender.send_if_modified(|current| {
                        let changed = **current != tree;
                        if changed {
                            *current = Arc::new(tree);
                        }
                        changed
                    });
                }
                Err(err) => eprintln!("axon-bus: snapshot failed: {err:#}"),
            }
        }
    });
    Ok(receiver)
}

fn loopback_host(headers: &HeaderMap, port: u16) -> bool {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    host.is_some_and(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"))
}

/// Compared in constant time, so response timing does not reveal the token.
fn same_secret(given: &[u8], expected: &[u8]) -> bool {
    given.len() == expected.len()
        && given
            .iter()
            .zip(expected)
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

async fn guard(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let mut allowed = loopback_host(headers, app.port);
    if request.method() != Method::GET {
        let origin = headers.get(header::ORIGIN).map(HeaderValue::as_bytes);
        let token = headers.get("x-axon-token").map(HeaderValue::as_bytes);
        let localhost = format!("http://localhost:{}", app.port);
        allowed &= (origin == Some(app.origin.as_bytes()) || origin == Some(localhost.as_bytes()))
            && token.is_some_and(|t| same_secret(t, app.token.as_bytes()));
    }
    let mut response = if allowed {
        next.run(request).await
    } else {
        StatusCode::FORBIDDEN.into_response()
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

/// Revalidated on every load, so the page never runs modules from an older binary.
fn asset(content_type: &'static str, body: &'static str) -> Response {
    let headers = [(header::CONTENT_TYPE, content_type), (header::CACHE_CONTROL, "no-cache")];
    (headers, body).into_response()
}

/// The page carries the token in a meta tag; the Host check keeps other sites from reading it.
async fn index(State(app): State<Arc<App>>) -> Response {
    let page = INDEX_HTML.replace("{{token}}", &app.token);
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], page).into_response()
}

async fn snapshot_json(State(app): State<Arc<App>>) -> Response {
    let db = app.db.clone();
    let built = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let conn = store::open(&db)?;
        let mut memory = Sampler::new();
        memory.sample(&conn)?;
        Ok(snapshot::build(&conn, &mut HashMap::new(), &memory, &mut Observer::default())?.to_string())
    })
    .await;
    match built {
        Ok(Ok(tree)) => ([(header::CONTENT_TYPE, "application/json")], tree).into_response(),
        Ok(Err(err)) => failure(StatusCode::SERVICE_UNAVAILABLE, format!("{err:#}")),
        Err(err) => failure(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

async fn stream(
    State(app): State<Arc<App>>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let updates = futures_util::stream::unfold(
        (app.snapshots.clone(), true),
        |(mut rx, first)| async move {
            if !first {
                rx.changed().await.ok()?;
            }
            let tree = rx.borrow_and_update().clone();
            Some((
                Ok(Event::default().event("snapshot").data(tree.as_str())),
                (rx, false),
            ))
        },
    );
    Sse::new(updates).keep_alive(KeepAlive::default())
}

/// A message from the human node, sent as `from_id` along the same edges as any agent.
/// `thread` continues an open thread; `reply_to` answers that question as `from_id`, its
/// addressee, and `kind` is then ignored.
#[derive(Deserialize)]
struct Outgoing {
    from_id: String,
    to_id: String,
    kind: String,
    body: String,
    #[serde(default)]
    thread: Option<String>,
    #[serde(default)]
    reply_to: Option<String>,
}

async fn send(State(app): State<Arc<App>>, Json(request): Json<Outgoing>) -> Response {
    let db = app.db.clone();
    let sent = tokio::task::spawn_blocking(
        move || -> anyhow::Result<Result<(String, String), msg::Refused>> {
            let mut conn = store::open(&db)?;
            let tx = store::write_tx(&mut conn)?;
            let sent = match &request.reply_to {
                Some(question) => msg::reply(&tx, question, &request.from_id, &request.body)?,
                None => msg::send(
                    &tx,
                    &msg::Outgoing {
                        from: &request.from_id,
                        to: &request.to_id,
                        kind: &request.kind,
                        body: &request.body,
                        thread: request.thread.as_deref(),
                        refs: &[],
                        wait: None,
                    },
                )?,
            };
            if sent.is_ok() {
                tx.commit()?;
            }
            Ok(sent)
        },
    )
    .await;
    match sent {
        Ok(Ok(Ok((id, thread)))) => (
            StatusCode::CREATED,
            Json(json!({"id": id, "thread": thread})),
        )
            .into_response(),
        Ok(Ok(Err(refused))) => {
            let (code, why) = msg::refused_error(refused);
            let status = if code == 3 {
                StatusCode::CONFLICT
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            };
            failure(status, why)
        }
        Ok(Err(err)) => failure(StatusCode::SERVICE_UNAVAILABLE, format!("{err:#}")),
        Err(err) => failure(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

fn failure(status: StatusCode, error: String) -> Response {
    (status, Json(json!({"error": error}))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Privacy gate (DESIGN.md §16): the page loads nothing remote. Links are fine; only
    /// asset-loading forms are refused.
    #[test]
    fn page_loads_no_remote_assets() {
        let files = std::iter::once(INDEX_HTML).chain(ASSETS.iter().map(|(_, _, body)| *body));
        for body in files {
            for needle in ["src=\"http", "@import", "url(http", "href=\"http", "import(\"http", "from \"http"] {
                assert!(!body.contains(needle), "the dashboard must not load {needle:?}");
            }
        }
    }
}
