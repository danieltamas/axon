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
/// Phosphor-oscilloscope aesthetic: monospace, signal grid + scanlines, traces with a
/// glowing synapse node for magnitude. All injected names are HTML-escaped (`esc`).
const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Axon · telemetry</title>
<link rel="icon" href="data:,">
<style>
  :root{
    --bg:#070b0a; --panel:#0c1311; --panel2:#0a0f0d; --line:#13201c; --hair:#0e1614;
    --txt:#cfe9dd; --dim:#5c7268; --faint:#3a473f;
    --grn:#54f0a0; --grn-dim:#2b9d6a; --grn-glow:#7dffb8; --amber:#f2b53d;
    --mono: ui-monospace, "SF Mono", SFMono-Regular, "JetBrains Mono", "Cascadia Code",
            "Roboto Mono", Menlo, Consolas, "Liberation Mono", monospace;
  }
  *{box-sizing:border-box;margin:0;padding:0}
  html{color-scheme:dark}
  body{
    background:radial-gradient(1200px 560px at 78% -12%, #0c1714 0%, transparent 60%), var(--bg);
    color:var(--txt); font-family:var(--mono); font-size:13px; line-height:1.5;
    letter-spacing:.02em; -webkit-font-smoothing:antialiased; min-height:100vh; padding-bottom:42px;
  }
  /* faint signal grid */
  body::before{content:"";position:fixed;inset:0;z-index:0;pointer-events:none;
    background-image:linear-gradient(var(--line) 1px,transparent 1px),
      linear-gradient(90deg,var(--line) 1px,transparent 1px);
    background-size:44px 44px;opacity:.32;
    -webkit-mask-image:radial-gradient(circle at 50% 26%,#000 0%,transparent 88%);
    mask-image:radial-gradient(circle at 50% 26%,#000 0%,transparent 88%);}
  /* CRT scanlines */
  body::after{content:"";position:fixed;inset:0;z-index:3;pointer-events:none;
    background:repeating-linear-gradient(0deg,rgba(0,0,0,.16) 0 1px,transparent 1px 3px);opacity:.5;}
  .wrap{position:relative;z-index:1;max-width:1080px;margin:0 auto;padding:0 28px}
  header{display:flex;align-items:center;justify-content:space-between;padding:30px 0 14px;border-bottom:1px solid var(--line)}
  .brand{display:flex;align-items:baseline;gap:11px}
  .brand h1{font-size:18px;font-weight:700;letter-spacing:.36em;text-transform:uppercase}
  .brand h1 b{color:var(--grn);text-shadow:0 0 13px var(--grn-dim)}
  .brand .tag{color:var(--dim);font-size:11px;letter-spacing:.12em}
  .status{display:flex;align-items:center;gap:8px;color:var(--dim);font-size:11px;letter-spacing:.16em;text-transform:uppercase}
  .led{width:7px;height:7px;border-radius:50%;background:var(--grn);box-shadow:0 0 9px var(--grn);animation:pulse 2.4s ease-in-out infinite}
  @keyframes pulse{0%,100%{opacity:.35}50%{opacity:1}}
  .strip{height:36px;margin:2px 0 24px;opacity:.6}
  .strip svg{width:100%;height:100%;display:block}
  .strip path{fill:none;stroke:var(--grn-dim);stroke-width:1.2;
    stroke-dasharray:2200;stroke-dashoffset:2200;animation:draw 2.4s ease forwards}
  @keyframes draw{to{stroke-dashoffset:0}}
  .gauges{display:grid;grid-template-columns:repeat(auto-fit,minmax(148px,1fr));gap:1px;background:var(--line);border:1px solid var(--line);margin-bottom:22px}
  .g{background:var(--panel);padding:15px 18px;opacity:0;transform:translateY(8px);animation:rise .5s ease forwards}
  .g .lbl{color:var(--dim);font-size:10px;letter-spacing:.18em;text-transform:uppercase;margin-bottom:9px}
  .g .val{font-size:25px;font-weight:600;font-variant-numeric:tabular-nums}
  .g .val .u{font-size:13px;color:var(--grn-dim);margin-left:3px}
  .g.cost .val{color:var(--grn);text-shadow:0 0 15px rgba(84,240,160,.25)}
  @keyframes rise{to{opacity:1;transform:translateY(0)}}
  .rtk{display:grid;grid-template-columns:1fr auto;gap:18px;align-items:center;
    border:1px solid var(--line);background:linear-gradient(180deg,#0a1612,var(--panel2));padding:18px 22px;margin-bottom:24px}
  .rtk .hd{color:var(--grn-dim);font-size:10px;letter-spacing:.24em;text-transform:uppercase;margin-bottom:11px}
  .rtk .big{font-size:32px;font-weight:700;color:var(--grn);text-shadow:0 0 18px rgba(84,240,160,.3);font-variant-numeric:tabular-nums}
  .rtk .big small{font-size:16px;color:var(--dim);font-weight:600}
  .rtk .sub{color:var(--dim);font-size:12px;margin-top:5px}
  .rtk .meter{height:8px;background:#0e1a16;border:1px solid var(--line);margin-top:13px;position:relative;overflow:hidden}
  .rtk .meter i{position:absolute;inset:0 auto 0 0;background:linear-gradient(90deg,var(--grn-dim),var(--grn-glow));box-shadow:0 0 14px var(--grn-dim)}
  .rtk .right{text-align:right;color:var(--dim);font-size:11px;letter-spacing:.12em;text-transform:uppercase}
  .rtk .right b{display:block;color:var(--grn);font-size:22px;font-weight:700;letter-spacing:0;font-variant-numeric:tabular-nums;text-shadow:0 0 12px rgba(84,240,160,.3)}
  .harness{margin-bottom:24px}
  .harness .hd{display:flex;align-items:center;gap:9px;color:var(--dim);font-size:10px;letter-spacing:.2em;text-transform:uppercase;margin-bottom:9px}
  .harness .hd::before{content:"";width:5px;height:5px;background:var(--grn);box-shadow:0 0 6px var(--grn);transform:rotate(45deg)}
  .hbar{height:14px;display:flex;border:1px solid var(--line);background:#0a0f0d;overflow:hidden}
  .hbar .seg{height:100%}
  .hleg{display:flex;gap:20px;flex-wrap:wrap;margin-top:10px;font-size:12px;color:var(--dim)}
  .hleg .it{display:flex;align-items:center;gap:7px}
  .hleg .dot{width:8px;height:8px;border-radius:2px}
  .hleg b{color:var(--txt);font-weight:600;font-variant-numeric:tabular-nums}
  .warn{border:1px solid #4a3a14;background:#15110a;color:var(--amber);padding:11px 16px;margin-bottom:22px;font-size:12px}
  .warn b{color:#ffd279}
  h2{display:flex;align-items:center;gap:9px;color:var(--dim);font-size:11px;letter-spacing:.2em;text-transform:uppercase;margin:26px 0 10px}
  h2::before{content:"";width:5px;height:5px;background:var(--grn);box-shadow:0 0 6px var(--grn);transform:rotate(45deg)}
  table{width:100%;border-collapse:collapse}
  th{text-align:left;color:var(--faint);font-weight:500;font-size:10px;letter-spacing:.14em;text-transform:uppercase;padding:6px 10px;border-bottom:1px solid var(--line)}
  td{padding:9px 10px;border-bottom:1px solid var(--hair);font-variant-numeric:tabular-nums}
  tr:hover td{background:#0b1210}
  td.n,th.n{text-align:right}
  td.cost{color:var(--grn-dim)}
  .tr{position:relative;height:14px;min-width:80px}
  .tr::before{content:"";position:absolute;left:0;right:0;top:50%;height:1px;background:var(--line)}
  .tr .fill{position:absolute;left:0;top:50%;height:1px;background:linear-gradient(90deg,transparent,var(--grn-dim))}
  .tr .node{position:absolute;top:50%;width:5px;height:5px;border-radius:50%;background:var(--grn-glow);box-shadow:0 0 8px var(--grn);transform:translate(-50%,-50%)}
  footer{margin-top:30px;padding-top:14px;border-top:1px solid var(--line);color:var(--faint);font-size:11px;letter-spacing:.06em;display:flex;gap:20px;flex-wrap:wrap}
  footer .k{color:var(--grn-dim)}
  @media(max-width:640px){.rtk{grid-template-columns:1fr}.tr{display:none}}
</style>
</head>
<body>
<div class="wrap">
  <header>
    <div class="brand"><h1>AX<b>O</b>N</h1><span class="tag">// agent telemetry</span></div>
    <div class="status"><span class="led"></span><span id="stat">snapshot</span></div>
  </header>
  <div class="strip" aria-hidden="true"><svg viewBox="0 0 1000 36" preserveAspectRatio="none"><path id="sig"></path></svg></div>
  <div id="warn"></div>
  <div class="gauges" id="gauges"></div>
  <div id="rtk"></div>
  <div class="harness" id="harness"></div>
  <h2>Agents</h2>
  <table><thead><tr><th>Agent</th><th class="n">Tokens out</th><th class="n">Cost</th><th>Signal</th></tr></thead><tbody id="agents"></tbody></table>
  <h2>Models</h2>
  <table><thead><tr><th>Model</th><th class="n">Tokens out</th><th class="n">Cost</th><th>Signal</th></tr></thead><tbody id="models"></tbody></table>
  <footer id="foot"></footer>
</div>
<script>
const $ = id => document.getElementById(id);
const esc = s => String(s).replace(/[&<>"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
const grp = n => Number(n).toLocaleString('en-US');
const eur = n => '€' + Number(n).toLocaleString('en-US', {minimumFractionDigits:2, maximumFractionDigits:2});
const cmp = n => n>=1e9?(n/1e9).toFixed(1)+'B':n>=1e6?(n/1e6).toFixed(1)+'M':n>=1e3?(n/1e3).toFixed(1)+'K':String(n);

// Deterministic oscilloscope trace for the header (no randomness needed).
(function(){
  let p=[];
  for(let x=0;x<=1000;x+=12){
    let y = 18 + Math.sin(x/48)*7 + Math.cos(x/15)*2.4 + (x%140<10 ? -10 : 0);
    p.push(x+','+y.toFixed(1));
  }
  $('sig').setAttribute('d','M'+p.join(' L'));
})();

const gauge = (lbl,val,cls) =>
  `<div class="g ${cls||''}"><div class="lbl">${lbl}</div><div class="val">${val}</div></div>`;

function rows(items,key,max){
  return items.map(i=>{
    const w = max ? Math.max(3, Math.round(100*i.tokens_out/max)) : 0;
    return `<tr><td>${esc(i[key] || '(unknown)')}</td>`+
      `<td class="n" title="${grp(i.tokens_out)} tokens">${cmp(i.tokens_out)}</td>`+
      `<td class="n cost">${eur(i.cost_eur)}</td>`+
      `<td><div class="tr"><div class="fill" style="width:${w}%"></div><div class="node" style="left:${w}%"></div></div></td></tr>`;
  }).join('');
}

fetch('/api/summary').then(r=>r.json()).then(s=>{
  $('gauges').innerHTML =
    gauge('Events', grp(s.events)) + gauge('Sessions', grp(s.sessions)) +
    gauge('Tokens in', cmp(s.tokens_in)) + gauge('Tokens out', cmp(s.tokens_out)) +
    gauge('Lines edited', grp(s.loc_added)) + gauge('Cost', eur(s.cost_eur), 'cost');
  [...$('gauges').children].forEach((el,i)=>{ el.style.animationDelay=(i*55)+'ms'; });

  if (s.rtk){
    const r=s.rtk, pct=r.saved_pct.toFixed(1);
    $('rtk').innerHTML =
      `<div class="rtk"><div><div class="hd">RTK · signal compression</div>`+
      `<div class="big">${cmp(r.tokens_saved)} <small>tokens saved</small></div>`+
      `<div class="sub">${pct}% reduction · ${grp(r.commands)} commands proxied · ${cmp(r.input_tokens)} → ${cmp(r.output_tokens)}</div>`+
      `<div class="meter"><i style="width:${pct}%"></i></div></div>`+
      `<div class="right">efficiency<b>${pct}%</b></div></div>`;
  }

  if (s.by_harness && s.by_harness.length){
    const HCOL = {'claude-code':'#54f0a0','codex':'#4fd6e6','opencode':'#f2b53d'};
    const pal = ['#54f0a0','#4fd6e6','#f2b53d','#b78bff','#8b93a7'];
    const hcol = (n,i) => HCOL[n] || pal[i%pal.length];
    const total = s.by_harness.reduce((a,h)=>a+h.cost_eur,0) || 1;
    const segs = s.by_harness.map((h,i)=>`<div class="seg" title="${esc(h.harness)} ${eur(h.cost_eur)}" style="width:${Math.max(1,100*h.cost_eur/total)}%;background:${hcol(h.harness,i)}"></div>`).join('');
    const leg = s.by_harness.map((h,i)=>`<span class="it"><span class="dot" style="background:${hcol(h.harness,i)}"></span>${esc(h.harness)} <b>${eur(h.cost_eur)}</b> · ${cmp(h.tokens_out)} out</span>`).join('');
    $('harness').innerHTML = `<div class="hd">Harnesses</div><div class="hbar">${segs}</div><div class="hleg">${leg}</div>`;
  }

  if (s.unpriced_models && s.unpriced_models.length){
    $('warn').innerHTML = `<div class="warn">⚠ unpriced models — cost is a floor, not exact: <b>${s.unpriced_models.map(esc).join(', ')}</b></div>`;
  }

  const aMax = Math.max(1, ...s.by_agent.map(a=>a.tokens_out));
  const mMax = Math.max(1, ...s.by_model.map(m=>m.tokens_out));
  $('agents').innerHTML = rows(s.by_agent.slice(0,14), 'agent', aMax);
  $('models').innerHTML = rows(s.by_model.slice(0,14), 'model', mMax);

  $('foot').innerHTML =
    `<span><span class="k">unattributed</span> ${s.unattributed_token_pct.toFixed(1)}%</span>`+
    `<span><span class="k">currency</span> EUR</span>`+
    `<span><span class="k">mode</span> snapshot · live updates land in M4</span>`;
  $('stat').textContent = grp(s.events) + ' events';
}).catch(()=>{ $('warn').innerHTML = '<div class="warn">failed to load /api/summary</div>'; });
</script>
</body>
</html>
"##;

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

    #[test]
    fn no_remote_assets_in_dashboard() {
        // Privacy gate (§16): the embedded UI must not reference any remote origin.
        assert!(!INDEX_HTML.contains("http://"));
        assert!(!INDEX_HTML.contains("https://"));
    }
}
