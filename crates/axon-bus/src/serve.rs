//! `serve` (BUS-PLAN §7): the dashboard and its API on 127.0.0.1. A reader of the same
//! database plus a message sender; killing it loses nothing.
//!
//! Security: the Host header must name this loopback server (DNS rebinding), every POST
//! needs this exact Origin, every `/api/*` route needs the owner's cookie session AND its
//! token (`session`, P2P-SPEC §1 and §12 C1), and the CSP allows self only. The page itself
//! is public and holds no secret. The cookie alone is not enough: browsers send it to every
//! port of 127.0.0.1, but the token lives in this origin's `localStorage` only.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::watch;

pub use crate::guard::require_owner;
use crate::guard::{self, Credentials};
use crate::memory::Sampler;
use crate::observed::Observer;
use crate::settings::{self, Federation};
use crate::{assets, end, fed, msg, session, snapshot, store, transcript};

const INDEX_HTML: &str = include_str!("../ui/index.html");
/// The page's static files, embedded: path, content type, body.
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
    snapshots: watch::Receiver<Arc<String>>,
    federation: Federation,
    /// This server's capture option; another server's lease never widens it.
    content: bool,
}

pub fn run(db: &Path, port: u16, ready_file: Option<&Path>, content: bool) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
        let address: SocketAddr = listener.local_addr()?;
        let (router, federation) = build(db, address.port(), content)?;
        let origin = format!("http://{address}");
        match ready_file {
            Some(path) => write_ready(path, &origin)?,
            None => eprintln!("axon-bus: dashboard at {origin}"),
        }
        federation.restart().await;
        let served = axum::serve(listener, router).await;
        federation.shutdown().await;
        Ok(served?)
    })
}

/// The dashboard and its API, for a server another binary bound on 127.0.0.1:`port`, and
/// the federation service the caller starts (`restart`) once it serves and stops at the end.
pub fn router(db: &Path, port: u16, content: bool) -> anyhow::Result<(Router, Federation)> {
    build(db, port, content)
}

fn build(db: &Path, port: u16, content: bool) -> anyhow::Result<(Router, Federation)> {
    let conn = store::open(db).context("no hub; run `axon bus init`")?;
    transcript::apply_capture(&conn, content)?;
    session::record_port(&conn, port)?;
    drop(conn);
    let federation = Federation::new(db);
    let app = Arc::new(App {
        db: db.to_owned(),
        port,
        origin: format!("http://127.0.0.1:{port}"),
        snapshots: watch_database(db.to_owned(), content)?,
        federation: federation.clone(),
        content,
    });
    let router = assets::routes(Router::new().route("/", get(index)));
    let router = router
        .route("/api/session", post(create_session))
        .route("/api/snapshot", get(snapshot_json))
        .route("/api/health", get(health))
        .route("/api/stream", get(stream))
        .route("/api/msg", post(send))
        .route("/api/end", post(end_sessions))
        .with_state(app.clone())
        .merge(settings::routes(db, content, federation.clone()))
        .merge(fed::api::routes(db, federation.clone()))
        .merge(crate::handled::api::routes(db))
        .layer(middleware::from_fn_with_state(app, guard));
    Ok((router, federation))
}

/// `{url}`, written whole (temp file + rename). The temp file is created fresh with
/// owner-only permissions, so a file or symlink planted at its name is refused.
fn write_ready(path: &Path, url: &str) -> anyhow::Result<()> {
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
    file.write_all(json!({"url": url}).to_string().as_bytes())?;
    drop(file);
    std::fs::rename(&temp, path)?;
    Ok(())
}

