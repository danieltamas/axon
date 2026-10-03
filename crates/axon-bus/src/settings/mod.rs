//! The Settings API (P2P-SPEC §2): every owner-changeable setting behind one read and a
//! write per section, each answering the full settings as they now are, so the page shows
//! what the server confirmed and never what it hoped for. Mounted by `serve`, so the owner
//! session and the Host/Origin checks apply to every route.
//!
//! One source per setting: switches live in the `settings` table, EUR caps in the file
//! `axon` already reads, hook state is whatever `doctor` would report.

mod budgets;
mod federation;
mod input;

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use rusqlite::{Connection, ErrorCode};
use serde_json::{json, Value};

pub use federation::Federation;

use crate::{doctor, fed, install, session, store, transcript, uninstall, usage, Harness};

/// How long compaction waits for the write lock before it answers `busy`.
const COMPACT_LOCK_WAIT: Duration = Duration::from_secs(1);
const COMPACT_RETRY: Duration = Duration::from_millis(50);

pub struct Settings {
    db: PathBuf,
    /// This server's capture option; `--no-content` stays off whatever is stored.
    content: bool,
    federation: Federation,
}

pub fn routes(db: &FsPath, content: bool, federation: Federation) -> Router {
    let state = Arc::new(Settings {
        db: db.to_owned(),
        content,
        federation,
    });
    Router::new()
        .route("/api/settings", get(read))
        .route("/api/settings/capture", put(put_capture))
        .route("/api/settings/usage", put(put_usage))
        .route("/api/settings/budgets", put(put_budgets))
        .route("/api/settings/federation", put(put_federation))
        .route("/api/settings/bus", put(put_bus))
        .route("/api/settings/hooks/:harness/install", post(install_hooks))
        .route(
            "/api/settings/hooks/:harness/uninstall",
            post(uninstall_hooks),
        )
        .route("/api/settings/storage/compact", post(compact))
        .route("/api/settings/sessions/revoke_others", post(revoke_others))
        .with_state(state)
}

/// Why a settings request did not apply; nothing was changed in any of these cases.
enum Fail {
    Invalid(String),
    Busy,
    /// The request was valid but the system state refuses it (a config that does not parse,
    /// a development build that cannot be wired); the message says why.
    Refused(String),
    Failed(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for Fail {
    fn from(err: E) -> Self {
        Fail::Failed(err.into())
    }
}

impl From<input::Invalid> for Fail {
    fn from(invalid: input::Invalid) -> Self {
        Fail::Invalid(invalid.0)
    }
}

fn refused(err: anyhow::Error) -> Fail {
    Fail::Refused(format!("{err:#}"))
}

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            Fail::Invalid(field) => (
                StatusCode::BAD_REQUEST,
                json!({"error": "invalid", "field": field}),
            ),
            Fail::Busy => (StatusCode::CONFLICT, json!({"error": "busy"})),
            Fail::Refused(why) => (StatusCode::CONFLICT, json!({"error": why})),
            Fail::Failed(err) => {
                // The chain can hold paths; the page gets a fixed word, the log the cause.
                eprintln!("axon-bus: a settings request failed: {err:#}");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({"error": "unavailable"}),
                )
            }
        };
        (status, Json(body)).into_response()
    }
}

type Answer = Result<Json<Value>, Fail>;

/// SQLite and file work blocks, so it runs off the async threads.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Fail> + Send + 'static,
) -> Result<T, Fail> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(Fail::from)?
}

impl Settings {
    /// Run `change` against the hub, then answer the settings as they now stand.
    async fn after(
        &self,
        change: impl FnOnce(&mut Connection) -> Result<(), Fail> + Send + 'static,
    ) -> Answer {
        let (db, content) = (self.db.clone(), self.content);
        blocking(move || {
            let mut conn = store::open(&db)?;
            change(&mut conn)?;
            view(&conn, &db, content)
        })
        .await
        .map(Json)
    }
}

async fn read(State(settings): State<Arc<Settings>>) -> Answer {
    settings.after(|_| Ok(())).await
}

async fn put_capture(State(settings): State<Arc<Settings>>, body: Bytes) -> Answer {
    let new = input::capture(&body)?;
    let content = settings.content;
    settings
        .after(move |conn| {
            let tx = store::write_tx(conn)?;
            transcript::set_capture_settings(&tx, new.enabled, new.narrative_days)?;
            tx.commit()?;
            // Applied now rather than at the poller's next tick, so a switch that turns
            // capture off never leaves a stretch of recording behind it.
            transcript::apply_capture(conn, content)?;
            transcript::expire(conn)?;
            Ok(())
        })
        .await
}

async fn put_usage(State(settings): State<Arc<Settings>>, body: Bytes) -> Answer {
    let days = input::usage_retention(&body)?;
    settings
        .after(move |conn| {
            usage::set_retention_days(conn, days)?;
            Ok(usage::expire(conn)?)
        })
        .await
}

async fn put_budgets(State(settings): State<Arc<Settings>>, body: Bytes) -> Answer {
    let changes = input::budgets(&body)?;
    settings
        .after(move |_| budgets::write(&changes).map_err(refused))
        .await
}

