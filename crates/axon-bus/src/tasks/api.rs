//! `GET /api/tasks?range=<range>[&repo=<repo>][&order=cost]`: the task list for the dashboard,
//! newest first, or the costliest with `order=cost`. Mounted
//! inside `serve`, so the owner session guards it like every other `/api` route.

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
    range: Option<String>,
    repo: Option<String>,
    order: Option<String>,
}

pub fn routes(db: &Path) -> Router {
    Router::new()
        .route("/api/tasks", get(read))
        .with_state(db.to_owned())
}

async fn read(State(db): State<PathBuf>, Query(ask): Query<Ask>) -> Response {
    let tasks = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let conn = store::open(&db)?;
        let range = ask.range.as_deref().unwrap_or("all");
        let by_cost = ask.order.as_deref() == Some("cost");
        Ok(super::list(&conn, range, ask.repo.as_deref(), by_cost)?)
    })
    .await;
    match tasks {
        Ok(Ok(tasks)) => Json(json!({"tasks": tasks})).into_response(),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "unavailable"})),
        )
            .into_response(),
    }
}