/// One thread watches `PRAGMA data_version` and publishes a new snapshot when another
/// connection changed the database. It also renews the capture lease and runs retention,
/// the handled ledger's included.
fn watch_database(db: PathBuf, content: bool) -> anyhow::Result<watch::Receiver<Arc<String>>> {
    let mut conn = store::open(&db)?;
    let mut cache = HashMap::new();
    let mut memory = Sampler::new();
    let mut observer = Observer::default();
    memory.sample(&conn)?;
    if let Err(err) = reap(&mut conn, &memory) {
        eprintln!("axon-bus: closing exited sessions failed: {err:#}");
    }
    let first = snapshot::build(&conn, &mut cache, &memory, &mut observer, content)?.to_string();
    let (sender, receiver) = watch::channel(Arc::new(first));
    std::thread::spawn(move || {
        let mut seen: Option<i64> = None;
        let mut renewed_at: Option<Instant> = None;
        let mut sampled_at = Instant::now();
        loop {
            if renewed_at.is_none_or(|at| at.elapsed() >= RENEW_EVERY) {
                let upkeep = transcript::apply_capture(&conn, content)
                    .and_then(|()| transcript::expire_if_due(&conn))
                    .and_then(|()| {
                        crate::handled::ensure(&conn)?;
                        crate::handled::expire(&conn, store::now_ms())
                    });
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
                } else if let Err(err) = reap(&mut conn, &memory) {
                    eprintln!("axon-bus: closing exited sessions failed: {err:#}");
                }
            }
            if version == seen && !sampled {
                continue;
            }
            seen = version;
            match snapshot::build(&conn, &mut cache, &memory, &mut observer, content) {
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

/// Sessions whose harness exited without a SessionEnd stay open until a sample finds the
/// process gone.
fn reap(conn: &mut rusqlite::Connection, memory: &Sampler) -> anyhow::Result<()> {
    // Deferred: a sample with nothing to close never takes the write lock hooks wait on.
    // A hook that writes in between makes the commit fail, and the next sample retries.
    let tx = conn.transaction()?;
    crate::registry::reap(&tx, |pid, last_seen| memory.running(pid, last_seen))?;
    tx.commit()?;
    Ok(())
}

fn loopback_host(headers: &HeaderMap, port: u16) -> bool {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    host.is_some_and(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"))
}

async fn guard(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let mut allowed = loopback_host(headers, app.port);
    if request.method() != Method::GET {
        let origin = headers.get(header::ORIGIN).map(HeaderValue::as_bytes);
        let localhost = format!("http://localhost:{}", app.port);
        allowed &= origin == Some(app.origin.as_bytes()) || origin == Some(localhost.as_bytes());
    }
    let path = request.uri().path();
    let is_api = path.starts_with("/api/");
    let needs_session = is_api && path != "/api/session";
    let signed_in = if allowed && needs_session {
        let credentials = Credentials::of(request.headers(), request.uri());
        guard::signed_in(&app.db, credentials).await
    } else {
        true
    };
    let mut response = if !allowed {
        StatusCode::FORBIDDEN.into_response()
    } else if !signed_in {
        guard::sign_in_required()
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    if is_api {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
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

async fn index() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        INDEX_HTML,
    )
        .into_response()
}

#[derive(Deserialize)]
struct Login {
    nonce: String,
}

/// Trade a login nonce for the owner's session: the cookie, and the token the page must send
/// with every request. Wrong, used and expired nonces all answer the same 401.
async fn create_session(State(app): State<Arc<App>>, Json(login): Json<Login>) -> Response {
    let db = app.db.clone();
    let traded =
        tokio::task::spawn_blocking(move || session::exchange(&store::open(&db)?, &login.nonce))
            .await;
    match traded {
        Ok(Ok(Some(granted))) => (
            [(header::SET_COOKIE, session::cookie_header(&granted.secret))],
            Json(json!({"token": granted.token})),
        )
            .into_response(),
        Ok(Ok(None)) => {
            (StatusCode::UNAUTHORIZED, Json(json!({"error": "sign_in"}))).into_response()
        }
        Ok(Err(err)) => failure(StatusCode::SERVICE_UNAVAILABLE, format!("{err:#}")),
        Err(err) => failure(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

async fn snapshot_json(State(app): State<Arc<App>>) -> Response {
    let (db, content) = (app.db.clone(), app.content);
    let built = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let conn = store::open(&db)?;
        let mut memory = Sampler::new();
        memory.sample(&conn)?;
        Ok(snapshot::build(
            &conn,
            &mut HashMap::new(),
            &memory,
            &mut Observer::default(),
            content,
        )?
        .to_string())
    })
    .await;
    match built {
        Ok(Ok(tree)) => ([(header::CONTENT_TYPE, "application/json")], tree).into_response(),
        Ok(Err(err)) => failure(StatusCode::SERVICE_UNAVAILABLE, format!("{err:#}")),
        Err(err) => failure(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

/// The dashboard server answers: the check that its HTTP side stays up while a peer is away.
async fn health() -> Json<serde_json::Value> {
    Json(json!({"status": "ok"}))
}

async fn stream(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    uri: Uri,
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
    let federation = fed::api::events(app.db.clone(), app.federation.clone());
    let signed_out = guard::signed_out(app.db.clone(), Credentials::of(&headers, &uri));
    let events = futures_util::stream::select(updates, federation);
    Sse::new(futures_util::StreamExt::take_until(events, signed_out))
        .keep_alive(KeepAlive::default())
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
    if request.to_id.starts_with("peer:") {
        return failure(StatusCode::BAD_REQUEST, "remote_target".to_owned());
    }
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

#[derive(Deserialize)]
struct Ending {
    agents: Vec<end::Target>,
}

/// End open harness sessions by their snapshot ids; see `end`.
async fn end_sessions(State(app): State<Arc<App>>, Json(request): Json<Ending>) -> Response {
    if request.agents.is_empty() || request.agents.len() > end::MAX_AT_ONCE {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("name 1 to {} sessions", end::MAX_AT_ONCE),
        );
    }
    let db = app.db.clone();
    match tokio::task::spawn_blocking(move || end::end(&db, &request.agents)).await {
        Ok(Ok(result)) => Json(result).into_response(),
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
        let files =
            std::iter::once(INDEX_HTML).chain(assets::ASSETS.iter().map(|(_, _, body)| *body));
        for body in files {
            for needle in [
                "src=\"http",
                "@import",
                "url(http",
                "href=\"http",
                "import(\"http",
                "from \"http",
            ] {
                assert!(
                    !body.contains(needle),
                    "the dashboard must not load {needle:?}"
                );
            }
        }
    }
}
