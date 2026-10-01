//! The dashboard's static files, embedded in the binary, and the service worker that keeps
//! the offline page.

use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

pub(crate) const ASSETS: [(&str, &str, &str); 27] = [
    (
        "/activity.js",
        "text/javascript",
        include_str!("../ui/activity.js"),
    ),
    ("/app.js", "text/javascript", include_str!("../ui/app.js")),
    ("/arcs.js", "text/javascript", include_str!("../ui/arcs.js")),
    (
        "/board.js",
        "text/javascript",
        include_str!("../ui/board.js"),
    ),
    (
        "/brain.js",
        "text/javascript",
        include_str!("../ui/brain.js"),
    ),
    (
        "/brain-math.js",
        "text/javascript",
        include_str!("../ui/brain-math.js"),
    ),
    (
        "/brain-render.js",
        "text/javascript",
        include_str!("../ui/brain-render.js"),
    ),
    (
        "/brain-worker.js",
        "text/javascript",
        include_str!("../ui/brain-worker.js"),
    ),
    (
        "/context.js",
        "text/javascript",
        include_str!("../ui/context.js"),
    ),
    ("/dom.js", "text/javascript", include_str!("../ui/dom.js")),
    ("/md.js", "text/javascript", include_str!("../ui/md.js")),
    (
        "/overview.js",
        "text/javascript",
        include_str!("../ui/overview.js"),
    ),
    (
        "/signin.js",
        "text/javascript",
        include_str!("../ui/signin.js"),
    ),
    (
        "/settings.js",
        "text/javascript",
        include_str!("../ui/settings.js"),
    ),
    (
        "/settings-fed.js",
        "text/javascript",
        include_str!("../ui/settings-fed.js"),
    ),
    (
        "/settings-kit.js",
        "text/javascript",
        include_str!("../ui/settings-kit.js"),
    ),
    (
        "/settings-sections.js",
        "text/javascript",
        include_str!("../ui/settings-sections.js"),
    ),
    (
        "/settings-system.js",
        "text/javascript",
        include_str!("../ui/settings-system.js"),
    ),
    (
        "/settings.css",
        "text/css",
        include_str!("../ui/settings.css"),
    ),
    ("/send.js", "text/javascript", include_str!("../ui/send.js")),
    ("/style.css", "text/css", include_str!("../ui/style.css")),
    ("/pwa.css", "text/css", include_str!("../ui/pwa.css")),
    ("/pwa.js", "text/javascript", include_str!("../ui/pwa.js")),
    (
        "/offline.html",
        "text/html; charset=utf-8",
        include_str!("../ui/offline.html"),
    ),
    (
        "/offline.js",
        "text/javascript",
        include_str!("../ui/offline.js"),
    ),
    (
        "/usage.js",
        "text/javascript",
        include_str!("../ui/usage.js"),
    ),
    (
        "/manifest.webmanifest",
        "application/manifest+json",
        include_str!("../ui/manifest.webmanifest"),
    ),
];
/// The icons that let the page be installed as an app (Add to Dock, Install).
const ICONS: [(&str, &[u8]); 3] = [
    (
        "/apple-touch-icon.png",
        include_bytes!("../ui/apple-touch-icon.png"),
    ),
    ("/icon-192.png", include_bytes!("../ui/icon-192.png")),
    ("/icon-512.png", include_bytes!("../ui/icon-512.png")),
];

/// Every static route: the modules, styles, icons, offline page and service worker.
pub fn routes<S: Clone + Send + Sync + 'static>(mut router: Router<S>) -> Router<S> {
    let worker = service_worker();
    router = router.route(
        "/sw.js",
        get(move || async move { asset("text/javascript", worker) }),
    );
    for (path, content_type, body) in ASSETS {
        router = router.route(path, get(move || async move { asset(content_type, body) }));
    }
    for (path, body) in ICONS {
        let headers = [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "no-cache"),
        ];
        router = router.route(
            path,
            get(move || async move { (headers, body).into_response() }),
        );
    }
    router
}

/// Revalidated on every load, so the page never runs modules from an older binary.
fn asset(content_type: &'static str, body: &'static str) -> Response {
    let headers = [
        (header::CONTENT_TYPE, content_type),
        (header::CACHE_CONTROL, "no-cache"),
    ];
    (headers, body).into_response()
}

/// The service worker, named after the offline page it keeps: when that page changes, the
/// worker's bytes change, so browsers install it again and drop the old copy.
fn service_worker() -> &'static str {
    static WORKER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    WORKER.get_or_init(|| {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for (path, _, body) in ASSETS {
            if ["/offline.html", "/offline.js", "/style.css", "/pwa.css"].contains(&path) {
                body.hash(&mut hasher);
            }
        }
        include_str!("../ui/sw.js").replace("{{version}}", &format!("{:016x}", hasher.finish()))
    })
}
