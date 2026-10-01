//! The pairing half of the federation API (P2P-SPEC §4 and §10): invites, join, confirm,
//! reject and `GET /api/fed`. Mounted by `serve` beside Settings, so the owner session and
//! the Host/Origin checks apply to every route.

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use iroh::EndpointId;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};

use super::pairing::{self, Confirm, JoinError, PeerRow};
use super::service::Handle;
use super::{identity, invite};
use crate::settings::Federation;
use crate::store;

/// No body of these routes is larger than an invite blob.
const MAX_BODY_BYTES: usize = 16 * 1024;

struct FedApi {
    db: PathBuf,
    federation: Federation,
}

pub fn routes(db: &FsPath, federation: Federation) -> Router {
    let state = Arc::new(FedApi {
        db: db.to_owned(),
        federation,
    });
    Router::new()
        .route("/api/fed", get(read))
        .route("/api/fed/invites", post(create_invite))
        .route("/api/fed/invites/:invite_id", delete(cancel_invite))
        .route("/api/fed/join", post(join))
        .route("/api/fed/peers/:peer_id/confirm", post(confirm))
        .route("/api/fed/peers/:peer_id/reject", post(reject))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

enum Fail {
    Invalid(&'static str),
    /// One answer for every way an invite can be unusable, so none can be told from another.
    InvalidInvite,
    LabelTaken,
    AlreadyPaired,
    UnknownPeer,
    NotPending,
    PairCodeMismatch,
    /// Federation is off, or on but its service did not start (see `GET /api/fed`).
    NotRunning,
    Unreachable,
    Unavailable,
}

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            Self::Invalid(field) => (
                StatusCode::BAD_REQUEST,
                json!({"error": "invalid", "field": field}),
            ),
            Self::InvalidInvite => (StatusCode::BAD_REQUEST, json!({"error": "invalid_invite"})),
            Self::LabelTaken => (StatusCode::BAD_REQUEST, json!({"error": "label_taken"})),
            Self::PairCodeMismatch => (
                StatusCode::BAD_REQUEST,
                json!({"error": "pair_code_mismatch"}),
            ),
            Self::AlreadyPaired => (StatusCode::CONFLICT, json!({"error": "already_paired"})),
            Self::NotPending => (StatusCode::CONFLICT, json!({"error": "not_pending"})),
            Self::NotRunning => (
                StatusCode::CONFLICT,
                json!({"error": "federation_not_running"}),
            ),
            Self::UnknownPeer => (StatusCode::NOT_FOUND, json!({"error": "unknown_peer"})),
            Self::Unreachable => (
                StatusCode::BAD_GATEWAY,
                json!({"error": "peer_unreachable"}),
            ),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error": "unavailable"}),
            ),
        };
        (status, Json(body)).into_response()
    }
}

impl From<JoinError> for Fail {
    fn from(err: JoinError) -> Self {
        match err {
            JoinError::OwnInvite | JoinError::Refused => Self::InvalidInvite,
            JoinError::LabelTaken => Self::LabelTaken,
            JoinError::AlreadyPaired => Self::AlreadyPaired,
            JoinError::Unreachable => Self::Unreachable,
            JoinError::Unavailable => Self::Unavailable,
        }
    }
}

type Answer = Result<Json<Value>, Fail>;
type Done = Result<StatusCode, Fail>;

/// SQLite work blocks, so it runs off the async threads. Detail goes to stderr, never to the
/// caller: an error here can carry paths.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, Fail> {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(done)) => Ok(done),
        Ok(Err(err)) => {
            eprintln!("axon-bus: federation request failed: {err:#}");
            Err(Fail::Unavailable)
        }
        Err(_) => Err(Fail::Unavailable),
    }
}

impl FedApi {
    async fn running(&self) -> Result<Handle, Fail> {
        self.federation.handle().await.ok_or(Fail::NotRunning)
    }
}

async fn read(State(api): State<Arc<FedApi>>) -> Answer {
    let live = api.federation.handle().await.map(|handle| handle.health());
    let db = api.db.clone();
    blocking(move || view(&store::open(&db)?, live))
        .await
        .map(Json)
}

async fn create_invite(State(api): State<Arc<FedApi>>) -> Answer {
    let addr = api.running().await?.addr();
    let db = api.db.clone();
    let issued = blocking(move || invite::issue(&mut store::open(&db)?, &addr)).await?;
    Ok(Json(issued.to_json()))
}

