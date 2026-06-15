//! Local HTTP server (DESIGN.md §9, §16) — the M3-lite slice of the dashboard.
//!
//! Security boundary (there is no auth — loopback + Origin checks ARE the boundary):
//! - bind `127.0.0.1` only;
//! - reject any request whose `Host` is not loopback (anti-DNS-rebind);
//! - reject any cross-origin `Origin` (anti-CSRF).
//!
//! Routes: `GET /` (inline analytics page, no external assets), `GET /api/summary`
//! (SQLite-derived aggregate), `GET /api/health`. The live 3D brain + SSE land in M4.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use tower_http::compression::CompressionLayer;

use crate::summary::Summary;

/// Shared, read-only application state. M1/M3-lite snapshots the data at boot; the live
/// file-watch → refresh pipeline arrives in M4.
pub struct AppState {
    pub summary: Summary,
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

async fn api_summary(State(state): State<Arc<AppState>>) -> Json<Summary> {
    Json(state.summary.clone())
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
    // HTTP/1.1 requires Host; a missing/garbled one is rejected.
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

/// The dashboard. Fully self-contained — no CDN, no remote fonts/scripts (DESIGN.md §16).
const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Axon</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  body { margin: 0; background: #0b0e14; color: #e6e9ef;
    font: 15px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; }
  header { padding: 28px 32px 8px; }
  h1 { margin: 0; font-size: 22px; letter-spacing: .5px; }
  h1 .dot { color: #7aa2f7; }
  .sub { color: #8b93a7; font-size: 13px; margin-top: 2px; }
  main { padding: 16px 32px 48px; max-width: 1000px; }
  .cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px,1fr)); gap: 12px; margin: 18px 0; }
  .card { background: #11151f; border: 1px solid #1d2433; border-radius: 12px; padding: 16px; }
  .card .v { font-size: 24px; font-weight: 600; }
  .card .k { color: #8b93a7; font-size: 12px; text-transform: uppercase; letter-spacing: .6px; margin-top: 4px; }
  .warn { background: #2a1d12; border: 1px solid #5a3a1a; color: #f0b66a; border-radius: 10px; padding: 12px 16px; margin: 12px 0; font-size: 13px; }
  h2 { font-size: 14px; color: #aeb6c7; text-transform: uppercase; letter-spacing: .8px; margin: 28px 0 10px; }
  table { width: 100%; border-collapse: collapse; }
  th, td { text-align: left; padding: 8px 10px; border-bottom: 1px solid #1a2030; font-variant-numeric: tabular-nums; }
  th { color: #8b93a7; font-weight: 500; font-size: 12px; }
  td.n, th.n { text-align: right; }
  .bar { height: 4px; background: #7aa2f7; border-radius: 2px; }
  footer { color: #5b6478; font-size: 12px; padding: 0 32px 32px; }
  a { color: #7aa2f7; }
</style>
</head>
<body>
<header>
  <h1>Axon<span class="dot">.</span></h1>
  <div class="sub">local, harness-agnostic observability for AI coding agents</div>
</header>
<main>
  <div id="warn"></div>
  <div class="cards" id="cards"></div>
  <h2>Agents</h2>
  <table><thead><tr><th>Agent</th><th class="n">Tokens out</th><th class="n">Cost</th><th></th></tr></thead>
    <tbody id="agents"></tbody></table>
  <h2>Models</h2>
  <table><thead><tr><th>Model</th><th class="n">Tokens out</th><th class="n">Cost</th><th></th></tr></thead>
    <tbody id="models"></tbody></table>
</main>
<footer id="foot"></footer>
<script>
const fmt = n => n.toLocaleString();
const eur = n => "€" + n.toFixed(2);
function card(k, v) { return `<div class="card"><div class="v">${v}</div><div class="k">${k}</div></div>`; }
function rows(items, label, max) {
  return items.map(i => {
    const w = max ? Math.max(2, Math.round(100 * i.tokens_out / max)) : 0;
    return `<tr><td>${i[label]}</td><td class="n">${fmt(i.tokens_out)}</td>` +
      `<td class="n">${eur(i.cost_eur)}</td><td style="width:30%"><div class="bar" style="width:${w}%"></div></td></tr>`;
  }).join("");
}
fetch("/api/summary").then(r => r.json()).then(s => {
  document.getElementById("cards").innerHTML =
    card("Events", fmt(s.events)) + card("Sessions", fmt(s.sessions)) +
    card("Tokens out", fmt(s.tokens_out)) + card("Lines edited", fmt(s.loc_added)) +
    card("Cost", eur(s.cost_eur));
  if (s.unpriced_models.length) {
    document.getElementById("warn").innerHTML =
      `<div class="warn">⚠ Unpriced models (cost is a floor, not exact): ${s.unpriced_models.join(", ")}</div>`;
  }
  const aMax = Math.max(1, ...s.by_agent.map(a => a.tokens_out));
  const mMax = Math.max(1, ...s.by_model.map(m => m.tokens_out));
  document.getElementById("agents").innerHTML = rows(s.by_agent.slice(0, 12), "agent", aMax);
  document.getElementById("models").innerHTML = rows(s.by_model.slice(0, 12), "model", mMax);
  document.getElementById("foot").textContent =
    `${(s.unattributed_token_pct).toFixed(1)}% of output tokens unattributed · cost in EUR · snapshot at load (live updates land in M4)`;
});
</script>
</body>
</html>
"#;

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
