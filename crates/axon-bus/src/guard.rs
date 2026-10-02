//! Who may call the dashboard's API: a request needs the owner's cookie AND the session
//! token (`session`), and an open event stream keeps being checked so that signing a
//! browser out, or its session expiring, ends the stream too.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::json;

use crate::{session, store};

/// How soon a signed-out session loses its open stream.
const RECHECK: Duration = Duration::from_millis(500);

/// The session a request claims: the cookie's secret and the token that must go with it.
#[derive(Clone)]
pub struct Credentials {
    secret: String,
    token: String,
}

impl Credentials {
    pub fn of(headers: &HeaderMap, uri: &Uri) -> Option<Self> {
        Some(Self {
            secret: session::presented(headers)?.to_owned(),
            token: session::presented_token(headers, uri)?.to_owned(),
        })
    }
}

pub fn sign_in_required() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "sign_in", "hint": "run axon open"})),
    )
        .into_response()
}

/// Whether `credentials` name a live owner session. A database error is "not signed in":
/// the guard fails closed.
pub async fn signed_in(db: &Path, credentials: Option<Credentials>) -> bool {
    let Some(credentials) = credentials else {
        return false;
    };
    let db = db.to_owned();
    tokio::task::spawn_blocking(move || {
        session::valid(&store::open(&db)?, &credentials.secret, &credentials.token)
    })
    .await
    .is_ok_and(|checked| checked.unwrap_or(false))
}

/// Completes once the session is no longer signed in. `take_until` this on a stream.
pub async fn signed_out(db: PathBuf, credentials: Option<Credentials>) {
    while signed_in(&db, credentials.clone()).await {
        tokio::time::sleep(RECHECK).await;
    }
}

/// Puts the owner session in front of every route of `routes` (the host binary's own API,
/// merged beside the dashboard's, which guards itself).
pub fn require_owner(routes: Router, db: &Path) -> Router {
    routes.layer(middleware::from_fn_with_state(
        db.to_owned(),
        |State(db): State<PathBuf>, request: Request, next: Next| async move {
            let credentials = Credentials::of(request.headers(), request.uri());
            if signed_in(&db, credentials).await {
                next.run(request).await
            } else {
                sign_in_required()
            }
        },
    ))
}

#[cfg(test)]
mod tests;