async fn put_bus(State(settings): State<Arc<Settings>>, body: Bytes) -> Answer {
    let on = input::auto_link(&body)?;
    settings
        .after(move |conn| Ok(crate::route::set_auto_link(conn, on)?))
        .await
}

async fn put_federation(State(settings): State<Arc<Settings>>, body: Bytes) -> Answer {
    let change = input::federation(&body)?;
    let db = settings.db.clone();
    let changed = blocking(move || {
        let data_dir = db
            .parent()
            .ok_or_else(|| anyhow::anyhow!("no data directory"))?;
        let mut conn = store::open(&db)?;
        let before = (fed::enabled(&conn), fed::relay_setting(&conn));
        let tx = store::write_tx(&mut conn)?;
        if let Some(on) = change.enabled {
            fed::enable(data_dir, &tx, on).map_err(refused)?;
        }
        if let Some(relay) = &change.relay {
            fed::set_relay(&tx, relay)?;
        }
        tx.commit()?;
        Ok(before != (fed::enabled(&conn), fed::relay_setting(&conn)))
    })
    .await?;
    // The endpoint and its relay are fixed when it is built, so a change means a restart.
    if changed {
        settings.federation.restart().await;
    }
    settings.after(|_| Ok(())).await
}

fn harness_named(name: &str) -> Result<Harness, Fail> {
    Harness::ALL
        .into_iter()
        .find(|harness| harness.as_str() == name)
        .ok_or_else(|| Fail::Invalid("harness".to_owned()))
}

async fn install_hooks(State(settings): State<Arc<Settings>>, Path(name): Path<String>) -> Answer {
    let harness = harness_named(&name)?;
    settings
        .after(move |_| install::install(harness).map_err(refused))
        .await
}

async fn uninstall_hooks(
    State(settings): State<Arc<Settings>>,
    Path(name): Path<String>,
) -> Answer {
    let harness = harness_named(&name)?;
    settings
        .after(move |_| uninstall::uninstall(harness).map_err(refused))
        .await
}

async fn compact(State(settings): State<Arc<Settings>>) -> Answer {
    settings
        .after(|conn| {
            // The deadline is kept here rather than by SQLite's busy handler, whose sleeps
            // overshoot on a loaded machine and stretch the promised one second.
            conn.busy_timeout(Duration::ZERO)?;
            let deadline = Instant::now() + COMPACT_LOCK_WAIT;
            loop {
                match conn.execute_batch("VACUUM") {
                    Err(rusqlite::Error::SqliteFailure(err, _))
                        if err.code == ErrorCode::DatabaseBusy =>
                    {
                        if Instant::now() + COMPACT_RETRY > deadline {
                            return Err(Fail::Busy);
                        }
                        std::thread::sleep(COMPACT_RETRY);
                    }
                    done => return Ok(done?),
                }
            }
        })
        .await
}

async fn revoke_others(State(settings): State<Arc<Settings>>, headers: HeaderMap) -> Answer {
    let secret = session::presented(&headers)
        .ok_or_else(|| Fail::Refused("sign_in".to_owned()))?
        .to_owned();
    settings
        .after(move |conn| Ok(session::revoke_others(conn, &secret)?))
        .await
}

/// The full `GET /api/settings` body.
fn view(conn: &Connection, db: &FsPath, content: bool) -> Result<Value, Fail> {
    let hooks_exe = install::current_exe().ok();
    let hooks: Vec<Value> = Harness::ALL
        .into_iter()
        .map(|harness| {
            let installed = hooks_exe
                .as_deref()
                .is_some_and(|exe| doctor::is_wired(harness, exe).unwrap_or(false));
            json!({
                "harness": harness.as_str(),
                "installed": installed,
                "config_path": install::layout(harness).config.display().to_string(),
            })
        })
        .collect();
    let federation_on = fed::enabled(conn);
    let identity = if federation_on { node_id(db) } else { None };
    Ok(json!({
        "capture": {
            "enabled": transcript::capture_enabled(conn)?,
            "forced_off": !content,
            "narrative_days": transcript::narrative_days(conn)?,
        },
        "usage": {"retention_days": usage::retention_days(conn)?},
        "budgets": budgets::read(),
        "hooks": hooks,
        "storage": {
            "db_bytes": file_bytes(db),
            "wal_bytes": file_bytes(&sibling(db, "-wal")),
            "sessions": session::live_count(conn)?,
        },
        "federation": {
            "enabled": federation_on,
            "relay": fed::relay_setting(conn),
            "node_id": identity.as_ref().map(|(id, _)| id),
            "fingerprint": identity.as_ref().map(|(_, print)| print),
        },
        "bus": {"auto_link": crate::route::auto_link_enabled(conn)?},
    }))
}

/// This node's id and fingerprint, from the identity key when there is a readable one.
fn node_id(db: &FsPath) -> Option<(String, String)> {
    let key_file = fed::identity::fed_dir(db.parent()?).join(fed::identity::KEY_FILE);
    let id = fed::identity::read_key(&key_file).ok()??.public();
    Some((id.to_string(), fed::identity::fingerprint(&id)))
}

fn sibling(db: &FsPath, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

/// A missing file (no WAL yet) is zero bytes.
fn file_bytes(path: &FsPath) -> u64 {
    std::fs::metadata(path).map_or(0, |meta| meta.len())
}
