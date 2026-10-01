// Keeps the offline page so an installed Axon can say how to start the server when
// nothing answers on its port. The dashboard itself is never cached: it holds the
// owner session and is only meaningful while the server runs.
const PREFIX = "axon-offline-";
const CACHE = `${PREFIX}{{version}}`;
const OFFLINE = ["/offline.html", "/offline.js", "/style.css", "/pwa.css"];

self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(CACHE).then((cache) => cache.addAll(OFFLINE)).then(() => self.skipWaiting()));
});

// Another app served from this origin earlier keeps its caches; only older Axon ones go.
self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches.keys()
      .then((keys) => Promise.all(keys.filter((key) => key.startsWith(PREFIX) && key !== CACHE).map((key) => caches.delete(key))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const { request } = event;
  const path = new URL(request.url).pathname;
  if (request.mode === "navigate") {
    event.respondWith(fetch(request).catch(() => caches.match("/offline.html")));
  } else if (OFFLINE.includes(path)) {
    event.respondWith(fetch(request).catch(() => caches.match(path)));
  }
});
