//! The lifecycle half of the federation API (P2P-SPEC §9): pause, resume, remove and relabel
//! a peer. The change commits first; the best-effort notice to the peer and the service's
//! reload follow in the background, so an owner never waits for a peer that is away.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, post, put};
use axum::Router;
use iroh::EndpointId;
use serde::Deserialize;
use serde_json::json;

use super::{blocking, Done, Fail, FedApi};
use crate::fed::lifecycle::{self, Change, Peer};
use crate::fed::pairing;
use crate::fed::service::Handle;
use crate::store;

/// How long the notice may take; the peer is not waited for beyond it.
const NOTICE_BUDGET: Duration = Duration::from_secs(2);

pub fn routes() -> Router<Arc<FedApi>> {
    Router::new()
        .route("/api/fed/peers/:peer_id/pause", post(pause))
        .route("/api/fed/peers/:peer_id/resume", post(resume))
        .route("/api/fed/peers/:peer_id/label", put(relabel))
        .route("/api/fed/peers/:peer_id", delete(remove))
}

fn applied(change: Change, wrong_state: Fail) -> Result<Peer, Fail> {
    match change {
        Change::Done(peer) => Ok(peer),
        Change::Unknown => Err(Fail::UnknownPeer),
        Change::WrongState => Err(wrong_state),
        Change::LabelTaken => Err(Fail::LabelTaken),
    }
}

/// Tell the peer what happened to the pairing, once and only while it is connected, then
/// make the service re-read the peers: that is what closes the connection.
async fn tell_then_reload(handle: Handle, peer: Peer, what: &'static str) {
    if let Ok(node) = peer.node_id.parse::<EndpointId>() {
        let frame = json!({"type": "notice", "v": 1, "generation": peer.generation, "what": what});
        let _ = tokio::time::timeout(NOTICE_BUDGET, handle.request(&node, &frame)).await;
    }
    handle.reload();
}

async fn pause(State(api): State<Arc<FedApi>>, Path(peer_id): Path<String>) -> Done {
    let db = api.db.clone();
    let change = blocking(move || lifecycle::pause(&mut store::open(&db)?, &peer_id)).await?;
    let peer = applied(change, Fail::PeerNotActive)?;
    if let Some(handle) = api.federation.handle().await {
        tokio::spawn(tell_then_reload(handle, peer, "paused"));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn resume(State(api): State<Arc<FedApi>>, Path(peer_id): Path<String>) -> Done {
    let db = api.db.clone();
    let change = blocking(move || lifecycle::resume(&mut store::open(&db)?, &peer_id)).await?;
    applied(change, Fail::WrongState)?;
    if let Some(handle) = api.federation.handle().await {
        handle.reload();
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn remove(State(api): State<Arc<FedApi>>, Path(peer_id): Path<String>) -> Done {
    let db = api.db.clone();
    let change = blocking(move || lifecycle::remove(&mut store::open(&db)?, &peer_id)).await?;
    let peer = applied(change, Fail::WrongState)?;
    if let Some(handle) = api.federation.handle().await {
        tokio::spawn(tell_then_reload(handle, peer, "removed"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LabelBody {
    label: String,
}

async fn relabel(State(api): State<Arc<FedApi>>, Path(peer_id): Path<String>, body: Bytes) -> Done {
    let body: LabelBody = serde_json::from_slice(&body).map_err(|_| Fail::Invalid("body"))?;
    if !pairing::label_ok(&body.label) {
        return Err(Fail::Invalid("label"));
    }
    let db = api.db.clone();
    let change =
        blocking(move || lifecycle::relabel(&mut store::open(&db)?, &peer_id, &body.label)).await?;
    applied(change, Fail::WrongState)?;
    if let Some(handle) = api.federation.handle().await {
        handle.reload();
    }
    Ok(StatusCode::NO_CONTENT)
}
