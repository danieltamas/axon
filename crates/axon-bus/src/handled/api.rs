//! `GET /api/handled?repo=<repo>`: one repository's ledger for the dashboard, newest first.
//! Mounted inside `serve`, so the owner session guards it like every other `/api` route.

use std::path::{Path, PathBuf};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::store;

#[derive(Deserialize)]
struct Ask {
    repo: String,
}

pub fn routes(db: &Path) -> Router {
    Router::new()
        .route("/api/handled", get(read))
        .with_state(db.to_owned())
}

async fn read(State(db): State<PathBuf>, Query(ask): Query<Ask>) -> Response {
    let entries = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let conn = store::open(&db)?;
        super::ensure(&conn)?;
        Ok(super::list(&conn, &ask.repo, None)?)
    })
    .await;
    match entries {
        Ok(Ok(entries)) => Json(json!({"entries": entries})).into_response(),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "unavailable"})),
        )
            .into_response(),
    }
}
