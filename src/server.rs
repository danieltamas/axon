//! Local HTTP server (DESIGN.md §9, §16) — serves the Mission Control dashboard + JSON API.
//!
//! Security boundary (there is no auth — loopback + Origin checks ARE the boundary):
//! - bind `127.0.0.1` only;
//! - reject any request whose `Host` is not loopback (anti-DNS-rebind);
//! - reject any cross-origin `Origin`/`Referer` (anti-CSRF).
//!
//! Routes: `GET /` (the embedded dashboard), `GET /api/summary`, `GET /api/health`.
//! The dashboard is the live three.js-style "brain" Mission Control; data comes from
//! `/api/summary`, which a background task keeps fresh (see `main::spawn_refresher`).

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use tower_http::compression::CompressionLayer;

use crate::model::Event;
use crate::summary::{build_summary, recent_events, Summary};

/// How many recent turns the live feed shows.
const FEED_LEN: usize = 12;

/// The dashboard — a single self-contained file, embedded at compile time. Edit
/// `ui/dist/index.html` to change the UI; no build step, no external assets (§16).
const INDEX_HTML: &str = include_str!("../ui/dist/index.html");

/// Shared application state. A background task periodically re-scans and swaps the summary
/// in, so the dashboard is live (poll-based).
pub struct AppState {
    /// All-time summary, refreshed in the background (carries rtk + budget panels).
    pub summary: std::sync::RwLock<Summary>,
    /// Raw events, so `/api/summary?range=` can re-aggregate for a selected window.
    pub events: std::sync::RwLock<Vec<Event>>,
}

/// Build the router with the loopback/Origin guard and gzip compression applied.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/health", get(api_health))
        .route("/api/summary", get(api_summary))
        .layer(middleware::from_fn(local_only))
        .layer(CompressionLayer::new())
        .with_state(state)
}

/// Bind `addr` (loopback) and serve until the process is stopped.
pub async fn serve(addr: SocketAddr, state: Arc<AppState>) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    axum::serve(listener, build_router(state))
        .await
        .context("axum serve")?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn api_health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

#[derive(serde::Deserialize)]
struct RangeQuery {
    range: Option<String>,
}

async fn api_summary(
    State(state): State<Arc<AppState>>,
    Query(q): Query<RangeQuery>,
) -> Json<Summary> {
    let all = state.summary.read().unwrap_or_else(|p| p.into_inner());
    let range = q.range.as_deref().unwrap_or("all");
    let events = state.events.read().unwrap_or_else(|p| p.into_inner());
    if range == "all" {
        let mut s = all.clone();
        s.recent = recent_events(events.iter(), FEED_LEN);
        return Json(s);
    }
    // Re-aggregate over the selected window; carry the range-independent panels (rtk, budget,
    // today/week/month spend) from the all-time summary.
    let since = since_ms(range);
    let mut s = build_summary(events.iter().filter(|e| e.ts >= since));
    s.rtk = all.rtk.clone();
    s.today_cost_eur = all.today_cost_eur;
    s.week_cost_eur = all.week_cost_eur;
    s.month_cost_eur = all.month_cost_eur;
    s.budget_day_eur = all.budget_day_eur;
    s.budget_week_eur = all.budget_week_eur;
    s.budget_month_eur = all.budget_month_eur;
    s.recent = recent_events(events.iter().filter(|e| e.ts >= since), FEED_LEN);
    Json(s)
}

/// Epoch-ms lower bound for a range key: `today` (local midnight), `7d`/`30d` (rolling), else 0.
fn since_ms(range: &str) -> i64 {
    use chrono::{Duration, Local};
    let now = Local::now();
    match range {
        "today" => now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .and_then(|d| d.and_local_timezone(Local).single())
            .map(|d| d.timestamp_millis())
            .unwrap_or(0),
        "7d" => (now - Duration::days(7)).timestamp_millis(),
        "30d" => (now - Duration::days(30)).timestamp_millis(),
        _ => 0,
    }
}

/// Reject non-loopback `Host` and cross-origin `Origin`/`Referer` (DESIGN.md §16).
async fn local_only(req: Request, next: Next) -> Response {
    let headers = req.headers();
    if !host_is_local(headers) {
        return (StatusCode::FORBIDDEN, "forbidden: non-local Host").into_response();
    }
    for key in ["origin", "referer"] {
        if let Some(val) = headers.get(key).and_then(|v| v.to_str().ok()) {
            if !url_is_local(val) {
                return (StatusCode::FORBIDDEN, "forbidden: cross-origin").into_response();
            }
        }
    }
    next.run(req).await
}

fn host_is_local(headers: &HeaderMap) -> bool {
    headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(|h| is_loopback(host_only(h)))
        .unwrap_or(false)
}

/// True if a full URL (Origin/Referer) points at a loopback host.
fn url_is_local(url: &str) -> bool {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let authority = after_scheme.split('/').next().unwrap_or(after_scheme);
    is_loopback(host_only(authority))
}

/// Strip a `:port` (and `[..]` IPv6 brackets) from an authority, leaving the bare host.
fn host_only(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest); // [::1]:7777 -> ::1
    }
    authority.split(':').next().unwrap_or(authority)
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts_accepted() {
        for h in [
            "localhost:7777",
            "127.0.0.1:7777",
            "[::1]:7777",
            "localhost",
        ] {
            assert!(is_loopback(host_only(h)), "{h}");
        }
    }

    #[test]
    fn foreign_hosts_rejected() {
        for h in ["evil.com", "evil.com:7777", "169.254.1.1:80"] {
            assert!(!is_loopback(host_only(h)), "{h}");
        }
    }

    #[test]
    fn origin_locality() {
        assert!(url_is_local("http://localhost:7777"));
        assert!(url_is_local("http://127.0.0.1:7777/api/summary"));
        assert!(!url_is_local("http://evil.com"));
        assert!(!url_is_local("https://attacker.example:7777/x"));
    }

    /// Privacy gate (§16): the embedded UI must load NO remote assets. Navigational
    /// `<a href="https://…">` links are fine (not fetched); asset-loading vectors are not.
    #[test]
    fn no_remote_assets_in_dashboard() {
        for needle in [
            "src=\"http",
            "@import",
            "url(http",
            "googleapis",
            "gstatic",
            "<link rel=\"stylesheet\" href=\"http",
        ] {
            assert!(
                !INDEX_HTML.contains(needle),
                "embedded dashboard must not load remote assets — found {needle:?}"
            );
        }
    }
}