async fn cancel_invite(State(api): State<Arc<FedApi>>, Path(invite_id): Path<String>) -> Done {
    let db = api.db.clone();
    match blocking(move || Ok(invite::cancel(&store::open(&db)?, &invite_id)?)).await? {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(Fail::InvalidInvite),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinBody {
    invite: String,
    label: String,
}

async fn join(State(api): State<Arc<FedApi>>, body: Bytes) -> Answer {
    let body: JoinBody = serde_json::from_slice(&body).map_err(|_| Fail::Invalid("body"))?;
    if !pairing::label_ok(&body.label) {
        return Err(Fail::Invalid("label"));
    }
    let invite = invite::parse(&body.invite).map_err(|_| Fail::InvalidInvite)?;
    let handle = api.running().await?;
    let peer = pairing::join(&handle, &api.db, invite, body.label).await?;
    Ok(Json(pending_json(&peer, Some(&handle.addr().id))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmBody {
    pair_code: String,
}

async fn confirm(State(api): State<Arc<FedApi>>, Path(peer_id): Path<String>, body: Bytes) -> Done {
    let body: ConfirmBody =
        serde_json::from_slice(&body).map_err(|_| Fail::Invalid("pair_code"))?;
    let handle = api.running().await?;
    let (db, own, id) = (api.db.clone(), handle.addr().id, peer_id.clone());
    let outcome =
        blocking(move || pairing::confirm(&mut store::open(&db)?, &own, &id, &body.pair_code))
            .await?;
    match outcome {
        Confirm::UnknownPeer => Err(Fail::UnknownPeer),
        Confirm::NotPending => Err(Fail::NotPending),
        Confirm::Mismatch {
            node_id,
            generation,
        } => {
            tokio::spawn(pairing::notify_removed(handle, node_id, generation));
            Err(Fail::PairCodeMismatch)
        }
        Confirm::Confirmed => {
            handle.reload();
            tokio::spawn(pairing::send_confirmed(handle, api.db.clone(), peer_id));
            Ok(StatusCode::NO_CONTENT)
        }
    }
}

async fn reject(State(api): State<Arc<FedApi>>, Path(peer_id): Path<String>) -> Done {
    let db = api.db.clone();
    let removed = blocking(move || Ok(pairing::reject(&store::open(&db)?, &peer_id)?)).await?;
    let Some((node_id, generation)) = removed else {
        return Err(Fail::NotPending);
    };
    // With no service running there is nobody to tell and no connection to drop.
    if let Some(handle) = api.federation.handle().await {
        tokio::spawn(pairing::notify_removed(handle, node_id, generation));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// What a freshly paired peer looks like to the screen that confirms it.
fn pending_json(peer: &PeerRow, own: Option<&EndpointId>) -> Value {
    json!({
        "peer_id": peer.peer_id,
        "label": peer.label,
        "fingerprint": fingerprint(&peer.node_id),
        "state": peer.state,
        "pair_code": own.and_then(|own| pairing::pair_code(own, &peer.node_id)),
    })
}

fn fingerprint(node_id: &str) -> Option<String> {
    node_id
        .parse::<EndpointId>()
        .ok()
        .map(|node| identity::fingerprint(&node))
}

/// `GET /api/fed` (§10). Peers come from the database, so paused, removed and not-yet-live
/// ones are listed too; `live` (the running service's health) adds what only memory knows.
/// With no running service `enabled` is false whether the owner switched federation off or
/// it failed to start, and every stored `active` peer reads `offline`: the shape is §10's
/// and has no field to tell the two apart, so a peer's `last_error` carries the reason.
fn view(conn: &Connection, live: Option<Value>) -> anyhow::Result<Value> {
    let running = live.is_some();
    let mut body = live.unwrap_or_else(|| {
        json!({
            "enabled": false, "node_id": null, "fingerprint": null,
            "relay": super::relay_setting(conn),
        })
    });
    let own: Option<EndpointId> = body["node_id"].as_str().and_then(|id| id.parse().ok());
    let mut live_peers: HashMap<String, Value> = body["peers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|peer| {
            (
                peer["peer_id"].as_str().unwrap_or_default().to_owned(),
                peer.clone(),
            )
        })
        .collect();
    let peers = pairing::all_peers(conn)?
        .iter()
        .map(|row| peer_json(row, own.as_ref(), live_peers.remove(&row.peer_id)))
        .collect::<Vec<_>>();
    body["invites"] = Value::Array(if running {
        invite::open(conn)?
    } else {
        Vec::new()
    });
    body["peers"] = Value::Array(peers);
    Ok(body)
}

/// One peer of §10. Shares, queue and counters are the defaults of a peer that has none
/// yet; the units that own those fill them in.
fn peer_json(row: &PeerRow, own: Option<&EndpointId>, live: Option<Value>) -> Value {
    let mut peer = json!({
        "peer_id": row.peer_id,
        "label": row.label,
        "fingerprint": fingerprint(&row.node_id),
        "state": if row.state == "active" { "offline" } else { &row.state },
        "path": "none",
        "last_handshake_at": null, "heartbeat_age_ms": null,
        "rtt_ms": null, "rtt_stale": true,
        "last_error": null, "next_retry_at": null,
        "queue": {"count": 0, "bytes": 0, "oldest_at": null},
        "counters": {"sent_accepted": 0, "received": 0, "expired": 0, "rejected": 0, "cancelled": 0},
        "shares": [],
    });
    for (key, value) in live.iter().filter_map(Value::as_object).flatten() {
        peer[key] = value.clone();
    }
    // The stored state is the floor: paused and removed win over a stale in-memory view.
    if matches!(row.state.as_str(), "paused" | "removed") {
        peer["state"] = json!(row.state);
    }
    peer["label"] = json!(row.label);
    if peer["last_error"].is_null() {
        peer["last_error"] = json!(row.last_error);
    }
    if row.state == "pending_confirm" {
        peer["pair_code"] = json!(own.and_then(|own| pairing::pair_code(own, &row.node_id)));
    }
    peer
}
