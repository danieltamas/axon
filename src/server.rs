//! Local HTTP server (DESIGN.md §9, §16) — the one dashboard (BUS-PLAN §0b) + the usage API.
//!
//! Security boundary (there is no auth — loopback + Origin checks ARE the boundary):
//! - bind `127.0.0.1` only;
//! - reject any request whose `Host` is not loopback (anti-DNS-rebind);
//! - reject any cross-origin `Origin`/`Referer` (anti-CSRF).
//!
//! Routes: `GET /api/summary`, `GET /api/health`, and the dashboard router from `axon-bus`
//! (the page, its assets, `/api/snapshot`, `/api/stream`, `POST /api/msg`), which adds its
//! own per-boot token and CSP. `/api/summary` is kept fresh by `main::spawn_refresher`.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use tower_http::compression::CompressionLayer;

use crate::model::Event;
use crate::summary::{build_summary, recent_events, Summary};

/// How many recent turns the live feed shows.
const FEED_LEN: usize = 12;

/// Shared application state. A background task periodically re-scans and swaps the summary
/// in, so the dashboard is live (poll-based).
pub struct AppState {
    /// All-time summary, refreshed in the background (carries rtk + budget panels).
    pub summary: std::sync::RwLock<Summary>,
    /// Raw events, so `/api/summary?range=` can re-aggregate for a selected window.
    pub events: std::sync::RwLock<Vec<Event>>,
}

/// The usage API merged with the dashboard, behind the loopback/Origin guard and gzip.
pub fn build_router(state: Arc<AppState>, dashboard: Router) -> Router {
    Router::new()
        .route("/api/health", get(api_health))
        .route("/api/summary", get(api_summary))
        .with_state(state)
        .merge(dashboard)
        .layer(middleware::from_fn(local_only))
        .layer(CompressionLayer::new())
}

/// Bind `addr` (loopback) and serve until the process is stopped.
pub async fn serve(
    addr: SocketAddr,
    state: Arc<AppState>,
    dashboard: Router,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    axum::serve(listener, build_router(state, dashboard))
        .await
        .context("axum serve")?;
    Ok(())
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
}
