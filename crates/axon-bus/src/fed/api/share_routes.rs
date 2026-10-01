//! The share half of the federation API (P2P-SPEC §6): offer, accept, change and remove.
//! Each change is committed first and then pushed to the peer; a peer that is away hears it
//! on its next connect.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::routing::{post, put};
use axum::{Json, Router};
use rusqlite::Transaction;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use super::{blocking, Answer, Fail, FedApi};
use crate::fed::shares::{self, Share};
use crate::store;

pub fn routes() -> Router<Arc<FedApi>> {
    Router::new()
        .route("/api/fed/peers/:peer_id/shares", post(offer))
        .route("/api/fed/shares/:share_id/accept", post(accept))
        .route("/api/fed/shares/:share_id", put(change).delete(remove))
}

impl From<shares::Fail> for Fail {
    fn from(err: shares::Fail) -> Self {
        match err {
            shares::Fail::UnknownPeer => Self::UnknownPeer,
            shares::Fail::PeerNotActive => Self::PeerNotActive,
            shares::Fail::UnknownShare => Self::UnknownShare,
            shares::Fail::WrongState => Self::WrongState,
            shares::Fail::Invalid(field) => Self::Invalid(field),
            shares::Fail::AlreadyShared => Self::AlreadyShared,
            shares::Fail::TooMany => Self::TooManyShares,
        }
    }
}

fn parse<T: DeserializeOwned>(body: &Bytes) -> Result<T, Fail> {
    serde_json::from_slice(body).map_err(|_| Fail::Invalid("body"))
}

/// Commit one share change, then push it. `update` says a changed flag, not an accept.
async fn mutate(
    api: &FedApi,
    update: bool,
    work: impl FnOnce(&Transaction) -> Result<Share, shares::Fail> + Send + 'static,
) -> Answer {
    let db = api.db.clone();
    let share = blocking(move || {
        let mut conn = store::open(&db)?;
        let tx = store::write_tx(&mut conn)?;
        let changed = work(&tx);
        if changed.is_ok() {
            tx.commit()?;
        }
        Ok(changed)
    })
    .await??;
    // With no service running the change waits for the next connect.
    if let Some(handle) = api.federation.handle().await {
        tokio::spawn(shares::push(
            handle,
            api.db.clone(),
            share.share_id.clone(),
            update,
        ));
    }
    Ok(Json(share.to_json()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OfferBody {
    local_repo: String,
    label: String,
    inbound: bool,
    outbound: bool,
}

async fn offer(State(api): State<Arc<FedApi>>, Path(peer_id): Path<String>, body: Bytes) -> Answer {
    let b: OfferBody = parse(&body)?;
    mutate(&api, false, move |tx| {
        shares::offer(tx, &peer_id, &b.local_repo, &b.label, b.inbound, b.outbound)
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptBody {
    local_repo: String,
    inbound: bool,
    outbound: bool,
}

async fn accept(
    State(api): State<Arc<FedApi>>,
    Path(share_id): Path<String>,
    body: Bytes,
) -> Answer {
    let b: AcceptBody = parse(&body)?;
    mutate(&api, false, move |tx| {
        shares::accept(tx, &share_id, &b.local_repo, b.inbound, b.outbound)
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FlagsBody {
    inbound: bool,
    outbound: bool,
}

async fn change(
    State(api): State<Arc<FedApi>>,
    Path(share_id): Path<String>,
    body: Bytes,
) -> Answer {
    let b: FlagsBody = parse(&body)?;
    mutate(&api, true, move |tx| {
        shares::change(tx, &share_id, b.inbound, b.outbound)
    })
    .await
}

async fn remove(State(api): State<Arc<FedApi>>, Path(share_id): Path<String>) -> Answer {
    mutate(&api, false, move |tx| shares::remove(tx, &share_id)).await
}
